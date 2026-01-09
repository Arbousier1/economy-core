use chrono::{Datelike, Local};
use rand::thread_rng;
use rand_distr::{Distribution, Normal};
use std::collections::HashMap;
use parking_lot::RwLock;

use crate::models::AppConfig;

/// 缓存结构：存储每秒计算一次的环境指数
struct EnvCache {
    index: f64,
    note: String,
    timestamp: i64, 
}

/// 全局静态缓存：利用 RwLock 实现多线程安全的高并发读
static GLOBAL_ENV_CACHE: RwLock<Option<EnvCache>> = RwLock::new(None);

/// 计算当前环境指数 ε(t)
/// 逻辑：优先从缓存读取，失效时重新计算并存入缓存
pub fn calculate_current_env_index(
    config: &AppConfig,
    holidays: &HashMap<String, bool>
) -> (f64, String) {
    let now = Local::now();
    let current_ts = now.timestamp();

    // 1. 第一次检查（读锁）：绝大多数请求在此处直接返回
    {
        let cache_read = GLOBAL_ENV_CACHE.read();
        if let Some(cache) = &*cache_read {
            // 时间戳校验：仅在同一秒内有效。若系统对时导致 current_ts < cache.timestamp 也会触发更新
            if cache.timestamp == current_ts {
                return (cache.index, cache.note.clone());
            }
        }
    }

    // 2. 第二次检查（写锁）：执行实际计算
    let mut cache_write = GLOBAL_ENV_CACHE.write();
    
    // 双重检查锁定（Double-Checked Locking）：防止多线程在高并发缓存失效瞬间重复计算
    if let Some(cache) = &*cache_write {
        if cache.timestamp == current_ts {
            return (cache.index, cache.note.clone());
        }
    }

    // 执行具体计算逻辑
    let (final_index, note) = perform_calculation(now, config, holidays);

    // 更新全局缓存
    *cache_write = Some(EnvCache {
        index: final_index,
        note: note.clone(),
        timestamp: current_ts,
    });

    (final_index, note)
}

/// 核心环境因子计算逻辑
fn perform_calculation(
    now: chrono::DateTime<Local>,
    config: &AppConfig,
    holidays: &HashMap<String, bool>
) -> (f64, String) {
    let mut epsilon: f64 = config.base_env_index;
    let mut reasons = Vec::with_capacity(3);

    // 预格式化日期字符串用于查找
    let today_ymd = now.format("%Y-%m-%d").to_string(); // 2024-05-20
    let today_md = now.format("%m-%d").to_string();   // 05-20

    // 1. 节假日逻辑（外部 API 数据优先）
    let mut is_api_workday = false;
    if let Some(&is_off) = holidays.get(&today_ymd) {
        if is_off {
            epsilon -= config.public_holiday_factor;
            reasons.push("Holiday");
        } else {
            // 如果 API 明确说是工作日（调休上班），后续将忽略周末因子
            is_api_workday = true;
        }
    }

    // 2. 寒暑假判定（支持跨年区间）
    if is_date_in_range(&today_md, &config.winter_start, &config.winter_end) {
        epsilon -= config.holiday_factor;
        reasons.push("Winter");
    } else if is_date_in_range(&today_md, &config.summer_start, &config.summer_end) {
        epsilon -= config.holiday_factor;
        reasons.push("Summer");
    }

    // 3. 周末判定（非调休上班的情况下减成）
    let weekday = now.weekday();
    if weekday.number_from_monday() >= 6 && !is_api_workday {
        if !reasons.contains(&"Holiday") {
            epsilon -= config.weekend_factor;
            reasons.push("Weekend");
        }
    }

    // 4. 随机波动噪声 (高斯分布)
    // 确保标准差 std_dev > 0 以防采样器崩溃
    let mut rng = thread_rng();
    let std_dev = config.noise_std.max(0.0001);
    let noise = if let Ok(dist) = Normal::new(0.0, std_dev) {
        dist.sample(&mut rng)
    } else {
        0.0
    };

    // 5. 结果收敛与备注生成
    let final_index = (epsilon + noise).max(0.1); // 指数保底，防止出现负数或零导致无法交易
    
    let note = if reasons.is_empty() {
        "Normal".to_string()
    } else {
        reasons.join("+")
    };

    (final_index, note)
}

/// 辅助函数：判断日期是否在区间内，兼容跨年区间（如 12-15 到 02-15）
#[inline]
fn is_date_in_range(current: &str, start: &str, end: &str) -> bool {
    if start <= end {
        // 普通区间：如 07-01 到 08-31
        current >= start && current <= end
    } else {
        // 跨年区间：如 12-01 到 02-01
        current >= start || current <= end
    }
}