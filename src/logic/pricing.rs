use crate::models::{AppConfig, SalesRecord};

/// 核心定价引擎：专注于经济学积分公式与时间恢复机制的实现
pub struct PricingEngine;

impl PricingEngine {
    
    // =========================================================================
    // 1. 核心数学积分引擎
    // =========================================================================

    /// 核心积分公式实现: Total = ∫_{start_n}^{start_n+Δn} (ε * P0) * e^(-λx) dx
    /// 
    /// 该公式模拟了价格随供应量增加而指数级衰减的行为。
    /// 积分结果 = (ε * P0 / λ) * (e^(-λ * start_n) - e^(-λ * (start_n + Δn)))
    pub fn calculate_integral_revenue(
        base_price: f64,
        env_index: f64,
        start_n: f64,
        amount: f64,
        lambda: f64,
    ) -> f64 {
        // 数值边界安全检查：防止非有限数值或无效参数导致崩溃
        if !amount.is_finite() || amount <= 0.0 || !base_price.is_finite() || base_price <= 0.0 {
            return 0.0;
        }

        let p_max = env_index * base_price;
        let abs_lambda = lambda.abs();

        // 线性处理优化：
        // 当 λ 极小（趋近于 0）时，指数衰减退化为线性定价，
        // 使用简单的矩形面积计算避免“除以零”导致的无穷大异常。
        if abs_lambda < 1e-10 {
            return Self::round_to_two_decimal(p_max * amount);
        }

        let n_end = start_n + amount;
        
        // 积分原函数计算: F(x) = -(P_max / λ) * e^(-λx)
        // 最终金额 = F(n_end) - F(start_n)
        let exp_start = (-abs_lambda * start_n).exp();
        let exp_end = (-abs_lambda * n_end).exp();
        
        let revenue = (p_max / abs_lambda) * (exp_start - exp_end);

        // 结果鲁棒性检查
        if !revenue.is_finite() || revenue < 0.0 {
            return 0.0;
        }

        Self::round_to_two_decimal(revenue)
    }

    // =========================================================================
    // 2. 时间恢复与有效库存计算
    // =========================================================================

    /// 时间恢复机制实现: n_eff = Σ (amount_i * e^(-δ * Δt / τ)) + ι(t)
    /// 
    /// 物理含义：
    /// 玩家卖出物品后，其对物价的冲击会随时间推移而衰减（恢复）。
    /// δ (recovery_delta) 控制恢复强度，τ (recovery_tau) 控制时间缩放（如 3600 代表以小时为单位）。
    pub fn calculate_effective_n(
        history: &[SalesRecord],
        iota: f64,
        config: &AppConfig,
        current_timestamp_ms: i64,
    ) -> f64 {
        let mut total_n_eff = 0.0;

        for record in history {
            // 计算距离现在的秒数
            let elapsed_secs = ((current_timestamp_ms - record.timestamp) as f64 / 1000.0).max(0.0);
            
            let decay = if config.recovery_delta > 0.0 {
                // 指数衰减公式：衰减量 = 原始数量 * exp(-δ * Δt / τ)
                (-config.recovery_delta * (elapsed_secs / config.recovery_tau)).exp()
            } else {
                1.0 // 如果 delta 为 0，则价格永不恢复（库存影响永久存在）
            };

            total_n_eff += record.amount * decay;
        }

        // 最终有效库存 n_eff = 历史衰减余量 + 特别物价指数偏移 iota
        // 使用 .max(0.0) 确保即便 iota 为负（资源极度短缺），计算基数也不会低于零。
        (total_n_eff + iota).max(0.0)
    }

    // =========================================================================
    // 3. 辅助工具
    // =========================================================================

    /// 生产级精准修约：保留 2 位小数，符合货币习惯
    #[inline]
    fn round_to_two_decimal(value: f64) -> f64 {
        (value * 100.0).round() / 100.0
    }
}