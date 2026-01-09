mod models;
mod logic;
mod api;

use axum::{
    routing::{get, post},
    Router,
};
use parking_lot::RwLock;
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{BufWriter, Cursor, Write},
    net::SocketAddr,
    sync::Arc,
};
use tokio::sync::mpsc;
// 关键：引入 ServeDir 用于托管前端网页
use tower_http::{cors::CorsLayer, services::ServeDir}; 
use tracing::{error, info, warn};
use chrono::{Datelike, Local};

// 引入模块内容
use models::*;
use api::*;

// --- 常量配置 ---
const CONFIG_FILE: &str = "config.bin";
const HISTORY_FILE: &str = "history.bin";
const CHANNEL_CAPACITY: usize = 10_000;

// --- 全局状态定义 ---
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub holidays: Arc<RwLock<HashMap<String, bool>>>,
    pub tx: mpsc::Sender<TransactionRecord>,
    pub history_cache: Arc<RwLock<Vec<TransactionRecord>>>,
    
    // --- 新增：用于缓存从 Java 插件同步过来的真实市场数据 ---
    // 这个字段必须存在，因为 api.rs 中的 sync_market/get_market 依赖它
    pub market_cache: Arc<RwLock<Vec<MarketItem>>>, 
}

// --- 后台持久化任务 ---
async fn background_writer_task(
    mut rx: mpsc::Receiver<TransactionRecord>,
    history_cache: Arc<RwLock<Vec<TransactionRecord>>>,
) {
    let file = OpenOptions::new().create(true).append(true).open(HISTORY_FILE);
    let mut writer = match file {
        Ok(f) => BufWriter::new(f),
        Err(e) => {
            error!("❌ 致命错误: 无法打开历史记录文件 {:?}，数据将不会被保存！", e);
            return;
        }
    };

    info!("💾 后台写入任务已启动");

    while let Some(record) = rx.recv().await {
        // 1. 写入磁盘
        if let Err(e) = bincode::serialize_into(&mut writer, &record) {
            error!("❌ 写入磁盘失败: {:?}", e);
        } else {
            let _ = writer.flush();
        }

        // 2. 更新内存缓存 (仅保留最新的 200 条)
        let mut cache = history_cache.write();
        cache.push(record);
        if cache.len() > 200 {
            cache.remove(0);
        }
    }
}

// --- 数据加载辅助类 ---
struct Storage;
impl Storage {
    fn load_config() -> AppConfig {
        if let Ok(data) = fs::read(CONFIG_FILE) {
            if let Ok(cfg) = bincode::deserialize(&data) {
                return cfg;
            }
        }
        
        warn!("⚠️ 配置文件不存在，生成默认配置...");
        let default_cfg = AppConfig::default();
        if let Ok(data) = bincode::serialize(&default_cfg) {
            let _ = fs::write(CONFIG_FILE, data);
        }
        default_cfg
    }

    fn load_history() -> Vec<TransactionRecord> {
        let mut records = Vec::new();
        if let Ok(data) = fs::read(HISTORY_FILE) {
            let mut cursor = Cursor::new(data);
            while let Ok(rec) = bincode::deserialize_from::<_, TransactionRecord>(&mut cursor) {
                records.push(rec);
            }
        }
        
        let len = records.len();
        if len > 200 {
            records.split_off(len - 200)
        } else {
            records
        }
    }
}

// --- 外部 API 调用 ---
async fn fetch_holidays() -> HashMap<String, bool> {
    let year = Local::now().year();
    let url = format!("https://holiday.cyi.me/api/holidays?year={}", year);
    let mut map = HashMap::new();

    info!("🌍 正在同步 {} 年节假日数据...", year);
    
    match reqwest::get(&url).await {
        Ok(resp) => {
            match resp.json::<HolidayApiResponse>().await {
                Ok(data) => {
                    for item in data.days {
                        map.insert(item.date, item.is_off_day);
                    }
                    info!("✅ 节假日同步成功: 获取到 {} 天数据", map.len());
                },
                Err(e) => warn!("⚠️ 节假日 JSON 解析失败: {:?}", e),
            }
        },
        Err(e) => warn!("⚠️ 无法连接节假日 API ({:?})，系统将仅使用周末逻辑。", e),
    }
    map
}

// --- 主程序入口 ---
#[tokio::main]
async fn main() {
    // 1. 初始化日志
    tracing_subscriber::fmt::init();
    info!("🚀 Economy Core 正在启动...");

    // 2. 加载数据
    let config_data = Storage::load_config();
    let port = config_data.port; 
    
    // 3. 构建各种状态
    let config = Arc::new(RwLock::new(config_data));
    let history_cache = Arc::new(RwLock::new(Storage::load_history()));
    let holidays = Arc::new(RwLock::new(fetch_holidays().await));
    // 初始化市场缓存（开始是空的，等待 Java 推送）
    let market_cache = Arc::new(RwLock::new(Vec::new())); 

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);

    // 4. 启动后台任务
    tokio::spawn(background_writer_task(rx, history_cache.clone()));

    let shared_state = AppState {
        config,
        holidays,
        tx,
        history_cache,
        market_cache, // 放入 State
    };

    // 5. 定义路由
    let app = Router::new()
        // --- 核心计算 ---
        .route("/calculate_sell", post(handle_sell))
        .route("/calculate_buy", post(handle_buy))
        .route("/batch_sell", post(handle_batch_sell))
        
        // --- 市场同步 (MC <-> Web) ---
        .route("/api/market/sync", post(sync_market)) // Java 插件推送数据到这里
        .route("/api/market", get(get_market))        // 前端网页从这里拉取数据
        
        // --- 配置与历史 ---
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/history", get(get_history))
        
        // --- 静态文件服务 ---
        // 访问 / 自动寻找 static/index.html
        .nest_service("/", ServeDir::new("static"))
        
        .layer(CorsLayer::permissive())
        .with_state(shared_state);

    // 6. 绑定端口 (强制 127.0.0.1)
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("❌ 端口 {} 绑定失败: {:?}", port, e);
            return;
        }
    };

    info!("✨ 服务器运行中: http://{}", addr);
    info!("📊 前端控制台: http://{}/index.html", addr);

    if let Err(e) = axum::serve(listener, app).await {
        error!("❌ 服务器运行出错: {:?}", e);
    }
}