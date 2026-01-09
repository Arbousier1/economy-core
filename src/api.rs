use axum::{
    extract::{State, Json, Path},
    response::IntoResponse,
};
use std::{fs, time::Duration, sync::atomic::Ordering};
use tracing::{info, warn};
use chrono::{Utc, Datelike};
use std::collections::HashMap;

// 引入项目内部模块
use crate::AppState; 
use crate::models::*;
use crate::logic::execute_trade_logic;

// --- 1. 内部核心：流水记录与玩家状态同步 ---

/// 将交易记录持久化到内存缓存，并尝试通过 Channel 发送至磁盘写入任务
async fn internal_save_record(state: AppState, record: TransactionRecord) {
    state.metrics.total_trades.fetch_add(1, Ordering::Relaxed);

    {
        let mut histories = state.player_histories.write();
        let history = histories.entry(record.player_id.clone()).or_insert_with(|| PlayerSalesHistory {
            player_id: record.player_id.clone(),
            player_name: record.player_name.clone(),
            item_sales: HashMap::new(),
        });

        history.player_name = record.player_name.clone();
        let item_history = history.item_sales.entry(record.item_id.clone()).or_default();
        
        item_history.push(SalesRecord {
            timestamp: record.timestamp,
            amount: if record.action == "SELL" { record.amount } else { -record.amount },
            env_index: record.env_index,
        });

        if item_history.len() > 100 { 
            item_history.remove(0); 
        }
    }

    let tx = state.tx.clone();
    match tokio::time::timeout(Duration::from_millis(100), tx.send(record.clone())).await {
        Ok(Ok(_)) => {}, 
        _ => {
            state.metrics.channel_dropped.fetch_add(1, Ordering::Relaxed);
            warn!("⚠️ 磁盘写入拥堵 [Player: {}] 记录转入丢弃缓存", record.player_name);
            
            let mut cache = state.history_cache.write();
            cache.push(record);
            if cache.len() > 1000 { cache.remove(0); }
        }
    }
}

// --- 2. 交易核心接口 ---

/// 处理卖出请求 (SELL)
#[axum::debug_handler]
pub async fn handle_sell(
    State(state): State<AppState>,
    Json(req): Json<TradeRequest>,
) -> impl IntoResponse {
    // 关键修复：在独立的块中获取数据，确保锁卫士在 .await 前被 Drop
    let (config, holidays, player_history) = {
        let config_inner = state.config.read().clone();
        let holidays_inner = state.holidays.read().clone();
        let histories = state.player_histories.read();
        let history = histories.get(&req.player_id).cloned().unwrap_or_else(|| PlayerSalesHistory {
            player_id: req.player_id.clone(),
            player_name: req.player_name.clone(),
            item_sales: HashMap::new(),
        });
        (config_inner, holidays_inner, history)
    };

    let (resp, record) = execute_trade_logic(&req, &config, &holidays, &player_history, false).await;

    if let Some(r) = record {
        tokio::spawn(internal_save_record(state.clone(), r));
    }

    Json(resp)
}

/// 处理买入请求 (BUY)
#[axum::debug_handler]
pub async fn handle_buy(
    State(state): State<AppState>,
    Json(req): Json<TradeRequest>,
) -> impl IntoResponse {
    // 关键修复：提前释放锁
    let (config, holidays, player_history) = {
        let config_inner = state.config.read().clone();
        let holidays_inner = state.holidays.read().clone();
        let histories = state.player_histories.read();
        let history = histories.get(&req.player_id).cloned().unwrap_or_else(|| PlayerSalesHistory {
            player_id: req.player_id.clone(),
            player_name: req.player_name.clone(),
            item_sales: HashMap::new(),
        });
        (config_inner, holidays_inner, history)
    };

    let (resp, record) = execute_trade_logic(&req, &config, &holidays, &player_history, true).await;

    if let Some(r) = record {
        tokio::spawn(internal_save_record(state.clone(), r));
    }

    Json(resp)
}

