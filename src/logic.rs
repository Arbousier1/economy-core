use crate::models::{
    AppConfig, TradeRequest, TradeResponse, TransactionRecord, 
    PlayerSalesHistory, EnvCache, Roundable, round_2, SalesRecord
};
use std::collections::HashMap;
use chrono::{Utc, Datelike, Local};
use reqwest::StatusCode;
use parking_lot::RwLock;

// [修复] 显式导入随机数相关模块，解决 E0425 错误
use rand::prelude::*;
use rand_distr::{Distribution, Normal};

// --- 子模块重新导出 ---
pub use self::pricing::PricingEngine;
pub use self::environment::calculate_current_env_index;

// =========================================================================
// 1. 数值常量与阈值 (Numerical Constants)
// =========================================================================

mod constants {
    pub const EPSILON_AMT: f64 = 1e-10;      // 最小交易量阈值
    pub const LAMBDA_MIN: f64 = 1e-9;        // λ 最小值
    pub const MIN_ENV_INDEX: f64 = 0.05;     // 市场环境硬底部
    pub const MOJANG_TIMEOUT_MS: u64 = 3000; // Mojang API 快速超时
}

// =========================================================================
// 2. 交易执行上下文 (Trade Context)
// =========================================================================

struct TradeContext<'a> {
    req: &'a TradeRequest,
    config: &'a AppConfig,
    holidays: &'a HashMap<String, bool>,
    player_history: &'a PlayerSalesHistory,
    env_cache: &'a RwLock<Option<EnvCache>>,
}

impl<'a> TradeContext<'a> {
    /// 执行核心交易流
    async fn execute(self, is_buy: bool, http_client: &reqwest::Client) -> (TradeResponse, Option<TransactionRecord>) {
        let now_ms = Utc::now().timestamp_millis();

        // 1. 异步身份验证 (Fast-fail)
        if !validate_player(self.req, self.config.is_online_mode, http_client).await {
            let mut resp = empty_resp(1.0, 0.0);
            resp.success = false;
            resp.message = "身份验证失败: ID无效或正版验证超时".into();
            return (resp, None);
        }

        // 2. 确定环境指数 (ε)
        let (env_idx, env_note) = self.resolve_env();

        // 3. 计算个体有效库存 (N_eff)
        let n_eff = self.calculate_n_eff(now_ms);

        // 4. 数学定价计算 (核心积分模型)
        let total_price = PricingEngine::calculate_price(
            self.req.base_price, env_idx, n_eff, self.req.amount, 
            self.req.decay_lambda, self.config.buy_premium, is_buy
        );

        // 5. 构造响应与流水记录
        let mut response = build_resp(total_price, self.req.amount, env_idx, n_eff);
        response.success = true;
        response.message = format!("交易成功 | 环境: {}", env_note);

        let record = self.create_record(&response, env_note, is_buy, now_ms);

        (response, record)
    }

