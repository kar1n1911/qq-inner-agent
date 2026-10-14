//! Six port interfaces; implementations arrive after P0 (plan §5/§12, X1).

use super::types::*;
use futures_util::future::BoxFuture;
use std::sync::MutexGuard;
use tokio::sync::mpsc;

pub trait ChatPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> Capabilities;
    fn state(&self) -> PortState;
    fn deliver<'a>(&'a self, command: Command) -> BoxFuture<'a, PortReply<CommandResult>>;
    fn attach(&self, sink: mpsc::Sender<Incoming>) -> Result<(), IoError>;
}

pub trait ModelPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> ModelCapabilities;
    fn complete<'a>(&'a self, request: ModelRequest<'a>) -> BoxFuture<'a, PortReply<ModelReply>>;
    fn stream<'a>(&'a self, _request: ModelRequest<'a>) -> BoxFuture<'a, PortReply<ModelStream>> {
        Box::pin(async { PortReply::Unsupported { capability: Capability::Custom("streaming".into()) } })
    }
    fn list_models<'a>(&'a self) -> BoxFuture<'a, PortReply<Vec<String>>>;
}

/// The one in-process exception: callers retain a lock guard and transaction
/// boundary. An associated store type keeps this leaf independent of the
/// existing concrete SQLite `Store`; it is not a sidecar/remote interface.
pub trait DbPort: Send + Sync {
    type Store: Send;
    fn store(&self) -> Result<MutexGuard<'_, Self::Store>, IoError>;
}

pub trait MemoryPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> MemoryCapabilities;
    fn upsert<'a>(&'a self, entry: MemoryEntry) -> BoxFuture<'a, PortReply<()>>;
    fn forget<'a>(&'a self, key: &'a MemoryKey) -> BoxFuture<'a, PortReply<()>>;
    fn retrieve<'a>(&'a self, query: RetrievalQuery) -> BoxFuture<'a, PortReply<Vec<MemoryHit>>>;
    fn reset<'a>(&'a self, scope: MemoryScope) -> BoxFuture<'a, PortReply<()>>;
}

pub trait ExternalPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn fetch<'a>(&'a self, request: FetchRequest) -> BoxFuture<'a, PortReply<Vec<FeedItem>>>;
    fn read_bytes<'a>(&'a self, source: MediaSrc, limit: u64) -> BoxFuture<'a, PortReply<Vec<u8>>>;
    fn ocr<'a>(&'a self, bytes: Vec<u8>) -> BoxFuture<'a, PortReply<String>>;
}

pub trait PersonaPort: Send + Sync {
    fn manifest(&self) -> &PortManifest;
    fn capabilities(&self) -> PersonaCapabilities { PersonaCapabilities::default() }
    fn load<'a>(&'a self, chat: &'a ChatAddress) -> BoxFuture<'a, PortReply<PersonaSource>>;
}
