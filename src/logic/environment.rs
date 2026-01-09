use chrono::{Datelike, Local};
use rand::thread_rng;
use rand_distr::{Distribution, Normal};
use std::collections::HashMap;
use parking_lot::RwLock;

use crate::models::AppConfig;

/// 缓存结构
struct EnvCache {
    index: f64,
    note: String,
    timestamp: i64, 
}

/// 全局静态缓存
static GLOBAL_ENV_CACHE: RwLock<Option<EnvCache>> = RwLock::new(None);

/// 计算当前环境指数 ε(t)
pub fn calculate_current_env_index(
    config: &AppConfig,
    holidays: &HashMap<String, bool>
) -> (f64, String) {
    let now = Local::now();
    let current_ts = now.timestamp();

    // 1. 第一次检查（读锁）：绝大多数请求在此处返回
    {
        let cache_read = GLOBAL_ENV_CACHE.read();
        if let Some(cache) = &*cache_read {
            // 时钟回拨保护：如果缓存时间戳大于当前时间，说明发生了对时，强制更新
            if cache.timestamp == current_ts {
                return (cache.index, cache.note.clone());
            }
        }
    }

    // 2. 第二次检查（写锁）：处理缓存失效
    let mut cache_write = GLOBAL_ENV_CACHE.write();
    
    // 双重检查：防止多个线程同时发现缓存过期后重复执行复杂计算
    if let Some(cache) = &*cache_write {
        if cache.timestamp == current_ts {
            return (cache.index, cache.note.clone());
        }
    }

    // 执行计算
    let (final_index, note) = perform_calculation(now, config, holidays);

    // 更新缓存
    *cache_write = Some(EnvCache {
        index: final_index,
        note: note.clone(),
        timestamp: current_ts,
    });

    (final_index, note)
}

/// 实际计算过程
fn perform_calculation(
    now: chrono::DateTime<Local>,
    config: &AppConfig,
    holidays: &HashMap<String, bool>
) -> (f64, String) {
    // 生产级优化：预估容量，避免 Vec 扩容内存拷贝
    let mut epsilon: f64 = config.base_env_index;
    let mut reasons = Vec::with_capacity(3);

    // 使用延迟加载格式化，仅在需要时生成字符串
    let today_ymd = now.format("%Y-%m-%d").to_string();

    // 1. 节假日判断
    let mut is_api_workday = false;
    if let Some(&is_off) = holidays.get(&today_ymd) {
        if is_off {
            epsilon -= config.public_holiday_factor;
            reasons.push("Holiday");
        } else {
            is_api_workday = true;
        }
    }

    // 2. 寒暑假与周末（合并时间处理）
    let today_md = now.format("%m-%d").to_string();
    if is_date_in_range(&today_md, &config.winter_start, &config.winter_end) {
        epsilon -= config.holiday_factor;
        reasons.push("Winter");
    } else if is_date_in_range(&today_md, &config.summer_start, &config.summer_end) {
        epsilon -= config.holiday_factor;
        reasons.push("Summer");
    }

    let weekday = now.weekday();
    if weekday.number_from_monday() >= 6 && !is_api_workday {
        if !reasons.contains(&"Holiday") {
            epsilon -= config.weekend_factor;
            reasons.push("Weekend");
        }
    }

    // 3. 高性能随机数生成
    let mut rng = thread_rng();
    let std_dev = config.noise_std.max(0.0001);
    // 生产环境注意：Normal::new 只有在 std_dev 有效时才返回 Ok
    let noise = if let Ok(dist) = Normal::new(0.0, std_dev) {
        dist.sample(&mut rng)
    } else {
        0.0
    };

    let final_index = (epsilon + noise).max(0.1);
    
    // 生产环境优化：手动拼接字符串优于 join，减少分配
    let note = if reasons.is_empty() {
        "Normal".to_string()
    } else {
        reasons.join("+")
    };

    (final_index, note)
}

#[inline]
fn is_date_in_range(current: &str, start: &str, end: &str) -> bool {
    if start <= end {
        current >= start && current <= end
    } else {
        current >= start || current <= end
    }
}