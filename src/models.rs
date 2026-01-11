use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use rustc_hash::FxHashMap;
use validator::Validate;

// =========================================================================
// 1. 宏定义 (适配 Postcard - 仅需 Serde)
// =========================================================================

/// 通用序列化宏：用于存盘和内部传输 (Postcard/Serde)
macro_rules! serializable {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Default)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

/// Web 模型宏：用于前端交互 (JSON/Serde + Validator)
macro_rules! web_model {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Default, Validate)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("无效的价格数值: {0}")]
    InvalidPrice(f64),
    #[error("物品 ID 不能为空")]
    EmptyId,
}

// =========================================================================
// 2. 工具方法 (Utilities)
// =========================================================================

pub trait Roundable {
    fn round_2(self) -> f64;
}

impl Roundable for f64 {
    #[inline(always)]
    fn round_2(self) -> f64 {
        (self * 100.0).round() / 100.0
    }
}

pub fn round_2(val: f64) -> f64 {
    val.round_2()
}

mod defaults {
    use std::borrow::Cow;
    pub const BUY_PREMIUM: f64 = 1.25;
    pub const UNKNOWN: Cow<'static, str> = Cow::Borrowed("Unknown");
    pub fn cow_unknown() -> Cow<'static, str> { UNKNOWN }
}

// =========================================================================
// 3. 核心配置与市场模型
// =========================================================================

serializable! {
    pub struct AppConfig {
        pub global_iota: f64,
        pub base_env_index: f64,
        pub noise_std: f64,
        pub weekend_factor: f64,
        pub holiday_factor: f64,
        pub public_holiday_factor: f64,
        pub buy_premium: f64,
        pub recovery_delta: f64,
        pub recovery_tau: f64,
        pub version: u32,
        pub port: u16,
        pub is_online_mode: bool,
        pub winter_start: Cow<'static, str>,
        pub winter_end: Cow<'static, str>,
        pub summer_start: Cow<'static, str>,
        pub summer_end: Cow<'static, str>,
    }
}

// 默认值实现
impl Default for AppConfig {
    fn default() -> Self {
        Self {
            global_iota: 0.0,
            base_env_index: 1.0,
            noise_std: 0.025,
            weekend_factor: 0.02,
            holiday_factor: 0.15,
            public_holiday_factor: 0.10,
            buy_premium: defaults::BUY_PREMIUM,
            recovery_delta: 0.05,
            recovery_tau: 3600.0,
            version: 1,
            port: 9981,
            is_online_mode: false,
            winter_start: "01-15".into(),
            winter_end: "02-20".into(),
            summer_start: "07-01".into(),
            summer_end: "08-31".into(),
        }
    }
}

serializable! {
    pub struct MarketItem {
        #[serde(alias = "key")] 
        pub id: String,
        #[serde(default = "defaults::cow_unknown")]
        pub name: Cow<'static, str>,
        pub base_price: f64,
        pub lambda: f64,
        #[serde(default)]
        pub n: f64,
        #[serde(default)]
        pub iota: f64,
    }
}

web_model! {
    pub struct MarketItemStatus {
        pub price: f64,
        pub buy_price: f64,
        pub neff: f64,
        pub base_price: f64,
    }
}

impl MarketItemStatus {
    pub fn new(price: f64, buy_price: f64, neff: f64, base_price: f64) -> Self {
        Self {
            price: price.round_2(),
            buy_price: buy_price.round_2(),
            neff: neff.round_2(),
            base_price,
        }
    }
}

serializable! {
    pub struct EnvCache {
        pub index: f64,
        pub last_update: i64,
        pub timestamp: i64,
        pub note: String,
    }
}

// =========================================================================
// 4. 交易历史记录
// =========================================================================

serializable! {
    pub struct SalesRecord {
        pub timestamp: i64,
        pub amount: f64,
        pub env_index: f64,
        #[serde(default)]
        pub price: f64,
    }
}

serializable! {
    pub struct PlayerSalesHistory {
        pub player_id: String,
        pub player_name: String,
        pub item_sales: FxHashMap<String, Vec<SalesRecord>>,
    }
}

serializable! {
    pub struct TransactionRecord {
        pub timestamp: i64,
        pub amount: f64,
        pub total_price: f64,
        pub avg_price: f64,
        pub env_index: f64,
        pub action: String,
        pub player_id: String,
        pub player_name: String,
        pub item_id: String,
        pub note: Cow<'static, str>,
    }
}

impl TransactionRecord {
    pub fn new(
        ts: i64, amt: f64, tp: f64, ap: f64,
        ei: f64, act: String, pid: String, 
        pnm: String, iid: String,
    ) -> Self {
        Self {
            timestamp: ts, amount: amt, total_price: tp, avg_price: ap,
            env_index: ei, action: act, player_id: pid, player_name: pnm, item_id: iid,
            note: "".into(),
        }
    }
}

// =========================================================================
// 5. API 请求/响应模型 (对齐 api.rs 和 logic.rs)
// =========================================================================

web_model! {
    pub struct TradeRequest {
        pub player_id: String,
        pub player_name: String,
        pub item_id: String,
        pub amount: f64,
        // logic.rs 需要的参数
        pub base_price: f64,
        pub decay_lambda: f64,
        pub iota: Option<f64>,
        pub manual_env_index: Option<f64>,
        pub is_preview: bool,
    }
}

web_model! {
    pub struct TradeResponse {
        pub success: bool,
        pub message: String,
        pub final_price: f64,
        pub total_price: f64,
        pub unit_price_avg: f64,
        pub env_index: f64,
        pub effective_n: f64,
    }
}

web_model! {
    pub struct BatchTradeRequest {
        pub player_id: String,
        pub player_name: String,
        pub requests: Vec<TradeRequest>, // 对应 api.rs 中的调用
    }
}

web_model! {
    pub struct BatchTradeResponse {
        pub results: Vec<TradeResponse>,
    }
}

web_model! {
    pub struct MarketPriceRequest {
        #[serde(default)]
        pub item_ids: Vec<String>,
    }
}