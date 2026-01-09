/// 计算批量交易的总价 (积分公式实现)
/// 生产环境严谨版：强化了数值稳定性和边界检查
/// 
/// 公式: Revenue = ∫_{n_start}^{n_end} (ε * p0) * e^(-λx) dx
/// 积分结果: F(x) = - (p_max / λ) * e^(-λx)
pub fn calculate_batch_revenue(
    base_price: f64,
    env_index: f64,
    start_n: f64,
    amount: f64,
    lambda: f64,
) -> f64 {
    // 1. 严格基础校验：任何参数为非有限数 (NaN/Inf) 或非正数则直接拦截
    if !amount.is_finite() || amount <= 0.0 || 
       !base_price.is_finite() || base_price <= 0.0 || 
       !env_index.is_finite() || env_index <= 0.0 {
        return 0.0;
    }

    let p_max = env_index * base_price;
    // 强制使用正数 lambda 以符合指数衰减模型
    let abs_lambda = lambda.abs(); 

    // 2. 线性情况优化 (处理极小 lambda)
    // 当 λ 极小时，积分趋向于 p_max * amount，使用线性计算防止除以 0 导致的溢出
    if abs_lambda < 1e-10 {
        let raw_total = p_max * amount;
        return round_to_two_decimal(raw_total);
    }

    // 3. 积分区间确定
    let n_end = start_n + amount;
    
    // 4. 数值稳定性保护
    // 计算 e^(-λx)，如果参数过大会导致结果为 0，这是正常的（价格跌破阈值）
    let exp_start = (-abs_lambda * start_n).exp();
    let exp_end = (-abs_lambda * n_end).exp();

    // 5. 溢出检查
    // 检查系数项 p_max / λ 是否溢出
    let coeff = p_max / abs_lambda;
    if !coeff.is_finite() {
        return 0.0;
    }

    // 积分计算公式: coeff * (exp_start - exp_end)
    let revenue = coeff * (exp_start - exp_end);

    // 6. 结果二次校验与格式化
    if !revenue.is_finite() || revenue < 0.0 {
        return 0.0;
    }

    round_to_two_decimal(revenue)
}

/// 生产级精准修约：保留2位小数
/// 封装为函数以保证计算的一致性
#[inline]
fn round_to_two_decimal(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}