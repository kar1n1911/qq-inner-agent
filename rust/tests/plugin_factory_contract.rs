//! Plugin-factory acceptance tests. Observe a model request at its actual HTTP
//! endpoint and inspect activation state; callback invocation alone is not success.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_qq-inner-core");

struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("plugin-factory-contract-{}", rand::random::<u64>()));
        fs::create_dir_all(path.join("plugins"))?;
        Ok(Self(path))
    }
    fn plugins(&self) -> PathBuf {
        self.0.join("plugins")
    }
    fn list(&self) -> PathBuf {
        self.plugins().join("plugins.json")
    }
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ModelServer {
    url: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}
impl ModelServer {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}/v1", listener.local_addr()?);
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let count = requests.clone();
        let quit = stop.clone();
        let task = thread::spawn(move || {
            while !quit.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut request_line = String::new();
                        if reader.read_line(&mut request_line).is_err() {
                            continue;
                        }
                        let mut content_length = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err()
                                || line == "\r\n"
                                || line.is_empty()
                            {
                                break;
                            }
                            if let Some(value) =
                                line.to_ascii_lowercase().strip_prefix("content-length:")
                            {
                                content_length = value.trim().parse::<usize>().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0; content_length.min(1_000_000)];
                        let _ = reader.read_exact(&mut body);
                        if request_line.contains("/chat/completions") {
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                        let response = r#"{"choices":[{"message":{"content":"{\"ok\":true}"},"finish_reason":"stop"}]}"#;
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            url,
            requests,
            stop,
            task: Some(task),
        })
    }
    fn calls(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}
impl Drop for ModelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

mod memory_wire {
    tonic::include_proto!("a_memorix.api.v1");
}

#[derive(Clone)]
struct MemoryProbeService;

impl tonic::server::NamedService for MemoryProbeService {
    const NAME: &'static str = "a_memorix.api.v1.NamespaceService";
}

impl tonic::codegen::Service<tonic::codegen::http::Request<tonic::body::BoxBody>> for MemoryProbeService {
    type Response = tonic::codegen::http::Response<tonic::body::BoxBody>;
    type Error = std::convert::Infallible;
    type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: tonic::codegen::http::Request<tonic::body::BoxBody>) -> Self::Future {
        match request.uri().path() {
            "/a_memorix.api.v1.NamespaceService/GetNamespace" => {
                struct GetNamespace;
                impl tonic::server::UnaryService<memory_wire::GetNamespaceRequest> for GetNamespace {
                    type Response = memory_wire::GetNamespaceResponse;
                    type Future = tonic::codegen::BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                    fn call(&mut self, request: tonic::Request<memory_wire::GetNamespaceRequest>) -> Self::Future {
                        Box::pin(async move {
                            Ok(tonic::Response::new(memory_wire::GetNamespaceResponse {
                                namespace: Some(memory_wire::NamespaceInfo {
                                    namespace_id: request.into_inner().namespace_id,
                                    status: memory_wire::NamespaceStatus::Active as i32,
                                    ..Default::default()
                                }),
                            }))
                        })
                    }
                }
                Box::pin(async move {
                    Ok(tonic::server::Grpc::new(tonic::codec::ProstCodec::default())
                        .unary(GetNamespace, request).await)
                })
            }
            "/a_memorix.api.v1.NamespaceService/GetNamespaceCapabilities" => {
                struct GetCapabilities;
                impl tonic::server::UnaryService<memory_wire::GetNamespaceCapabilitiesRequest> for GetCapabilities {
                    type Response = memory_wire::GetNamespaceCapabilitiesResponse;
                    type Future = tonic::codegen::BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                    fn call(&mut self, request: tonic::Request<memory_wire::GetNamespaceCapabilitiesRequest>) -> Self::Future {
                        Box::pin(async move {
                            Ok(tonic::Response::new(memory_wire::GetNamespaceCapabilitiesResponse {
                                capabilities: Some(memory_wire::NamespaceCapabilities {
                                    namespace_id: request.into_inner().namespace_id,
                                    capabilities: std::collections::HashMap::from([
                                        ("search_memory".into(), true),
                                        ("embedding".into(), false),
                                    ]),
                                    degraded: false,
                                    ..Default::default()
                                }),
                            }))
                        })
                    }
                }
                Box::pin(async move {
                    Ok(tonic::server::Grpc::new(tonic::codec::ProstCodec::default())
                        .unary(GetCapabilities, request).await)
                })
            }
            "/a_memorix.api.v1.NamespaceService/GetNamespaceHealth" => {
                struct GetHealth;
                impl tonic::server::UnaryService<memory_wire::GetNamespaceHealthRequest> for GetHealth {
                    type Response = memory_wire::GetNamespaceHealthResponse;
                    type Future = tonic::codegen::BoxFuture<tonic::Response<Self::Response>, tonic::Status>;
                    fn call(&mut self, request: tonic::Request<memory_wire::GetNamespaceHealthRequest>) -> Self::Future {
                        Box::pin(async move {
                            Ok(tonic::Response::new(memory_wire::GetNamespaceHealthResponse {
                                health: Some(memory_wire::NamespaceHealth {
                                    namespace: Some(memory_wire::NamespaceInfo {
                                        namespace_id: request.into_inner().namespace_id,
                                        status: memory_wire::NamespaceStatus::Active as i32,
                                        ..Default::default()
                                    }),
                                    runtime_state: memory_wire::NamespaceRuntimeState::Ready as i32,
                                    healthy: true,
                                    degraded: false,
                                    ..Default::default()
                                }),
                            }))
                        })
                    }
                }
                Box::pin(async move {
                    Ok(tonic::server::Grpc::new(tonic::codec::ProstCodec::default())
                        .unary(GetHealth, request).await)
                })
            }
            _ => Box::pin(async move {
                Ok(tonic::codegen::http::Response::builder()
                    .header("grpc-status", "12")
                    .header("content-type", "application/grpc")
                    .body(tonic::body::empty_body())
                    .unwrap())
            }),
        }
    }
}

