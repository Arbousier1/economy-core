use serde::{Deserialize, Serialize};
use bincode::{Encode, Decode};
use std::borrow::Cow;
use rustc_hash::FxHashMap;

// =========================================================================
// 1. 宏定义 (Macros) - 修正后的写法
// =========================================================================

macro_rules! serializable {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

macro_rules! web_model {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

// =========================================================================
// 2. 基础工具与函数 (Utilities)
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

// 补充报错中找不到的 models::round_2 函数
pub fn round_2(val: f64) -> f64 {
    val.round_2()
}

// =========================================================================
// 3. 模型定义 (Models)
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            global_iota: 0.0,
            base_env_index: 1.0,
            noise_std: 0.025,
            weekend_factor: 0.02,
            holiday_factor: 0.15,
            public_holiday_factor: 0.10,
            buy_premium: 1.25,
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
        pub name: Cow<'static, str>,
        pub base_price: f64,
        pub lambda: f64,
        #[serde(default)]
        pub n: f64,
        #[serde(default)]
        pub iota: f64,
    }
}

// 补全缺失的 SalesRecord
serializable! {
    pub struct SalesRecord {
        pub timestamp: i64,
        pub price: f64,
        pub amount: f64,
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

// 补全 API 需要的 Request/Response
web_model! {
    pub struct TradeRequest {
        pub player_id: String,
        pub player_name: String,
        pub item_id: String,
        pub amount: f64,
    }
}

// 补全 EnvCache
serializable! {
    pub struct EnvCache {
        pub index: f64,
        pub last_update: i64,
    }
}
