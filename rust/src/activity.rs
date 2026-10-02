//! 对应 `src/activity.mjs` 的 `activityProbability`。
//!
//! 这是一条**有界的高斯形「静默」曲线，而不是归一化的概率密度**：正中间最可能休息，
//! 越靠近静默窗口两端越可能活跃。`ActivityRhythm`（需要持久化的那块）留到后续阶段。
//!
//! 曲线按**静默区间**定位：从 `inactiveStart` 到 `activeStart`（跨午夜时回绕）。
//! 注意 JS 用的本地时间含**秒**（`hour*60 + minute + second/60`），所以这里不能用
//! 只看分钟的换算。
use crate::config::{Rhythm, Schedule};
use crate::policy::{active_at, local_minute_of_day, parse_hhmm};

/// 复刻 `activityProbability(now, schedule, rhythm)`。
pub fn activity_probability(now: f64, schedule: &Schedule, rhythm: &Rhythm) -> f64 {
    // 时间表未启用、或此刻本来就在活跃窗口内 —— 两者都直接返回白天的基准概率。
    if !schedule.enabled || active_at(now, schedule) {
        return rhythm.day_probability;
    }
    let Some(minute) = local_minute_of_day(now, &schedule.timezone) else {
        return rhythm.day_probability;
    };
    let (Some(inactive_start), Some(active_start)) = (
        parse_hhmm(&schedule.inactive_start),
        parse_hhmm(&schedule.active_start),
    ) else {
        return rhythm.day_probability;
    };
    let start = inactive_start as f64;
    let end = active_start as f64;
    let duration = (end - start + 1440.0) % 1440.0;
    let x = ((minute - start + 1440.0) % 1440.0) / duration;

    let sigma = rhythm.sigma;
    let edge = (-0.5 * (0.5 / sigma).powi(2)).exp();
    let gaussian = (-0.5 * ((x - 0.5) / sigma).powi(2)).exp();
    // JS 写的是 Math.max(0, Math.min(1, ...))，这里用 clamp；两者对 NaN 的行为一致（都得到 NaN）。
    let dip = ((gaussian - edge) / (1.0 - edge)).clamp(0.0, 1.0);
    rhythm.edge_probability - (rhythm.edge_probability - rhythm.center_probability) * dip
}
