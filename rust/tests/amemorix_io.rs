//! P0/P1/AB2: cross-process memory port, scoped IDs and explicit failures.
use qq_inner_core::engine::io::{
    adapters::amemorix::AmemorixPort, Evidence, MemoryEntry,
    MemoryKey, MemoryLayer, MemoryPort, MemorySubject, PortReply, RetrievalQuery,
};
use std::{net::TcpListener, path::PathBuf, process::{Child, Command}, time::Duration};

fn key() -> MemoryKey {
    MemoryKey {
        chat: "group:qq:123".parse().unwrap(),
        subject: MemorySubject::Person("456".into()),
        layer: MemoryLayer::LongTerm,
        slot: "garden".into(),
    }
}

#[tokio::test]
async fn unavailable_service_fails_explicitly_and_capabilities_do_not_claim_providers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let adapter = AmemorixPort::new(&format!("http://127.0.0.1:{port}"), String::new()).unwrap();
    assert!(!adapter.capabilities().embeddings);
    assert!(!adapter.capabilities().relation_extraction);
    assert!(adapter.manifest().dependencies.iter().any(|d| d.role == "embedding_provider" && d.needed_for == "semantic_retrieval"));
    let result = adapter.retrieve(RetrievalQuery {
        chat: key().chat, subject: MemorySubject::Person("456".into()),
        layers: vec![MemoryLayer::LongTerm], limit: 2, text: Some("garden".into()),
    }).await;
    assert!(matches!(result, PortReply::Failed { .. }), "offline service must not return empty success");
}

#[test]
fn stable_entry_id_is_distinct_from_message_evidence_id_and_scope_isolated() {
    let first = key();
    let second = MemoryKey { slot: "garden".into(), chat: "group:napcat:123".parse().unwrap(), ..first.clone() };
    assert_eq!(AmemorixPort::external_id(&first), AmemorixPort::external_id(&first));
    assert_ne!(AmemorixPort::external_id(&first), "message-key:42");
    assert_ne!(AmemorixPort::namespace(&first.chat, &first.subject), AmemorixPort::namespace(&second.chat, &second.subject));
    assert_ne!(AmemorixPort::external_id(&first), AmemorixPort::external_id(&second));
}

struct Service { child: Child, dir: PathBuf }
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn rust_grpc_python_roundtrip_when_a_memorix_is_installed() {
    // Runs on the deployment VM; CI without the optional Python package skips
    // only the external-service exercise, not the offline contract tests.
    let package = PathBuf::from(std::env::var("A_MEMORIX_PACKAGE_DIR")
        .unwrap_or_else(|_| format!("{}/.local/share/a-memorix-pkgs", std::env::var("HOME").unwrap_or_default())));
    let binary = package.join("bin/a-memorix");
    if !binary.is_file() { return; }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = std::env::temp_dir().join(format!("qqia-amx-{}-{}", std::process::id(), rand::random::<u64>()));
    let child = Command::new(&binary).env("PYTHONPATH", &package)
        .args(["serve", "--data-dir", dir.to_str().unwrap(), "--host", "127.0.0.1",
            "--port", &port.to_string(), "--allow-unauthenticated"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    let _service = Service { child, dir };
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..100 {
        if std::net::TcpStream::connect(&addr).is_ok() { break; }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let adapter = AmemorixPort::new(&format!("http://{addr}"), String::new()).unwrap();
    let entry = MemoryEntry {
        key: key(), text: "A rare blue orchid grows by the eastern window".into(),
        sources: vec![Evidence {
            chat: key().chat, message_key: "message-key:42".into(), message_id: Some("protocol:7".into()),
            ts: 1_700_000_000., received_at: Some(1_700_000_005.),
        }],
        confidence: 0.9, importance: 0.8, expires: None, keywords: vec!["orchid".into()],
    };
    let capability = adapter.refresh_capabilities(&entry.key.chat, &entry.key.subject).await;
    assert!(matches!(capability, PortReply::Ok(_)), "capability RPC: {capability:?}");
    let capability = adapter.capabilities();
    assert!(capability.text_retrieval);
    assert!(!capability.embeddings);
    assert!(!capability.relation_extraction);
    let wrote = adapter.upsert(entry.clone()).await;
    assert_eq!(wrote, PortReply::Ok(()), "ingest RPC");
    let found = adapter.retrieve(RetrievalQuery {
        chat: entry.key.chat.clone(), subject: entry.key.subject.clone(),
        layers: vec![MemoryLayer::LongTerm], limit: 10, text: Some("blue orchid".into()),
    }).await;
    let PortReply::Ok(hits) = found else { panic!("search RPC: {found:?}"); };
    assert!(hits.iter().any(|hit| hit.key == entry.key && hit.sources == entry.sources), "roundtrip: {hits:?}");
}
