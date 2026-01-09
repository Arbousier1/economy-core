use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- 1. 全局配置模型 ---

/// 全局经济环境配置
/// 存储于 config.bin，控制整个插件的核心参数
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_port")]
    pub port: u16,

    /// 服务器运行模式：true 则强制 Mojang API 校验，false 则允许离线 UUID
    /// 该字段由服务器启动时加载，前端仅作只读展示
    pub is_online_mode: bool,

    // 基础经济参数
    pub base_env_index: f64,         // 基础环境指数 ε0 (默认 1.0)
    pub noise_std: f64,              // 高斯噪声标准差 (随机价格波动)
    pub weekend_factor: f64,         // 周末对环境指数的减损
    pub holiday_factor: f64,         // 寒暑假对环境指数的减损
    pub public_holiday_factor: f64,  // 法定节假日对环境指数的减损
    pub buy_premium: f64,            // 玩家买入时的溢价倍率 (Spread，如 1.25 代表买入比卖出贵 25%)
    
    // 时间恢复参数 (Time-Decay)
    pub recovery_delta: f64,         // 基础恢复率 δ (每单位时间恢复的比例)
    pub recovery_tau: f64,           // 时间常数 τ (如 3600 代表以小时为单位进行恢复)

    // 日期范围配置 (格式 "MM-DD"，用于环境因子判定)
    pub winter_start: String,       
    pub winter_end: String,         
    pub summer_start: String,       
    pub summer_end: String,

    #[serde(default = "default_version")]
    pub version: u32,               // 配置文件版本号
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: 9981,
            is_online_mode: false, // 默认为离线模式，可在配置文件或后台修改
            base_env_index: 1.0,
            noise_std: 0.025,
            weekend_factor: 0.02,
            holiday_factor: 0.15,
            public_holiday_factor: 0.10,
            buy_premium: 1.25,
            recovery_delta: 0.05,
            recovery_tau: 3600.0,
            winter_start: "01-15".to_string(),
            winter_end: "02-20".to_string(),
            summer_start: "07-01".to_string(),
            summer_end: "08-31".to_string(),
            version: 1,
        }
    }
}

// --- 2. 交易核心模型 ---

/// 交易请求
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TradeRequest {
    pub player_id: String,      // 玩家 UUID
    pub player_name: String,    // 玩家名 (用于离线模式识别及审计)
    pub item_id: String,        // 物品 ID (如 minecraft:iron_ingot)
    pub base_price: f64,        // P0: 物品的基础单价
    pub amount: f64,            // Δn: 交易数量
    pub decay_lambda: f64,      // λ: 价格衰减系数
    
    #[serde(default)]
    pub iota: Option<f64>,      // ι: 特别物价指数 (由外部市场干预)

    #[serde(default)]
    pub is_preview: bool,       // 是否为预览模式 (预览模式不产生持久化流水记录)

    #[serde(default)]
    pub manual_env_index: Option<f64>, // 管理员手动覆盖环境指数
}

/// 交易响应
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TradeResponse {
    pub total_price: f64,       // 本次交易计算后的总金额 (积分结果)
    pub unit_price_avg: f64,    // 平均单价
    pub env_index: f64,         // 计算时实际生效的环境指数 ε
    pub effective_n: f64,       // 计算时该物品在该玩家名下的有效偏移量 n_eff
}

/// 批量交易包装
#[derive(Deserialize, Debug, Clone)]
pub struct BatchTradeRequest {
    pub requests: Vec<TradeRequest>,
}

#[derive(Serialize, Debug, Clone)]
pub struct BatchTradeResponse {
    pub results: Vec<TradeResponse>,
}

// --- 3. 玩家持久化状态模型 ---

/// 玩家销售历史聚合
/// 用于在重启后恢复各玩家对各物品的 $n_{eff}$ 影响
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSalesHistory {
    pub player_id: String,
    pub player_name: String,
    /// 物品 ID 映射 历史成交片段
    pub item_sales: HashMap<String, Vec<SalesRecord>>,
}

/// 单次成交片段记录
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SalesRecord {
    pub timestamp: i64,         // 发生时间 (ms)
    /// 数量：正数代表卖出（压低价格），负数代表买入（推高价格，抵消之前的卖出）
    pub amount: f64,            
    pub env_index: f64,         // 记录成交时的环境指数
}

// --- 4. 统计与审计模型 ---

/// 存入二进制流水文件 (history.bin) 的结构
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRecord {
    pub timestamp: i64,
    pub amount: f64,
    pub total_price: f64,
    pub avg_price: f64,
    pub env_index: f64,
    pub action: String,         // "BUY" 或 "SELL"
    pub player_id: String,
    pub player_name: String,
    pub item_id: String,
    pub note: String,           // 包含 Online/Offline 状态及节日备注
}

// --- 5. 外部 API 交互模型 ---

#[derive(Deserialize, Debug, Clone)]
pub struct HolidayApiResponse {
    #[serde(default)]
    pub days: Vec<HolidayItem>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct HolidayItem {
    pub date: String,            // "YYYY-MM-DD"
    #[serde(rename = "isOffDay")]
    pub is_off_day: bool,
}

/// 市场快照信息 (用于前端 Market 列表展示)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketItem {
    pub id: String,
    pub name: String,
    pub base_price: f64,
    pub lambda: f64,
    pub n: f64,                  // 当前全服或默认偏移量 (预览用)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMarketRequest {
    pub items: Vec<MarketItem>,
}

// --- 辅助默认值函数 ---

fn default_port() -> u16 { 9981 }
fn default_version() -> u32 { 1 }