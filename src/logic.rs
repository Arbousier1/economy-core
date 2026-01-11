use crate::models::{
    AppConfig, TradeRequest, TradeResponse, TransactionRecord, 
    PlayerSalesHistory, EnvCache, Roundable // 移除了未使用的 SalesRecord (已移至 pricing 模块)
};
use std::collections::HashMap;
use chrono::{Utc, Local}; // 移除了未使用的 Datelike
use reqwest::StatusCode;
use parking_lot::RwLock;

// --- 子模块重新导出 ---
pub use self::pricing::PricingEngine;
pub use self::environment::calculate_current_env_index;

// =========================================================================
// 1. 数值常量与阈值
// =========================================================================

mod constants {
    pub const EPSILON_AMT: f64 = 1e-10;
    pub const LAMBDA_MIN: f64 = 1e-9;
    pub const MIN_ENV_INDEX: f64 = 0.05;
    pub const MOJANG_TIMEOUT_MS: u64 = 3000;
}

// =========================================================================
// 2. 交易执行上下文
// =========================================================================

struct TradeContext<'a> {
    req: &'a TradeRequest,
    config: &'a AppConfig,
    holidays: &'a HashMap<String, bool>,
    player_history: &'a PlayerSalesHistory,
    env_cache: &'a RwLock<Option<EnvCache>>,
}

impl<'a> TradeContext<'a> {
    async fn execute(self, is_buy: bool, http_client: &reqwest::Client) -> (TradeResponse, Option<TransactionRecord>) {
        let now_ms = Utc::now().timestamp_millis();

        // 1. 验证
        if !validate_player(self.req, self.config.is_online_mode, http_client).await {
            let mut resp = empty_resp(1.0, 0.0);
            resp.success = false;
            resp.message = "身份验证失败".into();
            return (resp, None);
        }

        // 2. 环境
        let (env_idx, env_note) = self.resolve_env();

        // 3. 库存
        let n_eff = self.calculate_n_eff(now_ms);

        // 4. 定价
        let total_price = PricingEngine::calculate_price(
            self.req.base_price, env_idx, n_eff, self.req.amount, 
            self.req.decay_lambda, self.config.buy_premium, is_buy
        );

        // 5. 响应
        let mut response = build_resp(total_price, self.req.amount, env_idx, n_eff);
        response.success = true;
        response.message = format!("交易成功 ({})", env_note);

        let record = self.create_record(&response, env_note, is_buy, now_ms);

        (response, record)
    }

    fn resolve_env(&self) -> (f64, String) {
        match self.req.manual_env_index {
            Some(m) if m > 0.0 && m.is_finite() => (m, "Manual".into()),
            _ => calculate_current_env_index(self.config, self.holidays, self.env_cache),
        }
    }

    fn calculate_n_eff(&self, now_ms: i64) -> f64 {
        let history = self.player_history.item_sales.get(&self.req.item_id)
            .map(|v| v.as_slice()).unwrap_or(&[]);
        let iota = self.req.iota.unwrap_or(self.config.global_iota);
        PricingEngine::calculate_effective_n(history, iota, self.config, now_ms)
    }

    fn create_record(&self, resp: &TradeResponse, note: String, is_buy: bool, ts: i64) -> Option<TransactionRecord> {
        if self.req.is_preview || resp.total_price <= 0.0 { return None; }
        
        Some(TransactionRecord::new(
            ts, self.req.amount, resp.total_price, resp.unit_price_avg,
            resp.env_index, if is_buy { "BUY".into() } else { "SELL".into() },
            self.req.player_id.clone(), self.req.player_name.clone(), self.req.item_id.clone()
        ).with_note(note))
    }
}

pub async fn execute_trade_logic(
    req: &TradeRequest, config: &AppConfig, holidays: &HashMap<String, bool>,
    player_history: &PlayerSalesHistory, is_buy: bool,
    env_cache: &RwLock<Option<EnvCache>>, http_client: &reqwest::Client,
) -> (TradeResponse, Option<TransactionRecord>) {
    if req.amount.abs() < constants::EPSILON_AMT || !req.amount.is_finite() {
        let mut resp = empty_resp(1.0, 0.0);
        resp.message = "交易量无效".into();
        return (resp, None);
    }

    TradeContext { req, config, holidays, player_history, env_cache }
        .execute(is_buy, http_client).await
}

// =========================================================================
// 3. 定价引擎
// =========================================================================

pub mod pricing {
    use super::constants;
    // [修复] SalesRecord 移到这里引入，因为只有这里用到
    use crate::models::{AppConfig, SalesRecord, Roundable};

    pub struct PricingEngine;

    impl PricingEngine {
        pub fn calculate_price(base: f64, env: f64, n: f64, amt: f64, lambda: f64, premium: f64, is_buy: bool) -> f64 {
            if is_buy {
                Self::buy_logic(base * premium, env, n, amt, lambda)
            } else {
                Self::integral_revenue(base, env, n, amt, lambda)
            }
        }

        fn buy_logic(base: f64, env: f64, n_eff: f64, amt: f64, lambda: f64) -> f64 {
            let n_start = (n_eff - amt).max(0.0);
            let discount_amt = (n_eff - n_start).max(0.0);
            
            if discount_amt < amt {
                let premium_amt = amt - discount_amt;
                let p_discount = if discount_amt > constants::EPSILON_AMT {
                    Self::integral_revenue(base, env, n_start, discount_amt, lambda)
                } else { 0.0 };
                p_discount + (premium_amt * base * env)
            } else {
                Self::integral_revenue(base, env, n_start, amt, lambda)
            }
        }

