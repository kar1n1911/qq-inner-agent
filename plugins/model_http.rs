//! HTTP-only model protocol translation. No provider shape escapes this adapter.
use super::{ports::ModelPort, types::*};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{collections::{HashMap, VecDeque}, io::{BufRead, BufReader}, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wire { Ollama, OpenAi, Claude }

#[derive(Clone)]
pub struct HttpModel {
    pub(crate) wire: Wire,
    pub(crate) base: String,
    pub(crate) key: Option<String>,
    pub(crate) manifest: PortManifest,
    pub(crate) window: Option<u32>,
    calls: Arc<AtomicU64>,
    hourly_limit: Option<usize>,
    recent: Arc<Mutex<VecDeque<Instant>>>,
}

impl HttpModel {
    pub fn supports(port: &str) -> bool {
        let descriptor: Value = serde_json::from_str(include_str!("model_config.json")).expect("valid model plugin declarations");
        descriptor.get(port).is_some()
    }
    /// Adapter-side factory. The kernel passes a configured port name without
    /// knowing provider-specific request or authorization rules.
    pub fn configured(port: &str, base: &str, key: Option<String>, window: Option<u32>, hourly_limit: u32) -> Result<Self, IoError> {
        let mut adapter = match port {
            "ollama" => Self::new(Wire::Ollama, port, base, None, window),
            "openai" => Self::new(Wire::OpenAi, port, base, key, window),
            "claude" => Self::new(Wire::Claude, port, base, key, window),
            _ => Err(IoError::new("unknown_model_port")),
        }?;
        adapter.hourly_limit = Some(hourly_limit as usize);
        Ok(adapter)
    }
    pub(crate) fn new(wire: Wire, name: &str, base: &str, key: Option<String>, window: Option<u32>) -> Result<Self, IoError> {
        let url = url::Url::parse(base).map_err(|_| IoError::new("invalid_model_endpoint"))?;
        if !["http", "https"].contains(&url.scheme()) || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || name.is_empty() || name.contains(':') {
            return Err(IoError::new("invalid_model_endpoint"));
        }
        if wire == Wire::Ollama && !["localhost", "127.0.0.1", "[::1]"].contains(&url.host_str().unwrap_or_default()) {
            return Err(IoError::new("ollama_requires_local_endpoint"));
        }
        if url.scheme() == "http" && !["localhost", "127.0.0.1", "[::1]"].contains(&url.host_str().unwrap_or_default()) {
            return Err(IoError::new("model_tls_required"));
        }
        if wire != Wire::Ollama && key.as_deref().unwrap_or_default().is_empty() {
            return Err(IoError::new("model_api_key_required"));
        }
        let descriptor: Value = serde_json::from_str(include_str!("model_config.json")).expect("valid model plugin declarations");
        let selected = &descriptor[match wire { Wire::Ollama => "ollama", Wire::OpenAi => "openai", Wire::Claude => "claude" }];
        let fields: Vec<PortConfigField> = serde_json::from_value(selected["config_fields"].clone()).expect("valid model plugin form");
        let dependencies: Vec<PortDependency> = serde_json::from_value(selected["dependencies"].clone()).expect("valid model plugin dependencies");
        let base = base.trim_end_matches('/');
        let base = if wire != Wire::Ollama { base.strip_suffix("/v1").unwrap_or(base) } else { base };
        Ok(Self { wire, base: base.into(), key,
            manifest: PortManifest { name: name.into(), kind: PortKind::Model,
                transport: if wire == Wire::Ollama { TransportKind::Sidecar } else { TransportKind::Remote },
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![HostPermission::Http { origin: url.origin().ascii_serialization() }],
                dependencies,
                config_fields: fields }, window, calls: Arc::new(AtomicU64::new(0)),
                hourly_limit: None, recent: Arc::new(Mutex::new(VecDeque::new())) })
    }

    pub fn calls(&self) -> u64 { self.calls.load(Ordering::Relaxed) }

    fn reserve<T>(&self) -> Result<(), PortReply<T>> {
        if let Some(limit) = self.hourly_limit {
            let mut recent = self.recent.lock().map_err(|_| failed("model_budget_unavailable", false))?;
            let now = Instant::now();
            while recent.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(3600)) { recent.pop_front(); }
            if recent.len() >= limit { return Err(failed("hourly_api_budget", false)); }
            recent.push_back(now);
        }
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String { format!("{}{}", self.base, path) }

    pub fn request_body(&self, request: &ModelRequest<'_>, streaming: bool) -> PortReply<Value> {
        if request.max_tokens == 0 { return failed("invalid_max_tokens", false); }
        if request.format == OutputFormat::Json && !self.capabilities().json_mode {
            return PortReply::Unsupported { capability: Capability::JsonMode };
        }
        let Some(messages) = request.payload.as_array().or_else(|| request.payload.get("messages").and_then(Value::as_array)) else {
            return failed("invalid_model_messages", false);
        };
        let tools = request.payload.get("tools").and_then(Value::as_array);
        if request.payload.get("tools").is_some_and(|v| !v.is_array()) { return failed("invalid_model_tools", false); }
        if request.format == OutputFormat::Json && tools.is_some_and(|t| !t.is_empty()) {
            return PortReply::Unsupported { capability: Capability::Custom("json_with_tools".into()) };
        }
        let mut mapped = Vec::with_capacity(messages.len() + 1);
        for message in messages {
            let Some(role) = message["role"].as_str() else { return failed("invalid_model_message", false); };
            if !["user", "assistant", "tool"].contains(&role) { return failed("invalid_model_message", false); }
            let content = message["content"].as_str().unwrap_or("");
            if role == "tool" {
                let Ok(result) = serde_json::from_value::<ModelToolResult>(message.clone()) else { return failed("invalid_tool_result", false); };
                let item = match self.wire {
                    Wire::Claude => {
                        let Some(id) = result.tool_call_id else { return failed("tool_call_id_required", false); };
                        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":result.content,"is_error":result.is_error}]})
                    }
                    Wire::OpenAi => {
                        let Some(id) = result.tool_call_id else { return failed("tool_call_id_required", false); };
                        json!({"role":"tool","tool_call_id":id,"content":result.content})
                    }
                    Wire::Ollama => {
                        let Some(name) = result.name else { return failed("tool_name_required", false); };
                        json!({"role":"tool","tool_name":name,"content":result.content})
                    }
                };
                mapped.push(item);
                continue;
            }
            if !message["content"].is_string() { return failed("invalid_model_message", false); }
            let calls = match message.get("tool_calls") {
                Some(value) => match serde_json::from_value::<Vec<ModelToolCall>>(value.clone()) { Ok(calls) => calls, Err(_) => return failed("invalid_tool_call", false) },
                None => vec![],
            };
            if calls.is_empty() { mapped.push(json!({"role":role,"content":content})); continue; }
            if role != "assistant" { return failed("invalid_tool_call", false); }
            let item = match self.wire {
                Wire::Claude => {
                    let mut blocks = Vec::new();
                    if !content.is_empty() { blocks.push(json!({"type":"text","text":content})); }
                    for call in calls { let Some(id) = call.id else { return failed("tool_call_id_required", false); };
                        blocks.push(json!({"type":"tool_use","id":id,"name":call.name,"input":call.arguments})); }
                    json!({"role":"assistant","content":blocks})
                }
                Wire::OpenAi => {
                    let mut blocks = Vec::new();
                    for call in calls { let Some(id) = call.id else { return failed("tool_call_id_required", false); };
                        blocks.push(json!({"id":id,"type":"function","function":{"name":call.name,"arguments":call.arguments.to_string()}})); }
                    json!({"role":"assistant","content":content,"tool_calls":blocks})
                }
                Wire::Ollama => json!({"role":"assistant","content":content,"tool_calls":calls.iter().map(|c| json!({"function":{"name":c.name,"arguments":c.arguments}})).collect::<Vec<_>>() }),
            };
            mapped.push(item);
        }
        let mut tool_defs = Vec::new();
        for tool in tools.into_iter().flatten() {
            let Ok(def) = serde_json::from_value::<ModelToolDefinition>(tool.clone()) else { return failed("invalid_model_tools", false); };
            if def.name.is_empty() || !def.input_schema.is_object() { return failed("invalid_model_tools", false); }
            tool_defs.push(match self.wire {
                Wire::Claude => json!({"name":def.name,"description":def.description,"input_schema":def.input_schema}),
                Wire::OpenAi | Wire::Ollama => json!({"type":"function","function":{"name":def.name,"description":def.description,"parameters":def.input_schema}}),
            });
        }
        let body = match self.wire {
            Wire::Ollama => {
                mapped.insert(0, json!({"role":"system","content":request.system}));
                json!({"model":request.instance,"messages":mapped,"tools":tool_defs,"stream":streaming,
                    "options":{"num_predict":request.max_tokens},
                    "format": if request.format == OutputFormat::Json { Some("json") } else { None }})
            }
            Wire::OpenAi => {
                mapped.insert(0, json!({"role":"system","content":request.system}));
                let mut body = json!({"model":request.instance,"messages":mapped,"stream":streaming,"max_tokens":request.max_tokens});
                if !tool_defs.is_empty() { body["tools"] = json!(tool_defs); }
                if request.format == OutputFormat::Json { body["response_format"] = json!({"type":"json_object"}); }
                body
            }
            Wire::Claude => {
                let mut body = json!({"model":request.instance,"system":request.system,"messages":mapped,
                    "max_tokens":request.max_tokens,"stream":streaming});
                if !tool_defs.is_empty() { body["tools"] = json!(tool_defs); }
                body
            }
        };
        PortReply::Ok(body)
    }

    fn agent() -> ureq::Agent { ureq::AgentBuilder::new().timeout(Duration::from_secs(120)).build() }

    fn post(&self, body: Value) -> Result<ureq::Response, PortReply<ModelReply>> {
        self.reserve()?;
        let path = match self.wire { Wire::Ollama => "/api/chat", Wire::OpenAi => "/v1/chat/completions", Wire::Claude => "/v1/messages" };
        let mut req = Self::agent().post(&self.endpoint(path)).set("Content-Type", "application/json");
        match self.wire {
            Wire::OpenAi => req = req.set("Authorization", &format!("Bearer {}", self.key.as_deref().unwrap_or_default())),
            Wire::Claude => req = req.set("x-api-key", self.key.as_deref().unwrap_or_default()).set("anthropic-version", "2023-06-01"),
            Wire::Ollama => {},
        }
        req.send_json(body).map_err(map_error)
    }

    fn parse_reply(&self, value: &Value) -> PortReply<ModelReply> {
        if self.truncated(value) { return failed("model_output_truncated", false); }
        let text = match self.wire {
            Wire::Ollama => value.pointer("/message/content").and_then(Value::as_str).map(str::to_owned),
            Wire::OpenAi => value.pointer("/choices/0/message/content").and_then(Value::as_str).map(str::to_owned),
            Wire::Claude => value["content"].as_array().map(|parts| parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("")),
        };
        let raw_calls = match self.wire {
            Wire::Ollama => value.pointer("/message/tool_calls"),
            Wire::OpenAi => value.pointer("/choices/0/message/tool_calls"),
            Wire::Claude => value.get("content"),
        };
        if raw_calls.is_some_and(|v| !v.is_null() && !v.is_array()) { return failed("invalid_tool_call", false); }
        let mut tool_calls = Vec::new();
        if let Some(raw) = raw_calls.and_then(Value::as_array) {
            for item in raw {
                if self.wire == Wire::Claude && item["type"] != "tool_use" { continue; }
                let parsed = match self.wire {
                    Wire::Claude => item["id"].as_str().zip(item["name"].as_str()).zip(item.get("input"))
                        .map(|((id, name), args)| ModelToolCall { id: Some(id.into()), name: name.into(), arguments: args.clone() }),
                    Wire::OpenAi => item["id"].as_str().zip(item.pointer("/function/name").and_then(Value::as_str))
                        .zip(item.pointer("/function/arguments").and_then(Value::as_str))
                        .and_then(|((id, name), args)| serde_json::from_str(args).ok().map(|args| ModelToolCall { id: Some(id.into()), name: name.into(), arguments: args })),
                    Wire::Ollama => item.pointer("/function/name").and_then(Value::as_str)
                        .zip(item.pointer("/function/arguments"))
                        .map(|(name, args)| ModelToolCall { id: None, name: name.into(), arguments: args.clone() }),
                };
                let Some(parsed) = parsed else { return failed("invalid_tool_call", false); };
                tool_calls.push(parsed);
            }
        }
        if (self.wire == Wire::Claude && value["stop_reason"] == "tool_use" ||
            self.wire == Wire::OpenAi && value.pointer("/choices/0/finish_reason") == Some(&json!("tool_calls"))) && tool_calls.is_empty() {
            return failed("missing_tool_call", false);
        }
        match text { Some(text) if !text.is_empty() || !tool_calls.is_empty() => PortReply::Ok(ModelReply { text, tool_calls }), _ => failed("empty_model_reply", false) }
    }

    fn truncated(&self, value: &Value) -> bool {
        match self.wire {
            Wire::OpenAi => value.pointer("/choices/0/finish_reason") == Some(&json!("length")),
            Wire::Claude => value["stop_reason"] == "max_tokens",
            Wire::Ollama => value["done_reason"] == "length",
        }
    }

    fn parse_delta(&self, value: &Value) -> Option<String> {
        match self.wire {
            Wire::Ollama => value.pointer("/message/content").and_then(Value::as_str),
            Wire::OpenAi => value.pointer("/choices/0/delta/content").and_then(Value::as_str),
            Wire::Claude => if value["type"] == "content_block_delta" { value.pointer("/delta/text").and_then(Value::as_str) } else { None },
        }.filter(|s| !s.is_empty()).map(str::to_owned)
    }

    /// Bridge for the existing engine's single-user-message shape. Tool calls
    /// are rejected because that engine has no tool-execution loop.
    pub async fn complete_engine(&self, instance: &str, system: &str, payload: Value, max_tokens: u32) -> Result<String, IoError> {
        let messages = json!([{"role":"user","content":payload.to_string()}]);
        match self.complete(ModelRequest { instance, system, payload: &messages, format: OutputFormat::Text, max_tokens }).await {
            PortReply::Ok(reply) if reply.tool_calls.is_empty() => Ok(reply.text),
            PortReply::Ok(_) => Err(IoError::new("model_tool_call_unhandled")),
            PortReply::Unsupported { .. } => Err(IoError::new("model_capability_unsupported")),
            PortReply::Degraded { .. } => Err(IoError::new("model_reply_degraded")),
            PortReply::Failed { code, .. } => Err(IoError::new(code)),
        }
    }

    pub async fn complete_engine_json(&self, instance: &str, system: &str, payload: Value, max_tokens: u32) -> Result<Value, IoError> {
        let text = self.complete_engine(instance, system, payload, max_tokens).await?;
        let value: Value = serde_json::from_str(text.trim()).map_err(|_| IoError::new("invalid_model_json"))?;
        if !value.is_object() { return Err(IoError::new("invalid_model_json_object")); }
        Ok(value)
    }

    fn tool_deltas(&self, value: &Value, open: &mut HashMap<usize, (Option<String>, String)>) -> Result<Vec<ModelDelta>, &'static str> {
        let mut deltas = Vec::new();
        match self.wire {
            Wire::Claude => {
                let index = value["index"].as_u64().unwrap_or(0) as usize;
                match value["type"].as_str() {
                    Some("content_block_start") if value["content_block"]["type"] == "tool_use" => {
                        let id = value.pointer("/content_block/id").and_then(Value::as_str).ok_or("invalid_tool_call")?.to_owned();
                        let name = value.pointer("/content_block/name").and_then(Value::as_str).ok_or("invalid_tool_call")?.to_owned();
                        open.insert(index, (Some(id.clone()), name.clone()));
                        deltas.push(ModelDelta::ToolCallStart { id: Some(id), name });
                    }
                    Some("content_block_delta") if value["delta"]["type"] == "input_json_delta" => {
                        let (id, _) = open.get(&index).ok_or("invalid_tool_call")?;
                        let fragment = value.pointer("/delta/partial_json").and_then(Value::as_str).ok_or("invalid_tool_call")?;
                        deltas.push(ModelDelta::ToolCallArguments { id: id.clone(), fragment: fragment.into() });
                    }
                    Some("content_block_stop") => {
                        if let Some((id, _)) = open.remove(&index) { deltas.push(ModelDelta::ToolCallEnd { id }); }
                    }
                    _ => {},
                }
            }
            Wire::OpenAi => {
                if value.pointer("/choices/0/delta/tool_calls").is_some_and(|v| !v.is_array() && !v.is_null()) { return Err("invalid_tool_call"); }
                if let Some(calls) = value.pointer("/choices/0/delta/tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let index = call["index"].as_u64().ok_or("invalid_tool_call")? as usize;
                        if let std::collections::hash_map::Entry::Vacant(entry) = open.entry(index) {
                            let id = call["id"].as_str().ok_or("invalid_tool_call")?.to_owned();
                            let name = call.pointer("/function/name").and_then(Value::as_str).ok_or("invalid_tool_call")?.to_owned();
                            entry.insert((Some(id.clone()), name.clone()));
                            deltas.push(ModelDelta::ToolCallStart { id: Some(id), name });
                        }
                        if let Some(fragment) = call.pointer("/function/arguments").and_then(Value::as_str) {
                            let (id, _) = open.get(&index).ok_or("invalid_tool_call")?;
                            deltas.push(ModelDelta::ToolCallArguments { id: id.clone(), fragment: fragment.into() });
                        }
                    }
                }
            }
            Wire::Ollama => {
                if value.pointer("/message/tool_calls").is_some_and(|v| !v.is_array() && !v.is_null()) { return Err("invalid_tool_call"); }
                if let Some(calls) = value.pointer("/message/tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let name = call.pointer("/function/name").and_then(Value::as_str).ok_or("invalid_tool_call")?.to_owned();
                        let args = call.pointer("/function/arguments").ok_or("invalid_tool_call")?.to_string();
                        deltas.push(ModelDelta::ToolCallStart { id: None, name });
                        deltas.push(ModelDelta::ToolCallArguments { id: None, fragment: args });
                        deltas.push(ModelDelta::ToolCallEnd { id: None });
                    }
                }
            }
        }
        Ok(deltas)
    }
}

