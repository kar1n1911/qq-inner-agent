//! 对应 `src/policy.mjs` 的纯逻辑部分。
//!
//! 这里覆盖：准入判断、静默时段、活跃时间表、候选选择、长度分档、重复检测。
//! `normalize()` 将 OneBot 事件转换为内部强类型消息。
//!
//! 时区是最容易与 JS 产生偏差的地方：JS 走 ICU 的 `Intl.DateTimeFormat`，这里走
//! `chrono-tz`，两者的数据库版本不同。`tests/policy_parity.rs` 会用真实 Node 在多个
//! 时区与时刻上比对。
use crate::config::{Agent, QuietHours, Schedule};
use crate::memory::text::similarity;
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
            Self::Fixed(offset) => integer_minutes(utc.with_timezone(offset)),
            Self::Named(tz) => integer_minutes(utc.with_timezone(tz)),
        }
    }

    /// 保留秒的小数部分：活动节奏曲线按 `hour*60 + minute + second/60` 定位。
    fn fractional_minute_of_day(&self, utc: DateTime<Utc>) -> f64 {
        match self {
            Self::Fixed(offset) => fractional_minutes(utc.with_timezone(offset)),
            Self::Named(tz) => fractional_minutes(utc.with_timezone(tz)),
        }
    }
}

fn integer_minutes<Tz: TimeZone>(local: DateTime<Tz>) -> i64 {
    i64::from(local.hour() * 60 + local.minute())
}

fn fractional_minutes<Tz: TimeZone>(local: DateTime<Tz>) -> f64 {
    f64::from(local.hour() * 60 + local.minute()) + f64::from(local.second()) / 60.0
}

/// 把"秒（可含小数）"换算成 UTC 的 `DateTime`。
///
/// JS 的 `new Date(now * 1000)` 会先把毫秒截断成整数，这里照做。
fn to_utc(now: f64) -> Option<DateTime<Utc>> {
    let millis = (now * 1000.0).trunc();
    if !millis.is_finite() {
        return None;
    }
    let millis = millis as i64;
    Utc.timestamp_opt(
        millis.div_euclid(1000),
        (millis.rem_euclid(1000) * 1_000_000) as u32,
    )
    .single()
}

/// 把"秒（可含小数）"转成该时区的本地分钟数（0..1440）。
///
/// JS 的 `new Date(now * 1000)` 会先把毫秒截断成整数，这里照做，避免亚毫秒差异。
/// `timezone` 为 `None` 时 JS 用系统时区，这里用 `chrono::Local`。
///
/// 公开是为了让 parity 测试能直接与 JS 的 `Intl.DateTimeFormat` 逐点比对，从而把
/// "时区数据库版本不同"这种问题暴露出来，而不是被 `quiet()` 的布尔结果掩盖。
pub fn local_minutes_of_day(now: f64, timezone: Option<&str>) -> Option<i64> {
    let utc = to_utc(now)?;
    match timezone {
        Some(name) => Zone::parse(name).map(|zone| zone.minutes_of_day(utc)),
        None => {
            let local = utc.with_timezone(&Local);
            Some(integer_minutes(local))
        }
    }
}

/// 与 [`local_minutes_of_day`] 相同，但保留秒的小数部分。
/// 活动节奏曲线需要秒级精度（JS 里用的是 `hour*60 + minute + second/60`）。
pub fn local_minute_of_day(now: f64, timezone: &str) -> Option<f64> {
    let utc = to_utc(now)?;
    Zone::parse(timezone).map(|zone| zone.fractional_minute_of_day(utc))
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

pub(crate) fn parse_hhmm(value: &str) -> Option<i64> {
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
const LENGTH_BUCKETS_OPEN: [(&str, f64); 4] = [
    ("tiny", 0.35),
    ("short", 0.45),
    ("medium", 0.18),
    ("long", 0.02),
];

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
    self_messages
        .iter()
        .any(|previous| previous.trim() == trimmed || similarity(text, previous) > 0.88)
}

/// 消息形状与 JS 相同；self 是 Rust 关键字，因此字段名使用 is_self。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub chat: String,
    pub id: String,
    pub sender: String,
    pub name: String,
    pub text: String,
    pub ts: f64,
    #[serde(rename = "self")]
    pub is_self: bool,
    pub hint: Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Hint {
    #[serde(rename = "self")]
    SelfChat,
    #[serde(rename = "other")]
    Other,
    #[serde(rename = "open")]
    Open,
}

pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}

