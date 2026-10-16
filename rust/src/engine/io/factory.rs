//! Trusted constructors and a staged, all-or-nothing plugin generation.
//! A manifest selects a compiled descriptor or a validated sidecar protocol;
//! it never names a Rust symbol to execute.
use super::{
    adapters::{amemorix::AmemorixPort, model_http::HttpModel, null::NullDb,
        onebot::{Action, Connection, OneBotAdapter}, sillytavern::SillyTavernPersona},
    plugin_list::{self, ActivationRequest}, ports::{ChatPort, MemoryPort, ModelPort, PersonaPort},
    types::{ChatAddress, ChatKind, MemorySubject, PortReply, PortState}, PortRegistry,
};
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet}, net::{TcpStream, ToSocketAddrs}, path::{Path, PathBuf},
    process::{Child, Command, Stdio}, sync::Arc, time::Duration};

pub type SecretResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type HostAudit = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct HostServices {
    pub secret: SecretResolver,
    pub onebot_action: Option<Action>,
    pub onebot_connection: Option<Connection>,
    pub audit: Option<HostAudit>,
    pub model_hourly_limit: u32,
}

impl HostServices {
    pub fn without_runtime(secret: SecretResolver) -> Self {
        Self { secret, onebot_action: None, onebot_connection: None, audit: None,
            model_hourly_limit: u32::MAX }
    }
}

