use chrono::{Datelike, Local};
use rand::thread_rng;
use rand_distr::{Distribution, Normal};
use std::collections::HashMap;
use crate::models::AppConfig;

/// 计算当前环境指数 ε(t)
pub fn calculate_current_env_index(
    config: &AppConfig,
    holidays: &HashMap<String, bool>
) -> (f64, String) {
    let now = Local::now();
    let today_ymd = now.format("%Y-%m-%d").to_string();
    let today_md = now.format("%m-%d").to_string();
    let weekday = now.weekday();

    let mut epsilon: f64 = config.base_env_index;
    let mut reasons = Vec::new();

    // 1. 节假日 API
    let mut is_api_workday = false;
    if let Some(&is_off) = holidays.get(&today_ymd) {
        if is_off {
            epsilon -= config.public_holiday_factor;
            reasons.push("Holiday");
        } else {
            is_api_workday = true; // 补班日
        }
    }

    // 2. 寒暑假
    if is_date_in_range(&today_md, &config.winter_start, &config.winter_end) {
        epsilon -= config.holiday_factor;
        reasons.push("Winter");
    } else if is_date_in_range(&today_md, &config.summer_start, &config.summer_end) {
        epsilon -= config.holiday_factor;
        reasons.push("Summer");
    }

    // 3. 周末 (如果是补班日则不扣分)
    if weekday.number_from_monday() >= 6 && !is_api_workday {
        // 如果已经是法定节假日扣过分了，就不重复扣周末分
        if !reasons.contains(&"Holiday") {
            epsilon -= config.weekend_factor;
            reasons.push("Weekend");
        }
    }

    // 4. 随机噪声
    let mut rng = thread_rng();
    let std_dev = config.noise_std.max(0.0001);
    let normal = Normal::new(0.0, std_dev).unwrap();
    let noise: f64 = normal.sample(&mut rng);

    // 5. 结果合成 (最低 0.1)
    let final_index = (epsilon + noise).max(0.1);
    
    let note = if reasons.is_empty() { 
        "Normal".to_string() 
    } else { 
        reasons.join("+") 
    };

    (final_index, note)
}

fn is_date_in_range(current: &str, start: &str, end: &str) -> bool {
    if start <= end {
        current >= start && current <= end
    } else {
        // 处理跨年 (例如 12-25 到 01-15)
        current >= start || current <= end
    }
}