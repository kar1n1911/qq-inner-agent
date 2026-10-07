//! config.mjs 的配置协议。校验与归一化分开，避免通过 &Value 隐式修改数据。
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);
fn check(ok: bool, message: impl Into<String>) -> std::result::Result<(), ConfigError> {
    if ok {
        Ok(())
    } else {
        Err(ConfigError(message.into()))
    }
}

pub fn defaults() -> Value {
    // 独立快照来自真实 defaults，绝不使用已过期的 config.example.json。
    serde_json::from_str(include_str!("defaults.json")).expect("embedded defaults")
}

pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

// JS String(x) 的 JSON 值子集；数组使用 join，null 数组成员变成空串。
pub(crate) fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Number(n) => {
            let x = n.as_f64().unwrap();
            if x == 0.0 {
                return "0".into();
            }
            // JS 在 [1e-6,1e21) 内输出十进制，范围外使用带正号的指数。
            if (1e-6..1e21).contains(&x.abs()) {
                x.to_string()
            } else {
                let s = format!("{x:e}");
                let (a, b) = s.split_once('e').unwrap();
                let e: i32 = b.parse().unwrap();
                format!("{a}e{e:+}")
            }
        }
        _ => v.to_string(),
    }
}

pub fn merge(base: &Value, extra: &Value) -> Value {
    let mut out = base.clone();
    let entries: Vec<(String, Value)> = match extra {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v.clone()))
            .collect(),
        // Object.entries(string) 按 UTF-16 索引；孤立代理项不能由 serde_json 表示，见兼容说明。
        Value::String(s) => s
            .encode_utf16()
            .enumerate()
            .map(|(i, c)| (i.to_string(), Value::String(String::from_utf16_lossy(&[c]))))
            .collect(),
        _ => Vec::new(),
    };
    for (k, v) in entries {
        if ["__proto__", "constructor", "prototype"].contains(&k.as_str()) {
            continue;
        }
        let old = match &out {
            Value::Object(m) => m.get(&k),
            Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
            _ => None,
        };
        let next = if v.is_object() && old.is_some_and(|v| v.is_object() || v.is_array()) {
            merge(old.unwrap(), &v)
        } else {
            v
        };
        match &mut out {
            Value::Object(m) => {
                m.insert(k, next);
            }
            Value::Array(a) => {
                // JS 数组的普通属性不参与 JSON 序列化；有效索引赋值可以扩展数组。
                if k == "length" {
                    // 数组 length 是特殊属性；JSON 中合法的长度赋值会截断或补 null。
                    let len = match &next {
                        Value::Null => Some(0.0),
                        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                        _ => {
                            let text = js_string(&next);
                            if !nonblank(&text) {
                                Some(0.0)
                            } else {
                                text.parse::<f64>().ok()
                            }
                        }
                    };
                    if let Some(n) = len.filter(|n| {
                        n.is_finite() && *n >= 0.0 && *n <= u32::MAX as f64 && n.fract() == 0.0
                    }) {
                        a.resize(n as usize, Value::Null);
                    }
                } else if let Ok(i) = k.parse::<usize>() {
                    if i.to_string() == k && i < u32::MAX as usize {
                        a.resize(a.len().max(i + 1), Value::Null);
                        a[i] = next;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn at<'a>(c: &'a Value, key: &str) -> &'a Value {
    key.split('.').fold(c, |v, k| &v[k])
}
fn number(v: &Value, min: f64, max: f64, integer: bool) -> bool {
    v.as_f64()
        .is_some_and(|n| n.is_finite() && n >= min && n <= max && (!integer || n.fract() == 0.0))
}
// JS trim 的空白集合与 Rust trim 不同：包含 BOM，不包含 U+0085。
fn nonblank(s: &str) -> bool {
    !s.trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).is_empty()
}
// JS 无 u 标志的 \d 只匹配 ASCII；不用 Rust 的 Unicode 数字判断。$ 在这里严格匹配结尾。
fn qq_id(s: &str) -> bool {
    s.as_bytes()
        .first()
        .is_some_and(|b| (b'1'..=b'9').contains(b))
        && s.bytes().all(|b| b.is_ascii_digit())
}
fn clock_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && [b[0], b[1], b[3], b[4]].iter().all(u8::is_ascii_digit)
        && &s[..2] <= "23"
        && &s[3..] <= "59"
}
fn timezone(v: Option<&Value>) -> bool {
    // Intl 的 undefined 使用系统时区；IANA 名称大小写不敏感。数据库版本可能与 Node ICU 不同。
    let Some(v) = v else {
        return true;
    };
    let s = js_string(v);
    if chrono_tz::TZ_VARIANTS
        .iter()
        .any(|tz| tz.name().eq_ignore_ascii_case(&s))
    {
        return true;
    }
    // 新版 Intl 也接受 ±HH、±HHMM、±HH:MM 固定偏移。
    let b = s.as_bytes();
    if b.first().is_some_and(|c| *c == b'+' || *c == b'-') {
        let digits = s[1..].replace(':', "");
        return (b.len() == 3 || b.len() == 5 || (b.len() == 6 && b[3] == b':'))
            && digits.bytes().all(|b| b.is_ascii_digit())
            && digits.len() >= 2
            && &digits[..2] <= "23"
            && (digits.len() == 2 || &digits[2..] <= "59");
    }
    false
}
fn url_host(raw: &Value, name: &str, schemes: &[&str]) -> std::result::Result<String, ConfigError> {
    // request_url 只解析 URL，不执行请求；复用现有 ureq 的 WHATWG url 实现，不加依赖。
    let parsed = ureq::get(&js_string(raw))
        .request_url()
        .map_err(|_| ConfigError("Invalid URL".into()))?;
    let u = parsed.as_url();
    check(
        schemes.contains(&u.scheme())
            && u.username().is_empty()
            && u.password().unwrap_or("").is_empty()
            && u.query().unwrap_or("").is_empty()
            && u.fragment().unwrap_or("").is_empty(),
        format!("{name}: invalid URL (no credentials/query/fragment)"),
    )?;
    let host = u.host_str().unwrap_or("");
    check(
        !["http", "ws"].contains(&u.scheme())
            || ["127.0.0.1", "localhost", "[::1]"].contains(&host),
        format!("{name}: use TLS outside localhost"),
    )?;
    Ok(host.to_owned())
}

pub const LEGACY_PERSONAS: [&str;2]=[
    "你是 QQ 聊天中的 AI 参与者。友善、简洁、真诚，保持好奇心，结合聊天内容提供有用的回应。不要编造亲身经历，也不要冒充真人。",
    "You are a thoughtful AI participant in a QQ conversation. Be helpful, concise, curious, and honest. Match the language and tone of the conversation. Never invent personal experiences or claim to be human."
];

pub fn validate(c: &Value) -> std::result::Result<(), ConfigError> {
    let a = &c["agent"];
    if let Some(v) = a.get("backstory") {
        serde_json::from_value::<Backstory>(v.clone()).map_err(|e| ConfigError(e.to_string()))?;
    }
    if let Some(v) = a.get("identity") {
        let identity: Identity =
            serde_json::from_value(v.clone()).map_err(|e| ConfigError(e.to_string()))?;
        check(
            identity.cooldown_days.is_finite() && identity.cooldown_days >= 0.,
            "agent.identity.cooldownDays must be finite and nonnegative",
        )?;
        check(
            identity.min_age_days.is_finite() && identity.min_age_days >= 0.,
            "agent.identity.minAgeDays must be finite and nonnegative",
        )?;
    }
    if let Some(v) = a.get("relay") {
        let settings: crate::topic::relay::RelaySettings =
            serde_json::from_value(v.clone()).map_err(|e| ConfigError(e.to_string()))?;
        settings
            .validate()
            .map_err(|e| ConfigError(e.to_string()))?;
    }
    if let Some(v) = a.get("topicSource") {
        let settings: crate::topic::Settings =
            serde_json::from_value(v.clone()).map_err(|e| ConfigError(e.to_string()))?;
        settings
            .validate()
            .map_err(|e| ConfigError(e.to_string()))?;
    }
    if let Some(t) = a.get("ownerTeaching") {
        check(t.is_object(), "agent.ownerTeaching must be object")?;
        if let Some(v) = t.get("enabled") {
            check(
                v.is_boolean(),
                "agent.ownerTeaching.enabled must be boolean",
            )?;
        }
        if let Some(v) = t.get("ownerUin") {
            let id = v.as_str().unwrap_or("");
            check(
                id.starts_with(|c: char| ('1'..='9').contains(&c))
                    && id.bytes().all(|b| b.is_ascii_digit()),
                "agent.ownerTeaching.ownerUin must be a QQ ID string",
            )?;
        }
    }
    let p = &a["personality"];
    let e = &a["expression"];
    let emoji = &a["emoji"];
    for key in ["learnFrequency", "faceOnly"] {
        if let Some(value) = emoji.get(key) {
            check(
                value.is_boolean(),
                format!("agent.emoji.{key} must be boolean"),
            )?;
        }
    }
    check(
        [json!("zh-CN"), json!("en")].contains(&c["ui"]["language"]),
        "Invalid interface language",
    )?;
    check(
        [json!("auto"), json!("zh-CN"), json!("en")].contains(&a["replyLanguage"]),
        "Invalid reply language",
    )?;
    check(
        truthy(p) && truthy(e) && truthy(emoji),
        "Invalid expression settings",
    )?;
    for k in ["behavior", "replyStyle"] {
        check(
            p[k].as_str()
                .is_some_and(|s| s.encode_utf16().count() <= 2000),
            format!("Invalid personality.{k}"),
        )?;
    }
    // JS length 是 UTF-16 单元数；emoji 不能按 UTF-8 字节数或 chars() 计数。
    for (v, max, len) in [
        (&p["interests"], 20, 80),
        (&p["variants"], 8, 500),
        (&emoji["symbols"], 30, 24),
        (&emoji["faceIds"], 30, 5),
    ] {
        check(
            v.as_array().is_some_and(|a| {
                a.len() <= max
                    && a.iter().all(|v| {
                        v.as_str()
                            .is_some_and(|s| nonblank(s) && s.encode_utf16().count() <= len)
                    })
            }),
            "Invalid expression list",
        )?;
    }
    check(
        emoji["faceIds"].as_array().unwrap().iter().all(|v| {
            v.as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 5 && s.bytes().all(|b| b.is_ascii_digit())
            })
        }),
        "Invalid QQ face ID",
    )?;
    for v in [&e["learn"], &e["useLearned"], &emoji["enabled"]] {
        check(v.is_boolean(), "Invalid expression switch")?;
    }
    for v in [
        &p["variantProbability"],
        &e["minConfidence"],
        &emoji["probability"],
    ] {
        check(number(v, 0., 1., false), "Invalid expression probability")?;
    }
    for (v, min, max) in [
        (&e["maxPerReply"], 1., 5.),
        (&e["maxEntries"], 1., 500.),
        (&e["retentionDays"], 1., 3650.),
        (&e["reuseSeconds"], 0., 86400.),
        (&emoji["cooldownSeconds"], 0., 86400.),
    ] {
        check(number(v, min, max, true), "Invalid expression limit")?;
    }
    let r = &a["rhythm"];
    check(r["enabled"].is_boolean(), "Invalid activity rhythm")?;
    for k in ["dayProbability", "edgeProbability", "centerProbability"] {
        check(number(&r[k], 0., 1., false), format!("Invalid rhythm.{k}"))?;
    }
    check(
        r["centerProbability"].as_f64() <= r["edgeProbability"].as_f64(),
        "Rhythm center probability must not exceed edge probability",
    )?;
    check(number(&r["sigma"], 0.05, 1., false), "Invalid rhythm.sigma")?;
    for kind in ["active", "rest"] {
        for bound in ["Min", "Max"] {
            let k = format!("{kind}{bound}Seconds");
            check(
                number(&r[&k], 30., 86400., true),
                format!("Invalid rhythm.{k}"),
            )?;
        }
        check(
            r[format!("{kind}MinSeconds")].as_f64() <= r[format!("{kind}MaxSeconds")].as_f64(),
            format!("Invalid rhythm.{kind} duration range"),
        )?;
    }
    let o = &a["observation"];
    if let Some(digest) = o.get("backlogDigest") {
        let settings: crate::engine::backlog::Settings = serde_json::from_value(digest.clone())
            .map_err(|_| ConfigError("Invalid observation.backlogDigest settings".into()))?;
        settings.validate().map_err(|e| ConfigError(e.to_string()))?;
    }
    check(
        o["enabled"].is_boolean() && [json!("both"), json!("either")].contains(&o["thresholdMode"]),
        "Invalid observation settings",
    )?;
    for (k, min, max) in [
        ("minSeconds", 1., 604800.),
        ("minMessages", 1., 10000.),
        ("historyLimit", 1., 100.),
    ] {
        check(
            number(&o[k], min, max, true),
            format!("Invalid observation.{k}"),
        )?;
    }
    check(
        a["memory"].is_object() || a["memory"].is_array(),
        "Invalid memory settings",
    )?;
    if let Some(n) = a["memory"].get("partialEvidence") {
        check(
            number(n, 1., 100., true),
            "Invalid memory.partialEvidence: expected 1..100",
        )?;
    }
    for &(k, min, max) in MEMORY_RANGES {
        check(
            number(&a["memory"][k], min, max, true),
            format!("Invalid memory.{k}: expected {min}..{max}"),
        )?;
    }
    check(
        a["learning"]["enabled"].is_boolean(),
        "Invalid learning settings",
    )?;
    for k in [
        "minMessages",
        "intervalSeconds",
        "maxMemories",
        "memoryDays",
        "retrievalLimit",
    ] {
        check(
            a["learning"][k]
                .as_f64()
                .is_some_and(|v| v.is_finite() && v.fract() == 0.),
            format!("Invalid learning.{k}: expected integer"),
        )?;
    }
    check(
        a["sending"]["enabled"].is_boolean(),
        "Invalid sending policy",
    )?;
    let s = &a["schedule"];
    check(s["enabled"].is_boolean(), "Invalid activity schedule")?;
    for k in ["activeStart", "inactiveStart"] {
        check(
            s[k].as_str().is_some_and(clock_time),
            format!("Invalid schedule.{k}: use HH:MM"),
        )?;
    }
    check(
        s["activeStart"] != s["inactiveStart"],
        "Active and inactive start times must differ; disable the schedule for all-day activity",
    )?;
    check(
        s["timezone"].as_str().is_some_and(nonblank),
        "Invalid schedule timezone",
    )?;
    check(timezone(s.get("timezone")), "Invalid time zone")?;
    check(
        [json!("openai"), json!("anthropic")].contains(&c["provider"]["kind"]),
        "provider.kind must be openai or anthropic",
    )?;
    url_host(
        &c["provider"]["baseUrl"],
        "provider.baseUrl",
        &["https", "http"],
    )?;
    url_host(&c["onebot"]["url"], "onebot.url", &["ws", "wss"])?;
    check(
        [json!("max_tokens"), json!("max_completion_tokens")]
            .contains(&c["provider"]["tokenParameter"]),
        "Invalid tokenParameter",
    )?;
    check(
        [json!("x-api-key"), json!("bearer")].contains(&c["provider"]["anthropicAuth"]),
        "Invalid anthropicAuth",
    )?;
    check(
        [Value::Null, json!("disabled")].contains(&c["provider"]["thinking"]),
        "thinking must be null or disabled",
    )?;
    for &(k, min, max) in RANGES {
        check(
            number(at(c, k), min, max, false),
            format!("Invalid {k}: expected {min}..{max}"),
        )?;
    }
    for k in ["allowedGroups", "allowedUsers", "ignoredUsers"] {
        check(
            a[k].as_array()
                .is_some_and(|a| a.iter().all(|v| qq_id(&js_string(v)))),
            format!("agent.{k}: expected QQ IDs"),
        )?;
    }
    check(
        a["aliases"]
            .as_array()
            .is_some_and(|a| a.iter().all(|v| v.as_str().is_some_and(nonblank))),
        "Invalid aliases",
    )?;
    let id = js_string(&c["onebot"]["selfId"]);
    check(id.is_empty() || qq_id(&id), "Invalid selfId")?;
    for k in ["proactive", "dryRun", "proactiveTone"] {
        check(a[k].is_boolean(), format!("Invalid {k}"))?;
    }
    let q = &a["quietHours"];
    if truthy(q) {
        for k in ["start", "end"] {
            check(number(&q[k], 0., 23., true), "Invalid quiet hours")?;
        }
        check(timezone(q.get("timezone")), "Invalid time zone")?;
    }
    Ok(())
}