struct MemoryServer {
    url: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
}
impl MemoryServer {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let incoming = futures_util::stream::unfold(listener, |listener| async move {
                    let result = listener.accept().await.map(|(stream, _)| stream);
                    Some((result, listener))
                });
                tonic::transport::Server::builder()
                    .add_service(MemoryProbeService)
                    .serve_with_incoming_shutdown(incoming, async { let _ = stopped.await; })
                    .await
                    .unwrap();
            });
        });
        Ok(Self { url, stop: Some(stop), task: Some(task) })
    }

    fn verify(&self) -> Result<()> {
        use qq_inner_core::engine::io::{adapters::amemorix::AmemorixPort, ChatAddress, ChatKind, MemorySubject, PortReply};
        let address = ChatAddress::new(ChatKind::Group, "onebot", "10")?;
        tokio::runtime::Runtime::new()?.block_on(async {
            let port = AmemorixPort::new(&self.url, String::new())?;
            let result = port.refresh_capabilities(&address, &MemorySubject::Group).await;
            anyhow::ensure!(matches!(result, PortReply::Ok(ref capabilities)
                if capabilities.text_retrieval && !capabilities.embeddings),
                "Memory fixture must probe healthy base retrieval without embeddings: {result:?}");
            Ok(())
        })
    }
}
impl Drop for MemoryServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(task) = self.task.take() { let _ = task.join(); }
    }
}

