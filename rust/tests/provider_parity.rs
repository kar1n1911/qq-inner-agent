//! `provider.rs` 纯逻辑与 `src/provider.mjs` 的一致性测试。
//!
//! 这里覆盖的是最容易写错、也最容易与 JS 产生分歧的部分：端点拼接、围栏剥离、
//! 状态码分类与响应正文提取。HTTP 传输与预算计数留到接入 store 的阶段。
#[path = "golden/mod.rs"]
mod golden;
use qq_inner_core::provider::{
    classify_status, endpoint, extract_text, models_endpoint, parse_object, status_error,
    ProviderError, ProviderKind, StatusClass,
};
use serde_json::{json, Value};

fn expected(payload: &Value) -> Value {
    golden::expected(include_str!("golden/provider.json"), payload)
}

fn kind(value: &str) -> ProviderKind {
    ProviderKind::parse(value).expect("kind")
}

#[test]
fn endpoint_matches_javascript_across_trailing_slashes_and_suffixes() {
    let bases = [
        "https://api.openai.com/v1",
        "https://api.openai.com/v1/",
        "https://api.openai.com/v1//",
        "https://api.deepseek.com",
        "https://api.deepseek.com/",
        "https://api.deepseek.com/anthropic",
        "https://api.deepseek.com/anthropic/",
        "https://example.org",
        "https://example.org/",
        "http://127.0.0.1:8080/v1",
        "http://127.0.0.1:8080",
        "https://example.org/messages",
        "https://example.org/v1/messages",
        "https://example.org/chat/completions",
        "https://example.org/v1/chat/completions",
        "",
        "/",
    ];
    let kinds = ["openai", "anthropic"];
    let mut endpoints = Vec::new();
    for base in bases {
        for k in kinds {
            endpoints.push(json!({ "base": base, "kind": k }));
        }
    }
    let js = expected(&json!({ "endpoints": endpoints, "parses": [], "models": [] }));

    for (index, case) in endpoints.iter().enumerate() {
        let base = case["base"].as_str().unwrap();
        let k = kind(case["kind"].as_str().unwrap());
        let expected = js["endpoints"][index].as_str().unwrap();
        assert_eq!(endpoint(base, k), expected, "endpoint({base:?}, {k:?})");
    }
}

#[test]
fn models_endpoint_matches_what_list_models_actually_requests() {
    let bases = [
        "https://example.org",
        "https://example.org/v1",
        "https://example.org/v1/",
        "https://example.org/anthropic",
        "http://127.0.0.1:8080/v1",
    ];
    let kinds = ["openai", "anthropic"];
    let mut models = Vec::new();
    for base in bases {
        for k in kinds {
            models.push(json!({ "base": base, "kind": k }));
        }
    }
    let js = expected(&json!({ "endpoints": [], "parses": [], "models": models }));

    for (index, case) in models.iter().enumerate() {
        let base = case["base"].as_str().unwrap();
        let k = kind(case["kind"].as_str().unwrap());
        let expected = js["models"][index].as_str().unwrap();
        assert_eq!(
            models_endpoint(base, k),
            expected,
            "models({base:?}, {k:?})"
        );
    }
}

#[test]
fn parse_object_matches_javascript_including_error_codes() {
    let parses = [
        "{\"a\":1}",
        "  {\"a\":1}  ",
        "```json\n{\"a\":1}\n```",
        "```JSON\n{\"a\":1}\n```",
        "```\n{\"a\":1}\n```",
        "```json{\"a\":1}```",
        "{\"a\":1}```",
        "```json\n{\"a\":1}",
        "[1,2,3]",
        "null",
        "42",
        "\"text\"",
        "true",
        "not json",
        "",
        "   ",
        "{\"a\":1} trailing",
        "{\"nested\":{\"b\":[1,2]}}",
    ];
    let js = expected(&json!({ "endpoints": [], "parses": parses, "models": [] }));

    for (index, text) in parses.iter().enumerate() {
        match parse_object(text) {
            Ok(value) => {
                assert_eq!(
                    js["parses"][index]["ok"],
                    json!(true),
                    "{text:?} should parse"
                );
                let expected: Value =
                    serde_json::from_str(strip_expected(text)).expect("expected json");
                assert_eq!(value, expected, "parse_object({text:?})");
            }
            Err(error) => {
                assert_eq!(
                    js["parses"][index]["ok"],
                    json!(false),
                    "{text:?} should fail"
                );
                assert_eq!(
                    error.to_string(),
                    js["parses"][index]["code"].as_str().unwrap(),
                    "error code for {text:?}"
                );
            }
        }
    }
}