/// 对应 JS validate 的可观察修改；不可变 validate 只检查，加载必须走本函数。
pub fn normalize(c: &Value) -> std::result::Result<Value, ConfigError> {
    validate(c)?;
    let mut c = c.clone();
    if c["agent"]["persona"]
        .as_str()
        .is_some_and(|s| LEGACY_PERSONAS.contains(&s))
    {
        c["agent"]["persona"] = defaults()["agent"]["persona"].clone();
    }
    for k in ["allowedGroups", "allowedUsers", "ignoredUsers"] {
        c["agent"][k] = Value::Array(
            c["agent"][k]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| Value::String(js_string(v)))
                .collect(),
        );
    }
    Ok(c)
}

const RANGES: &[(&str, f64, f64)] = &[
    ("agent.memory.minConfidence", 0.0, 1.0),
    ("agent.learning.minMessages", 1.0, 100.0),
    ("agent.learning.intervalSeconds", 30.0, 86400.0),
    ("agent.learning.maxMemories", 1.0, 500.0),
    ("agent.learning.memoryDays", 1.0, 365.0),
    ("agent.learning.retrievalLimit", 1.0, 20.0),
    ("agent.sending.proactiveProbability", 0.0, 1.0),
    ("agent.sending.addressedProbability", 0.0, 1.0),
    ("agent.sending.settleSeconds", 1.0, 3600.0),
    ("agent.sending.recoverySeconds", 1.0, 86400.0),
    ("agent.sending.burstScale", 1.0, 100.0),
    ("agent.sending.maxNegativeProbability", 0.0, 1.0),
    ("agent.sending.expectationSeconds", 1.0, 86400.0),
    ("provider.maxTokens", 128.0, 32000.0),
    ("provider.timeoutSeconds", 1.0, 300.0),
    ("provider.retries", 0.0, 5.0),
    ("provider.requestsPerHour", 1.0, 10000.0),
    ("onebot.heartbeatSeconds", 1.0, 300.0),
    ("onebot.requestTimeoutSeconds", 1.0, 120.0),
    ("onebot.reconnectMaxSeconds", 1.0, 300.0),
    ("agent.threshold", 1.0, 5.0),
    ("agent.interruptThreshold", 1.0, 5.0),
    ("agent.system1Probability", 0.0, 1.0),
    ("agent.pauseSeconds", 1.0, 3600.0),
    ("agent.debounceSeconds", 0.0, 60.0),
    ("agent.minThinkIntervalSeconds", 1.0, 3600.0),
    ("agent.proactiveCooldownSeconds", 0.0, 86400.0),
    ("agent.maxProactivePerHour", 0.0, 100.0),
    ("agent.maxMessagesPerHour", 1.0, 200.0),
    ("agent.activeWindowSeconds", 1.0, 86400.0),
    ("agent.thoughtTtlSeconds", 1.0, 86400.0),
    ("agent.thoughtLimit", 1.0, 30.0),
    ("agent.historyLimit", 1.0, 100.0),
    ("agent.maxInputChars", 100.0, 10000.0),
    ("agent.maxOutputChars", 10.0, 4000.0),
    ("agent.maxConcurrentChats", 1.0, 8.0),
    ("agent.maxActiveChats", 1.0, 500.0),
    ("storage.retentionDays", 1.0, 3650.0),
    ("storage.maxMessagesPerChat", 25.0, 10000.0),
];
const MEMORY_RANGES: &[(&str, f64, f64)] = &[
    ("recallChars", 200.0, 12000.0),
    ("recallHalfLifeDays", 1.0, 3650.0),
    ("revisionLimit", 1.0, 10.0),
    ("shortHours", 1.0, 720.0),
    ("shortLimit", 1.0, 200.0),
    ("shortChars", 100.0, 2000.0),
    ("longChars", 200.0, 8000.0),
    ("traitChars", 100.0, 4000.0),
    ("longDays", 1.0, 3650.0),
    ("traitDays", 1.0, 3650.0),
    ("maxPeople", 1.0, 1000.0),
];

