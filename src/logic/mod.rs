pub mod pricing;
pub mod environment;

use crate::models::{TradeRequest, TradeResponse, TransactionRecord, AppConfig, PlayerSalesHistory};
use crate::logic::pricing::PricingEngine;
use std::collections::HashMap;
use chrono::Utc;
use tracing::warn;

/// 综合交易执行逻辑
/// 
/// 流程：
/// 1. 基础校验 -> 2. 身份校验 (基于 Config) -> 3. 环境因子获取 -> 
/// 4. 有效库存 (n_eff) 计算 -> 5. 核心积分定价 -> 6. 构造响应与流水记录
pub async fn execute_trade_logic(
    req: &TradeRequest,
    config: &AppConfig,
    holidays: &HashMap<String, bool>,
    player_history: &PlayerSalesHistory,
    is_buy: bool
) -> (TradeResponse, Option<TransactionRecord>) {
    
    // --- 1. 基础参数预校验 ---
    if req.amount <= 1e-10 || !req.amount.is_finite() {
        return (TradeResponse {
            total_price: 0.0,
            unit_price_avg: 0.0,
            env_index: 1.0,
            effective_n: 0.0,
        }, None);
    }

    // --- 2. 身份校验 ---
    // 强制使用后端 config 中的 is_online_mode，忽略请求中的干扰字段
    if !validate_player(req, config.is_online_mode).await {
        warn!(
            "⚠️ 玩家身份验证失败: [ID: {}, Name: {}, Mode: {}]", 
            req.player_id, req.player_name, if config.is_online_mode { "Online" } else { "Offline" }
        );
        return (TradeResponse {
            total_price: 0.0,
            unit_price_avg: 0.0,
            env_index: 1.0,
            effective_n: 0.0,
        }, None);
    }

    // --- 3. 获取当前环境指数 (ε) ---
    let (env_index, mut note) = if let Some(m) = req.manual_env_index {
        if m.is_finite() && m > 0.0 {
            (m, "Manual".to_string())
        } else {
            (config.base_env_index, "Fallback".to_string())
        }
    } else {
        environment::calculate_current_env_index(config, holidays)
    };

    // 记录全局校验模式到备注中
    let mode_label = if config.is_online_mode { "OnlineAuth" } else { "OfflineAuth" };
    note = format!("{}; {}", mode_label, note);

    // --- 4. 时间与有效库存偏移准备 ---
    let current_ms = Utc::now().timestamp_millis();
    let history_items = player_history.item_sales.get(&req.item_id).map(|v| v.as_slice()).unwrap_or(&[]);
    let iota_val = req.iota.unwrap_or(0.0);

    // 计算当前的有效偏移量 n_eff (考虑时间衰减)
    let effective_n = PricingEngine::calculate_effective_n(history_items, iota_val, config, current_ms);

    // --- 5. 价格参数预处理 ---
    // 只有买入时才应用配置中的溢价系数 (Spread)
    let effective_base = if is_buy { 
        req.base_price * config.buy_premium 
    } else { 
        req.base_price 
    };

    // --- 6. 核心定价计算 (积分逻辑) ---
    // 
    let total_price_raw = if is_buy {
        // 【买入逻辑】：消耗库存记录，价格向左（高价区）滑动
        // n_start 是买入后的起始点
        let n_start = (effective_n - req.amount).max(0.0);
        let actual_integral_amount = (effective_n - n_start).max(0.0);
        
        if actual_integral_amount < req.amount {
            // 情况 A: 买入量超过了历史抛售积累的有效库存。超出部分按原价保底计算
            let premium_part = (req.amount - actual_integral_amount) * (effective_base * env_index);
            let integral_part = if actual_integral_amount > 1e-10 {
                PricingEngine::calculate_integral_revenue(
                    effective_base, 
                    env_index, 
                    n_start, 
                    actual_integral_amount, 
                    req.decay_lambda
                )
            } else {
                0.0
            };
            integral_part + premium_part
        } else {
            // 情况 B: 完整积分区间买入
            PricingEngine::calculate_integral_revenue(
                effective_base, 
                env_index, 
                n_start, 
                req.amount, 
                req.decay_lambda
            )
        }
    } else {
        // 【卖出逻辑】：增加有效库存记录，价格沿曲线向右滑动
        PricingEngine::calculate_integral_revenue(
            effective_base,
            env_index,
            effective_n,
            req.amount,
            req.decay_lambda,
        )
    };

    // --- 7. 封装响应 (数值修约) ---
    let final_total_price = total_price_raw.abs();
    let resp = TradeResponse {
        total_price: (final_total_price * 100.0).round() / 100.0,
        unit_price_avg: if req.amount > 1e-10 { 
            (final_total_price / req.amount * 100.0).round() / 100.0 
        } else { 
            0.0 
        },
        env_index: (env_index * 1000.0).round() / 1000.0,
        effective_n: (effective_n * 100.0).round() / 100.0,
    };

    // --- 8. 生成流水记录 ---
    let record = if !req.is_preview && resp.total_price > 0.0 {
        Some(TransactionRecord {
            timestamp: current_ms,
            action: if is_buy { "BUY" } else { "SELL" }.to_string(),
            amount: req.amount,
            total_price: resp.total_price,
            avg_price: resp.unit_price_avg,
            env_index: resp.env_index,
            player_id: req.player_id.clone(),
            player_name: req.player_name.clone(),
            item_id: req.item_id.clone(),
            note,
        })
    } else {
        None
    };

    (resp, record)
}

/// 验证玩家身份合法性
/// 根据服务器全局配置决定是进行 Mojang API 校验还是本地长度校验
async fn validate_player(req: &TradeRequest, is_online_mode: bool) -> bool {
    if req.player_id.is_empty() || req.player_name.is_empty() {
        return false;
    }

    if is_online_mode {
        // --- 正版模式 ---
        let clean_uuid = req.player_id.replace("-", "");
        let url = format!("https://sessionserver.mojang.com/session/minecraft/profile/{}", clean_uuid);
        
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_default();

        match client.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false // 校验服务超时或失败，默认安全拒绝
        }
    } else {
        // --- 离线模式 ---
        // 验证基本 UUID 格式长度即可
        req.player_id.len() >= 32
    }
}