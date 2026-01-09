mod models;
mod logic;
mod api;

use axum::{
    routing::{get, post},
    Router,
};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Cursor, Write}; 
use std::net::SocketAddr;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::Duration;
use tokio::{sync::mpsc, signal};
use tower_http::{cors::CorsLayer, services::ServeDir, timeout::TimeoutLayer};
use tracing::{error, info};
use chrono::Local;

// 引入内部模块内容
use crate::models::*;

// --- 常量配置 ---
const CONFIG_FILE: &str = "config.bin";
const HISTORY_FILE: &str = "history.bin";
const PLAYER_DATA_FILE: &str = "player_data.bin"; 
const CHANNEL_CAPACITY: usize = 20_000; 
const MAX_CACHE_SIZE: usize = 1000;    

/// 全局系统指标监控
pub struct SystemMetrics {
    pub total_trades: AtomicU64,      
    pub write_failures: AtomicU64,    
    pub channel_dropped: AtomicU64,   
    pub start_time: i64,              
}

/// 共享应用状态
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub holidays: Arc<RwLock<HashMap<String, bool>>>,
    pub tx: mpsc::Sender<TransactionRecord>,
    pub history_cache: Arc<RwLock<Vec<TransactionRecord>>>,
    pub market_cache: Arc<RwLock<Vec<MarketItem>>>,
    pub metrics: Arc<SystemMetrics>,
    pub player_histories: Arc<RwLock<HashMap<String, PlayerSalesHistory>>>,
}

// --- 1. 后台持久化协程 (Disk IO Worker) ---

/// 负责从通道接收交易记录，利用 BufWriter 批量写入磁盘，减少 IO 系统调用
async fn background_writer_task(
    mut rx: mpsc::Receiver<TransactionRecord>,
    history_cache: Arc<RwLock<Vec<TransactionRecord>>>,
    metrics: Arc<SystemMetrics>,
) {
    let file_res = OpenOptions::new()
        .create(true)
        .append(true)
        .open(HISTORY_FILE);

    let mut writer = match file_res {
        Ok(f) => BufWriter::with_capacity(128 * 1024, f), // 128KB 缓冲区
        Err(e) => {
            error!("🚨 核心历史文件打开失败: {}", e);
            return;
        }
    };

    let mut flush_interval = tokio::time::interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            // 接收新记录
            record_opt = rx.recv() => {
                match record_opt {
                    Some(record) => {
                        // 同步更新最近交易缓存 (内存)
                        {
                            let mut cache = history_cache.write();
                            cache.push(record.clone());
                            if cache.len() > MAX_CACHE_SIZE { cache.remove(0); }
                        }

                        // 使用 bincode 高效序列化到文件流
                        if let Err(e) = bincode::serialize_into(&mut writer, &record) {
                            error!("❌ 交易记录序列化失败: {:?}", e);
                            metrics.write_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    None => {
                        info!("👋 写入通道已关闭，正在执行最终刷盘...");
                        let _ = writer.flush();
                        break; 
                    }
                }
            }
            // 定时刷盘，防止意外掉电丢失太多数据
            _ = flush_interval.tick() => {
                let _ = writer.flush(); 
            }
        }
    }
}

// --- 2. 存储辅助引擎 ---

struct Storage;
impl Storage {
    /// 加载配置文件
    fn load_config() -> AppConfig {
        if let Ok(data) = fs::read(CONFIG_FILE) {
            if let Ok(cfg) = bincode::deserialize::<AppConfig>(&data) { return cfg; }
        }
        let default_cfg = AppConfig::default();
        Self::atomic_save_config(&default_cfg);
        default_cfg
    }

    /// 原子化保存配置（先写临时文件再重命名，防止写入崩溃导致原文件损坏）
    pub fn atomic_save_config(cfg: &AppConfig) {
        let temp_path = format!("{}.tmp", CONFIG_FILE);
        if let Ok(data) = bincode::serialize(cfg) {
            if fs::write(&temp_path, data).is_ok() {
                let _ = fs::rename(&temp_path, CONFIG_FILE).unwrap_or_else(|e| {
                    error!("❌ 重命名配置文件失败: {}", e);
                });
            }
        }
    }

