use axum::{
    extract::{State, Json},
    response::IntoResponse,
};
use rayon::prelude::*;
use std::fs;
use tracing::{info, warn};

// 引入项目内部模块
use crate::AppState; // 引用 main.rs 中定义的全局状态
use crate::models::*;
use crate::logic::execute_trade_logic;

// --- 1. 单笔交易接口 ---

/// 处理单次“卖出”请求
/// POST /calculate_sell
pub async fn handle_sell(
    State(state): State<AppState>,
    Json(req): Json<TradeRequest>
) -> impl IntoResponse {
    let config = state.config.read();
    let holidays = state.holidays.read();

    // is_buy = false
    let (resp, record) = execute_trade_logic(&req, &config, &holidays, false);

    if let Some(r) = record {
        if let Err(_) = state.tx.try_send(r) {
            warn!("⚠️ 写入通道已满，丢失一条交易记录");
        }
    }

    Json(resp)
}

/// 处理单次“买入”请求
/// POST /calculate_buy
pub async fn handle_buy(
    State(state): State<AppState>,
    Json(req): Json<TradeRequest>
) -> impl IntoResponse {
    let config = state.config.read();
    let holidays = state.holidays.read();

    // is_buy = true
    let (resp, record) = execute_trade_logic(&req, &config, &holidays, true);

    if let Some(r) = record {
        let _ = state.tx.try_send(r);
    }

    Json(resp)
}

// --- 2. 批量交易接口 (高性能) ---

/// 处理批量“卖出”请求
/// POST /batch_sell
pub async fn handle_batch_sell(
    State(state): State<AppState>,
    Json(batch): Json<BatchTradeRequest>
) -> impl IntoResponse {
    let cfg = state.config.read().clone();
    let holidays = state.holidays.read().clone();

    // 将计算卸载到 blocking 线程池，避免阻塞 HTTP IO
    let results_and_records = tokio::task::spawn_blocking(move || {
        batch.requests
            .par_iter() // Rayon 并行
            .map(|req| execute_trade_logic(req, &cfg, &holidays, false))
            .collect::<Vec<(TradeResponse, Option<TransactionRecord>)>>()
    }).await.unwrap();

    let mut responses = Vec::with_capacity(results_and_records.len());
    
    for (resp, record) in results_and_records {
        if let Some(r) = record {
            let _ = state.tx.try_send(r);
        }
        responses.push(resp);
    }
    
    Json(BatchTradeResponse { results: responses })
}

// --- 3. 市场数据同步接口 (新增) ---

/// 接收 Java 插件推送的真实市场数据快照
/// POST /api/market/sync
pub async fn sync_market(
    State(state): State<AppState>,
    Json(req): Json<SyncMarketRequest>
) -> impl IntoResponse {
    // 获取写锁并更新缓存
    let mut cache = state.market_cache.write();
    *cache = req.items;
    
    info!("🔄 已同步 {} 个物品的真实市场数据", cache.len());
    Json("Synced")
}

/// 给前端提供真实物品列表
/// GET /api/market
pub async fn get_market(State(state): State<AppState>) -> impl IntoResponse {
    // 获取读锁并克隆数据
    Json(state.market_cache.read().clone())
}

// --- 4. 系统管理接口 ---

/// 获取当前配置
/// GET /api/config
pub async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.config.read().clone())
}

/// 热更新配置
/// POST /api/config
pub async fn update_config(
    State(state): State<AppState>,
    Json(new_cfg): Json<AppConfig>
) -> impl IntoResponse {
    {
        let mut cfg = state.config.write();
        *cfg = new_cfg.clone();
    }
    
    // 异步保存到硬盘
    tokio::spawn(async move {
        let file_path = "config.bin"; 
        if let Ok(data) = bincode::serialize(&new_cfg) {
            if let Err(e) = fs::write(file_path, data) {
                warn!("❌ 无法保存配置文件: {:?}", e);
            } else {
                info!("💾 配置已热更新并保存");
            }
        }
    });

    Json("Config updated successfully")
}

/// 获取最近的历史记录 (内存缓存)
/// GET /api/history
pub async fn get_history(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.history_cache.read().clone())
}