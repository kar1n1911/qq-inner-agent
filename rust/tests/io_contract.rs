//! P0 availability + target checks: io-port-plan §12 and X1.

use futures_util::future::BoxFuture;
use qq_inner_core::engine::io::*;
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
};
use tokio::sync::mpsc;

struct TestDb(Mutex<()>);
impl DbPort for TestDb {
    type Store = ();
    fn store(&self) -> Result<MutexGuard<'_, Self::Store>, IoError> {
        self.0.lock().map_err(|_| IoError::new("poisoned"))
    }
}

struct TestChat {
    manifest: PortManifest,
    deliveries: AtomicUsize,
}

impl TestChat {
    fn new(name: &str) -> Self {
        Self {
            manifest: PortManifest {
                name: name.into(),
                kind: PortKind::Chat,
                transport: TransportKind::InProcess,
                core_api_version: VersionReq {
                    major: 1,
                    min_minor: 0,
                    max_minor: None,
                },
                host_permissions: vec![],
                dependencies: vec![],
                config_fields: vec![],
            },
            deliveries: AtomicUsize::new(0),
        }
    }
}

impl ChatPort for TestChat {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            history: true,
            ..Default::default()
        }
    }
    fn state(&self) -> PortState {
        PortState::Available
    }
    fn deliver<'a>(&'a self, _: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        self.deliveries.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { PortReply::Ok(CommandResult::Ok) })
    }
    fn attach(&self, _: mpsc::Sender<Incoming>) -> Result<(), IoError> {
        Ok(())
    }
}

#[tokio::test]
async fn registry_and_router_dispatch_by_explicit_port() {
    // Availability: §12 P0 registry lookup and source-port routing.
    let mut registry = PortRegistry::new(Arc::new(TestDb(Mutex::new(()))));
    let qq = Arc::new(TestChat::new("qq"));
    let odd = Arc::new(TestChat::new("odd"));
    registry.register_chat(qq.clone()).unwrap();
    registry.register_chat(odd.clone()).unwrap();
    assert!(registry.chat.get("odd").is_some());
    assert!(
        registry
            .chat
            .capabilities_for(&"group:odd:room:7".parse().unwrap())
            .unwrap()
            .history
    );
    let sent = registry
        .chat
        .deliver(Command::Health { port: "odd".into() })
        .await;
    assert_eq!(sent, PortReply::Ok(CommandResult::Ok));
    assert_eq!(qq.deliveries.load(Ordering::SeqCst), 0);
    assert_eq!(odd.deliveries.load(Ordering::SeqCst), 1);
    assert_eq!(
        registry
            .chat
            .deliver(Command::Health {
                port: "missing".into()
            })
            .await,
        PortReply::Failed {
            code: "unknown_port".into(),
            uncertain: false
        }
    );
}

#[test]
fn three_component_address_is_the_only_wire_shape() {
    // Target ②: §12 ChatAddress + X1. Opaque protocol IDs may contain colons.
    let address: ChatAddress = "group:odd:room:7".parse().unwrap();
    assert_eq!(address.port, "odd");
    assert_eq!(address.id, "room:7");
    assert_eq!(address.to_string(), "group:odd:room:7");
    assert_eq!(
        serde_json::to_string(&address).unwrap(),
        "\"group:odd:room:7\""
    );
    assert!("group:123".parse::<ChatAddress>().is_err());
}

#[test]
fn memory_evidence_locates_by_internal_message_key() {
    // Target ③: §12 Evidence. Protocol IDs are audit metadata, not the key.
    let chat = "group:qq:123".parse().unwrap();
    let source = Evidence {
        chat,
        message_key: "local-message:41".into(),
        message_id: Some("7".into()),
        ts: 100.,
        received_at: Some(105.),
    };
    let encoded = serde_json::to_value(&source).unwrap();
    assert_eq!(encoded["message_key"], "local-message:41");
    assert_eq!(encoded["message_id"], "7");
    assert_eq!(encoded["ts"], 100.);
    assert_eq!(encoded["received_at"], 105.);
    assert_ne!(source.message_key, source.message_id.unwrap());
}