struct ManagedSidecar(Child);
impl Drop for ManagedSidecar {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

enum PreparedPort {
    Model { port: Arc<HttpModel>, instance: String, kind: String, sidecar: Option<ManagedSidecar> },
    Memory { port: Arc<AmemorixPort> },
    Chat { port: Arc<OneBotAdapter> },
    Persona { port: Arc<SillyTavernPersona>, chats: Vec<ChatAddress> },
}

pub struct PublishedPorts {
    pub registry: PortRegistry<NullDb>,
    pub model: Option<Arc<HttpModel>>,
    pub model_instance: Option<String>,
    pub model_kind: Option<String>,
    pub onebot: Option<Arc<OneBotAdapter>>,
    pub persona_bindings: Vec<(ChatAddress, Arc<SillyTavernPersona>)>,
    observations: HashMap<String, Value>,
    sidecars: Vec<ManagedSidecar>,
}

impl PublishedPorts {
    fn publish(prepared: Vec<PreparedPort>) -> Result<Self, String> {
        let mut registry = PortRegistry::new(Arc::new(NullDb));
        let mut model = None;
        let mut model_instance = None;
        let mut model_kind = None;
        let mut onebot = None;
        let mut persona_bindings = Vec::new();
        let mut sidecars = Vec::new();
        for candidate in prepared {
            match candidate {
                PreparedPort::Model { port, instance, kind, sidecar } => {
                    registry.register_model(port.clone()).map_err(|e| e.code)?;
                    if model.is_none() { model = Some(port); model_instance = Some(instance); model_kind = Some(kind); }
                    if let Some(sidecar) = sidecar { sidecars.push(sidecar); }
                }
                PreparedPort::Memory { port } => registry.register_memory(port).map_err(|e| e.code)?,
                PreparedPort::Chat { port } => {
                    registry.register_chat(port.clone()).map_err(|e| e.code)?;
                    onebot = Some(port);
                }
                PreparedPort::Persona { port, chats } => {
                    registry.register_persona(port.clone()).map_err(|e| e.code)?;
                    persona_bindings.extend(chats.into_iter().map(|chat| (chat, port.clone())));
                }
            }
        }
        Ok(Self { registry, model, model_instance, model_kind, onebot, persona_bindings,
            observations: HashMap::new(), sidecars })
    }
    pub fn sidecar_count(&self) -> usize { self.sidecars.len() }
    /// Read only: the snapshot uses ports already published by this generation.
    pub fn status_snapshot(&self, revision: &str, snapshot_at: &str) -> Value {
        let items = self.observations.iter().map(|(id, evidence)| {
            let state = self.registry.chat.get(id).map(|port| match port.state() {
                PortState::Available => "available", PortState::Degraded => "degraded",
                PortState::Disconnected => "disconnected",
            }).unwrap_or("unknown");
            (id.clone(), json!({"effective_state":evidence["effective_state"],
                "instanceRevision":revision,"capabilities":evidence["capabilities"],
                "portState":state,"observedAt":evidence["observedAt"],
                "snapshotAt":snapshot_at,"evidenceSource":"factory_prepared_port_capabilities"}))
        });
        Value::Object(items.collect())
    }
}

pub struct FactoryOutcome {
    pub status: Value,
    pub published: Option<PublishedPorts>,
}

fn probe_http(base: &str) -> Result<(), String> {
    let url = url::Url::parse(base).map_err(|_| "invalid_model_endpoint".to_owned())?;
    let host = url.host_str().ok_or_else(|| "invalid_model_endpoint".to_owned())?;
    let port = url.port_or_known_default().ok_or_else(|| "invalid_model_endpoint".to_owned())?;
    let addresses = (host, port).to_socket_addrs().map_err(|_| "backend_unavailable".to_owned())?;
    for address in addresses {
        if TcpStream::connect_timeout(&address, Duration::from_millis(600)).is_ok() { return Ok(()); }
    }
    Err("backend_unavailable".to_owned())
}

fn managed_sidecar(root: &Path, request: &ActivationRequest) -> Result<ManagedSidecar, String> {
    let relative = request.manifest["entry"]["executable"].as_str()
        .ok_or_else(|| "sidecar_entry_missing".to_owned())?;
    let package = root.join(&request.directory).canonicalize()
        .map_err(|_| "sidecar_entry_missing".to_owned())?;
    let candidate = package.join(relative);
    let path = if candidate.is_file() { candidate } else { root.join(relative) };
    let path = path.canonicalize()
        .map_err(|_| "sidecar_entry_missing".to_owned())?;
    if !path.starts_with(&package) || !path.is_file() { return Err("sidecar_entry_invalid".into()); }
    let mut child = Command::new(&path).stdin(Stdio::null()).stdout(Stdio::null())
        .stderr(Stdio::null()).spawn().map_err(|_| "sidecar_start_failed".to_owned())?;
    std::thread::sleep(Duration::from_millis(60));
    if child.try_wait().map_err(|_| "sidecar_probe_failed".to_owned())?.is_some() {
        return Err("sidecar_probe_failed".into());
    }
    Ok(ManagedSidecar(child))
}

fn optional_missing(request: &ActivationRequest) -> Vec<String> {
    request.manifest["dependencies"].as_array().into_iter().flatten()
        .filter(|dep| dep["required"] == false)
        .filter_map(|dep| dep["role"].as_str())
        .filter(|role| request.bindings.get(*role).is_none())
        .map(str::to_owned).collect()
}

fn prepared_capabilities(port: &PreparedPort) -> Value {
    match port {
        PreparedPort::Model { port, sidecar, .. } => {
            // Process liveness and an unrelated HTTP socket do not establish a
            // versioned sidecar handshake. Keep it staged but never advertise it.
            if sidecar.is_some() { return json!({"capabilities":[]}); }
            let caps = port.capabilities();
            let mut names = vec!["text_completion"];
            if caps.json_mode { names.push("json_mode"); }
            if caps.thinking_field { names.push("thinking_field"); }
            if caps.truncation_signal { names.push("truncation_signal"); }
            if caps.list_models { names.push("list_models"); }
            json!({"capabilities":names})
        }
        PreparedPort::Memory { port } => {
            let caps = port.capabilities();
            let mut names = vec![];
            if caps.text_retrieval { names.push("text_retrieval"); }
            if caps.embeddings { names.push("embeddings"); names.push("semantic_retrieval"); }
            if caps.relation_extraction { names.push("relation_extraction"); }
            json!({"capabilities":names})
        }
        PreparedPort::Chat { port } => {
            let caps = port.capabilities();
            let mut names = Vec::new();
            if caps.mention { names.push("mention"); }
            if caps.reply { names.push("reply"); }
            if caps.media { names.push("media"); }
            if caps.forward { names.push("forward"); }
            json!({"capabilities":names})
        }
        PreparedPort::Persona { port, .. } => json!({"capabilities":port.capabilities().supported}),
    }
}

fn prepare_one(root: &Path, request: &ActivationRequest, host: &HostServices) -> Result<PreparedPort, String> {
    let directory = request.directory.as_str();
    let kind = request.manifest["kind"].as_str().unwrap_or_default();
    let transport = request.manifest["transport"].as_str().unwrap_or_default();
    let params = &request.params;
    match (directory, kind, transport) {
        ("openai" | "claude" | "ollama", "Model", "Remote" | "Sidecar") => {
            let expected = if directory == "ollama" { "Sidecar" } else { "Remote" };
            if transport != expected { return Err("constructor_mismatch".into()); }
            if directory == "ollama" && request.manifest["entry"]["executable"].is_null() {
                return Err("sidecar_entry_missing".into());
            }
            let supported = ["text_completion", "truncation_signal", "list_models", "json_mode"];
            if request.manifest["capabilities"]["supported"].as_array().is_some_and(|claims| {
                claims.iter().filter_map(Value::as_str).any(|claim| {
                    !supported.contains(&claim) || (claim == "json_mode" && directory == "claude")
                })
            }) { return Err("capability_mismatch".into()); }
            let sidecar = if directory == "ollama" { Some(managed_sidecar(root, request)?) } else { None };
            let base = params["base_url"].as_str().ok_or("model_endpoint_missing")?;
            let key = request.secret_refs["api_key"].as_str().and_then(|reference| (host.secret)(reference));
            let mut port = HttpModel::configured(directory, base, key,
                params["context_window_tokens"].as_u64().and_then(|n| u32::try_from(n).ok()), host.model_hourly_limit)
                .map_err(|error| error.code)?;
            port.manifest.name = request.id.clone();
            probe_http(base)?;
            if let Some(audit) = &host.audit {
                let id = request.id.clone();
                let audit = audit.clone();
                port = port.with_audit(Arc::new(move |target| audit(&id, target)));
            }
            Ok(PreparedPort::Model { port: Arc::new(port), instance: params["model"].as_str().unwrap_or_default().into(), kind: directory.into(), sidecar })
        }
        ("a_memorix", "Memory", "Remote") => {
            let endpoint = params["endpoint"].as_str().ok_or("memory_endpoint_missing")?;
            let token = request.secret_refs["token"].as_str()
                .and_then(|reference| (host.secret)(reference)).unwrap_or_default();
            let mut adapter = AmemorixPort::new(endpoint, token).map_err(|e| e.code)?;
            if let Some(audit) = &host.audit {
                let id = request.id.clone();
                let audit = audit.clone();
                adapter = adapter.with_audit(Arc::new(move |target| audit(&id, target)));
            }
            let port = Arc::new(adapter);
            let candidate = port.clone();
            let result = std::thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new().map_err(|_| "memory_probe_failed".to_owned())?;
                let chat = ChatAddress::new(ChatKind::Group, "onebot", "factory-probe").map_err(|e| e.code)?;
                Ok::<_, String>(runtime.block_on(candidate.refresh_capabilities(&chat, &MemorySubject::Group)))
            }).join().map_err(|_| "memory_probe_failed".to_owned())??;
            match result {
                PortReply::Ok(capabilities) if capabilities.text_retrieval => Ok(PreparedPort::Memory { port }),
                _ => Err("backend_unavailable".into()),
            }
        }
        ("onebot", "Chat", "InProcess") => {
            let action = host.onebot_action.clone().ok_or("host_onebot_unavailable")?;
            let connection = host.onebot_connection.clone().ok_or("host_onebot_unavailable")?;
            Ok(PreparedPort::Chat { port: Arc::new(OneBotAdapter::new(&request.id, action, connection)
                .map_err(|e| e.code)?) })
        }
        ("sillytavern", "Persona", "InProcess") => {
            let card_root = params["card_root"].as_str().ok_or("persona_root_missing")?;
            let cards: HashMap<ChatAddress, PathBuf> = params["chat_card_bindings"].as_object()
                .into_iter().flat_map(|map| map.iter())
                .map(|(chat, path)| Ok((chat.parse().map_err(|e: super::types::IoError| e.code)?,
                    PathBuf::from(path.as_str().ok_or("persona_card_invalid")?))))
                .collect::<Result<_, String>>()?;
            let chats = cards.keys().cloned().collect();
            Ok(PreparedPort::Persona { port: Arc::new(SillyTavernPersona::new(
                &request.id, PathBuf::from(card_root), cards).map_err(|e| e.code)?), chats })
        }
        (_, "Model", "Sidecar") if request.manifest["protocol"]["name"] == "http-json" => {
            // The manifest names a user-selected executable, never a Rust symbol.
            let sidecar = managed_sidecar(root, request)?;
            let base = params["base_url"].as_str().ok_or("model_endpoint_missing")?;
            let key = request.secret_refs["api_key"].as_str().and_then(|reference| (host.secret)(reference));
            let mut port = HttpModel::configured("openai", base, key, None, host.model_hourly_limit).map_err(|e| e.code)?;
            port.manifest.name = request.id.clone();
            probe_http(base)?;
            if let Some(audit) = &host.audit {
                let id = request.id.clone();
                let audit = audit.clone();
                port = port.with_audit(Arc::new(move |target| audit(&id, target)));
            }
            Ok(PreparedPort::Model { port: Arc::new(port), instance: params["model"].as_str().unwrap_or_default().into(), kind: "openai".into(), sidecar: Some(sidecar) })
        }
        _ => Err("constructor_missing".into()),
    }
}