/// 与 JS 相同的清洗后文本，供断言比较解析结果。
fn strip_expected(text: &str) -> &str {
    let trimmed = text.trim();
    let rest = match trimmed.strip_prefix("```") {
        Some(rest) => match rest.get(..4) {
            Some(head) if head.eq_ignore_ascii_case("json") => rest[4..].trim_start(),
            _ => rest.trim_start(),
        },
        None => trimmed,
    };
    match rest.strip_suffix("```") {
        Some(head) => head.trim_end(),
        None => rest,
    }
}

#[test]
fn status_classification_follows_the_javascript_if_chain() {
    // 配置类：退避 300 秒且不重试。
    for status in [400, 401, 403, 404, 422] {
        assert_eq!(
            classify_status(status),
            StatusClass::CheckConfig,
            "status {status}"
        );
        assert_eq!(
            status_error(status).to_string(),
            format!("http_{status}_check_provider_config")
        );
    }
    // 其余 4xx（非 429）：致命，不重试。
    for status in [402, 405, 409, 418, 499] {
        assert_eq!(
            classify_status(status),
            StatusClass::Fatal,
            "status {status}"
        );
        assert_eq!(status_error(status).to_string(), format!("http_{status}"));
    }
    // 429 与 5xx：可重试。
    for status in [429, 500, 502, 503, 504] {
        assert_eq!(
            classify_status(status),
            StatusClass::Transient,
            "status {status}"
        );
        assert_eq!(status_error(status), ProviderError::TransientHttp);
    }
    // <400 视为正常。
    for status in [200, 201, 204, 301, 399] {
        assert_eq!(classify_status(status), StatusClass::Ok, "status {status}");
    }
}

#[test]
fn extract_text_matches_the_javascript_response_handling() {
    // Anthropic：只有 type=text 的块参与拼接，用换行连接。
    let anthropic = json!({
        "stop_reason": "end_turn",
        "content": [
            { "type": "text", "text": "第一段" },
            { "type": "thinking", "thinking": "不该出现" },
            { "type": "text", "text": "第二段" }
        ]
    });
    assert_eq!(
        extract_text(ProviderKind::Anthropic, &anthropic).unwrap(),
        "第一段\n第二段"
    );

    // 截断与空正文。
    let truncated =
        json!({ "stop_reason": "max_tokens", "content": [{ "type": "text", "text": "半截" }] });
    assert_eq!(
        extract_text(ProviderKind::Anthropic, &truncated),
        Err(ProviderError::OutputTruncated)
    );
    let blank =
        json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": "   " }] });
    assert_eq!(
        extract_text(ProviderKind::Anthropic, &blank),
        Err(ProviderError::EmptyModelResponse)
    );
    let missing = json!({ "stop_reason": "end_turn" });
    assert_eq!(
        extract_text(ProviderKind::Anthropic, &missing),
        Err(ProviderError::EmptyModelResponse)
    );

    // OpenAI。
    let openai =
        json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "答一句" } }] });
    assert_eq!(
        extract_text(ProviderKind::OpenAi, &openai).unwrap(),
        "答一句"
    );
    let openai_truncated =
        json!({ "choices": [{ "finish_reason": "length", "message": { "content": "半截" } }] });
    assert_eq!(
        extract_text(ProviderKind::OpenAi, &openai_truncated),
        Err(ProviderError::OutputTruncated)
    );
    let openai_empty =
        json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "" } }] });
    assert_eq!(
        extract_text(ProviderKind::OpenAi, &openai_empty),
        Err(ProviderError::EmptyModelResponse)
    );
    let openai_missing = json!({ "choices": [] });
    assert_eq!(
        extract_text(ProviderKind::OpenAi, &openai_missing),
        Err(ProviderError::EmptyModelResponse)
    );
}
