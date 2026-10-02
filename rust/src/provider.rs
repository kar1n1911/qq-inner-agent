//! Provider 纯逻辑与 HTTP 传输；阻塞/取消及预算契约见 `rust/PROVIDER.md`。
use serde_json::Value;
use thiserror::Error;

/// 与 JS 抛出的错误码逐字一致 —— 这些字符串会出现在日志与仪表盘上。
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum ProviderError {
    #[error("save_api_key_first")]
    SaveApiKeyFirst,
    #[error("invalid_model_list")]
    InvalidModelList,
    #[error("{0}")]
    ModelsHttp(String),
    #[error("invalid_json")]
    InvalidJson,
    #[error("invalid_json_object")]
    InvalidJsonObject,
    #[error("response_too_large")]
    ResponseTooLarge,
    #[error("invalid_provider_response")]
    InvalidProviderResponse,
    #[error("empty_model_response")]
    EmptyModelResponse,
    #[error("output_truncated_increase_maxTokens")]
    OutputTruncated,
    #[error("provider_backoff")]
    Backoff,
    #[error("hourly_api_budget")]
    HourlyBudget,
    #[error("provider_unavailable")]
    ProviderUnavailable,
    #[error("transient_http")]
    TransientHttp,
    /// `http_<status>_check_provider_config` —— 需要携带状态码，因此用字符串保存。
    #[error("{0}")]
    HttpCheckConfig(String),
    /// `http_<status>` —— 同上。
    #[error("{0}")]
    Http(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
}

impl ProviderKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            _ => None,
        }
    }
}

/// 复刻 `endpoint(base, kind)`。
///
/// 三个容易写错的点：先去**所有**尾斜杠；已经以目标后缀结尾时原样返回；
/// 只有 anthropic 且 base 不以 `/v1` 结尾时才插入 `/v1`。
pub fn endpoint(base: &str, kind: ProviderKind) -> String {
    let suffix = match kind {
        ProviderKind::Anthropic => "/messages",
        ProviderKind::OpenAi => "/chat/completions",
    };
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        return trimmed.to_string();
    }
    let mut out = String::from(trimmed);
    if kind == ProviderKind::Anthropic && !trimmed.ends_with("/v1") {
        out.push_str("/v1");
    }
    out.push_str(suffix);
    out
}

/// 复刻 `listModels` 里把端点换成 `/models` 的那一步。
pub fn models_endpoint(base: &str, kind: ProviderKind) -> String {
    let resolved = endpoint(base, kind);
    for suffix in ["/chat/completions", "/messages"] {
        if let Some(head) = resolved.strip_suffix(suffix) {
            return format!("{head}/models");
        }
    }
    resolved
}

/// 剥掉开头的 ```json / ``` 围栏（`json` 大小写不敏感）以及紧随其后的空白。
fn strip_opening_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = match rest.get(..4) {
        Some(head) if head.eq_ignore_ascii_case("json") => &rest[4..],
        _ => rest,
    };
    rest.trim_start()
}

/// 剥掉结尾的 ``` 以及它前面的空白。
fn strip_closing_fence(text: &str) -> &str {
    match text.strip_suffix("```") {
        Some(head) => head.trim_end(),
        None => text,
    }
}

/// 复刻 `parseObject(text)`。
///
/// 连续两次 `replace` 的顺序有意义：先处理开头围栏，再处理结尾围栏。
/// 结果必须是对象 —— 数组、`null`、标量都算 `invalid_json_object`。
pub fn parse_object(text: &str) -> Result<Value, ProviderError> {
    let trimmed = text.trim();
    let clean = strip_closing_fence(strip_opening_fence(trimmed));
    let value: Value = serde_json::from_str(clean).map_err(|_| ProviderError::InvalidJson)?;
    if !value.is_object() {
        return Err(ProviderError::InvalidJsonObject);
    }
    Ok(value)
}

/// HTTP 状态码的分类结果，对应 JS 里那段 `if` 链。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    /// 401/403/400/404/422 —— 配置问题，退避 300 秒且**不重试**。
    CheckConfig,
    /// 其余 4xx（非 429）—— 不重试。
    Fatal,
    /// 429 与 5xx —— 重试。
    Transient,
    Ok,
}

/// 复刻 JS 的状态码分类：`[401,403,400,404,422]` 先判；然后"非 429 且 <500"为致命；
/// 其余（429 与 >=500）为可重试。
pub fn classify_status(status: u16) -> StatusClass {
    if status < 400 {
        return StatusClass::Ok;
    }
    if matches!(status, 401 | 403 | 400 | 404 | 422) {
        return StatusClass::CheckConfig;
    }
    if status != 429 && status < 500 {
        return StatusClass::Fatal;
    }
    StatusClass::Transient
}

/// 把状态码映射成 JS 用的错误码字符串。
pub fn status_error(status: u16) -> ProviderError {
    match classify_status(status) {
        StatusClass::CheckConfig => ProviderError::HttpCheckConfig(format!(
            "http_{status}_check_provider_config"
        )),
        StatusClass::Fatal => ProviderError::Http(format!("http_{status}")),
        StatusClass::Ok => ProviderError::InvalidProviderResponse,
        StatusClass::Transient => ProviderError::TransientHttp,
    }
}

/// 从响应 JSON 里取出正文，并识别"输出被截断"。
///
/// `max_tokens`（Anthropic 的 `stop_reason`）与 `length`（OpenAI 的 `finish_reason`）在 JS 里
/// 抛同一个错误码。
pub fn extract_text(kind: ProviderKind, data: &Value) -> Result<String, ProviderError> {
    let content = match kind {
        ProviderKind::Anthropic => {
            if data.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
                return Err(ProviderError::OutputTruncated);
            }
            let Some(blocks) = data.get("content").and_then(Value::as_array) else {
                return Err(ProviderError::EmptyModelResponse);
            };
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        ProviderKind::OpenAi => {
            let choice = data.get("choices").and_then(|c| c.get(0));
            if choice.and_then(|c| c.get("finish_reason")).and_then(Value::as_str) == Some("length") {
                return Err(ProviderError::OutputTruncated);
            }
            choice
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        }
    };
    if content.trim().is_empty() {
        return Err(ProviderError::EmptyModelResponse);
    }
    Ok(content)
}

#[path = "provider_transport.rs"]
mod transport;
pub use transport::Provider;
