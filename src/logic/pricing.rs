/// 计算批量交易的总价 (积分公式实现)
/// Revenue = ∫ (ε * p0) * e^(-λx) dx
pub fn calculate_batch_revenue(
    base_price: f64,
    env_index: f64,
    start_n: f64,
    amount: f64,
    lambda: f64,
) -> f64 {
    // 基础校验
    if amount <= 0.0 || base_price <= 0.0 || env_index <= 0.0 {
        return 0.0;
    }

    let p_max = env_index * base_price;
    let abs_lambda = lambda.abs();

    // 线性情况 (无衰减)
    if abs_lambda < 1e-9 {
        let raw_total = p_max * amount;
        return (raw_total * 100.0).round() / 100.0;
    }

    // 积分计算
    // F(x) = - (p_max / λ) * e^(-λx)
    let n_end = start_n + amount;
    
    let exp_start = (-abs_lambda * start_n).exp();
    let exp_end = (-abs_lambda * n_end).exp();

    // 防止溢出
    if !exp_start.is_finite() || !exp_end.is_finite() {
        return 0.0; 
    }

    let revenue = (p_max / abs_lambda) * (exp_start - exp_end);

    // 保留2位小数
    (revenue.abs() * 100.0).round() / 100.0
}