struct Agent(Child);
impl Agent {
    fn start(dir: &Directory) -> Result<Self> {
        let child = Command::new(BIN)
            .arg("--root")
            .arg(&dir.0)
            .arg("start")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(Self(child))
    }
    fn control(&mut self, dir: &Directory, method: &str) -> Result<Value> {
        let socket = dir.state().join("control.sock");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            if let Some(status) = self.0.try_wait()? {
                let mut stderr = String::new();
                if let Some(pipe) = self.0.stderr.as_mut() {
                    pipe.read_to_string(&mut stderr)?;
                }
                anyhow::bail!("agent exited before control socket ({status}): {stderr}");
            }
            if socket.exists() {
                match UnixStream::connect(&socket) {
                    Ok(stream) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
                    Err(error) => return Err(error.into()),
                }
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "agent control socket did not accept connections"
            );
            thread::sleep(Duration::from_millis(20));
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        writeln!(
            stream,
            "{}",
            json!({"id":"factory-contract","method":method,"params":{}})
        )?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let response: Value = serde_json::from_str(&line).context("control response JSON")?;
        anyhow::ensure!(
            response["ok"] == true,
            "control {method} failed: {response}"
        );
        Ok(response["result"].clone())
    }
}
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn config(dir: &Directory, old_endpoint: &str) -> Result<()> {
    fs::write(
        dir.0.join("config.json"),
        serde_json::to_vec_pretty(&json!({
            "storage":{"directory":"state"},
            "provider":{"kind":"openai","ioPort":"openai","baseUrl":old_endpoint,"model":"fixture"},
            "onebot":{"url":"ws://127.0.0.1:9/","selfId":"99"},
            "agent":{"allowedGroups":["10"],"observation":{"enabled":false},"identity":{"enabled":false}}
        }))?,
    )?;
    fs::write(dir.0.join("secrets.json"), br#"{"apiKey":"test-key"}"#)?;
    Ok(())
}

fn model_manifest(directory: &str, claims: &[&str], permissions: Value) -> Value {
    json!({
        "name":directory,"version":"1.0.0","kind":"Model","transport":"Remote",
        "core_api_version":{"major":1,"min_minor":0,"max_minor":null},
        "protocol":{"name":"http-json","schema_version":"1.0","schema":"model-v1"},
        "entry":{"type":"http"},
        "capabilities":{"supported":claims,"unsupported":[],"unknown":[]},
        "dependencies":[],"host_permissions":permissions,
        "config_schema":[
            {"key":"base_url","label":"URL","type":"string","required":true,"secret":false},
            {"key":"model","label":"Model","type":"string","required":true,"secret":false},
            {"key":"api_key","label":"Key","type":"string","required":true,"secret":true}
        ]
    })
}

fn package(dir: &Directory, name: &str, manifest: Value) -> Result<()> {
    let path = dir.plugins().join(name);
    fs::create_dir_all(&path)?;
    fs::write(
        path.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn entry(id: &str, directory: &str, endpoint: &str, enabled: bool) -> Value {
    json!({"id":id,"directory":directory,"enabled":enabled,
        "params":{"base_url":endpoint,"model":"fixture"},"bindings":{},
        "secret_refs":{"api_key":"secrets.apiKey"}})
}

fn list(dir: &Directory, entries: Vec<Value>) -> Result<()> {
    fs::write(
        dir.list(),
        serde_json::to_vec_pretty(&json!({
            "schema_version":1,"mode":"manual","plugins":entries
        }))?,
    )?;
    Ok(())
}

fn reconcile(dir: &Directory) -> Result<(bool, Value)> {
    let output = Command::new(BIN)
        .arg("--root")
        .arg(&dir.plugins())
        .args(["plugins", "reconcile", "--config"])
        .arg(dir.list())
        .arg("--json")
        .output()?;
    let bytes = if output.status.success() {
        &output.stdout
    } else {
        &output.stderr
    };
    let value: Value =
        serde_json::from_slice(bytes).context("reconcile must report structured status/error")?;
    Ok((output.status.success(), value))
}

fn plugin<'a>(status: &'a Value, id: &str) -> Result<&'a Value> {
    status["plugins"]
        .as_array()
        .with_context(|| format!("queryable plugin status, got {status}"))?
        .iter()
        .find(|p| p["id"] == id)
        .context("plugin must appear in status")
}

#[test]
fn list_changes_the_real_model_endpoint_and_absent_list_preserves_baseline() -> Result<()> {
    let old = ModelServer::start()?;
    let selected = ModelServer::start()?;
    let dir = Directory::new()?;
    config(&dir, &old.url)?;
    {
        let mut agent = Agent::start(&dir)?;
        assert_eq!(agent.control(&dir, "test.model")?["ok"], true);
    }
    assert_eq!(
        old.calls(),
        1,
        "without a list the existing configured model must handle requests"
    );
    assert_eq!(selected.calls(), 0);
    package(
        &dir,
        "openai",
        model_manifest("openai", &["text_completion", "json_mode"], json!([])),
    )?;
    list(&dir, vec![entry("openai", "openai", &selected.url, true)])?;
    let mut agent = Agent::start(&dir)?;
    assert_eq!(agent.control(&dir, "test.model")?["ok"], true);
    assert_eq!(
        selected.calls(),
        1,
        "a real model request must reach the list's endpoint"
    );
    assert_eq!(
        old.calls(),
        1,
        "the old endpoint must not silently handle the plugin request"
    );
    Ok(())
}

#[test]
fn declared_host_activity_is_queryable_without_making_it_an_authorization_gate() -> Result<()> {
    let dir = Directory::new()?;
    let origin = "http://127.0.0.1:43210";
    package(
        &dir,
        "openai",
        model_manifest(
            "openai",
            &["text_completion"],
            json!([{"type":"http","origin":origin}]),
        ),
    )?;
    list(&dir, vec![entry("openai", "openai", origin, false)])?;
    let (ok, status) = reconcile(&dir)?;
    assert!(
        ok,
        "disabled package must be inspectable without construction: {status}"
    );
    let visible = plugin(&status, "openai")?;
    assert_eq!(visible["effective_state"], "disabled");
    assert!(
        visible.to_string().contains(origin),
        "the declared host destination must be visible in status even when disabled"
    );
    Ok(())
}

#[test]
fn a_declared_host_call_leaves_an_observable_audit_record() -> Result<()> {
    let old = ModelServer::start()?;
    let selected = ModelServer::start()?;
    let dir = Directory::new()?;
    config(&dir, &old.url)?;
    let origin = selected
        .url
        .strip_suffix("/v1")
        .context("model URL origin")?;
    package(
        &dir,
        "openai",
        model_manifest(
            "openai",
            &["text_completion"],
            json!([{"type":"http","origin":origin}]),
        ),
    )?;
    list(&dir, vec![entry("openai", "openai", &selected.url, true)])?;
    let mut agent = Agent::start(&dir)?;
    let log_path = dir.state().join("agent.log");
    let before = fs::read_to_string(&log_path).unwrap_or_default();
    agent.control(&dir, "test.model")?;
    assert_eq!(
        selected.calls(),
        1,
        "the declared destination must actually receive the request"
    );
    let after = fs::read_to_string(log_path)?;
    let added = after
        .strip_prefix(&before)
        .context("audit log must append")?;
    assert!(
        added.contains(origin) && added.contains("plugin"),
        "an actual plugin HTTP call must leave a queryable audit record of its destination"
    );
    Ok(())
}

#[test]
fn disabled_and_unlisted_packages_are_visible_but_do_not_start_sidecars() -> Result<()> {
    let dir = Directory::new()?;
    let marker = dir.0.join("sidecar-started");
    for name in ["disabled", "unlisted"] {
        let mut manifest = model_manifest(name, &["text_completion"], json!([]));
        manifest["transport"] = json!("Sidecar");
        manifest["entry"] = json!({"executable":format!("{name}/start.sh")});
        package(&dir, name, manifest)?;
        fs::write(
            dir.plugins().join(name).join("start.sh"),
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )?;
    }
    list(
        &dir,
        vec![entry(
            "disabled",
            "disabled",
            "http://127.0.0.1:9/v1",
            false,
        )],
    )?;
    let (ok, status) = reconcile(&dir)?;
    assert!(
        ok,
        "disabled/unlisted packages must not require construction: {status}"
    );
    assert_eq!(plugin(&status, "disabled")?["effective_state"], "disabled");
    assert!(!marker.exists(), "neither package may start a sidecar");
    assert!(
        status["discovery"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["directory"] == "unlisted")),
        "unlisted package must remain discoverable"
    );
    Ok(())
}

