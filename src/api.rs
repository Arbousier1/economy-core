use axum::{
    extract::{State, Json},
    response::IntoResponse,
};
use rayon::prelude::*;
use std::{fs, time::Duration, sync::atomic::Ordering};
use tracing::{error, info, warn};
use chrono::{Local, Datelike};

// 引入项目内部模块
use crate::AppState; 
use crate::models::*;
use crate::logic::execute_trade_logic;

// --- 1. 内部辅助函数：可靠发送记录 (生产加固版) ---

async fn internal_save_record(state: AppState, record: TransactionRecord) {
    // 增加总交易计数
    state.metrics.total_trades.fetch_add(1, Ordering::Relaxed);

    let tx = state.tx.clone();
    // 尝试发送，带 100ms 超时背压控制
    match tokio::time::timeout(Duration::from_millis(100), tx.send(record.clone())).await {
        Ok(Ok(_)) => {}, 
        _ => {
            // 通道满或超时：记录丢失指标并降级到缓存
            state.metrics.channel_dropped.fetch_add(1, Ordering::Relaxed);
            warn!("⚠️ 磁盘写入拥堵，流水 [TS:{}] 转入紧急内存缓存", record.timestamp);
            
            let mut cache = state.history_cache.write();
            cache.push(record);
            if cache.len() > 1000 { cache.remove(0); }
        }
    }
}

// --- 2. 交易处理接口 ---

pub async fn handle_sell(
    State(state): State<AppState>,
    Json(req): Json<TradeRequest>,
) -> impl IntoResponse {
    let config = state.config.read();
    let holidays = state.holidays.read();

    let (resp, record) = execute_trade_logic(&req, &config, &holidays, false);

    if let Some(r) = record {
        // 使用 spawn 确保 IO 不阻塞 HTTP 响应
        tokio::spawn(internal_save_record(state.clone(), r));
    }

    Json(resp)
}

pub async fn handle_buy(
    State(state): State<AppState>,
    Json(req): Json<TradeRequest>,
) -> impl IntoResponse {
    let config = state.config.read();
    let holidays = state.holidays.read();

    let (resp, record) = execute_trade_logic(&req, &config, &holidays, true);

    if let Some(r) = record {
        tokio::spawn(internal_save_record(state.clone(), r));
    }

    Json(resp)
}

pub async fn handle_batch_sell(
    State(state): State<AppState>,
    Json(batch): Json<BatchTradeRequest>,
) -> impl IntoResponse {
    let cfg = state.config.read().clone();
    let holidays = state.holidays.read().clone();

    // 卸载 CPU 密集型并行计算
    let results: Vec<(TradeResponse, Option<TransactionRecord>)> = 
        tokio::task::spawn_blocking(move || {
            batch.requests
                .par_iter()
                .map(|req| execute_trade_logic(req, &cfg, &holidays, false))
                .collect()
        })
        .await
        .unwrap_or_default();

    let mut responses = Vec::with_capacity(results.len());
    
    for (resp, record) in results {
        if let Some(r) = record {
            // 批量模式采用 try_send 避免阻塞循环
            if let Err(_) = state.tx.try_send(r) {
                state.metrics.channel_dropped.fetch_add(1, Ordering::Relaxed);
            }
            state.metrics.total_trades.fetch_add(1, Ordering::Relaxed);
        }
        responses.push(resp);
    }
    
    Json(BatchTradeResponse { results: responses })
}

// --- 3. 市场与监控接口 ---

pub async fn sync_market(State(state): State<AppState>, Json(req): Json<SyncMarketRequest>) -> impl IntoResponse {
    {
        let mut cache = state.market_cache.write();
        *cache = req.items;
    }
    info!("🔄 市场数据已同步 ({} items)", state.market_cache.read().len());
    Json("Synced")
}

pub async fn get_market(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.market_cache.read().clone())
}

pub async fn get_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = Local::now().timestamp() - state.metrics.start_time;
    Json(serde_json::json!({
        "total_trades": state.metrics.total_trades.load(Ordering::Relaxed),
        "write_errors": state.metrics.write_failures.load(Ordering::Relaxed),
        "channel_dropped": state.metrics.channel_dropped.load(Ordering::Relaxed),
        "uptime_sec": uptime,
        "history_cache_usage": state.history_cache.read().len(),
    }))
}

// --- 4. 系统管理与节假日任务 ---

pub async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.config.read().clone())
}

pub async fn update_config(State(state): State<AppState>, Json(new_cfg): Json<AppConfig>) -> impl IntoResponse {
    {
        let mut cfg = state.config.write();
        *cfg = new_cfg.clone();
    }
    
    tokio::spawn(async move {
        let final_path = "config.bin";
        let temp_path = "config.bin.tmp";
        let save_res = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let data = bincode::serialize(&new_cfg)?;
            fs::write(temp_path, data)?;
            fs::rename(temp_path, final_path)?;
            Ok(())
        })();

        if let Err(e) = save_res {
            error!("❌ 配置文件保存失败: {:?}", e);
            let _ = fs::remove_file(temp_path);
        }
    });

    Json("Config Updated")
}

pub async fn get_history(State(state): State<AppState>) -> impl IntoResponse {
    let mut history = state.history_cache.read().clone();
    history.reverse();
    Json(history)
}

// --- 5. 节假日后台任务 (由 main.rs 调用) ---

pub async fn fetch_holidays() -> std::collections::HashMap<String, bool> {
    let year = Local::now().year();
    let url = format!("https://holiday.cyi.me/api/holidays?year={}", year);
    let mut map = std::collections::HashMap::new();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    if let Ok(resp) = client.get(&url).send().await {
        if let Ok(data) = resp.json::<HolidayApiResponse>().await {
            for item in data.days {
                map.insert(item.date, item.is_off_day);
            }
        }
    }
    map
}

pub async fn holiday_refresh_task(holidays: std::sync::Arc<parking_lot::RwLock<std::collections::HashMap<String, bool>>>) {
    loop {
        // 每天凌晨同步一次
        tokio::time::sleep(Duration::from_secs(86400)).await;
        let new_map = fetch_holidays().await;
        if !new_map.is_empty() {
            let mut lock = holidays.write();
            *lock = new_map;
            info!("✅ 节假日数据已执行每日定时更新");
        }
    }
}