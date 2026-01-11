use serde::{Deserialize, Serialize};
use bincode::{Encode, Decode};
use std::borrow::Cow;
use rustc_hash::FxHashMap;
use validator::Validate;

// =========================================================================
// 1. 宏定义 (Macros)
// =========================================================================

/// 自动为结构体添加常用派生属性：Debug, Clone, Serde, Bincode
macro_rules! serializable {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode, Default)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

/// 专门用于 Web API 交互的模型（不包含 Bincode 编解码）
macro_rules! web_model {
    ($($item:tt)*) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Default, Validate)]
        #[serde(rename_all = "camelCase")]
        $($item)*
    };
}

// =========================================================================
// 2. 辅助特征与函数 (Utilities)
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

// =========================================================================
// 3. 配置模型 (Config)
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

// =========================================================================
// 4. 市场模型 (Market)
// =========================================================================

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

serializable! {
    pub struct EnvCache {
        pub index: f64,
        pub last_update: i64,
    }
}

// =========================================================================
// 5. 交易历史模型 (History)
// =========================================================================

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

// =========================================================================
// 6. API 请求与响应模型 (Web Requests)
// =========================================================================

web_model! {
    pub struct TradeRequest {
        pub player_id: String,
        pub player_name: String,
        pub item_id: String,
        pub amount: f64,
    }
}

web_model! {
    pub struct BatchTradeRequest {
        pub player_id: String,
        pub player_name: String,
        pub trades: Vec<BatchTradeItem>,
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
        pub results: Vec<String>, // 或者更具体的 Result 结构
    }
}

web_model! {
    pub struct MarketPriceRequest {
        pub item_ids: Vec<String>,
    }
}

// 对应逻辑中可能用到的 TradeResponse
web_model! {
    pub struct TradeResponse {
        pub success: bool,
        pub message: String,
        pub final_price: f64,
    }
}