        pub fn integral_revenue(base: f64, env: f64, n1: f64, amt: f64, lambda: f64) -> f64 {
            let p_max = base * env;
            let l = lambda.abs();

            if l < constants::LAMBDA_MIN {
                return (p_max * amt).round_2();
            }

            let n2 = n1 + amt;
            let revenue = (p_max / l) * ((-l * n1).exp() - (-l * n2).exp());
            revenue.max(0.0).round_2()
        }

        pub fn calculate_effective_n(history: &[SalesRecord], iota: f64, config: &AppConfig, now_ms: i64) -> f64 {
            let n_sum: f64 = history.iter().map(|r| {
                let dt = ((now_ms - r.timestamp) as f64 / 1000.0).max(0.0);
                let decay = if config.recovery_delta > 0.0 {
                    (-config.recovery_delta * (dt / config.recovery_tau)).exp()
                } else { 1.0 };
                r.amount * decay
            }).sum();

            (n_sum + iota).max(0.0)
        }
    }
}

// =========================================================================
// 4. 环境模拟
// =========================================================================

pub mod environment {
    use super::constants;
    use crate::models::{AppConfig, EnvCache};
    use chrono::{Datelike, Local};
    use std::collections::HashMap;
    use parking_lot::RwLock;
    
    // [核心修复] 将 rand 引入移到这里，因为 mod 是独立作用域
    use rand::thread_rng; 
    use rand_distr::{Distribution, Normal};

    pub fn calculate_current_env_index(config: &AppConfig, holidays: &HashMap<String, bool>, 
                                      cache: &RwLock<Option<EnvCache>>) -> (f64, String) {
        let now = Local::now();
        let ts = now.timestamp();

        if let Some(c) = cache.read().as_ref() {
            if c.timestamp == ts { return (c.index, c.note.clone()); }
        }

        let mut wg = cache.write();
        if let Some(c) = wg.as_ref() {
            if c.timestamp == ts { return (c.index, c.note.clone()); }
        }

        let (idx, note) = perform_calc(now, config, holidays);
        *wg = Some(EnvCache { 
            index: idx, 
            note: note.clone(), 
            timestamp: ts,
            last_update: ts 
        });
        (idx, note)
    }

    fn perform_calc(now: chrono::DateTime<Local>, config: &AppConfig, hols: &HashMap<String, bool>) -> (f64, String) {
        let mut eps = config.base_env_index;
        let mut tags = Vec::new();
        let ymd = now.format("%Y-%m-%d").to_string();
        let md = now.format("%m-%d").to_string();

        let is_off = hols.get(&ymd).copied().unwrap_or(false);
        if is_off {
            eps -= config.public_holiday_factor;
            tags.push("Holiday");
        }

        if is_range(&md, &config.winter_start, &config.winter_end) {
            eps -= config.holiday_factor; tags.push("Winter");
        } else if is_range(&md, &config.summer_start, &config.summer_end) {
            eps -= config.holiday_factor; tags.push("Summer");
        }

        if now.weekday().number_from_monday() >= 6 && !is_off {
            eps -= config.weekend_factor; tags.push("Weekend");
        }

        // [修复] 现在这里的 thread_rng 能够被找到了
        let mut r = thread_rng(); 
        let noise = Normal::new(0.0, config.noise_std.max(0.0001))
            .unwrap_or_else(|_| Normal::new(0.0, 1.0).unwrap())
            .sample(&mut r);

        let note = if tags.is_empty() { "Normal".into() } else { tags.join("+") };
        ((eps + noise).max(constants::MIN_ENV_INDEX), note)
    }

    fn is_range(curr: &str, s: &str, e: &str) -> bool {
        if s <= e { curr >= s && curr <= e } 
        else { curr >= s || curr <= e }
    }
}

// =========================================================================
// 5. 辅助工具
// =========================================================================

fn build_resp(total: f64, amt: f64, env: f64, n_eff: f64) -> TradeResponse {
    let t = total.abs();
    TradeResponse {
        success: true,
        message: String::new(),
        final_price: t.round_2(),
        total_price: t.round_2(),
        unit_price_avg: if amt.abs() > constants::EPSILON_AMT { (t / amt).round_2() } else { 0.0 },
        env_index: (env * 1000.0).round() / 1000.0,
        effective_n: n_eff.round_2(),
    }
}

async fn validate_player(req: &TradeRequest, online: bool, client: &reqwest::Client) -> bool {
    if !online { return req.player_id.len() >= 32; }
    let url = format!("https://sessionserver.mojang.com/session/minecraft/profile/{}", req.player_id.replace("-", ""));
    
    client.get(&url)
          .timeout(std::time::Duration::from_millis(constants::MOJANG_TIMEOUT_MS))
          .send()
          .await
          .map(|r: reqwest::Response| r.status() == StatusCode::OK)
          .unwrap_or(false)
}

fn empty_resp(env: f64, n: f64) -> TradeResponse {
    TradeResponse { 
        success: false,
        message: "无效交易".into(),
        final_price: 0.0,
        total_price: 0.0, 
        unit_price_avg: 0.0, 
        env_index: env, 
        effective_n: n 
    }
}

impl TransactionRecord {
    fn with_note(mut self, note: String) -> Self { self.note = note.into(); self }
}