/// The callback only prepares candidates. The returned generation owns every
/// resource and becomes publishable only after resolution and registry checks.
pub fn prepare(root: &Path, list: &Value, host: &HostServices) -> FactoryOutcome {
    let mut candidates = Vec::new();
    let mut ready_ids = HashSet::new();
    let mut errors = HashMap::new();
    let mut missing = HashMap::new();
    let mut unhealthy = HashMap::new();
    let mut measured = HashMap::new();
    let mut result = plugin_list::resolve_with(root, list, &mut |request| {
        let mut missing_bindings = Vec::new();
        for (role, target) in request.bindings.as_object().into_iter().flat_map(|bindings| bindings.iter()) {
            if target.as_str().is_some_and(|id| ready_ids.contains(id)) { continue; }
            let required = request.manifest["dependencies"].as_array().is_some_and(|dependencies| {
                dependencies.iter().any(|dependency| dependency["role"] == role.as_str()
                    && dependency["required"] == true)
            });
            if required {
                errors.insert(request.id.clone(), "dependency_unavailable".to_owned());
                return Err("dependency_unavailable".to_owned());
            }
            missing_bindings.push(role.clone());
        }
        match prepare_one(root, request, host) {
            Ok(port) => {
                let actual = prepared_capabilities(&port);
                measured.insert(request.id.clone(), actual.clone());
                let unverified_sidecar = matches!(&port, PreparedPort::Model { sidecar: Some(_), .. });
                if unverified_sidecar {
                    errors.insert(request.id.clone(), "sidecar_protocol_unverified".to_owned());
                }
                let mut missing_optional = optional_missing(request);
                missing_optional.extend(missing_bindings);
                missing_optional.sort();
                missing_optional.dedup();
                missing.insert(request.id.clone(), missing_optional);
                let not_ready = matches!(&port, PreparedPort::Chat { port } if port.state() != PortState::Available);
                if not_ready {
                    unhealthy.insert(request.id.clone(), "onebot_connection".to_owned());
                }
                let claims = request.manifest["capabilities"]["supported"].as_array();
                let actual_names = actual["capabilities"].as_array();
                let capabilities_match = claims.zip(actual_names).is_some_and(|(claimed, observed)| {
                    claimed.iter().all(|cap| observed.contains(cap))
                });
                if !unverified_sidecar && !not_ready && capabilities_match {
                    ready_ids.insert(request.id.clone());
                }
                candidates.push(port);
                Ok(actual)
            }
            Err(code) => { errors.insert(request.id.clone(), code.clone()); Err(code) }
        }
    });
    let successful = result["registered"].as_array().is_some_and(|items| items.len() == candidates.len())
        && errors.is_empty() && result["reason"] != "preflight_failed";
    let mut published = if successful {
        match PublishedPorts::publish(candidates) {
            Ok(ports) => Some(ports),
            Err(code) => {
                result["registered"] = json!([]);
                result["reason"] = json!(code);
                if let Some(states) = result["plugins"].as_object_mut() {
                    for state in states.values_mut() {
                        if state["effective"] == true {
                            state["effective"] = json!(false);
                            state["reason"] = json!("activation_aborted");
                        }
                    }
                }
                None
            }
        }
    } else { None };
    if published.is_none() && result["registered"].as_array().is_some_and(|items| !items.is_empty()) {
        result["registered"] = json!([]);
        if let Some(states) = result["plugins"].as_object_mut() {
            for state in states.values_mut() {
                if state["effective"] == true {
                    state["effective"] = json!(false);
                    state["reason"] = json!("activation_aborted");
                }
            }
        }
    }
    if let Some(states) = result["plugins"].as_object_mut() {
        for (id, state) in states {
            if let Some(code) = errors.get(id) {
                state["reason"] = json!(code);
            }
            if published.is_some() && state["effective"] == true {
                if let Some(items) = missing.get(id).filter(|items| !items.is_empty()) {
                    state["reason"] = json!("degraded");
                    state["missing"] = json!(items);
                }
                if let Some(reason) = unhealthy.get(id) {
                    state["reason"] = json!("degraded");
                    state["health"] = json!(reason);
                }
            }
        }
    }
    if let Some(ports) = &mut published {
        for (id, capabilities) in measured {
            let state = &result["plugins"][&id];
            if state["effective"] == true {
                let degraded = state["reason"] == "degraded";
                let effective_capabilities = json!({"supported":capabilities["capabilities"],
                    "unsupported":[],"unknown":[]});
                result["plugins"][&id]["capabilities"] = effective_capabilities.clone();
                let observed_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                ports.observations.insert(id, json!({"effective_state":
                    if degraded { "degraded" } else { "active" },
                    "capabilities":effective_capabilities,"observedAt":observed_at}));
            }
        }
    }
    FactoryOutcome { status: result, published }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_snapshot_reads_live_chat_state_without_repreparing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let connected = Arc::new(AtomicBool::new(false));
        let state = connected.clone();
        let action: Action = Arc::new(|_, _| Box::pin(async { PortReply::Ok(Value::Null) }));
        let connection: Connection = Arc::new(move || (state.load(Ordering::Relaxed), true));
        let chat = Arc::new(OneBotAdapter::new("onebot", action, connection).unwrap());
        let mut ports = PublishedPorts::publish(vec![PreparedPort::Chat { port: chat }]).unwrap();
        ports.observations.insert("onebot".into(), json!({"effective_state":"active",
            "capabilities":{"supported":["reply"]},"observedAt":42}));
        let offline = ports.status_snapshot("revision", "snapshot");
        assert_eq!(offline["onebot"]["portState"], "disconnected");
        assert_eq!(offline["onebot"]["capabilities"]["supported"][0], "reply");
        assert_eq!(offline["onebot"]["observedAt"], 42);
        connected.store(true, Ordering::Relaxed);
        let online = ports.status_snapshot("revision", "later");
        assert_eq!(online["onebot"]["portState"], "available");
        assert_eq!(online["onebot"]["snapshotAt"], "later");
    }

    #[test]
    fn unsupported_claim_fails_before_secret_or_network_activity() {
        let request = ActivationRequest {
            id: "model".into(), directory: "openai".into(),
            manifest: json!({"kind":"Model","transport":"Remote",
                "capabilities":{"supported":["imaginary_capability"]}}),
            params: json!({"base_url":"http://127.0.0.1:1/v1"}),
            bindings: json!({}), secret_refs: json!({"api_key":"missing"}),
        };
        let host = HostServices::without_runtime(Arc::new(|_| panic!("secret must not be read")));
        assert_eq!(prepare_one(Path::new("/unused"), &request, &host).err().as_deref(),
            Some("capability_mismatch"));
    }

    #[test]
    fn failed_required_binding_never_constructs_a_dependent_port() {
        let root = std::env::temp_dir().join(format!("qqia-factory-dependency-{}", rand::random::<u64>()));
        for (directory, dependencies) in [
            ("openai", json!([])),
            ("claude", json!([{"role":"ModelPort","needed_for":"completion","required":true}])),
        ] {
            let package = root.join(directory);
            std::fs::create_dir_all(&package).unwrap();
            let manifest = json!({"name":directory,"version":"1.0.0","kind":"Model","transport":"Remote",
                "core_api_version":{"major":1,"min_minor":0,"max_minor":null},
                "protocol":{"name":"http-json"},"entry":{"type":"http"},
                "capabilities":{"supported":["text_completion"]},
                "dependencies":dependencies,"host_permissions":[],
                "config_schema":[{"key":"base_url","type":"string","required":true},
                    {"key":"api_key","type":"string","required":true,"secret":true}]});
            std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
        }
        let list = json!({"schema_version":1,"mode":"manual","plugins":[
            {"id":"base","directory":"openai","enabled":true,
                "params":{"base_url":"http://127.0.0.1:1/v1"},"bindings":{},
                "secret_refs":{"api_key":"test"}},
            {"id":"dependent","directory":"claude","enabled":true,
                "params":{"base_url":"http://127.0.0.1:1/v1"},
                "bindings":{"ModelPort":"base"},"secret_refs":{"api_key":"test"}}]});
        let host = HostServices::without_runtime(Arc::new(|_| Some("key".into())));
        let outcome = prepare(&root, &list, &host);
        assert_eq!(outcome.status["plugins"]["base"]["reason"], "backend_unavailable");
        assert_eq!(outcome.status["plugins"]["dependent"]["reason"], "dependency_unavailable");
        assert!(outcome.published.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
