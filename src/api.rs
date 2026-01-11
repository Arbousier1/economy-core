use axum::{extract::{State, Json}, response::IntoResponse, http::StatusCode};
use std::{collections::HashSet, sync::atomic::Ordering};
use futures::{stream, StreamExt};
use rustc_hash::FxHashMap;

use crate::AppState;
use crate::models::{self, *}; // 确保引入了 SalesRecord, TradeRequest 等
use crate::logic::{execute_trade_logic, pricing::PricingEngine, environment};

// =========================================================================
// 1. 错误处理与辅助
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
// 2. 交易处理 (Trade Handlers)
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

    // 2. 获取快照 (最小化锁竞争)
    let config = state.config.read().clone();
    let holidays = state.holidays.read().clone();
    let player_history = state.player_histories.read()
        .get(&req.player_id).cloned().unwrap_or_default();

    // 3. 执行核心逻辑 (纯计算)
    let (resp, record) = execute_trade_logic(
        &req, &config, &holidays, &player_history, is_buy, 
        &state.env_cache, &state.http_client
    ).await;

    // 4. 异步持久化
    if let Some(r) = record {
        tokio::spawn(persist_transaction(state, r));
    }

    Json(resp)
}

// =========================================================================
// 3. 市场行情 (Market Prices)
// =========================================================================

pub async fn get_market_prices(
    State(state): State<AppState>,
    Json(payload): Json<MarketPriceRequest>,
) -> impl IntoResponse {
    let config = state.config.read().clone();
    let market_items = state.market_cache.read().clone();
    
    // 计算环境指数
    let (env_index, env_note) = environment::calculate_current_env_index(
        &config, &state.holidays.read(), &state.env_cache
    );

    // 确定查询范围
    let target_ids: HashSet<String> = if payload.item_ids.is_empty() {
        market_items.iter().map(|i| i.id.clone()).collect()
    } else {
        payload.item_ids.into_iter().collect()
    };

    let current_time = chrono::Utc::now().timestamp_millis();
    
    // [优化] 锁外计算全局有效库存 (Global N_eff)
    let global_neff = calculate_global_neff_optimized(&state, &target_ids, &config, current_time).await;

    // 组装结果
    let response_items: FxHashMap<String, MarketItemStatus> = market_items.into_iter()
        .filter(|i| target_ids.contains(&i.id))
        .map(|item| {
            let history_n = global_neff.get(&item.id).copied().unwrap_or(0.0);
            
            // 公式: N_total = N_history + N_static + Iota_item + Iota_global
            let final_neff = (history_n + item.n + item.iota + config.global_iota).max(0.0);
            
            // 公式: Price = Base * Env * exp(-|λ| * N_total)
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

/// [修复] 正确实现的聚合逻辑
async fn calculate_global_neff_optimized(
    state: &AppState, 
    targets: &HashSet<String>, 
    config: &AppConfig, 
    ts: i64
) -> FxHashMap<String, f64> {
    // 1. 快速快照：只克隆相关物品的交易记录
    // 数据结构: Vec<(ItemId, Vec<SalesRecord>)>
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

    // 2. 锁外聚合计算
    let mut accumulator = FxHashMap::default();
    
    for (item_id, records) in history_snapshot {
        let val = PricingEngine::calculate_effective_n(&records, 0.0, config, ts);
        
        // 累加不同玩家对同一物品贡献的 N_eff
        accumulator.entry(item_id)
            .and_modify(|v| *v += val)
            .or_insert(val);
    }
    
    accumulator
}

// =========================================================================
// 4. 批量交易 (Batch)
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
                
                let (resp, record) = execute_trade_logic(
                    &req, &cfg, &hols, &hist, false, &s.env_cache, &s.http_client
                ).await;

                if let Some(r) = record { 
                    persist_transaction(s, r).await; 
                }
                resp
            }
        })
        .buffer_unordered(10) // 并行度控制
        .collect::<Vec<_>>()
        .await;

    Json(BatchTradeResponse { results })
}

// =========================================================================
// 5. 持久化与同步 (Persistence & Sync)
// =========================================================================

async fn persist_transaction(state: AppState, record: TransactionRecord) {
    state.metrics.total_trades.fetch_add(1, Ordering::Relaxed);
    
    // 更新内存
    {
        let mut histories = state.player_histories.write();
        let entry = histories.entry(record.player_id.clone()).or_default();
        // 确保名字是最新的
        if entry.player_name != record.player_name {
            entry.player_name = record.player_name.clone();
        }
        
        let items = entry.item_sales.entry(record.item_id.clone()).or_default();
        
        // [修复] 补全 SalesRecord 的 price 字段
        items.push(SalesRecord {
            timestamp: record.timestamp,
            amount: if record.action == "SELL" { record.amount } else { -record.amount },
            env_index: record.env_index,
            // 简单计算单价，避免除以零
            price: if record.amount.abs() > 1e-9 { 
                record.total_price / record.amount 
            } else { 
                0.0 
            },
        });
        
        // 简单的滑动窗口清理
        if items.len() > 100 { items.remove(0); }
    }

    // 发送到后台写入通道
    if let Err(_) = state.tx.try_send(record) {
        state.metrics.channel_dropped.fetch_add(1, Ordering::Relaxed);
        tracing::warn!("🔥 写入通道背压过高，丢弃日志以保护服务");
    }
}

// [新增] 补充 main.rs 缺失的 sync_market 接口
// 用于管理面板手动刷新市场配置或缓存
pub async fn sync_market(State(_state): State<AppState>) -> impl IntoResponse {
    // 这里可以实现重新加载 Config 或清理缓存的逻辑
    // 目前仅返回成功，作为占位符
    Json(serde_json::json!({ 
        "success": true, 
        "message": "Market synced (Placeholder)" 
    }))
}