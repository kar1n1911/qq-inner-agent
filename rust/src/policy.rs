//! 对应 `src/policy.mjs` 的纯逻辑部分。
//!
//! 这里覆盖：准入判断、静默时段、活跃时间表、候选选择、长度分档、重复检测。
//! **`normalize()`（OneBot 事件 → 内部消息）不在这里** —— 它与传输层的事件形状耦合，
//! 由引擎阶段一并实现。
//!
//! 时区是最容易与 JS 产生偏差的地方：JS 走 ICU 的 `Intl.DateTimeFormat`，这里走
//! `chrono-tz`，两者的数据库版本不同。`tests/policy_parity.rs` 会用真实 Node 在多个
//! 时区与时刻上比对。
use crate::config::{Agent, QuietHours, Schedule};
use crate::text::similarity;
use chrono::{DateTime, FixedOffset, Local, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use std::cmp::Ordering;

/// 时区：**两步解析**。
///
/// 1. 先试固定偏移（`UTC`、`UTC+8`、`UTC-3`、`UTC+05:30`、`GMT+8`、`+08:00`）。
///    它不需要时区数据库，也不受夏令时影响，覆盖绝大多数使用场景。
/// 2. 再试 IANA 区域名（`Europe/Stockholm`）。只有确实需要夏令时的地方才用这一步，
///    它依赖时区数据库，代价更高。
#[derive(Debug, Clone, Copy)]
pub enum Zone {
    Fixed(FixedOffset),
    Named(Tz),
}

/// 解析固定偏移写法；不是偏移写法时返回 `None`，由调用方继续尝试区域名。
fn parse_fixed_offset(value: &str) -> Option<FixedOffset> {
    let upper = value.trim().to_ascii_uppercase();
    let rest = upper
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
        .unwrap_or(upper.as_str());
    if rest.is_empty() {
        // 裸 "UTC" / "GMT" 表示零偏移。
        return FixedOffset::east_opt(0);
    }
    let sign = match rest.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let body = &rest[1..];
    let (hours, minutes) = body.split_once(':').unwrap_or((body, "0"));
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours > 18 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

impl Zone {
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(fixed) = parse_fixed_offset(value) {
            return Some(Self::Fixed(fixed));
        }
        chrono_tz::TZ_VARIANTS
            .iter()
            .copied()
            .find(|tz| tz.name().eq_ignore_ascii_case(value.trim()))
            .map(Self::Named)
    }

    fn minutes_of_day(&self, utc: DateTime<Utc>) -> i64 {
        match self {
            Self::Fixed(offset) => {
                let local = utc.with_timezone(offset);
                i64::from(local.hour() * 60 + local.minute())
            }
            Self::Named(tz) => {
                let local = utc.with_timezone(tz);
                i64::from(local.hour() * 60 + local.minute())
            }
        }
    }
}

/// 把"秒（可含小数）"转成该时区的本地分钟数（0..1440）。
///
/// JS 的 `new Date(now * 1000)` 会先把毫秒截断成整数，这里照做，避免亚毫秒差异。
/// `timezone` 为 `None` 时 JS 用系统时区，这里用 `chrono::Local`。
///
/// 公开是为了让 parity 测试能直接与 JS 的 `Intl.DateTimeFormat` 逐点比对，从而把
/// "时区数据库版本不同"这种问题暴露出来，而不是被 `quiet()` 的布尔结果掩盖。
pub fn local_minutes_of_day(now: f64, timezone: Option<&str>) -> Option<i64> {
    let millis = (now * 1000.0).trunc();
    if !millis.is_finite() {
        return None;
    }
    let millis = millis as i64;
    let utc = Utc
        .timestamp_opt(millis.div_euclid(1000), (millis.rem_euclid(1000) * 1_000_000) as u32)
        .single()?;
    match timezone {
        Some(name) => Zone::parse(name).map(|zone| zone.minutes_of_day(utc)),
        None => {
            let local = utc.with_timezone(&Local);
            Some(i64::from(local.hour() * 60 + local.minute()))
        }
    }
}

/// 复刻 `allowed()`：`chat` 形如 `group:<id>` 或 `private:<id>`。
pub fn allowed(chat: &str, agent: &Agent) -> bool {
    match chat.split_once(':') {
        Some(("group", id)) => agent.allowed_groups.iter().any(|x| x == id),
        Some(("private", id)) => agent.allowed_users.iter().any(|x| x == id),
        _ => false,
    }
}

/// 复刻 `quiet()`：判断 `now`（秒）是否落在静默时段内。
///
/// 注意 JS 的两个短路条件：没有配置、或起止相同，都直接返回 false（表示"不静默"）。
pub fn quiet(now: f64, hours: Option<&QuietHours>) -> bool {
    let Some(hours) = hours else { return false };
    if hours.start == hours.end {
        return false;
    }
    let Some(minutes) = local_minutes_of_day(now, hours.timezone.as_deref()) else {
        return false;
    };
    // JS 取的是"小时"而不是分钟。
    let hour = (minutes / 60) as f64;
    if hours.start < hours.end {
        hour >= hours.start && hour < hours.end
    } else {
        // 跨夜窗口，例如 23:00–08:00。
        hour >= hours.start || hour < hours.end
    }
}

fn parse_hhmm(value: &str) -> Option<i64> {
    let (h, m) = value.split_once(':')?;
    Some(h.trim().parse::<i64>().ok()? * 60 + m.trim().parse::<i64>().ok()?)
}

/// 复刻 `activeAt()`：时间表未启用时永远活跃。
pub fn active_at(now: f64, schedule: &Schedule) -> bool {
    if !schedule.enabled {
        return true;
    }
    let Some(minute) = local_minutes_of_day(now, Some(&schedule.timezone)) else {
        return true;
    };
    let (Some(start), Some(end)) = (
        parse_hhmm(&schedule.active_start),
        parse_hhmm(&schedule.inactive_start),
    ) else {
        return true;
    };
    if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allocation {
    SelfChat,
    Other,
    Open,
}

impl Allocation {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "self" => Some(Self::SelfChat),
            "other" => Some(Self::Other),
            "open" => Some(Self::Open),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    System1,
    System2,
}

impl CandidateKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system1" => Some(Self::System1),
            "system2" => Some(Self::System2),
            _ => None,
        }
    }
}

