pub mod pricing;
pub mod environment;

use crate::models::{TradeRequest, TradeResponse, TransactionRecord, AppConfig};
use std::collections::HashMap;

/// 综合交易执行逻辑：解耦环境计算与价格积分，生成最终响应与记录
/// 
/// 生产级严谨化改进：
/// 1. 严格校验交易数量，拦截非法或零交易请求。
/// 2. 增强浮点数计算的边界保护。
/// 3. 优化记录生成逻辑，确保流水数据的一致性。
pub fn execute_trade_logic(
    req: &TradeRequest,
    config: &AppConfig,
    holidays: &HashMap<String, bool>,
    is_buy: bool
) -> (TradeResponse, Option<TransactionRecord>) {
    // 1. 基础参数预校验 (生产环境：拦截任何可能导致计算异常的非法数值)
    if req.amount <= 1e-10 || !req.amount.is_finite() {
        return (TradeResponse::default(), None);
    }

    // 2. 获取环境指数 (ε)
    // 调用的 environment::calculate_current_env_index 已具备 1 秒原子缓存
    let (env_index, note) = if let Some(m) = req.manual_env_index {
        if m.is_finite() && m > 0.0 {
            (m, "Manual".to_string())
        } else {
            (config.base_env_index, "Fallback".to_string())
        }
    } else {
        environment::calculate_current_env_index(config, holidays)
    };

    // 3. 价格参数预处理
    // 如果是买入，应用买入溢价系数（例如 1.25 倍）
    let effective_base = if is_buy { 
        req.base_price * config.buy_premium 
    } else { 
        req.base_price 
    };

    // 4. 核心定价计算 (积分面积法)
    // 卖出：库存增加，价格向右滑动；买入：库存减少，价格向左滑动
    let total_price_raw = if is_buy {
        let n_start = (req.start_n - req.amount).max(0.0);
        let actual_buy_amount = (req.start_n - n_start).max(0.0);
        
        if actual_buy_amount <= 0.0 {
            0.0
        } else {
            pricing::calculate_batch_revenue(effective_base, env_index, n_start, actual_buy_amount, req.decay_lambda)
        }
    } else {
        pricing::calculate_batch_revenue(effective_base, env_index, req.start_n, req.amount, req.decay_lambda)
    };

    // 5. 封装响应 (TradeResponse)
    // 确保结果非负且经过修约
    let final_total_price = total_price_raw.abs();
    
    let resp = TradeResponse {
        total_price: final_total_price,
        unit_price_avg: if req.amount > 1e-10 { 
            (final_total_price / req.amount * 100.0).round() / 100.0 
        } else { 
            0.0 
        },
        env_index: (env_index * 1000.0).round() / 1000.0,
    };

    // 6. 生成持久化交易流水记录 (TransactionRecord)
    // 严谨性：只有在非预览模式、实际产生金额且交易量有效时才生成记录
    let record = if !req.is_preview && resp.total_price > 0.0 {
        Some(TransactionRecord {
            timestamp: chrono::Local::now().timestamp_millis(),
            action: if is_buy { "BUY" } else { "SELL" }.to_string(),
            amount: req.amount,
            total_price: resp.total_price,
            avg_price: resp.unit_price_avg,
            env_index: resp.env_index,
            note,
        })
    } else {
        None
    };

    (resp, record)
}