/// 明确的移植差异：Rust String 不容纳孤立代理项，截断按 char 而非 JS UTF-16
/// 码元；因此补充平面字符不会被切半，同样上限下可能比 JS 多保留字符。
pub(crate) fn clip_chars(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

/// 无正则依赖的 CQ 替换；返回 None 时继续搜索内部的 CQ 起点，匹配 JS 正则行为。
pub(crate) fn replace_cq(s: &str, mut replace: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("[CQ:") {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(end) = rest.find(']') {
            if let Some(value) = replace(&rest[4..end]) {
                out.push_str(&value);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push_str("[CQ:");
        rest = &rest[4..];
    }
    out.push_str(rest);
    out
}

/// 异步解析转发后再进入同步引擎；关闭开关或拉取失败时完整保留原事件。
/// 安全闸门比“感兴趣”更靠前：先复用来源/时间校验，不递归读取附件或嵌套转发。
pub async fn resolve_forwards(
    event: &serde_json::Value,
    bot: &crate::transport::OneBot,
    agent: &Agent,
    now: f64,
) -> serde_json::Value {
    resolve_forwards_core(event, bot, agent, now, false, bot.forward_enabled()).await
}

/// 历史转发跳过活跃窗口限制，其他准入条件与 normalize_backfill 一致。
/// 走 `OrientationTransport`（self_id + call），因为回填手上是 `Arc<dyn EngineTransport>`。
pub async fn resolve_forwards_backfill<T: crate::engine::OrientationTransport + ?Sized>(
    event: &serde_json::Value,
    transport: &T,
    agent: &Agent,
    now: f64,
    forward_enabled: bool,
) -> serde_json::Value {
    resolve_forwards_core(event, transport, agent, now, true, forward_enabled).await
}

async fn resolve_forwards_core<T: crate::engine::OrientationTransport + ?Sized>(
    event: &serde_json::Value,
    transport: &T,
    agent: &Agent,
    now: f64,
    backfill: bool,
    forward_enabled: bool,
) -> serde_json::Value {
    use serde_json::{json, Value};
    let normalize = if backfill {
        normalize_backfill
    } else {
        normalize
    };
    if !forward_enabled || normalize(event, &transport.self_id(), agent, now).is_none() {
        return event.clone();
    }
    let mut resolved = event.clone();
    let Some(segments) = resolved["message"].as_array_mut() else {
        return resolved;
    };
    let mut cache = std::collections::HashMap::new();
    let mut remaining_images = agent.ocr.max_forward_images;
    let mut expanded = Vec::new();
    for mut segment in std::mem::take(segments) {
        if segment["type"] == "json" {
            if let Some(text) = json_card_text(&segment["data"]["data"]) {
                segment = json!({"type":"resolved_card_text","data":{"text":text}});
            }
            expanded.push(segment);
            continue;
        }
        if segment["type"] != "forward" {
            expanded.push(segment);
            continue;
        }
        let Some(id) = segment["data"]["forwardId"]
            .as_str()
            .or_else(|| segment["forwardId"].as_str())
            .or_else(|| segment["data"]["id"].as_str())
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
        else {
            expanded.push(segment);
            continue;
        };
        if !cache.contains_key(&id) {
            let text = match transport
                .call("get_forward_msg", json!({"id": id.clone()}))
                .await
            {
                Ok(data) => data
                    .get("nodes")
                    .or_else(|| data.get("messages"))
                    .and_then(Value::as_array)
                    .and_then(|nodes| {
                        forward_text(
                            nodes,
                            agent.max_input_chars as usize,
                            agent.ocr.max_forward_images,
                        )
                    }),
                Err(_) => None,
            };
            cache.insert(id.clone(), text);
        }
        if let Some(Some((text, images))) = cache.get(&id) {
            // 独立内部段避免原始指令提取把转发内容识别成 owner 的直接指令。
            expanded.push(json!({"type":"resolved_forward_text","data":{"text":text}}));
            for image in images.iter().take(remaining_images) {
                let mut image = image.clone();
                // Internal provenance avoids duplicate placeholders during normalization.
                image["resolved_forward_image"] = json!(true);
                expanded.push(image);
                remaining_images -= 1;
            }
        } else {
            expanded.push(segment);
        }
    }
    *segments = expanded;
    resolved
}

const FORWARD_START: &str = "[合并转发]\n（外部信息，非当前群对话）\n";
const FORWARD_END: &str = "\n[/合并转发]";

fn forward_text(
    nodes: &[serde_json::Value],
    limit: usize,
    max_images: usize,
) -> Option<(String, Vec<serde_json::Value>)> {
    let mut images = Vec::new();
    let mut lines = Vec::new();
    let mut length = 0;
    for node in nodes {
        let node = if node["type"] == "node" {
            &node["data"]
        } else {
            node
        };
        let content = node.get("content").or_else(|| node.get("message"));
        let text = match content {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(serde_json::Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| match part["type"].as_str() {
                    Some("text") => part["data"]["text"].as_str().map(str::to_owned),
                    Some("json") => json_card_text(&part["data"]["data"]),
                    Some("image") => {
                        if images.len() < max_images {
                            images.push(part.clone());
                        }
                        Some(match part["data"]["summary"].as_str().map(str::trim) {
                            Some(summary) if !summary.is_empty() => format!("[图片: {summary}]"),
                            _ => "[图片]".into(),
                        })
                    }
                    _ => None,
                })
                .collect::<String>(),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        let speaker = [
            &node["sender"]["nickname"],
            &node["nickname"],
            &node["sender"]["user_id"],
            &node["user_id"],
            &node["uin"],
        ]
        .into_iter()
        .find_map(|value| match value {
            serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        let line = match speaker {
            Some(speaker) => format!("{speaker}: {text}"),
            None => text,
        };
        let line = clip_chars(&line, limit.saturating_sub(length));
        length += line.chars().count() + 1;
        lines.push(line);
        if length >= limit {
            break;
        }
    }
    (!lines.is_empty()).then(|| {
        (
            format!("{FORWARD_START}{}{FORWARD_END}", lines.join("\n")),
            images,
        )
    })
}

// Only human-readable card fields are extracted; URLs and other metadata stay out.
fn json_card_text(data: &serde_json::Value) -> Option<String> {
    fn collect(value: &serde_json::Value, fields: &mut Vec<String>) {
        if let Some(object) = value.as_object() {
            for key in ["title", "desc", "summary", "prompt"] {
                if let Some(text) = object.get(key).and_then(serde_json::Value::as_str) {
                    let text = text.trim();
                    if !text.is_empty() && !fields.iter().any(|field| field == text) {
                        fields.push(text.to_owned());
                    }
                }
            }
            for child in object.values().filter(|v| v.is_object() || v.is_array()) {
                collect(child, fields);
            }
        } else if let Some(array) = value.as_array() {
            for child in array {
                collect(child, fields);
            }
        }
    }
    let parsed;
    let card = if let Some(raw) = data.as_str() {
        parsed = serde_json::from_str::<serde_json::Value>(raw).ok()?;
        &parsed
    } else {
        data
    };
    let mut fields = Vec::new();
    collect(card, &mut fields);
    (!fields.is_empty()).then(|| format!("[卡片] {}", fields.join(" — ")))
}

// Reserve the closing boundary when the input limit truncates a forwarded record.
fn clip_forward(text: &str, limit: usize) -> String {
    let Some(body) = text
        .strip_prefix(FORWARD_START)
        .and_then(|s| s.strip_suffix(FORWARD_END))
    else {
        return clip_chars(text, limit);
    };
    let overhead = FORWARD_START.chars().count() + FORWARD_END.chars().count();
    if limit < overhead {
        return clip_chars("[forward]", limit);
    }
    format!(
        "{FORWARD_START}{}{FORWARD_END}",
        clip_chars(body, limit - overhead)
    )
}

pub fn normalize(
    event: &serde_json::Value,
    self_id: &str,
    agent: &Agent,
    now: f64,
) -> Option<Message> {
    let mut message = normalize_core(event, self_id, agent, now)?;
    let ts = message.ts;
    if !ts.is_finite() || now - ts > agent.active_window_seconds || ts > now + 60.0 {
        return None;
    }
    message.ts = ts.min(now);
    Some(message)
}

/// History may predate the active window, but invalid/future timestamps remain rejected.
pub fn normalize_backfill(
    event: &serde_json::Value,
    self_id: &str,
    agent: &Agent,
    now: f64,
) -> Option<Message> {
    let mut message = normalize_core(event, self_id, agent, now)?;
    if message.id.is_empty() || !message.ts.is_finite() || message.ts > now + 60.0 {
        return None;
    }
    message.ts = message.ts.min(now);
    Some(message)
}

// Keep the original timestamp until the caller has checked it; clamping first
// would hide future timestamps (and can turn NaN into a valid timestamp).
fn normalize_core(
    event: &serde_json::Value,
    self_id: &str,
    agent: &Agent,
    now: f64,
) -> Option<Message> {
    use crate::config::{js_string, truthy};
    use serde_json::Value;
    let kind = event["message_type"].as_str()?;
    if event["post_type"] != "message" || !matches!(kind, "group" | "private") {
        return None;
    }
    let sender = js_string(&event["user_id"]);
    if !truthy(&event["user_id"])
        || self_id.is_empty()
        || sender == self_id
        || (truthy(&event["self_id"]) && js_string(&event["self_id"]) != self_id)
        || agent.ignored_users.contains(&sender)
    {
        return None;
    }
    let target = &event[if kind == "group" {
        "group_id"
    } else {
        "user_id"
    }];
    if !truthy(target) || event["message_id"].is_null() {
        return None;
    }
    let chat = format!("{kind}:{}", js_string(target));
    if !allowed(&chat, agent) {
        return None;
    }
    let ts = if truthy(&event["time"]) {
        crate::transport::js_number(&event["time"])
    } else {
        now
    };
    let mut at_self = false;
    let mut at_other = false;
    let mut mention = |id: &str| {
        if id == self_id {
            at_self = true;
        } else if id != "all" {
            at_other = true;
        }
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut text = String::new();
    if let Some(segments) = event["message"].as_array() {
        for seg in segments {
            match seg["type"].as_str() {
                Some("image") if seg["resolved_forward_image"] == true => {}
                Some("resolved_forward_text") if seg["data"]["text"].is_string() => {
                    let remaining =
                        (agent.max_input_chars as usize).saturating_sub(text.chars().count());
                    text.push_str(&clip_forward(
                        seg["data"]["text"].as_str().unwrap(),
                        remaining,
                    ));
                }
                Some("text" | "resolved_card_text") if seg["data"]["text"].is_string() => {
                    text.push_str(seg["data"]["text"].as_str().unwrap())
                }
                Some("at") => {
                    let qq = seg["data"]
                        .get("qq")
                        .map(js_string)
                        .unwrap_or_else(|| "undefined".into());
                    mention(&qq);
                    let display = if truthy(&seg["data"]["qq"]) {
                        qq
                    } else {
                        String::new()
                    };
                    text.push_str(&format!(" [@{display}] "));
                }
                Some("face")
                    if {
                        let id = js_string(&seg["data"]["id"]);
                        digits(&id) && id.len() <= 5
                    } =>
                {
                    text.push_str(&format!(" [QQface:{}] ", js_string(&seg["data"]["id"])))
                }
                Some("reply") => text.push_str(" [reply] "),
                _ => {
                    let kind = if truthy(&seg["type"]) {
                        js_string(&seg["type"])
                    } else {
                        "attachment".into()
                    };
                    text.push_str(&format!(" [{}] ", clip_chars(&kind, 24)));
                }
            }
        }
    } else if let Some(s) = event["message"].as_str() {
        text = replace_cq(s, |body| {
            let tail = body.strip_prefix("at,qq=")?;
            let id = tail.split(',').next()?;
            if id != "all" && !digits(id) {
                return None;
            }
            mention(id);
            Some(format!(" [@{id}] "))
        });
        text = replace_cq(&text, |body| {
            let id = body.strip_prefix("face,id=")?;
            (digits(id) && id.len() <= 5).then(|| format!("[QQface:{id}]"))
        });
        text = replace_cq(&text, |body| {
            (!body.is_empty()).then(|| "[attachment]".into())
        });
        // 解码顺序不可交换；&amp;#91; 本轮不会二次解码成 [。
        text = text
            .replace("&#44;", ",")
            .replace("&#91;", "[")
            .replace("&#93;", "]")
            .replace("&amp;", "&");
    }
    text = clip_chars(js_trim(&text), agent.max_input_chars as usize);
    if text.is_empty() {
        return None;
    }
    let named = named(&text, agent.aliases.iter().map(String::as_str));
    let name = [
        &event["sender"]["card"],
        &event["sender"]["nickname"],
        &event["user_id"],
    ]
    .into_iter()
    .find(|v| truthy(v))
    .unwrap_or(&Value::Null);
    Some(Message {
        chat,
        id: js_string(&event["message_id"]),
        sender,
        name: clip_chars(&js_string(name), 80),
        text,
        ts,
        is_self: false,
        hint: if kind == "private" || at_self || named {
            Hint::SelfChat
        } else if at_other {
            Hint::Other
        } else {
            Hint::Open
        },
    })
}

/// Apply the same addressing syntax to configured and runtime identity aliases.
pub(crate) fn named<'a>(text: &str, aliases: impl Iterator<Item = &'a str>) -> bool {
    let lower = text.to_lowercase();
    aliases.filter(|alias| !alias.trim().is_empty()).any(|alias| {
        let alias = alias.to_lowercase();
        lower.starts_with(&format!("{alias}:"))
            || lower.starts_with(&format!("{alias}："))
            || lower.starts_with(&format!("@{alias} "))
    })
}