/// 经评估后的候选。字段对应 `engine.mjs` 里传给 `select()` 的对象。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub kind: CandidateKind,
    pub text: String,
    pub motivation: f64,
    pub relevance: f64,
    pub originality: f64,
    pub for_tags: Vec<String>,
    pub against_tags: Vec<String>,
}

/// `select()` 的返回值：原候选加上调整后的分数。
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    pub candidate: Candidate,
    pub adjusted: f64,
}

/// 复刻 `select()`。
///
/// 三个阈值语义：`allocation == self` 一定选第一个；非主动模式下直接返回 `None`；
/// 只有 `relevance >= 3 && originality >= 3` 的候选才有资格，`other` 用更高的
/// `interruptThreshold`。
pub fn select(
    rated: &[Candidate],
    allocation: Allocation,
    agent: &Agent,
    turns_silent: f64,
    random: impl FnOnce() -> f64,
) -> Option<Selected> {
    if rated.is_empty() {
        return None;
    }
    let factor = 1.02_f64.powf(turns_silent.max(0.0)).min(1.2);
    let mut pool: Vec<Selected> = rated
        .iter()
        .map(|candidate| Selected {
            adjusted: (candidate.motivation * factor).min(5.0),
            candidate: candidate.clone(),
        })
        .collect();
    // JS 的 Array#sort 是稳定的；Rust 的 sort_by 同样是稳定排序，因此并列时保持原顺序。
    pool.sort_by(|a, b| {
        b.adjusted
            .partial_cmp(&a.adjusted)
            .unwrap_or(Ordering::Equal)
    });
    if allocation == Allocation::SelfChat {
        return pool.into_iter().next();
    }
    if !agent.proactive {
        return None;
    }
    let appropriate: Vec<&Selected> = pool
        .iter()
        .filter(|x| x.candidate.relevance >= 3.0 && x.candidate.originality >= 3.0)
        .collect();
    let threshold = if allocation == Allocation::Other {
        agent.interrupt_threshold
    } else {
        agent.threshold
    };
    if let Some(top) = appropriate.iter().find(|x| x.adjusted >= threshold) {
        return Some((*top).clone());
    }
    if allocation == Allocation::Open && random() < agent.system1_probability {
        return appropriate
            .iter()
            .find(|x| x.candidate.kind == CandidateKind::System1)
            .map(|x| (*x).clone());
    }
    None
}

/// 复刻 `pickLengthTarget()`：回复长度分档。
///
/// 长度均匀是最强的"机器味"信号。被直接点名时禁用 `tiny` —— 不能用一个"哈哈"敷衍提问。
const LENGTH_BUCKETS_ADDRESSED: [(&str, f64); 3] =
    [("short", 0.70), ("medium", 0.28), ("long", 0.02)];
const LENGTH_BUCKETS_OPEN: [(&str, f64); 4] =
    [("tiny", 0.35), ("short", 0.45), ("medium", 0.18), ("long", 0.02)];

pub fn pick_length_target(hint: &str, random: impl FnOnce() -> f64) -> &'static str {
    let buckets: &[(&str, f64)] = if hint == "self" {
        &LENGTH_BUCKETS_ADDRESSED
    } else {
        &LENGTH_BUCKETS_OPEN
    };
    let draw = random();
    let mut cumulative = 0.0;
    for (name, weight) in buckets {
        cumulative += weight;
        if draw < cumulative {
            return name;
        }
    }
    "short"
}

/// 复刻 `repeated()`：与 agent 自己最近说过的话重复（完全相同，或相似度 > 0.88）。
pub fn repeated(text: &str, self_messages: &[String]) -> bool {
    let trimmed = text.trim();
    self_messages.iter().any(|previous| {
        previous.trim() == trimmed || similarity(text, previous) > 0.88
    })
}
