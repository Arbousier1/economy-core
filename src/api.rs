use axum::{extract::{State, Json}, response::IntoResponse, http::StatusCode};
use std::{collections::{HashSet, HashMap}, sync::atomic::Ordering};
use futures::{stream, StreamExt};
use rustc_hash::FxHashMap;

use crate::AppState;
use crate::models::{self, *};
use crate::logic::{execute_trade_logic, pricing::PricingEngine, environment};

// =========================================================================
// 1. 错误处理与验证
// =========================================================================

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("请求参数错误: {0}")]
    BadRequest(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
        };
        (status, Json(serde_json::json!({ "error": self.to_string() }))).into_response()
    }
}

impl TradeRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if self.amount.abs() <= 1e-10 { 
            return Err(ApiError::BadRequest("交易量绝对值必须大于 0".into())); 
        }
        if self.player_id.is_empty() { 
            return Err(ApiError::BadRequest("玩家ID缺失".into())); 
        }
        Ok(())
    }
}

// =========================================================================
// 2. 交易核心路由 (Trade Handlers)
// =========================================================================

pub async fn handle_sell(s: State<AppState>, j: Json<TradeRequest>) -> impl IntoResponse {
    process_trade(s, j, false).await
}

pub async fn handle_buy(s: State<AppState>, j: Json<TradeRequest>) -> impl IntoResponse {
    process_trade(s, j, true).await
}

async fn process_trade(
    State(state): State<AppState>, 
    Json(req): Json<TradeRequest>, 
    is_buy: bool
) -> impl IntoResponse {
    // 1. 输入验证
    if let Err(e) = req.validate() { return e.into_response(); }

    // 2. 获取状态快照
    let config = state.config.read().clone();
    let holidays = state.holidays.read().clone();
    let player_history = state.player_histories.read()
        .get(&req.player_id).cloned().unwrap_or_default();

    // 3. [新增] 获取当前物品的持久化状态 n (解决重启重置问题)
    let current_n = {
        let market = state.market_cache.read();
        market.iter()
            .find(|i| i.id == req.item_id)
            .map(|i| i.n)
            .unwrap_or(0.0)
    };

    // 4. 执行纯计算逻辑 (传入 current_n)
    let (resp, record) = execute_trade_logic(
        &req, &config, &holidays, &player_history, is_buy, 
        &state.env_cache, &state.http_client,
        current_n // <--- 关键参数
    ).await;

    // 5. 异步持久化
    if let Some(r) = record {
        tokio::spawn(persist_transaction(state, r));
    }

    Json(resp).into_response()
}

// =========================================================================
// 3. 市场行情查询 (Market Prices)
// =========================================================================

pub async fn get_market_prices(
    State(state): State<AppState>,
    Json(payload): Json<MarketPriceRequest>,
) -> impl IntoResponse {
    let config = state.config.read().clone();
    let market_items = state.market_cache.read().clone();
    
    let (env_index, env_note) = environment::calculate_current_env_index(
        &config, &state.holidays.read(), &state.env_cache
    );

    let target_ids: HashSet<String> = if payload.item_ids.is_empty() {
        market_items.iter().map(|i| i.id.clone()).collect()
    } else {
        payload.item_ids.into_iter().collect()
    };

    let current_time = chrono::Utc::now().timestamp_millis();
    
    // 计算基于历史的库存
    let global_history_neff = calculate_global_neff_optimized(&state, &target_ids, &config, current_time).await;

    let response_items: FxHashMap<String, MarketItemStatus> = market_items.into_iter()
        .filter(|i| target_ids.contains(&i.id))
        .map(|item| {
            let history_n = global_history_neff.get(&item.id).copied().unwrap_or(0.0);
            
            // [关键公式] N_total = N_history + N_static(持久化) + Iota(偏移) + Global
            let final_neff = (history_n + item.n + item.iota + config.global_iota).max(0.0);
            
            let raw_price = env_index * item.base_price * (-item.lambda.abs() * final_neff).exp();
            
            (item.id, MarketItemStatus::new(
                raw_price, 
                raw_price * config.buy_premium, 
                final_neff, 
                item.base_price
            ))
        })
        .collect();

    Json(serde_json::json!({
        "items": response_items,
        "envIndex": models::round_2(env_index),
        "envNote": env_note,
        "serverTime": current_time
    }))
}