#[test]
fn failed_batch_keeps_the_old_generation_and_cleans_prepared_sidecars() -> Result<()> {
    let old = ModelServer::start()?;
    let selected = ModelServer::start()?;
    let dir = Directory::new()?;
    config(&dir, &old.url)?;
    package(
        &dir,
        "openai",
        model_manifest("openai", &["text_completion"], json!([])),
    )?;
    list(&dir, vec![entry("openai", "openai", &selected.url, true)])?;
    let mut agent = Agent::start(&dir)?;
    agent.control(&dir, "test.model")?;
    assert_eq!(selected.calls(), 1);
    let pid_file = dir.0.join("prepared-sidecar.pid");
    for (name, fail) in [("prepared", false), ("failing", true)] {
        let mut manifest = model_manifest(name, &["text_completion"], json!([]));
        manifest["transport"] = json!("Sidecar");
        manifest["entry"] = json!({"executable":format!("{name}/start.sh")});
        package(&dir, name, manifest)?;
        let script = if fail {
            "#!/bin/sh\nexit 7\n".to_owned()
        } else {
            format!(
                "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
                pid_file.display()
            )
        };
        let path = dir.plugins().join(name).join("start.sh");
        fs::write(&path, script)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    list(
        &dir,
        vec![
            entry("openai", "openai", &selected.url, true),
            entry("prepared", "prepared", &selected.url, true),
            entry("failing", "failing", &selected.url, true),
        ],
    )?;
    // A changed config requests the same production reload path as an operator edit.
    let config_path = dir.0.join("config.json");
    let mut config_value: Value = serde_json::from_slice(&fs::read(&config_path)?)?;
    config_value["agent"]["aliases"] = json!(["factory-reload"]);
    fs::write(config_path, serde_json::to_vec_pretty(&config_value)?)?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let state = agent.control(&dir, "state.get")?;
        if state["reloadError"].is_string() && state["reloading"] == false {
            break;
        }
        anyhow::ensure!(Instant::now() < deadline, "failed plugin batch must be reported by live reload");
        thread::sleep(Duration::from_millis(50));
    }
    agent.control(&dir, "test.model")?;
    assert_eq!(
        selected.calls(),
        2,
        "failed batch must preserve the old effective endpoint"
    );
    let (ok, status) = reconcile(&dir)?;
    assert!(
        !ok,
        "failing batch must be visible, not reported active: {status}"
    );
    let pid = fs::read_to_string(pid_file)
        .context("the batch must exercise cleanup of a started sidecar")?;
    let output = Command::new("kill").arg("-0").arg(pid.trim()).output()?;
    if output.status.success() {
        let _ = Command::new("kill").arg(pid.trim()).status();
    }
    assert!(
        !output.status.success(),
        "prepared sidecar must be stopped after rollback"
    );
    Ok(())
}

