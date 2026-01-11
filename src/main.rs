mod models;
mod logic;
mod api;

// [修复] ax_rt -> axum
use axum::{routing::{get, post}, Router, http::StatusCode};
use parking_lot::RwLock;
use std::{collections::{HashMap, VecDeque}, fs, io, net::SocketAddr, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::Duration};
use tokio::{sync::mpsc, signal, task, time};
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer};
use tracing::{error, info, warn};
use chrono::Local;

// [新增] 引入 postcard
use postcard;

use crate::models::*;

// --- 核心常量 ---
const CONFIG_FILE: &str = "config.bin";
const HISTORY_FILE: &str = "history.bin";
const PLAYER_DATA_FILE: &str = "player_data.bin";
const CHANNEL_CAPACITY: usize = 2_000;
const MAX_CACHE_SIZE: usize = 1000;
const BATCH_SIZE: usize = 50;

// [移除] BINCODE_CFG (Postcard 不需要配置对象)

pub struct SystemMetrics {
    pub total_trades: AtomicU64,
    pub write_failures: AtomicU64,
    pub channel_dropped: AtomicU64,
    pub start_time: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RwLock<AppConfig>>,
    pub holidays: Arc<RwLock<HashMap<String, bool>>>,
    pub tx: mpsc::Sender<TransactionRecord>,
    pub history_cache: Arc<RwLock<VecDeque<TransactionRecord>>>,
    pub market_cache: Arc<RwLock<Vec<MarketItem>>>,
    pub metrics: Arc<SystemMetrics>,
    pub player_histories: Arc<RwLock<HashMap<String, PlayerSalesHistory>>>,
    pub http_client: reqwest::Client,
    pub env_cache: Arc<RwLock<Option<EnvCache>>>,
}

// =========================================================================
// 1. 强化存储引擎 (适配 Postcard)
// =========================================================================

struct Storage;
impl Storage {
    // [修改] 泛型约束仅需 DeserializeOwned
    fn load<T: serde::de::DeserializeOwned>(file: &str) -> Option<T> {
        fs::read(file).ok().and_then(|data| {
            // [修改] 使用 postcard 反序列化
            postcard::from_bytes(&data).ok()
        })
    }

    // [修改] 泛型约束仅需 Serialize
    fn atomic_save<T: serde::Serialize>(file: &str, data: &T) -> io::Result<()> {
        let temp_path = format!("{}.tmp", file);
        
        // [修改] 使用 postcard 序列化 (to_stdvec 需要开启 use-std feature)
        let bytes = postcard::to_stdvec(data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        
        fs::write(&temp_path, bytes)?;
        fs::rename(&temp_path, file)
    }
}

// =========================================================================
// 2. 批量持久化核心 (Batch Writer)
// =========================================================================

async fn background_writer_task(
    mut rx: mpsc::Receiver<TransactionRecord>,
    history_cache: Arc<RwLock<VecDeque<TransactionRecord>>>,
    metrics: Arc<SystemMetrics>,
) {
    use tokio::io::AsyncWriteExt;
    
    let file = match tokio::fs::OpenOptions::new().create(true).append(true).open(HISTORY_FILE).await {
        Ok(f) => f,
        Err(e) => { error!("🚨 历史文件打开失败: {}", e); return; }
    };
    
    // 使用 buffer 减少系统调用
    let mut writer = tokio::io::BufWriter::with_capacity(256 * 1024, file);
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    let mut flush_interval = time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            Some(record) = rx.recv() => {
                // 1. 更新内存循环缓存
                {
                    let mut cache = history_cache.write();
                    cache.push_back(record.clone());
                    if cache.len() > MAX_CACHE_SIZE { cache.pop_front(); }
                }

                // 2. 加入批处理队列
                batch.push(record);
                if batch.len() >= BATCH_SIZE {
                    flush_batch(&mut batch, &mut writer, &metrics).await;
                }
            }
            _ = flush_interval.tick() => {
                if !batch.is_empty() {
                    flush_batch(&mut batch, &mut writer, &metrics).await;
                }
            }
            else => {
                info!("👋 写入通道关闭，正在保存剩余 {} 条记录...", batch.len());
                flush_batch(&mut batch, &mut writer, &metrics).await;
                let _ = writer.flush().await;
                break;
            }
        }
    }
}

