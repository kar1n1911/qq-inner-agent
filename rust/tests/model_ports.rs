use qq_inner_core::engine::io::{adapters::{claude, ollama, openai}, *};
use serde_json::{json, Value};
use std::{io::{Read, Write}, net::TcpListener, thread};

fn request<'a>(payload: &'a Value, format: OutputFormat) -> ModelRequest<'a> {
    ModelRequest { instance: "example", system: "独立系统提示", payload, format, max_tokens: 128 }
}

#[test]
fn provider_bodies_keep_system_in_the_correct_place() {
    let payload = json!([{"role":"user","content":"你好"}]);
    let claude = claude::adapter("claude", "https://api.anthropic.com", "secret".into(), None).unwrap();
    let openai = openai::adapter("openai", "https://api.openai.com", "secret".into(), None).unwrap();
    let ollama = ollama::adapter("ollama", "http://localhost:11434", None).unwrap();
    let PortReply::Ok(c) = claude.request_body(&request(&payload, OutputFormat::Text), false) else { panic!("claude body"); };
    assert_eq!(c["system"], "独立系统提示");
    assert_eq!(c["messages"][0]["role"], "user");
    assert!(c["max_tokens"].is_number());
    for adapter in [&openai, &ollama] {
        let PortReply::Ok(body) = adapter.request_body(&request(&payload, OutputFormat::Text), false) else { panic!("body"); };
        assert!(body.get("system").is_none());
        assert_eq!(body["messages"][0], json!({"role":"system","content":"独立系统提示"}));
    }
    let payload = json!({"messages":[
        {"role":"assistant","content":"","tool_calls":[{"id":"call_1","name":"weather","arguments":{"city":"Tokyo"}}]},
        {"role":"tool","tool_call_id":"call_1","name":"weather","content":"sunny","is_error":false}
    ],"tools":[{"name":"weather","description":"Weather lookup","input_schema":{"type":"object"}}]});
    let PortReply::Ok(c) = claude.request_body(&request(&payload, OutputFormat::Text), false) else { panic!("claude tools"); };
    assert_eq!(c["messages"][0]["content"][0]["type"], "tool_use");
    assert_eq!(c["messages"][1]["content"][0]["type"], "tool_result");
    assert_eq!(c["tools"][0]["input_schema"]["type"], "object");
    let PortReply::Ok(o) = openai.request_body(&request(&payload, OutputFormat::Text), false) else { panic!("openai tools"); };
    assert_eq!(o["messages"][1]["tool_calls"][0]["function"]["name"], "weather");
    let PortReply::Ok(l) = ollama.request_body(&request(&payload, OutputFormat::Text), false) else { panic!("ollama tools"); };
    assert_eq!(l["messages"][2]["tool_name"], "weather");
}

#[tokio::test]
async fn unsupported_capability_is_explicit_and_config_is_declarative() {
    let adapter = claude::adapter("claude", "https://api.anthropic.com", "secret".into(), None).unwrap();
    let payload = json!([{"role":"user","content":"你好"}]);
    assert!(matches!(adapter.complete(request(&payload, OutputFormat::Json)).await,
        PortReply::Unsupported { capability: Capability::JsonMode }));
    assert_eq!(adapter.capabilities().context_window_tokens, None);
    assert!(!adapter.capabilities().json_mode);
    assert_eq!(adapter.manifest().dependencies[0].role, "remote_model_api");
    assert!(adapter.manifest().config_fields.iter().any(|f| f.key == "api_key" && f.secret));
}

#[tokio::test]
async fn complete_and_stream_use_real_http_and_classify_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for n in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            let mut head = Vec::new(); let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") { socket.read_exact(&mut byte).unwrap(); head.push(byte[0]); }
            let headers = String::from_utf8(head).unwrap();
            let len = headers.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|s| s.parse::<usize>().ok())).unwrap();
            let mut body = vec![0; len]; socket.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["system"], "独立系统提示");
            assert_eq!(body["messages"][0]["role"], "user");
            let (status, response) = match n {
                0 => ("200 OK", r#"{"content":[{"type":"text","text":"完成"}]}"#),
                1 => ("200 OK", "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"增量\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
                _ => ("429 Too Many Requests", "{}"),
            };
            write!(socket, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
    });
    let adapter = claude::adapter("claude", &base, "secret".into(), Some(200_000)).unwrap();
    let payload = json!([{"role":"user","content":"你好"}]);
    assert_eq!(adapter.capabilities().context_window_tokens, Some(200_000));
    assert_eq!(adapter.complete(request(&payload, OutputFormat::Text)).await, PortReply::Ok(ModelReply { text: "完成".into(), tool_calls: vec![] }));
    let PortReply::Ok(mut stream) = adapter.stream(request(&payload, OutputFormat::Text)).await else { panic!("stream"); };
    assert_eq!(stream.recv().await, Some(PortReply::Ok(ModelDelta::Text("增量".into()))));
    assert_eq!(stream.recv().await, Some(PortReply::Ok(ModelDelta::Done)));
    assert!(matches!(adapter.complete(request(&payload, OutputFormat::Text)).await,
        PortReply::Failed { code, uncertain: false } if code == "model_rate_limited"));
    server.join().unwrap();
}

#[tokio::test]
async fn tool_use_is_structured_in_complete_and_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for n in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            let mut head = Vec::new(); let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") { socket.read_exact(&mut byte).unwrap(); head.push(byte[0]); }
            let len = String::from_utf8(head).unwrap().lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").and_then(|s| s.parse::<usize>().ok())).unwrap();
            let mut body = vec![0; len]; socket.read_exact(&mut body).unwrap();
            let response = if n == 0 { r#"{"content":[{"type":"tool_use","id":"call_1","name":"weather","input":{"city":"Tokyo"}}],"stop_reason":"tool_use"}"# }
            else { "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_1\",\"name\":\"weather\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\\\"Tokyo\\\"}\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n" };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
    });
    let adapter = claude::adapter("claude", &base, "secret".into(), None).unwrap();
    let payload = json!([{"role":"user","content":"weather"}]);
    let PortReply::Ok(reply) = adapter.complete(request(&payload, OutputFormat::Text)).await else { panic!("tool response lost"); };
    assert!(reply.text.is_empty());
    assert_eq!(reply.tool_calls, vec![ModelToolCall { id: Some("call_1".into()), name: "weather".into(), arguments: json!({"city":"Tokyo"}) }]);
    let PortReply::Ok(mut stream) = adapter.stream(request(&payload, OutputFormat::Text)).await else { panic!("tool stream"); };
    assert_eq!(stream.recv().await, Some(PortReply::Ok(ModelDelta::ToolCallStart { id: Some("call_1".into()), name: "weather".into() })));
    assert_eq!(stream.recv().await, Some(PortReply::Ok(ModelDelta::ToolCallArguments { id: Some("call_1".into()), fragment: r#"{"city":"Tokyo"}"#.into() })));
    assert_eq!(stream.recv().await, Some(PortReply::Ok(ModelDelta::ToolCallEnd { id: Some("call_1".into()) })));
    assert_eq!(stream.recv().await, Some(PortReply::Ok(ModelDelta::Done)));
    server.join().unwrap();
}