#[test]
fn constructed_port_with_failed_backend_probe_is_not_reported_available() -> Result<()> {
    let dir = Directory::new()?;
    package(
        &dir,
        "openai",
        model_manifest("openai", &["text_completion"], json!([])),
    )?;
    list(
        &dir,
        vec![entry("openai", "openai", "http://127.0.0.1:1/v1", true)],
    )?;
    let (_, down) = reconcile(&dir)?;
    let state = plugin(&down, "openai")?;
    assert_ne!(
        state["effective_state"], "active",
        "constructed object is not healthy service"
    );
    assert_ne!(
        state["reason"], "load_failed",
        "backend probe failure must be identified, not called missing factory"
    );
    Ok(())
}

#[test]
fn manifest_claim_not_supported_by_real_port_is_not_advertised() -> Result<()> {
    let server = ModelServer::start()?;
    let dir = Directory::new()?;
    package(
        &dir,
        "openai",
        model_manifest(
            "openai",
            &["text_completion", "imaginary_capability"],
            json!([]),
        ),
    )?;
    list(&dir, vec![entry("openai", "openai", &server.url, true)])?;
    let (_, mismatch) = reconcile(&dir)?;
    assert_eq!(
        plugin(&mismatch, "openai")?["reason"],
        "capability_mismatch",
        "manifest claims must be checked against the actual port"
    );
    Ok(())
}

#[test]
fn missing_optional_embedding_dependency_is_explicitly_degraded() -> Result<()> {
    let server = MemoryServer::start()?;
    server.verify()?;
    let dir = Directory::new()?;
    let memory = json!({
        "name":"a_memorix","version":"1.0.0","kind":"Memory","transport":"Remote",
        "core_api_version":{"major":1,"min_minor":0,"max_minor":null},
        "protocol":{"name":"gRPC","schema_version":"1.0","schema":"a_memorix.api.v1.NamespaceService"},
        "entry":{"type":"grpc","endpoint":server.url},
        "capabilities":{"supported":["text_retrieval"],"unsupported":[],"unknown":["embeddings"]},
        "dependencies":[{"role":"embedding_provider","needed_for":"semantic_retrieval","required":false}],
        "host_permissions":[],
        "config_schema":[{"key":"endpoint","label":"gRPC endpoint","type":"string","required":true,"secret":false}]
    });
    package(&dir, "a_memorix", memory)?;
    list(
        &dir,
        vec![json!({"id":"a_memorix","directory":"a_memorix","enabled":true,
            "params":{"endpoint":server.url},"bindings":{},"secret_refs":{}})],
    )?;
    let (_, degraded) = reconcile(&dir)?;
    let memory_state = plugin(&degraded, "a_memorix")?;
    assert_eq!(
        memory_state["effective_state"], "degraded",
        "missing optional embedding provider must be reported as degraded"
    );
    assert!(memory_state.to_string().contains("embedding_provider"));
    assert!(!memory_state
        .to_string()
        .contains("\"semantic_retrieval\":true"));
    Ok(())
}