async fn calculate_global_neff_optimized(
    state: &AppState, 
    targets: &HashSet<String>, 
    config: &AppConfig, 
    ts: i64
) -> FxHashMap<String, f64> {
    let history_snapshot: Vec<(String, Vec<SalesRecord>)> = {
        let histories = state.player_histories.read();
        histories.values()
            .flat_map(|h| {
                h.item_sales.iter()
                    .filter(|(id, _)| targets.contains(*id))
                    .map(|(id, records)| (id.clone(), records.clone()))
            })
            .collect()
    };

    let mut accumulator = FxHashMap::default();
    for (item_id, records) in history_snapshot {
        // 这里只计算历史衰减部分
        let val = PricingEngine::calculate_history_decay(&records, config, ts);
        
        accumulator.entry(item_id)
            .and_modify(|v| *v += val)
            .or_insert(val);
    }
    accumulator
}

// =========================================================================
// 4. 批量处理
// =========================================================================

pub async fn handle_batch_sell(
    State(state): State<AppState>, 
    Json(batch): Json<BatchTradeRequest>
) -> impl IntoResponse {
    let results = stream::iter(batch.requests)
        .map(|req| {
            let s = state.clone();
            async move {
                let (cfg, hols, hist) = (
                    s.config.read().clone(), 
                    s.holidays.read().clone(), 
                    s.player_histories.read().get(&req.player_id).cloned().unwrap_or_default()
                );
                
                // [新增] 获取 n
                let current_n = {
                    let market = s.market_cache.read();
                    market.iter().find(|i| i.id == req.item_id).map(|i| i.n).unwrap_or(0.0)
                };
                
                let (resp, record) = execute_trade_logic(
                    &req, &cfg, &hols, &hist, false, &s.env_cache, &s.http_client,
                    current_n
                ).await;

                if let Some(r) = record { 
                    persist_transaction(s, r).await; 
                }
                resp
            }
        })
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await;

    Json(BatchTradeResponse { results })
}

// =========================================================================
// 5. 持久化与内存更新
// =========================================================================

async fn persist_transaction(state: AppState, record: TransactionRecord) {
    state.metrics.total_trades.fetch_add(1, Ordering::Relaxed);
    
    // 1. 更新玩家交易历史
    {
        let mut histories = state.player_histories.write();
        let entry = histories.entry(record.player_id.clone()).or_default();
        if entry.player_name != record.player_name {
            entry.player_name = record.player_name.clone();
        }
        let items = entry.item_sales.entry(record.item_id.clone()).or_default();
        items.push(SalesRecord {
            timestamp: record.timestamp,
            amount: if record.action == "SELL" { record.amount } else { -record.amount },
            env_index: record.env_index,
            price: if record.amount.abs() > 1e-9 { record.total_price / record.amount } else { 0.0 },
        });
        if items.len() > 100 { items.remove(0); }
    }

    // 2. [可选] 如果你需要交易直接改变全局 n (不仅仅是历史记录计算)，在这里更新 market_cache
    // 如果 n 是静态参数，这里不需要动。如果 n 是累积量，这里可以加减。
    // 假设 n 是静态配置带来的基础偏移，我们这里不动它。

    if let Err(_) = state.tx.try_send(record) {
        state.metrics.channel_dropped.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("🔥 写入通道背压过高，丢弃日志以保护 API 响应速度");
    }
}

// =========================================================================
// 6. 管理与同步接口
// =========================================================================

pub async fn sync_market(
    State(state): State<AppState>,
    Json(payload): Json<MarketSyncRequest>
) -> impl IntoResponse {
    let new_items = payload.items;
    let item_count = new_items.len();
    
    {
        let mut cache = state.market_cache.write();
        let mut old_state_map: HashMap<String, MarketItem> = cache.drain(..)
            .map(|item| (item.id.clone(), item))
            .collect();
            
        *cache = new_items.into_iter().map(|mut new_item| {
            if let Some(old_item) = old_state_map.remove(&new_item.id) {
                // [关键] 保留旧状态
                new_item.n = old_item.n;
                new_item.iota = old_item.iota;
            }
            new_item
        }).collect();
    }
    
    tracing::info!("♻️ 已智能同步 {} 个物品 (状态已保留)", item_count);

    Json(serde_json::json!({ 
        "success": true, 
        "message": format!("Synced {} items", item_count) 
    }))
}

pub async fn get_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = chrono::Utc::now().timestamp() - state.metrics.start_time;
    Json(serde_json::json!({
        "totalTrades": state.metrics.total_trades.load(Ordering::Relaxed),
        "dropped": state.metrics.channel_dropped.load(Ordering::Relaxed),
        "uptime": uptime,
        "cachedItems": state.market_cache.read().len()
    }))
}