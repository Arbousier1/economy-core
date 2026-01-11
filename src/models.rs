use serde::{Deserialize, Serialize};
use bincode::{Encode, Decode};
use std::borrow::Cow;
use rustc_hash::FxHashMap; // 需要在 Cargo.toml 添加 rustc_hash = "2.1.0"

// =========================================================================
// 1. 宏与工具方法 (Macros & Utilities)
// =========================================================================

macro_rules! serializable {
    ($struct_name:ident) => {
        #[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode)]
        #[serde(rename_all = "camelCase")]
    };
}

macro_rules! web_model {
    ($struct_name:ident) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
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
// 2. 默认值与性能常量 (Performance Constants)
// =========================================================================

mod defaults {
    use std::borrow::Cow;
    pub const PORT: u16 = 9981;
    pub const VERSION: u32 = 1;
    pub const BUY_PREMIUM: f64 = 1.25;
    pub const UNKNOWN: Cow<'static, str> = Cow::Borrowed("Unknown");

    pub fn port() -> u16 { PORT }
    pub fn version() -> u32 { VERSION }
    pub fn buy_premium() -> f64 { BUY_PREMIUM }
    pub fn cow_unknown() -> Cow<'static, str> { UNKNOWN }
}

// =========================================================================
// 3. 配置模型 (Optimized Memory Layout)
// =========================================================================

serializable!(AppConfig);
pub struct AppConfig {
    // 按照内存步长由大到小排列，减少 Padding
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
            version: defaults::VERSION,
            port: defaults::PORT,
            is_online_mode: false,
            winter_start: "01-15".into(),
            winter_end: "02-20".into(),
            summer_start: "07-01".into(),
            summer_end: "08-31".into(),
        }
    }
}

// =========================================================================
// 4. 市场与物品模型 (Alloc-Reduction)
// =========================================================================

serializable!(MarketItem);
pub struct MarketItem {
    #[serde(alias = "key")] 
    pub id: String, // ID 由于经常从 Java 动态生成，保留 String
    #[serde(default = "defaults::cow_unknown")]
    pub name: Cow<'static, str>, // 使用 Cow 减少堆分配
    pub base_price: f64,
    pub lambda: f64,
    #[serde(default)]
    pub n: f64,
    #[serde(default)]
    pub iota: f64,
}

web_model!(MarketItemStatus);
pub struct MarketItemStatus {
    pub price: f64,
    pub buy_price: f64,
    pub neff: f64,
    pub base_price: f64,
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

// =========================================================================
// 5. 交易历史 (High Performance Collections)
// =========================================================================

serializable!(PlayerSalesHistory);
#[derive(Default)]
pub struct PlayerSalesHistory {
    pub player_id: String,
    pub player_name: String,
    // 使用 FxHashMap 提升 UUID/ID 类型的哈希查询效率
    pub item_sales: FxHashMap<String, Vec<SalesRecord>>,
}

serializable!(TransactionRecord);
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

impl TransactionRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timestamp: i64, amount: f64, total_price: f64, avg_price: f64,
        env_index: f64, action: String, player_id: String, 
        player_name: String, item_id: String,
    ) -> Self {
        Self {
            timestamp, amount, total_price, avg_price,
            env_index, action, player_id, player_name, item_id,
            note: "".into(),
        }
    }
}

// =========================================================================
// 6. 辅助特征 (Traits)
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