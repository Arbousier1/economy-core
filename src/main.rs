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
    sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::Duration,
};
use tokio::{sync::mpsc, signal};
use tower_http::{cors::CorsLayer, services::ServeDir, timeout::TimeoutLayer};
use tracing::{error, info, warn};
use chrono::Local;

// 引入内部模块内容
use models::*;
use api::*;

// --- 生产环境常量配置 ---
const CONFIG_FILE: &str = "config.bin";
const HISTORY_FILE: &str = "history.bin";
const CHANNEL_CAPACITY: usize = 20_000; // 缓冲高频交易高峰
const MAX_CACHE_SIZE: usize = 1000;    // 内存预览历史深度

/// 生产级监控指标统计结构
pub struct SystemMetrics {
    pub total_trades: AtomicU64,      // 已处理交易总数
    pub write_failures: AtomicU64,    // 磁盘 IO 失败计数
    pub channel_dropped: AtomicU64,   // 因通道溢出丢失的记录数
    pub start_time: i64,              // 启动时间戳 (Unix ms)
}

/// 全局应用状态 (共享 Context)
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub holidays: Arc<RwLock<HashMap<String, bool>>>,
    pub tx: mpsc::Sender<TransactionRecord>,
    pub history_cache: Arc<RwLock<Vec<TransactionRecord>>>,
    pub market_cache: Arc<RwLock<Vec<MarketItem>>>,
    pub metrics: Arc<SystemMetrics>,
}

// --- 1. 后台持久化协程 (具备安全退出逻辑) ---
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
        Ok(f) => BufWriter::with_capacity(128 * 1024, f), // 128KB 缓冲区减少系统调用
        Err(e) => {
            error!("🚨 [CRITICAL] 磁盘文件打开失败: {}. 交易记录持久化功能已瘫痪!", e);
            return;
        }
    };

    let mut flush_interval = tokio::time::interval(Duration::from_secs(5));
    info!("💾 磁盘写入服务已就绪: 异步批量模式开启");

    loop {
        tokio::select! {
            // 监听通道数据
            record_opt = rx.recv() => {
                match record_opt {
                    Some(record) => {
                        // 更新内存热缓存
                        {
                            let mut cache = history_cache.write();
                            cache.push(record.clone());
                            if cache.len() > MAX_CACHE_SIZE { cache.remove(0); }
                        }

                        // 序列化并存入写缓冲区
                        if let Err(e) = bincode::serialize_into(&mut writer, &record) {
                            error!("❌ 磁盘序列化失败: {:?}", e);
                            metrics.write_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    None => {
                        // 关键：通道所有 Sender 已关闭，执行最后刷盘并安全退出
                        info!("👋 正在执行最终数据持久化...");
                        let _ = writer.flush();
                        break; 
                    }
                }
            }
            // 每 5 秒强制 Flush 缓冲区，防止意外断电丢失过多数据
            _ = flush_interval.tick() => {
                let _ = writer.flush();
            }
        }
    }
    info!("🛑 持久化服务已安全停止");
}

// --- 2. 跨平台优雅关机信号监听 ---
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("无法注册 Ctrl+C 信号处理器");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("无法注册 SIGTERM 信号处理器")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("📥 接收到退出信号 (Ctrl+C)"),
        _ = terminate => info!("📥 接收到关机信号 (SIGTERM)"),
    }
}

// --- 3. 稳健的数据存储辅助 ---
struct Storage;
impl Storage {
    fn load_config() -> AppConfig {
        if let Ok(data) = fs::read(CONFIG_FILE) {
            if let Ok(cfg) = bincode::deserialize(&data) { return cfg; }
        }
        warn!("⚠️ 配置损坏或不存在，正在部署初始化配置...");
        let default_cfg = AppConfig::default();
        Self::atomic_save_config(&default_cfg);
        default_cfg
    }

    /// 原子替换保存配置：防止写入中途崩溃导致文件损坏
    pub fn atomic_save_config(cfg: &AppConfig) {
        let temp_path = format!("{}.tmp", CONFIG_FILE);
        if let Ok(data) = bincode::serialize(cfg) {
            if fs::write(&temp_path, data).is_ok() {
                let _ = fs::rename(&temp_path, CONFIG_FILE);
            }
        }
    }

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
}

// --- 4. 主程序流程 ---
#[tokio::main]
async fn main() {
    // 1. 初始化日志系统
    tracing_subscriber::fmt::init();
    info!("🚀 Economy Core [PROD] 正在启动...");

    // 2. 指标与初始数据加载
    let metrics = Arc::new(SystemMetrics {
        total_trades: AtomicU64::new(0),
        write_failures: AtomicU64::new(0),
        channel_dropped: AtomicU64::new(0),
        start_time: Local::now().timestamp(),
    });

    let config_data = Storage::load_config();
    let port = config_data.port;
    
    let config = Arc::new(RwLock::new(config_data));
    let history_cache = Arc::new(RwLock::new(Storage::load_history()));
    let holidays = Arc::new(RwLock::new(api::fetch_holidays().await));
    let market_cache = Arc::new(RwLock::new(Vec::new()));

    // 3. 通道与核心异步任务
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    
    // 启动后台写入协程并保留句柄
    let writer_handle = tokio::spawn(background_writer_task(
        rx, 
        history_cache.clone(), 
        metrics.clone()
    ));
    
    // 启动节假日定时刷新协程
    tokio::spawn(api::holiday_refresh_task(holidays.clone()));

    let shared_state = AppState {
        config,
        holidays,
        tx: tx.clone(),
        history_cache,
        market_cache,
        metrics,
    };

    // 4. 定义路由与加固中间件
    let app = Router::new()
        .route("/calculate_sell", post(handle_sell))
        .route("/calculate_buy", post(handle_buy))
        .route("/batch_sell", post(handle_batch_sell))
        .route("/api/market/sync", post(sync_market))
        .route("/api/market", get(get_market))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/history", get(get_history))
        .route("/api/metrics", get(get_metrics))
        .nest_service("/", ServeDir::new("static")) // 托管 index.html 所在目录
        .layer(CorsLayer::permissive())
        .layer(TimeoutLayer::new(Duration::from_secs(10))) // 请求硬超时保护
        .with_state(shared_state);

    // 5. 服务绑定与优雅停机逻辑
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await
        .expect("端口绑定失败，请检查 9981 是否被占用");

    info!("✨ 系统运行中: http://{}", addr);

    

    // Axum 阻塞主进程并等待信号
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    // 6. 严谨收尾：触发后台写入任务退出
    info!("⏳ 正在收尾，请勿强制关闭...");
    
    // 显式释放最初的 tx，当所有 handle 里的 clone tx 也随请求结束释放后，rx 将收到 None
    drop(tx); 
    
    // 等待磁盘写入协程完成最后一份数据的保存
    let _ = writer_handle.await;

    info!("🛑 Economy Core 已完全停止，数据安全。");
}