async fn flush_batch(
    batch: &mut Vec<TransactionRecord>,
    writer: &mut tokio::io::BufWriter<tokio::fs::File>,
    metrics: &Arc<SystemMetrics>
) {
    use tokio::io::AsyncWriteExt;
    for record in batch.drain(..) {
        // [修改] 使用 postcard 序列化单条记录
        if let Ok(bytes) = postcard::to_stdvec(&record) {
            if let Err(e) = writer.write_all(&bytes).await {
                metrics.write_failures.fetch_add(1, Ordering::Relaxed);
                error!("❌ 批量写入中单条记录失败: {:?}", e);
            }
        }
    }
    let _ = writer.flush().await;
}

// =========================================================================
// 3. 入口与生命周期
// =========================================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    info!("🚀 Kyochigo Economy Core v4.0 (Postcard Edition) 启动中...");

    let metrics = Arc::new(SystemMetrics {
        total_trades: AtomicU64::new(0),
        write_failures: AtomicU64::new(0),
        channel_dropped: AtomicU64::new(0),
        start_time: Local::now().timestamp(),
    });

    // 数据加载与初始化
    let config_data = Storage::load::<AppConfig>(CONFIG_FILE).unwrap_or_default();
    let initial_history = Storage::load::<VecDeque<TransactionRecord>>(HISTORY_FILE).unwrap_or_default();
    
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let state = AppState {
        config: Arc::new(RwLock::new(config_data)),
        holidays: Arc::new(RwLock::new(HashMap::new())),
        tx,
        history_cache: Arc::new(RwLock::new(initial_history)),
        market_cache: Arc::new(RwLock::new(Vec::new())),
        metrics: metrics.clone(),
        player_histories: Arc::new(RwLock::new(Storage::load(PLAYER_DATA_FILE).unwrap_or_default())),
        // 修正 reqwest 客户端构建
        http_client: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("HTTP Client 构建失败"),
        env_cache: Arc::new(RwLock::new(None)),
    };

    let writer_handle = tokio::spawn(background_writer_task(rx, state.history_cache.clone(), metrics));

    // 路由构建 (Axum 0.8)
    let app = Router::new()
        .route("/calculate_sell", post(api::handle_sell))
        .route("/calculate_buy", post(api::handle_buy))
        .route("/api/market/sync", post(api::sync_market))
        .layer(CorsLayer::permissive())
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(10)))
        .with_state(state.clone());

    let port = state.config.read().port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    
    // 绑定端口
    let listener = tokio::net::TcpListener::bind(addr).await.expect("端口绑定失败");
    info!("✨ API 节点已上线: {}", addr);

    // 启动服务
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();

    // 优雅停机
    perform_graceful_cleanup(state, writer_handle).await;
}

async fn perform_graceful_cleanup(state: AppState, writer_handle: task::JoinHandle<()>) {
    info!("💾 执行最终同步...");
    drop(state.tx); // 触发 background_writer 退出
    
    // 给后台任务 10 秒处理缓冲区
    if let Err(_) = time::timeout(Duration::from_secs(10), writer_handle).await {
        warn!("⏰ 刷盘任务超时，部分流水可能丢失。");
    }

    // 内部函数：保存逻辑
    async fn save_with_retry<T: serde::Serialize>(name: &str, data: &T) {
        for i in 1..=3 {
            match Storage::atomic_save(name, data) {
                Ok(_) => { 
                    info!("✅ {} 保存成功", name); 
                    return; 
                }
                Err(e) => warn!("⚠️ {} 保存失败 (第{}次重试): {:?}", name, i, e),
            }
            time::sleep(Duration::from_millis(500)).await;
        }
    }

    // 获取读锁并克隆/引用数据进行保存
    let final_histories = state.player_histories.read();
    let final_config = state.config.read();

    save_with_retry(PLAYER_DATA_FILE, &*final_histories).await;
    save_with_retry(CONFIG_FILE, &*final_config).await;

    info!("👋 所有数据已同步，系统安全退出。");
}

// [修复] 正确的信号处理，避免临时值生命周期问题
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}