fn failed<T>(code: &str, uncertain: bool) -> PortReply<T> { PortReply::Failed { code: code.into(), uncertain } }

fn status_code(status: u16) -> &'static str {
    match status {
        401 | 403 => "model_auth_failed", 404 => "model_not_found", 429 => "model_rate_limited",
        408 | 504 => "model_timeout", 500..=599 => "model_server_error", _ => "model_request_rejected",
    }
}

fn map_error<T>(error: ureq::Error) -> PortReply<T> {
    match error {
        ureq::Error::Status(status, response) => {
            let code = status_code(status);
            let _ = response; failed(code, false)
        }
        ureq::Error::Transport(t) => if t.to_string().to_ascii_lowercase().contains("timed out") { failed("model_timeout", true) }
            else { failed("model_unreachable", true) },
    }
}

impl ModelPort for HttpModel {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities { json_mode: self.wire != Wire::Claude, thinking_field: false,
            truncation_signal: true, list_models: true, context_window_tokens: self.window }
    }
    fn complete<'a>(&'a self, request: ModelRequest<'a>) -> BoxFuture<'a, PortReply<ModelReply>> {
        Box::pin(async move {
            let body = match self.request_body(&request, false) { PortReply::Ok(body) => body,
                PortReply::Unsupported { capability } => return PortReply::Unsupported { capability },
                PortReply::Failed { code, uncertain } => return PortReply::Failed { code, uncertain },
                PortReply::Degraded { .. } => unreachable!() };
            let model = self.clone();
            match tokio::task::spawn_blocking(move || model.post(body).and_then(|res| res.into_json::<Value>().map_err(|_| failed("invalid_model_reply", false)))).await {
                Ok(Ok(value)) => self.parse_reply(&value), Ok(Err(err)) => err,
                Err(_) => failed("model_task_failed", true),
            }
        })
    }
    fn stream<'a>(&'a self, request: ModelRequest<'a>) -> BoxFuture<'a, PortReply<ModelStream>> {
        Box::pin(async move {
            let body = match self.request_body(&request, true) { PortReply::Ok(body) => body,
                PortReply::Unsupported { capability } => return PortReply::Unsupported { capability },
                PortReply::Failed { code, uncertain } => return PortReply::Failed { code, uncertain },
                PortReply::Degraded { .. } => unreachable!() };
            let (tx, rx) = tokio::sync::mpsc::channel(32);
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let model = self.clone();
            tokio::task::spawn_blocking(move || {
                let response = match model.post(body) {
                    Ok(response) => response,
                    Err(error) => { let _ = ready_tx.send(Err(error)); return; }
                };
                let _ = ready_tx.send(Ok(()));
                let mut complete = false;
                let mut open = HashMap::new();
                for line in BufReader::new(response.into_reader()).lines() {
                    let line = match line { Ok(line) => line, Err(_) => { let _ = tx.blocking_send(failed("model_stream_interrupted", true)); return; } };
                    let data = if model.wire == Wire::Ollama { line.trim() } else { line.strip_prefix("data: ").unwrap_or("") };
                    if data.is_empty() { continue; }
                    if data == "[DONE]" { complete = true; break; }
                    let Ok(value) = serde_json::from_str::<Value>(data) else { let _ = tx.blocking_send(failed("invalid_model_stream", true)); return; };
                    if model.truncated(&value) { let _ = tx.blocking_send(failed("model_output_truncated", false)); return; }
                    if model.wire == Wire::Claude && value["type"] == "error" { let _ = tx.blocking_send(failed("model_stream_error", true)); return; }
                    if let Some(text) = model.parse_delta(&value) { if tx.blocking_send(PortReply::Ok(ModelDelta::Text(text))).is_err() { return; } }
                    let deltas = match model.tool_deltas(&value, &mut open) { Ok(deltas) => deltas, Err(code) => { let _ = tx.blocking_send(failed(code, true)); return; } };
                    for delta in deltas { if tx.blocking_send(PortReply::Ok(delta)).is_err() { return; } }
                    if (model.wire == Wire::Ollama && value["done"] == true) || (model.wire == Wire::Claude && value["type"] == "message_stop") { complete = true; break; }
                }
                if model.wire == Wire::OpenAi && complete {
                    for (_, (id, _)) in open { if tx.blocking_send(PortReply::Ok(ModelDelta::ToolCallEnd { id })).is_err() { return; } }
                } else if !open.is_empty() { let _ = tx.blocking_send(failed("model_stream_interrupted", true)); return; }
                let _ = tx.blocking_send(if complete { PortReply::Ok(ModelDelta::Done) } else { failed("model_stream_interrupted", true) });
            });
            match ready_rx.await { Ok(Ok(())) => PortReply::Ok(rx), Ok(Err(error)) => match error {
                PortReply::Failed { code, uncertain } => PortReply::Failed { code, uncertain },
                _ => failed("model_stream_failed", true) }, Err(_) => failed("model_task_failed", true) }
        })
    }
    fn list_models<'a>(&'a self) -> BoxFuture<'a, PortReply<Vec<String>>> {
        Box::pin(async move {
            let path = match self.wire { Wire::Ollama => "/api/tags", Wire::OpenAi | Wire::Claude => "/v1/models" };
            let model = self.clone();
            let result = tokio::task::spawn_blocking(move || {
                model.reserve()?;
                let mut req = Self::agent().get(&model.endpoint(path));
                match model.wire {
                    Wire::OpenAi => req = req.set("Authorization", &format!("Bearer {}", model.key.as_deref().unwrap_or_default())),
                    Wire::Claude => req = req.set("x-api-key", model.key.as_deref().unwrap_or_default()).set("anthropic-version", "2023-06-01"),
                    Wire::Ollama => {},
                }
                req.call().map_err(map_error)?.into_json::<Value>().map_err(|_| failed("invalid_model_list", false))
            }).await;
            match result {
                Ok(Ok(value)) => {
                    let entries = if self.wire == Wire::Ollama { &value["models"] } else { &value["data"] };
                    match entries.as_array() { Some(entries) => PortReply::Ok(entries.iter().filter_map(|v| {
                        if self.wire == Wire::Ollama { v["name"].as_str() } else { v["id"].as_str() }
                    }).map(str::to_owned).collect()), None => failed("invalid_model_list", false) }
                }
                Ok(Err(err)) => err, Err(_) => failed("model_task_failed", true),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::status_code;
    #[test]
    fn provider_failures_remain_distinguishable() {
        assert_eq!(status_code(401), "model_auth_failed");
        assert_eq!(status_code(404), "model_not_found");
        assert_eq!(status_code(429), "model_rate_limited");
        assert_eq!(status_code(504), "model_timeout");
    }
}
