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
    fn attach(&self, _: mpsc::Sender<Incoming>) {}
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
    fn inspect(dir: &Path) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                inspect(&path);
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
            if path.file_name().is_some_and(|name| name == "mod.rs") {
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
            assert!(
                !source.contains("#[path"),
                "hidden module in {}",
                path.display()
            );
        }
    }
    inspect(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine/io"));
}
