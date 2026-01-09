use serde::{Deserialize, Serialize};

// --- 1. 全局配置模型 ---

/// 全局经济环境配置
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default = "default_port")]
    pub port: u16,                  // 服务器端口

    pub base_env_index: f64,        // 基础环境指数 (默认 1.0)
    pub noise_std: f64,             // 随机波动标准差
    pub weekend_factor: f64,        // 周末减益系数
    pub holiday_factor: f64,        // 寒暑假减益系数
    pub public_holiday_factor: f64, // 法定节假日减益系数
    pub buy_premium: f64,           // 玩家买入时的溢价倍率 (如 1.25)
    
    // 日期范围配置 (格式 "MM-DD")
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
            base_env_index: 1.0,
            noise_std: 0.025,
            weekend_factor: 0.02,
            holiday_factor: 0.15,
            public_holiday_factor: 0.10,
            buy_premium: 1.25,
            winter_start: "01-15".to_string(),
            winter_end: "02-20".to_string(),
            summer_start: "07-01".to_string(),
            summer_end: "08-31".to_string(),
            version: 1,
        }
    }
}

fn default_port() -> u16 { 9981 }
fn default_version() -> u32 { 1 }

// --- 2. 交易核心模型 ---

/// 交易请求
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TradeRequest {
    pub base_price: f64,      // p0: 基础单价
    pub start_n: f64,         // n: 当前累计交易量
    pub amount: f64,          // Δn: 本次交易数量
    pub decay_lambda: f64,    // λ: 价格衰减系数
    
    #[serde(default)]
    pub is_preview: bool,     // 是否为预览模式 (不记录流水)

    #[serde(default)]
    pub manual_env_index: Option<f64>, // 手动锁定环境指数 (管理员测试用)
}

/// 交易响应
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TradeResponse {
    pub total_price: f64,     // 本次交易总额
    pub unit_price_avg: f64,  // 本次交易平均单价
    pub env_index: f64,       // 计算时生效的环境指数
}

impl Default for TradeResponse {
    fn default() -> Self {
        Self {
            total_price: 0.0,
            unit_price_avg: 0.0,
            env_index: 1.0,
        }
    }
}

/// 批量交易请求
#[derive(Deserialize, Debug, Clone)]
pub struct BatchTradeRequest {
    pub requests: Vec<TradeRequest>,
}

/// 批量交易响应
#[derive(Serialize, Debug, Clone)]
pub struct BatchTradeResponse {
    pub results: Vec<TradeResponse>,
}

// --- 3. 持久化记录模型 ---

/// 存入二进制文件的交易流水记录
/// 严谨性：按字节大小降序排列字段以优化内存对齐
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRecord {
    pub timestamp: i64,       // 8 bytes - 时间戳 (ms)
    pub amount: f64,          // 8 bytes - 交易数量
    pub total_price: f64,     // 8 bytes - 总金额
    pub avg_price: f64,       // 8 bytes - 平均单价
    pub env_index: f64,       // 8 bytes - 环境指数
    pub action: String,       // "BUY" 或 "SELL"
    pub note: String,         // 备注 (Holiday, Weekend 等)
}

// --- 4. 外部 API 适配模型 ---

/// 节假日 API 响应结构
#[derive(Deserialize, Debug, Clone)]
pub struct HolidayApiResponse {
    #[serde(default)]
    pub days: Vec<HolidayItem>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct HolidayItem {
    pub date: String,            // "YYYY-MM-DD"
    #[serde(rename = "isOffDay")]
    pub is_off_day: bool,        // 是否为休息日
}

// --- 5. 真实市场数据同步模型 ---

/// 单个物品的市场快照
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketItem {
    pub id: String,          // 物品ID
    pub name: String,        // 显示名称
    pub base_price: f64,     // 基础单价 P0
    pub n: f64,              // 当前真实库存量
    pub lambda: f64,         // 衰减系数
}

/// 接收同步请求包装
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMarketRequest {
    pub items: Vec<MarketItem>,
}