    /// 加载历史记录末尾部分至内存
    fn load_history() -> Vec<TransactionRecord> {
        let mut records = Vec::with_capacity(MAX_CACHE_SIZE);
        if let Ok(data) = fs::read(HISTORY_FILE) {
            let mut cursor = Cursor::new(data);
            while let Ok(rec) = bincode::deserialize_from::<_, TransactionRecord>(&mut cursor) {
                records.push(rec);
            }
        }
        if records.len() > MAX_CACHE_SIZE {
            records.split_off(records.len() - MAX_CACHE_SIZE)
        } else {
            records
        }
    }

    /// 加载玩家抛售历史（n_eff 计算的关键）
    fn load_player_data() -> HashMap<String, PlayerSalesHistory> {
        if let Ok(data) = fs::read(PLAYER_DATA_FILE) {
            if let Ok(map) = bincode::deserialize(&data) { return map; }
        }
        HashMap::new()
    }

    fn save_player_data(data: &HashMap<String, PlayerSalesHistory>) {
        if let Ok(bytes) = bincode::serialize(data) {
            let _ = fs::write(PLAYER_DATA_FILE, bytes);
        }
    }
}

// --- 3. 停机信号监听 ---

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("无法安装 Ctrl+C 处理器");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("无法安装信号处理器")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("📥 接收到 Ctrl+C，开始安全停机..."),
        _ = terminate => info!("📥 接收到 SIGTERM，开始安全停机..."),
    }
}

// --- 4. 主程序入口 ---

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt::init();
    info!("🚀 Economy Core (Ver 2.0) 正在启动...");

    // 1. 初始化指标监控
    let metrics = Arc::new(SystemMetrics {
        total_trades: AtomicU64::new(0),
        write_failures: AtomicU64::new(0),
        channel_dropped: AtomicU64::new(0),
        start_time: Local::now().timestamp(),
    });

    // 2. 加载持久化数据
    let config_data = Storage::load_config();
    let port = config_data.port;
    
    let config = Arc::new(RwLock::new(config_data));
    let history_cache = Arc::new(RwLock::new(Storage::load_history()));
    let holidays = Arc::new(RwLock::new(api::fetch_holidays().await));
    let market_cache = Arc::new(RwLock::new(Vec::new()));
    let player_histories = Arc::new(RwLock::new(Storage::load_player_data()));

    // 3. 开启后台异步持久化通道
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    
    let writer_handle = tokio::spawn(background_writer_task(
        rx, 
        history_cache.clone(), 
        metrics.clone()
    ));
    
    // 4. 开启节假日自动更新任务
    tokio::spawn(api::holiday_refresh_task(holidays.clone()));

    // 5. 构造应用状态
    let shared_state = AppState {
        config,
        holidays,
        tx: tx.clone(),
        history_cache,
        market_cache,
        metrics,
        player_histories,
    };

    // 6. 路由配置
    let app = Router::new()
        .route("/calculate_sell", post(api::handle_sell))
        .route("/calculate_buy", post(api::handle_buy))
        .route("/batch_sell", post(api::handle_batch_sell))
        .route("/api/market/sync", post(api::sync_market))
        .route("/api/market", get(api::get_market))
        .route("/api/config", get(api::get_config).post(api::update_config))
        .route("/api/history", get(api::get_history))
        .route("/api/metrics", get(api::get_metrics))
        .route("/api/player/:player_id", get(api::get_player_history))
        .route("/api/player/sync", post(api::sync_player_history))
        .nest_service("/", ServeDir::new("static")) // 托管 UI 前端
        .layer(CorsLayer::permissive())
        .layer(TimeoutLayer::new(Duration::from_secs(10))) 
        .with_state(shared_state.clone());

    // 7. 启动 HTTP 服务
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await
        .expect("端口绑定失败，请检查端口是否被占用");

    info!("✨ 服务已上线: http://{}", addr);
    

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    // 8. 关机序列：确保所有内存数据刷回磁盘
    info!("💾 正在持久化玩家历史数据...");
    {
        let data = shared_state.player_histories.read();
        Storage::save_player_data(&data);
        
        let cfg = shared_state.config.read();
        Storage::atomic_save_config(&cfg);
    }
    
    // 关闭 tx，通知后台写入协程刷盘并退出
    drop(shared_state.tx); 
    let _ = writer_handle.await;
    
    info!("👋 所有数据已安全同步，服务已关闭");
}