#[test]
fn io_source_has_no_dependency_on_kernel_implementation() {
    // Target ①: P-5 / §12 + X1. The CI workflow runs this same test.
    fn inspect(dir: &Path, root: &Path) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                inspect(&path, root);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let source = fs::read_to_string(&path).unwrap();
            assert!(
                !source.contains("crate::"),
                "crate dependency in {}",
                path.display()
            );
            assert!(
                !source.contains("super::super"),
                "parent escape in {}",
                path.display()
            );
            if path == root.join("mod.rs") {
                assert!(
                    !source.contains("super::"),
                    "io root depends on engine sibling in {}",
                    path.display()
                );
            }
            assert!(
                !source.contains("include!("),
                "hidden source in {}",
                path.display()
            );
            if path == root.join("adapters/mod.rs") {
                let paths: Vec<_> = source.lines().filter(|line| line.starts_with("#[path = ")).collect();
                assert_eq!(paths, [
                    "#[path = \"../../../../../plugins/null/adapter.rs\"]",
                    "#[path = \"../../../../../plugins/onebot/adapter.rs\"]",
                    "#[path = \"../../../../../plugins/a_memorix/adapter.rs\"]",
                    "#[path = \"../../../../../plugins/claude/adapter.rs\"]",
                    "#[path = \"../../../../../plugins/model_http.rs\"]",
                    "#[path = \"../../../../../plugins/ollama/adapter.rs\"]",
                    "#[path = \"../../../../../plugins/openai/adapter.rs\"]",
                    "#[path = \"../../../../../plugins/sillytavern/adapter.rs\"]",
                ]);
                assert_eq!(source.matches("#[path").count(), paths.len(), "unexpected hidden module");
            } else {
                assert!(!source.contains("#[path"), "hidden module in {}", path.display());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine/io");
    inspect(&root, &root);
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins");
    inspect(&plugins, &root);
}

#[tokio::test]
async fn null_io_is_complete_and_real_calls_fail_explicitly() {
    // Availability + target ①: P1 requires all six faces, but no default success.
    let io = Io::null();
    let chat: ChatAddress = "group:null-chat:room".parse().unwrap();
    assert_eq!(
        io.chat().get("null-chat").unwrap().state(),
        PortState::Disconnected
    );
    let (sink, _) = mpsc::channel(1);
    assert_eq!(
        io.chat()
            .get("null-chat")
            .unwrap()
            .attach(sink)
            .err()
            .unwrap()
            .code,
        "chat_port_unavailable"
    );
    assert_eq!(io.model().manifest().kind, PortKind::Model);
    assert_eq!(io.memory().manifest().kind, PortKind::Memory);
    assert_eq!(io.external().manifest().kind, PortKind::External);
    assert_eq!(io.persona().manifest().kind, PortKind::Persona);
    assert_eq!(io.db().store().err().unwrap().code, "db_port_unavailable");
    assert_eq!(
        io.chat()
            .deliver(Command::Health {
                port: "null-chat".into()
            })
            .await,
        PortReply::Failed {
            code: "chat_port_unavailable".into(),
            uncertain: false
        }
    );
    let payload = serde_json::json!({});
    assert_eq!(
        io.model()
            .complete(ModelRequest {
                instance: "null-model",
                system: "",
                payload: &payload,
                format: OutputFormat::Json,
                max_tokens: 1,
            })
            .await,
        PortReply::Failed {
            code: "model_port_unavailable".into(),
            uncertain: false
        }
    );
    assert_eq!(
        io.memory()
            .retrieve(RetrievalQuery {
                chat: chat.clone(),
                subject: MemorySubject::Group,
                layers: vec![MemoryLayer::ShortTerm],
                limit: 1,
                text: None,
            })
            .await,
        PortReply::Failed {
            code: "memory_port_unavailable".into(),
            uncertain: false
        }
    );
    assert_eq!(
        io.external()
            .fetch(FetchRequest {
                source: "https://example.invalid".into(),
                timeout_ms: 1,
                max_bytes: 1,
                max_redirects: 0,
            })
            .await,
        PortReply::Failed {
            code: "external_port_unavailable".into(),
            uncertain: false
        }
    );
    assert_eq!(
        io.persona().load(&chat).await,
        PortReply::Failed {
            code: "persona_port_unavailable".into(),
            uncertain: false
        }
    );
    assert_eq!(
        io.chat()
            .deliver(Command::Health {
                port: "absent".into()
            })
            .await,
        PortReply::Failed {
            code: "unknown_port".into(),
            uncertain: false
        }
    );
}

#[test]
fn composition_rejects_missing_required_faces() {
    // Availability: P1 composition requires chat + model + memory + one DB.
    let registry = PortRegistry::new(Arc::new(TestDb(Mutex::new(()))));
    assert_eq!(
        Io::compose(registry, "model", "memory", None, None)
            .err()
            .unwrap()
            .code,
        "missing_chat_port"
    );
    let mut registry = PortRegistry::new(Arc::new(TestDb(Mutex::new(()))));
    registry
        .register_chat(Arc::new(TestChat::new("qq")))
        .unwrap();
    assert_eq!(
        Io::compose(registry, "model", "memory", None, None)
            .err()
            .unwrap()
            .code,
        "missing_model_port"
    );
}
