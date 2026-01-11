use serde::{Deserialize, Serialize};
use bincode::{Encode, Decode};
use std::borrow::Cow;
use rustc_hash::FxHashMap;
use validator::Validate;

// =========================================================================
// 1. 宏定义 (修正后的语法)
// =========================================================================

macro_rules! serializable {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode, Default)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

macro_rules! web_model {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Default, Validate)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

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

// =========================================================================
// 2. 核心模型 (配置、市场、历史)
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

serializable! {
    pub struct MarketItem {
        #[serde(alias = "key")] 
        pub id: String,
        pub name: Cow<'static, str>,
        pub base_price: f64,
        pub lambda: f64,
        pub n: f64,
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
        Self { price, buy_price, neff, base_price }
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

serializable! {
    pub struct SalesRecord {
        pub timestamp: i64,
        pub price: f64,
        pub amount: f64,
        pub env_index: f64,
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
    pub fn new(ts: i64, amt: f64, tp: f64, ap: f64, ei: f64, act: String, pid: String, pnm: String, iid: String) -> Self {
        Self {
            timestamp: ts, amount: amt, total_price: tp, avg_price: ap,
            env_index: ei, action: act, player_id: pid, player_name: pnm,
            item_id: iid, note: "".into(),
        }
    }
}

// =========================================================================
// 3. API 请求/响应模型 (修复字段缺失)
// =========================================================================

web_model! {
    pub struct TradeRequest {
        pub player_id: String,
        pub player_name: String,
        pub item_id: String,
        pub amount: f64,
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
        pub trades: Vec<BatchTradeItem>,
        pub requests: Vec<BatchTradeItem>, 
    }
}

web_model! {
    pub struct BatchTradeItem {
        pub item_id: String,
        pub amount: f64,
    }
}

web_model! {
    pub struct BatchTradeResponse {
        pub results: Vec<String>,
    }
}

web_model! {
    pub struct MarketPriceRequest {
        pub item_ids: Vec<String>,
    }
}
