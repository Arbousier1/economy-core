pub mod pricing;
pub mod environment;

use crate::models::{TradeRequest, TradeResponse, TransactionRecord, AppConfig};
use std::collections::HashMap;

/// 综合交易执行逻辑：解耦环境计算与价格积分，生成最终响应与记录
pub fn execute_trade_logic(
    req: &TradeRequest,
    config: &AppConfig,
    holidays: &HashMap<String, bool>,
    is_buy: bool
) -> (TradeResponse, Option<TransactionRecord>) {
    // 1. 计算环境指数
    let (env_index, note) = if let Some(m) = req.manual_env_index {
        (m, "Manual".to_string())
    } else {
        environment::calculate_current_env_index(config, holidays)
    };

    // 2. 调整生效基础单价 (买入溢价)
    let effective_base = if is_buy { 
        req.base_price * config.buy_premium 
    } else { 
        req.base_price 
    };

    // 3. 计算总成交价 (积分面积)
    // 买入：库存减少 (向左积分)
    // 卖出：库存增加 (向右积分)
    let total_price = if is_buy {
        let n_start = (req.start_n - req.amount).max(0.0);
        let amount = req.start_n - n_start; // 实际能买的数量(防止扣到负数)
        pricing::calculate_batch_revenue(effective_base, env_index, n_start, amount, req.decay_lambda)
    } else {
        pricing::calculate_batch_revenue(effective_base, env_index, req.start_n, req.amount, req.decay_lambda)
    };

    // 4. 封装响应
    let resp = TradeResponse {
        total_price: total_price.abs(),
        unit_price_avg: if req.amount > 0.0 { 
            (total_price / req.amount * 100.0).round() / 100.0 
        } else { 
            0.0 
        },
        env_index: (env_index * 1000.0).round() / 1000.0,
    };

    // 5. 生成持久化流水记录 (非预览模式且交易量>0)
    let record = if !req.is_preview && req.amount > 0.0 {
        Some(TransactionRecord {
            timestamp: chrono::Local::now().timestamp_millis(),
            action: if is_buy { "BUY" } else { "SELL" }.to_string(),
            amount: req.amount,
            total_price: resp.total_price,
            avg_price: resp.unit_price_avg,
            env_index: resp.env_index, // 使用修约后的值记录
            note,
        })
    } else {
        None
    };

    (resp, record)
}