/// JS 未限制类型的文本字段，加载时缓存 String(value) 和原始 truthiness。
/// 热路径无需访问 JSON；原始类型（包括 null/数组）仍完整保存在 Loaded.raw。
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeText {
    pub text: String,
    pub is_truthy: bool,
}
impl<'de> Deserialize<'de> for RuntimeText {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let v = Value::deserialize(deserializer)?;
        Ok(Self {
            text: js_string(&v),
            is_truthy: truthy(&v),
        })
    }
}
fn deserialize_js_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<String, D::Error> {
    Ok(js_string(&Value::deserialize(deserializer)?))
}
// 所有数值用 f64：JS 的普通 ranges 允许小数，专用整数规则由 validate 保证。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ui {
    /// 配置键 `ui.language`。
    pub language: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// 配置键 `provider.kind`。
    pub kind: String,
    /// 配置键 `provider.baseUrl`。
    #[serde(deserialize_with = "deserialize_js_string")]
    pub base_url: String,
    /// 配置键 `provider.model`。
    pub model: RuntimeText,
    /// 配置键 `provider.maxTokens`。
    pub max_tokens: f64,
    /// 配置键 `provider.tokenParameter`。
    pub token_parameter: String,
    /// 配置键 `provider.timeoutSeconds`。
    pub timeout_seconds: f64,
    /// 配置键 `provider.retries`。
    pub retries: f64,
    /// 配置键 `provider.requestsPerHour`。
    pub requests_per_hour: f64,
    /// 配置键 `provider.anthropicAuth`。
    pub anthropic_auth: String,
    /// 配置键 `provider.workspaceId`。
    pub workspace_id: RuntimeText,
    /// 配置键 `provider.thinking`。
    pub thinking: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Onebot {
    /// Rust 专用开关：转发引用收发默认关闭。
    #[serde(default)]
    pub forward_enabled: bool,
    /// 配置键 `onebot.url`。
    #[serde(deserialize_with = "deserialize_js_string")]
    pub url: String,
    /// 配置键 `onebot.selfId`。
    pub self_id: RuntimeText,
    /// 配置键 `onebot.heartbeatSeconds`。
    pub heartbeat_seconds: f64,
    /// 配置键 `onebot.requestTimeoutSeconds`。
    pub request_timeout_seconds: f64,
    /// 配置键 `onebot.reconnectMaxSeconds`。
    pub reconnect_max_seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Personality {
    /// 配置键 `agent.personality.behavior`。
    pub behavior: String,
    /// 配置键 `agent.personality.replyStyle`。
    pub reply_style: String,
    /// 配置键 `agent.personality.interests`。
    pub interests: Vec<String>,
    /// 配置键 `agent.personality.variants`。
    pub variants: Vec<String>,
    /// 配置键 `agent.personality.variantProbability`。
    pub variant_probability: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    /// 配置键 `agent.expression.learn`。
    pub learn: bool,
    /// 配置键 `agent.expression.useLearned`。
    pub use_learned: bool,
    /// 配置键 `agent.expression.minConfidence`。
    pub min_confidence: f64,
    /// 配置键 `agent.expression.maxPerReply`。
    pub max_per_reply: f64,
    /// 配置键 `agent.expression.maxEntries`。
    pub max_entries: f64,
    /// 配置键 `agent.expression.retentionDays`。
    pub retention_days: f64,
    /// 配置键 `agent.expression.reuseSeconds`。
    pub reuse_seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Emoji {
    /// P6c 门控新功能：缺省关闭，省略 false 保持配置序列化 parity。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub learn_frequency: bool,
    /// 仅允许轻松附和的单 face 回复；不是长度 parity 开关。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub face_only: bool,
    /// 配置键 `agent.emoji.enabled`。
    pub enabled: bool,
    /// 配置键 `agent.emoji.probability`。
    pub probability: f64,
    /// 配置键 `agent.emoji.cooldownSeconds`。
    pub cooldown_seconds: f64,
    /// 配置键 `agent.emoji.symbols`。
    pub symbols: Vec<String>,
    /// 配置键 `agent.emoji.faceIds`。
    pub face_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Learning {
    /// 配置键 `agent.learning.enabled`。
    pub enabled: bool,
    /// 配置键 `agent.learning.minMessages`。
    pub min_messages: f64,
    /// 配置键 `agent.learning.intervalSeconds`。
    pub interval_seconds: f64,
    /// 配置键 `agent.learning.maxMemories`。
    pub max_memories: f64,
    /// 配置键 `agent.learning.memoryDays`。
    pub memory_days: f64,
    /// 配置键 `agent.learning.retrievalLimit`。
    pub retrieval_limit: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    /// 配置键 `agent.observation.enabled`。
    pub enabled: bool,
    /// 配置键 `agent.observation.minSeconds`。
    pub min_seconds: f64,
    /// 配置键 `agent.observation.minMessages`。
    pub min_messages: f64,
    /// 配置键 `agent.observation.thresholdMode`。
    pub threshold_mode: String,
    /// 配置键 `agent.observation.historyLimit`。
    pub history_limit: f64,
    /// 独立于入群观察开关：仅在大量积压时压缩本轮模型上下文。
    #[serde(default, skip_serializing_if = "crate::engine::backlog::Settings::is_default")]
    pub backlog_digest: crate::engine::backlog::Settings,
}

fn default_partial_evidence() -> usize {
    2
}
fn is_default_partial_evidence(n: &usize) -> bool {
    *n == 2
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    /// Independent new evidence required after a partial baseline (Rust only).
    #[serde(
        default = "default_partial_evidence",
        skip_serializing_if = "is_default_partial_evidence"
    )]
    pub partial_evidence: usize,
    /// 配置键 `agent.memory.recallChars`。
    pub recall_chars: f64,
    /// 配置键 `agent.memory.recallHalfLifeDays`。
    pub recall_half_life_days: f64,
    /// 配置键 `agent.memory.minConfidence`。
    pub min_confidence: f64,
    /// 配置键 `agent.memory.revisionLimit`。
    pub revision_limit: f64,
    /// 配置键 `agent.memory.shortHours`。
    pub short_hours: f64,
    /// 配置键 `agent.memory.shortLimit`。
    pub short_limit: f64,
    /// 配置键 `agent.memory.shortChars`。
    pub short_chars: f64,
    /// 配置键 `agent.memory.longChars`。
    pub long_chars: f64,
    /// 配置键 `agent.memory.traitChars`。
    pub trait_chars: f64,
    /// 配置键 `agent.memory.longDays`。
    pub long_days: f64,
    /// 配置键 `agent.memory.traitDays`。
    pub trait_days: f64,
    /// 配置键 `agent.memory.maxPeople`。
    pub max_people: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sending {
    /// 配置键 `agent.sending.enabled`。
    pub enabled: bool,
    /// 配置键 `agent.sending.proactiveProbability`。
    pub proactive_probability: f64,
    /// 配置键 `agent.sending.addressedProbability`。
    pub addressed_probability: f64,
    /// 配置键 `agent.sending.settleSeconds`。
    pub settle_seconds: f64,
    /// 配置键 `agent.sending.recoverySeconds`。
    pub recovery_seconds: f64,
    /// 配置键 `agent.sending.burstScale`。
    pub burst_scale: f64,
    /// 配置键 `agent.sending.maxNegativeProbability`。
    pub max_negative_probability: f64,
    /// 配置键 `agent.sending.expectationSeconds`。
    pub expectation_seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rhythm {
    /// 配置键 `agent.rhythm.enabled`。
    pub enabled: bool,
    /// 配置键 `agent.rhythm.dayProbability`。
    pub day_probability: f64,
    /// 配置键 `agent.rhythm.edgeProbability`。
    pub edge_probability: f64,
    /// 配置键 `agent.rhythm.centerProbability`。
    pub center_probability: f64,
    /// 配置键 `agent.rhythm.sigma`。
    pub sigma: f64,
    /// 配置键 `agent.rhythm.activeMinSeconds`。
    pub active_min_seconds: f64,
    /// 配置键 `agent.rhythm.activeMaxSeconds`。
    pub active_max_seconds: f64,
    /// 配置键 `agent.rhythm.restMinSeconds`。
    pub rest_min_seconds: f64,
    /// 配置键 `agent.rhythm.restMaxSeconds`。
    pub rest_max_seconds: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// 配置键 `agent.schedule.enabled`。
    pub enabled: bool,
    /// 配置键 `agent.schedule.activeStart`。
    pub active_start: String,
    /// 配置键 `agent.schedule.inactiveStart`。
    pub inactive_start: String,
    /// 配置键 `agent.schedule.timezone`。
    pub timezone: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietHours {
    /// 配置键 `agent.quietHours.start`。
    pub start: f64,
    /// 配置键 `agent.quietHours.end`。
    pub end: f64,
    /// 配置键 `agent.quietHours.timezone`；JS 允许省略，表示系统时区。
    #[serde(default)]
    pub timezone: Option<String>,
}

/// Rust 专用教学开关；不改 JS 默认配置快照。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OwnerTeaching {
    pub enabled: bool,
    pub owner_uin: String,
}
impl Default for OwnerTeaching {
    fn default() -> Self {
        Self {
            enabled: false,
            owner_uin: "1950202917".into(),
        }
    }
}

/// 身份修改属高风险；总开关和每种修改权限都默认关闭。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Identity {
    pub enabled: bool,
    pub min_traits: usize,
    pub min_age_days: f64,
    pub cooldown_days: f64,
    pub grow_persona: bool,
    pub allow_nickname: bool,
    pub allow_group_card: bool,
    pub allow_avatar: bool,
    pub allow_signature: bool,
}
impl Default for Identity {
    fn default() -> Self {
        Self {
            enabled: false,
            min_traits: 3,
            min_age_days: 7.0,
            cooldown_days: 14.0,
            grow_persona: false,
            allow_nickname: false,
            allow_group_card: false,
            allow_avatar: false,
            allow_signature: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Backstory {
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    #[serde(default)]
    pub backstory: Backstory,
    #[serde(default)]
    pub identity: Identity,
    #[serde(default)]
    pub relay: crate::topic::relay::RelaySettings,
    #[serde(default)]
    pub topic_source: crate::topic::Settings,
    /// P6h：三个功能的入口均默认关闭。
    #[serde(default)]
    pub affect: crate::persona::affect::Settings,
    #[serde(default)]
    pub memory_recall: bool,
    /// P6d：默认关闭，关闭时保持 JS 的触发顺序与模型输入。
    #[serde(default)]
    pub three_layer_decision: bool,
    #[serde(default)]
    pub owner_teaching: OwnerTeaching,
    /// 配置键 `agent.name`。
    pub name: RuntimeText,
    /// 配置键 `agent.persona`。
    pub persona: RuntimeText,
    /// 配置键 `agent.replyLanguage`。
    pub reply_language: String,
    /// 配置键 `agent.personality`。
    pub personality: Personality,
    /// 配置键 `agent.expression`。
    pub expression: Expression,
    /// 配置键 `agent.emoji`。
    pub emoji: Emoji,
    /// 配置键 `agent.multiBubble`（默认关闭）：articulation 可返回 `bubbles` 数组，
    /// 引擎按间隔依次发送并加与长度成比例的打字延迟（含抖动）。
    #[serde(default)]
    pub multi_bubble: bool,
    /// 配置键 `agent.learning`。
    pub learning: Learning,
    /// 配置键 `agent.observation`。
    pub observation: Observation,
    /// 配置键 `agent.memory`。
    pub memory: Memory,
    /// 配置键 `agent.aliases`。
    pub aliases: Vec<String>,
    /// 配置键 `agent.allowedGroups`。
    pub allowed_groups: Vec<String>,
    /// 配置键 `agent.allowedUsers`。
    pub allowed_users: Vec<String>,
    /// 配置键 `agent.ignoredUsers`。
    pub ignored_users: Vec<String>,
    /// 配置键 `agent.proactive`。
    pub proactive: bool,
    /// 配置键 `agent.dryRun`。
    pub dry_run: bool,
    /// 配置键 `agent.threshold`。
    pub threshold: f64,
    /// 配置键 `agent.interruptThreshold`。
    pub interrupt_threshold: f64,
    /// 配置键 `agent.sending`。
    pub sending: Sending,
    /// 配置键 `agent.rhythm`。
    pub rhythm: Rhythm,
    /// 配置键 `agent.schedule`。
    pub schedule: Schedule,
    /// 配置键 `agent.system1Probability`。
    pub system1_probability: f64,
    /// 配置键 `agent.proactiveTone`。
    pub proactive_tone: bool,
    /// 配置键 `agent.pauseSeconds`。
    pub pause_seconds: f64,
    /// 配置键 `agent.debounceSeconds`。
    pub debounce_seconds: f64,
    /// 配置键 `agent.minThinkIntervalSeconds`。
    pub min_think_interval_seconds: f64,
    /// 配置键 `agent.proactiveCooldownSeconds`。
    pub proactive_cooldown_seconds: f64,
    /// 配置键 `agent.maxProactivePerHour`。
    pub max_proactive_per_hour: f64,
    /// 配置键 `agent.maxMessagesPerHour`。
    pub max_messages_per_hour: f64,
    /// 配置键 `agent.activeWindowSeconds`。
    pub active_window_seconds: f64,
    /// 配置键 `agent.thoughtTtlSeconds`。
    pub thought_ttl_seconds: f64,
    /// 配置键 `agent.thoughtLimit`。
    pub thought_limit: f64,
    /// 配置键 `agent.historyLimit`。
    pub history_limit: f64,
    /// 配置键 `agent.maxInputChars`。
    pub max_input_chars: f64,
    /// 配置键 `agent.maxOutputChars`。
    pub max_output_chars: f64,
    /// 配置键 `agent.maxConcurrentChats`。
    pub max_concurrent_chats: f64,
    /// 配置键 `agent.maxActiveChats`。
    pub max_active_chats: f64,
    /// 配置键 `agent.quietHours`。
    pub quiet_hours: Option<QuietHours>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    /// 配置键 `storage.directory`。
    pub directory: String,
    /// 配置键 `storage.retentionDays`。
    pub retention_days: f64,
    /// 配置键 `storage.maxMessagesPerChat`。
    pub max_messages_per_chat: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// 配置键 `ui`。
    pub ui: Ui,
    /// 配置键 `provider`。
    pub provider: Provider,
    /// 配置键 `onebot`。
    pub onebot: Onebot,
    /// 配置键 `agent`。
    pub agent: Agent,
    /// 配置键 `storage`。
    pub storage: Storage,
    /// 加载派生键 `apiKey`。
    pub api_key: RuntimeText,
    /// 加载派生键 `onebotToken`。
    pub onebot_token: RuntimeText,
    /// 加载派生键 `dataDir`。
    pub data_dir: PathBuf,
}

pub struct Loaded {
    /// 完整强类型运行时视图。
    pub config: Config,
    /// JS loadConfig 返回的完整对象，含未知字段、派生键和归一化 ID。
    pub raw: Value,
}

impl Config {
    pub fn from_value(value: &Value) -> Result<Self> {
        let mut view = normalize(value)?;
        // JS quietHours 的任何 falsy 值都关闭静默；原始值保留在 Loaded.raw。
        if !truthy(&view["agent"]["quietHours"]) {
            view["agent"]["quietHours"] = Value::Null;
        }
        serde_json::from_value(view).context("construct runtime configuration")
    }
}

/// Editable defaults, including serde-defaulted fields, without loading files or secrets.
pub fn public_defaults() -> Result<Value> {
    let defaults = defaults();
    let mut input = defaults.clone();
    for key in ["apiKey", "onebotToken", "dataDir"] {
        input[key] = json!("");
    }
    let config = Config::from_value(&input)?;
    // Restore public scalar values in place of RuntimeText's internal representation.
    let mut schema = merge(&serde_json::to_value(&config)?, &defaults);
    for key in ["apiKey", "onebotToken", "dataDir"] {
        schema.as_object_mut().unwrap().remove(key);
    }
    // These defaults are deliberately omitted by runtime serialization for parity.
    schema["agent"]["emoji"]["learnFrequency"] = json!(config.agent.emoji.learn_frequency);
    schema["agent"]["emoji"]["faceOnly"] = json!(config.agent.emoji.face_only);
    schema["agent"]["memory"]["partialEvidence"] = json!(config.agent.memory.partial_evidence);
    Ok(schema)
}

fn absolute(root: &Path, directory: &str) -> Result<PathBuf> {
    // path.resolve 是词法解析，不能 canonicalize：目录未创建或为符号链接时也必须成功。
    let path = std::env::current_dir()?.join(root).join(directory);
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            p => out.push(p.as_os_str()),
        }
    }
    Ok(out)
}

pub fn load_config(root: &Path) -> Result<Loaded> {
    load_with_env(root, |key| std::env::var(key).ok())
}

// 环境读取可注入，测试不修改全局环境，避免 cargo test 并行时污染其他测试。
pub fn load_with_env(root: &Path, env: impl Fn(&str) -> Option<String>) -> Result<Loaded> {
    let read = |name: &str| {
        crate::settings::read_json(&root.join(name)).map(|v| v.unwrap_or_else(|| json!({})))
    };
    let mut raw = normalize(&merge(&defaults(), &read("config.json")?))?;
    let secrets = read("secrets.json")?;
    anyhow::ensure!(
        !secrets.is_null(),
        "Cannot read properties of null (reading 'apiKey')"
    );
    let host = url_host(
        &raw["provider"]["baseUrl"],
        "provider.baseUrl",
        &["https", "http"],
    )?;
    let vendor = if host == "api.deepseek.com" {
        "DEEPSEEK_API_KEY"
    } else if raw["provider"]["kind"] == "openai" {
        "OPENAI_API_KEY"
    } else {
        "ANTHROPIC_API_KEY"
    };
    let key = |names: &[&str], fallback: &Value| -> Value {
        names
            .iter()
            .find_map(|name| env(name).filter(|s| !s.is_empty()).map(Value::String))
            .unwrap_or_else(|| {
                if truthy(fallback) {
                    fallback.clone()
                } else {
                    json!("")
                }
            })
    };
    raw["apiKey"] = key(&["LLM_API_KEY", vendor], &secrets["apiKey"]);
    raw["onebotToken"] = key(&["ONEBOT_TOKEN"], &secrets["onebotToken"]);
    raw["dataDir"] = json!(absolute(
        root,
        raw["storage"]["directory"]
            .as_str()
            .context("storage.directory must be a string")?
    )?);
    let config = Config::from_value(&raw)?;
    Ok(Loaded { config, raw })
}

pub fn readiness(c: &Config) -> Vec<String> {
    let mut missing = Vec::new();
    if !c.api_key.is_truthy {
        missing.push("API key".into());
    }
    if !c.provider.model.is_truthy {
        missing.push("model".into());
    }
    if c.agent.allowed_groups.is_empty() && c.agent.allowed_users.is_empty() {
        missing.push("selected chat IDs".into());
    }
    missing
}

/// 白名单摘要不输出手动添加的未知键；密钥只有是否已设置，不输出前后缀。
pub fn summary(c: &Config) -> Value {
    json!({"provider":{"kind":c.provider.kind,"model":c.provider.model.text},"dataDir":c.data_dir,
        "selectedChats":c.agent.allowed_groups.len()+c.agent.allowed_users.len(),"dryRun":c.agent.dry_run,
        "apiKey":if c.api_key.is_truthy {"[redacted]"} else {""},
        "onebotToken":if c.onebot_token.is_truthy {"[redacted]"} else {""},"missing":readiness(c)})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn set(c: &mut Value, key: &str, value: Value) {
        *c.pointer_mut(&format!("/{}", key.replace('.', "/")))
            .unwrap() = value;
    }
    #[test]
    fn every_numeric_range_boundary() {
        for &(key, min, max) in RANGES {
            for n in [min, max] {
                let mut c = defaults();
                set(&mut c, key, json!(n));
                assert!(validate(&c).is_ok(), "{key}={n}");
            }
            for v in [
                json!(min - 0.01),
                json!(max + 0.01),
                json!("1"),
                Value::Null,
                json!(false),
            ] {
                let mut c = defaults();
                set(&mut c, key, v);
                assert!(validate(&c).is_err(), "{key}");
            }
        }
        for &(key, min, max) in MEMORY_RANGES {
            for n in [min, max] {
                let mut c = defaults();
                c["agent"]["memory"][key] = json!(n);
                validate(&c).unwrap();
            }
            for n in [min - 1., max + 1., min + 0.5] {
                let mut c = defaults();
                c["agent"]["memory"][key] = json!(n);
                assert!(validate(&c).is_err());
            }
        }
    }
    #[test]
    fn specialized_numeric_boundaries() {
        for (key, min, max, integer) in [
            ("agent.personality.variantProbability", 0., 1., false),
            ("agent.expression.minConfidence", 0., 1., false),
            ("agent.emoji.probability", 0., 1., false),
            ("agent.expression.maxPerReply", 1., 5., true),
            ("agent.expression.maxEntries", 1., 500., true),
            ("agent.expression.retentionDays", 1., 3650., true),
            ("agent.expression.reuseSeconds", 0., 86400., true),
            ("agent.emoji.cooldownSeconds", 0., 86400., true),
            ("agent.observation.minSeconds", 1., 604800., true),
            ("agent.observation.minMessages", 1., 10000., true),
            ("agent.observation.historyLimit", 1., 100., true),
            ("agent.rhythm.dayProbability", 0., 1., false),
            ("agent.rhythm.sigma", 0.05, 1., false),
            ("agent.quietHours.start", 0., 23., true),
            ("agent.quietHours.end", 0., 23., true),
        ] {
            for n in [min, max] {
                let mut c = defaults();
                set(&mut c, key, json!(n));
                validate(&c).unwrap();
            }
            for n in [min - 0.01, max + 0.01] {
                let mut c = defaults();
                set(&mut c, key, json!(n));
                assert!(validate(&c).is_err(), "{key}");
            }
            if integer {
                let mut c = defaults();
                set(&mut c, key, json!(min + 0.5));
                assert!(validate(&c).is_err(), "{key}");
            }
        }
        for n in [30., 86400.] {
            let c = merge(
                &defaults(),
                &json!({"agent":{"rhythm":{"activeMinSeconds":n,"activeMaxSeconds":n,"restMinSeconds":n,"restMaxSeconds":n,"edgeProbability":1,"centerProbability":1}}}),
            );
            validate(&c).unwrap();
        }
    }
    #[test]
    fn unicode_and_regex_semantics() {
        assert!(nonblank("\u{85}"));
        assert!(!nonblank("\u{feff}"));
        assert!(!qq_id("１２３"));
        assert!(!qq_id("12\n"));
        let c = merge(
            &defaults(),
            &json!({"agent":{"personality":{"behavior":"🙂".repeat(1000)},"aliases":["\u{85}"]}}),
        );
        validate(&c).unwrap();
        assert_eq!(js_string(&json!(1e21)), "1e+21");
        assert_eq!(js_string(&json!(1e20)), "100000000000000000000");
    }
}
