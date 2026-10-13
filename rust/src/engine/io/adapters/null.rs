//! Explicit-failure P1 adapters. No operation silently succeeds or returns an
//! empty/default result in place of a missing backend.

use super::{ports::*, types::*};
use futures_util::future::BoxFuture;
use std::sync::MutexGuard;
use tokio::sync::mpsc;

fn manifest(name: &str, kind: PortKind) -> PortManifest {
    PortManifest {
        name: name.into(),
        kind,
        transport: TransportKind::InProcess,
        core_api_version: VersionReq {
            major: 1,
            min_minor: 0,
            max_minor: None,
        },
        host_permissions: Vec::new(),
    }
}

fn unavailable<T>(code: &'static str) -> PortReply<T> {
    PortReply::Failed {
        code: code.into(),
        uncertain: false,
    }
}

pub struct NullChat {
    manifest: PortManifest,
}
impl NullChat {
    pub fn new(name: &str) -> Self {
        Self {
            manifest: manifest(name, PortKind::Chat),
        }
    }
}
impl ChatPort for NullChat {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn state(&self) -> PortState {
        PortState::Disconnected
    }
    fn deliver<'a>(&'a self, _: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(async { unavailable("chat_port_unavailable") })
    }
    fn attach(&self, _: mpsc::Sender<Incoming>) -> Result<(), IoError> {
        Err(IoError::new("chat_port_unavailable"))
    }
}

pub struct NullModel {
    manifest: PortManifest,
}
impl NullModel {
    pub fn new(name: &str) -> Self {
        Self {
            manifest: manifest(name, PortKind::Model),
        }
    }
}
impl ModelPort for NullModel {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    fn complete<'a>(&'a self, _: ModelRequest<'a>) -> BoxFuture<'a, PortReply<ModelReply>> {
        Box::pin(async { unavailable("model_port_unavailable") })
    }
    fn list_models<'a>(&'a self) -> BoxFuture<'a, PortReply<Vec<String>>> {
        Box::pin(async { unavailable("model_port_unavailable") })
    }
}

/// In-process placeholder only. `Store=()` cannot substitute for the real
/// SQLite handle in existing call sites; it makes the unavailability explicit.
pub struct NullDb;
impl DbPort for NullDb {
    type Store = ();
    fn store(&self) -> Result<MutexGuard<'_, Self::Store>, IoError> {
        Err(IoError::new("db_port_unavailable"))
    }
}

pub struct NullMemory {
    manifest: PortManifest,
}
impl NullMemory {
    pub fn new(name: &str) -> Self {
        Self {
            manifest: manifest(name, PortKind::Memory),
        }
    }
}
impl MemoryPort for NullMemory {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }
    fn upsert<'a>(&'a self, _: MemoryEntry) -> BoxFuture<'a, PortReply<()>> {
        Box::pin(async { unavailable("memory_port_unavailable") })
    }
    fn forget<'a>(&'a self, _: &'a MemoryKey) -> BoxFuture<'a, PortReply<()>> {
        Box::pin(async { unavailable("memory_port_unavailable") })
    }
    fn retrieve<'a>(&'a self, _: RetrievalQuery) -> BoxFuture<'a, PortReply<Vec<MemoryHit>>> {
        Box::pin(async { unavailable("memory_port_unavailable") })
    }
    fn reset<'a>(&'a self, _: MemoryScope) -> BoxFuture<'a, PortReply<()>> {
        Box::pin(async { unavailable("memory_port_unavailable") })
    }
}

pub struct NullExternal {
    manifest: PortManifest,
}
impl NullExternal {
    pub fn new(name: &str) -> Self {
        Self {
            manifest: manifest(name, PortKind::External),
        }
    }
}
impl ExternalPort for NullExternal {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }
    fn fetch<'a>(&'a self, _: FetchRequest) -> BoxFuture<'a, PortReply<Vec<FeedItem>>> {
        Box::pin(async { unavailable("external_port_unavailable") })
    }
    fn read_bytes<'a>(&'a self, _: MediaSrc, _: u64) -> BoxFuture<'a, PortReply<Vec<u8>>> {
        Box::pin(async { unavailable("external_port_unavailable") })
    }
    fn ocr<'a>(&'a self, _: Vec<u8>) -> BoxFuture<'a, PortReply<String>> {
        Box::pin(async { unavailable("external_port_unavailable") })
    }
}

pub struct NullPersona {
    manifest: PortManifest,
}
impl NullPersona {
    pub fn new(name: &str) -> Self {
        Self {
            manifest: manifest(name, PortKind::Persona),
        }
    }
}
impl PersonaPort for NullPersona {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }
    fn load<'a>(&'a self, _: &'a ChatAddress) -> BoxFuture<'a, PortReply<PersonaSource>> {
        Box::pin(async { unavailable("persona_port_unavailable") })
    }
}