    fn resolve_env(&self) -> (f64, String) {
        match self.req.manual_env_index {
            // [类型推导] 明确数值有效性
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
    // 基础边界校验
    if req.amount.abs() < constants::EPSILON_AMT || !req.amount.is_finite() {
        let mut resp = empty_resp(1.0, 0.0);
        resp.message = "交易量无效 (过小或非数值)".into();
        return (resp, None);
    }

    TradeContext { req, config, holidays, player_history, env_cache }
        .execute(is_buy, http_client).await
}

// =========================================================================
// 3. 定价引擎 (Numerical Stability Pricing)
// =========================================================================

pub mod pricing {
    use super::constants;
    use crate::models::{AppConfig, SalesRecord, Roundable};

    pub struct PricingEngine;

    impl PricingEngine {
        /// 入口：自动处理买卖差异逻辑
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
            
            // 混合计价：库存内部分享受衰减折扣，超出部分按溢价原价计算
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

        /// 核心积分公式优化：R = (P_max / λ) * [e^(-λ*n1) - e^(-λ*n2)]
        pub fn integral_revenue(base: f64, env: f64, n1: f64, amt: f64, lambda: f64) -> f64 {
            let p_max = base * env;
            let l = lambda.abs();

            // 稳定性修正：当 λ 极小时，使用线性极限计算防止 NaN
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
// 4. 环境模拟 (Atomic Environment Simulation)
// =========================================================================

pub mod environment {
    use super::constants;
    use crate::models::{AppConfig, EnvCache};
    use chrono::{Datelike, Local};
    use rand::prelude::*; 
    use rand_distr::{Distribution, Normal};
    use std::collections::HashMap;
    use parking_lot::RwLock;

    pub fn calculate_current_env_index(config: &AppConfig, holidays: &HashMap<String, bool>, 
                                      cache: &RwLock<Option<EnvCache>>) -> (f64, String) {
        let now = Local::now();
        let ts = now.timestamp();

        // 读锁快速路径 (DCL)
        if let Some(c) = cache.read().as_ref() {
            if c.timestamp == ts { return (c.index, c.note.clone()); }
        }

        let mut wg = cache.write();
        if let Some(c) = wg.as_ref() {
            if c.timestamp == ts { return (c.index, c.note.clone()); }
        }

        let (idx, note) = perform_calc(now, config, holidays);
        
        // [修复] 补全 last_update 字段
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

        // 季节修正逻辑
        if is_range(&md, &config.winter_start, &config.winter_end) {
            eps -= config.holiday_factor; tags.push("Winter");
        } else if is_range(&md, &config.summer_start, &config.summer_end) {
            eps -= config.holiday_factor; tags.push("Summer");
        }

        if now.weekday().number_from_monday() >= 6 && !is_off {
            eps -= config.weekend_factor; tags.push("Weekend");
        }

        // [修复] 高性能高斯噪声生成，处理 Result unwrap
        let mut r = thread_rng(); 
        let noise = Normal::new(0.0, config.noise_std.max(0.0001))
            .unwrap_or_else(|_| Normal::new(0.0, 1.0).unwrap()) // 容错回退
            .sample(&mut r);

        let note = if tags.is_empty() { "Normal".into() } else { tags.join("+") };
        ((eps + noise).max(constants::MIN_ENV_INDEX), note)
    }

    fn is_range(curr: &str, s: &str, e: &str) -> bool {
        if s <= e { curr >= s && curr <= e } 
        else { curr >= s || curr <= e } // 跨年修正
    }
}

// =========================================================================
// 5. 辅助工具 (Helpers)
// =========================================================================

fn build_resp(total: f64, amt: f64, env: f64, n_eff: f64) -> TradeResponse {
    let t = total.abs();
    // [修复] 补全 success/message/final_price 字段
    TradeResponse {
        success: true,
        message: String::new(), // 将由调用者填充
        final_price: t.round_2(),
        total_price: t.round_2(),
        unit_price_avg: if amt > constants::EPSILON_AMT { (t / amt).round_2() } else { 0.0 },
        env_index: (env * 1000.0).round() / 1000.0,
        effective_n: n_eff.round_2(),
    }
}

async fn validate_player(req: &TradeRequest, online: bool, client: &reqwest::Client) -> bool {
    if !online { return req.player_id.len() >= 32; }
    let url = format!("https://sessionserver.mojang.com/session/minecraft/profile/{}", req.player_id.replace("-", ""));
    
    // [类型推导] 明确闭包类型，避免编译器困惑
    client.get(&url)
          .timeout(std::time::Duration::from_millis(constants::MOJANG_TIMEOUT_MS))
          .send()
          .await
          .map(|r: reqwest::Response| r.status() == StatusCode::OK)
          .unwrap_or(false)
}

fn empty_resp(env: f64, n: f64) -> TradeResponse {
    // [修复] 补全失败响应的字段
    TradeResponse { 
        success: false,
        message: "交易无效".into(),
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