/// 批量处理交易
#[axum::debug_handler]
pub async fn handle_batch_sell(
    State(state): State<AppState>,
    Json(batch): Json<BatchTradeRequest>,
) -> impl IntoResponse {
    // 提前克隆所需状态，避免在循环和异步任务中持有锁
    let (cfg, holidays, histories_snapshot) = {
        let c = state.config.read().clone();
        let h = state.holidays.read().clone();
        let hist = state.player_histories.read().clone();
        (c, h, hist)
    };

    let mut tasks = Vec::with_capacity(batch.requests.len());

    for req in batch.requests {
        let cfg_clone = cfg.clone();
        let holidays_clone = holidays.clone();
        let p_history = histories_snapshot.get(&req.player_id).cloned().unwrap_or_default();
        let state_clone = state.clone();

        tasks.push(tokio::spawn(async move {
            let (resp, record) = execute_trade_logic(&req, &cfg_clone, &holidays_clone, &p_history, false).await;
            if let Some(r) = record {
                internal_save_record(state_clone, r).await;
            }
            resp
        }));
    }

    let mut results = Vec::with_capacity(tasks.len());
    for task in tasks {
        if let Ok(res) = task.await {
            results.push(res);
        }
    }
    
    Json(BatchTradeResponse { results })
}

// --- 3. 数据管理与监控接口 ---

pub async fn get_market(State(state): State<AppState>) -> impl IntoResponse {
    let market = state.market_cache.read().clone();
    Json(market)
}

pub async fn sync_market(
    State(state): State<AppState>, 
    Json(req): Json<SyncMarketRequest>
) -> impl IntoResponse {
    {
        let mut cache = state.market_cache.write();
        *cache = req.items;
    }
    Json(serde_json::json!({"status": "synced"}))
}

pub async fn get_player_history(
    State(state): State<AppState>, 
    Path(player_id): Path<String>
) -> impl IntoResponse {
    let history = {
        let histories = state.player_histories.read();
        histories.get(&player_id).cloned().unwrap_or_else(|| PlayerSalesHistory {
            player_id: player_id.clone(),
            ..Default::default()
        })
    };
    Json(history)
}

pub async fn sync_player_history(
    State(state): State<AppState>, 
    Json(history): Json<PlayerSalesHistory>
) -> impl IntoResponse {
    {
        let mut histories = state.player_histories.write();
        histories.insert(history.player_id.clone(), history);
    }
    Json(serde_json::json!({"status": "success"}))
}

pub async fn get_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = Utc::now().timestamp() - state.metrics.start_time;
    let active_players = state.player_histories.read().len();
    Json(serde_json::json!({
        "total_trades": state.metrics.total_trades.load(Ordering::Relaxed),
        "channel_dropped": state.metrics.channel_dropped.load(Ordering::Relaxed),
        "active_players": active_players,
        "uptime_sec": uptime,
    }))
}

pub async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.config.read().clone();
    Json(cfg)
}

pub async fn update_config(
    State(state): State<AppState>, 
    Json(new_cfg): Json<AppConfig>
) -> impl IntoResponse {
    {
        let mut cfg = state.config.write();
        *cfg = new_cfg.clone();
    }
    tokio::spawn(async move {
        if let Ok(json) = serde_json::to_string_pretty(&new_cfg) {
            let _ = fs::write("config.json", json);
        }
    });
    Json("Config Updated")
}

pub async fn get_history(State(state): State<AppState>) -> impl IntoResponse {
    let history = state.history_cache.read().clone();
    Json(history)
}

// --- 4. 后台工具函数 ---

pub async fn fetch_holidays() -> HashMap<String, bool> {
    let year = Utc::now().year();
    let url = format!("https://holiday.cyi.me/api/holidays?year={}", year);
    let mut map = HashMap::new();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
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

pub async fn holiday_refresh_task(holidays: std::sync::Arc<parking_lot::RwLock<HashMap<String, bool>>>) {
    loop {
        let new_map = fetch_holidays().await;
        if !new_map.is_empty() {
            let mut lock = holidays.write();
            *lock = new_map;
            info!("✅ 节假日数据已自动刷新");
        }
        tokio::time::sleep(Duration::from_secs(86400)).await;
    }
}