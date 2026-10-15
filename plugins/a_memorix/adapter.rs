//! A_memorix 2.0.0a4 gRPC adapter. This module is optional and is not wired
//! into the legacy engine: registering it never changes the default memory.

use super::{ports::MemoryPort, types::*};
use futures_util::future::BoxFuture;
use prost_types::{value::Kind, Struct, Timestamp, Value as ProtoValue};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::Duration};
use tonic::{transport::{Channel, Endpoint}, Code, Request, Status};

mod wire { tonic::include_proto!("a_memorix.api.v1"); }

type MemoryAudit = Arc<dyn Fn(&str) + Send + Sync>;

pub struct AmemorixPort {
    manifest: PortManifest,
    channel: Channel,
    token: String,
    capabilities: Mutex<MemoryCapabilities>,
    endpoint: String,
    audit: Option<MemoryAudit>,
}

impl AmemorixPort {
    pub fn new(endpoint: &str, token: String) -> Result<Self, IoError> {
        let target = endpoint.to_owned();
        let endpoint = Endpoint::from_shared(endpoint.to_owned())
            .map_err(|_| IoError::new("invalid_memory_endpoint"))?
            .timeout(Duration::from_secs(4));
        Ok(Self {
            manifest: PortManifest {
                name: "amemorix".into(), kind: PortKind::Memory,
                transport: TransportKind::Remote,
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![],
                dependencies: vec![
                    PortDependency { role: "grpc_service".into(), needed_for: "memory_access".into(), required: true },
                    PortDependency { role: "embedding_provider".into(), needed_for: "semantic_retrieval".into(), required: false },
                    PortDependency { role: "language_model".into(), needed_for: "relation_extraction".into(), required: false },
                ],
                config_fields: vec![],
            },
            channel: endpoint.connect_lazy(), token,
            capabilities: Mutex::new(MemoryCapabilities::default()),
            endpoint: target, audit: None,
        })
    }

    pub fn with_audit(mut self, audit: MemoryAudit) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Namespace is scoped to one full port-qualified chat and one subject.
    /// Hashing produces a valid A_memorix ID without imposing its syntax on
    /// neutral chat addresses. Full values remain in each record's metadata.
    pub fn namespace(chat: &ChatAddress, subject: &MemorySubject) -> String {
        let input = serde_json::to_vec(&(chat, subject)).expect("serializable memory scope");
        let digest = Sha256::digest(input);
        let mut result = String::from("qqia-v1-");
        for byte in digest { result.push_str(&format!("{byte:02x}")); }
        result
    }

    /// A_memory's external_id identifies a memory entry, not one of its
    /// evidence messages. Every evidence source retains its own message_key.
    pub fn external_id(key: &MemoryKey) -> String {
        let input = serde_json::to_vec(key).expect("serializable memory key");
        let digest = Sha256::digest(input);
        let mut result = String::from("memory-v1-");
        for byte in digest { result.push_str(&format!("{byte:02x}")); }
        result
    }

    fn request<T>(&self, value: T) -> Result<Request<T>, IoError> {
        if let Some(audit) = &self.audit { audit(&self.endpoint); }
        let mut request = Request::new(value);
        if !self.token.is_empty() {
            let auth = format!("Bearer {}", self.token).parse()
                .map_err(|_| IoError::new("invalid_memory_token"))?;
            request.metadata_mut().insert("authorization", auth);
        }
        Ok(request)
    }

    async fn ensure_namespace(&self, id: &str) -> Result<(), Status> {
        let mut client = wire::namespace_service_client::NamespaceServiceClient::new(self.channel.clone());
        let get = self.request(wire::GetNamespaceRequest { namespace_id: id.into() })
            .map_err(|_| Status::invalid_argument("invalid_memory_token"))?;
        match client.get_namespace(get).await {
            Ok(_) => return Ok(()),
            Err(error) if error.code() == Code::NotFound => {},
            Err(error) => return Err(error),
        }
        let create = self.request(wire::CreateNamespaceRequest {
            namespace_id: id.into(), quota: None, config: None,
        }).map_err(|_| Status::invalid_argument("invalid_memory_token"))?;
        match client.create_namespace(create).await {
            Ok(_) => Ok(()),
            Err(error) if error.code() == Code::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Refresh the synchronous P0 capability snapshot from the remote service.
    pub async fn refresh_capabilities(&self, chat: &ChatAddress, subject: &MemorySubject) -> PortReply<MemoryCapabilities> {
        let id = Self::namespace(chat, subject);
        if let Err(error) = self.ensure_namespace(&id).await { return failed_status(error); }
        let mut client = wire::namespace_service_client::NamespaceServiceClient::new(self.channel.clone());
        let request = match self.request(wire::GetNamespaceCapabilitiesRequest { namespace_id: id }) {
            Ok(request) => request, Err(error) => return failed(&error.code, false),
        };
        match client.get_namespace_capabilities(request).await {
            Ok(reply) => {
                let Some(value) = reply.into_inner().capabilities else { return failed("missing_memory_capabilities", false); };
                let capabilities = MemoryCapabilities {
                    text_retrieval: value.capabilities.get("search_memory").copied().unwrap_or(false),
                    embeddings: value.capabilities.get("embedding").copied().unwrap_or(false),
                    relation_extraction: value.capabilities.get("relation_extraction").copied().unwrap_or(false),
                };
                *self.capabilities.lock().unwrap() = capabilities.clone();
                PortReply::Ok(capabilities)
            }
            Err(error) => failed_status(error),
        }
    }

    async fn write(&self, entry: MemoryEntry) -> PortReply<()> {
        if entry.text.trim().is_empty() || entry.sources.iter().any(|source| source.chat != entry.key.chat
            || source.message_key.is_empty() || !source.ts.is_finite() || source.ts <= 0.
            || source.received_at.is_some_and(|t| !t.is_finite() || t <= 0.)) {
            return failed("invalid_memory_entry", false);
        }
        let namespace = Self::namespace(&entry.key.chat, &entry.key.subject);
        if let Err(error) = self.ensure_namespace(&namespace).await { return failed_status(error); }
        let external_id = Self::external_id(&entry.key);
        let key_json = match serde_json::to_string(&entry.key) { Ok(value) => value, Err(_) => return failed("invalid_memory_key", false) };
        let sources_json = match serde_json::to_string(&entry.sources) { Ok(value) => value, Err(_) => return failed("invalid_memory_sources", false) };
        let mut fields = BTreeMap::new();
        fields.insert("key_json".into(), proto_string(key_json.clone()));
        fields.insert("sources_json".into(), proto_string(sources_json.clone()));
        fields.insert("confidence".into(), proto_number(entry.confidence));
        fields.insert("importance".into(), proto_number(entry.importance));
        let metadata = Struct { fields };
        let mut client = wire::memory_service_client::MemoryServiceClient::new(self.channel.clone());
        let get = wire::GetMemoryRequest {
            context: Some(context(&namespace)),
            selector: Some(wire::get_memory_request::Selector::ExternalId(external_id.clone())),
        };
        let existing = match self.request(get) {
            Ok(request) => match client.get_memory(request).await {
                Ok(reply) => reply.into_inner().memory,
                Err(error) if error.code() == Code::NotFound => None,
                Err(error) => return failed_status(error),
            },
            Err(error) => return failed(&error.code, false),
        };
        let replacing = existing.is_some();
        if let Some(record) = existing {
            let old_key = string_field(record.metadata.as_ref(), "key_json");
            let old_sources = string_field(record.metadata.as_ref(), "sources_json");
            if record.content == entry.text && old_key == Some(key_json.as_str())
                && old_sources == Some(sources_json.as_str()) { return PortReply::Ok(()); }
            // A_memorix ingestion deduplicates external_id rather than replacing
            // content. Delete-before-insert is explicit; a failed insert is
            // uncertain because the old record may already be gone.
            let delete = wire::DeleteMemoryRequest {
                context: Some(context(&namespace)),
                selector: Some(wire::delete_memory_request::Selector::ExternalId(external_id.clone())),
                reason: "memory_update".into(),
            };
            let request = match self.request(delete) { Ok(request) => request, Err(error) => return failed(&error.code, false) };
            if let Err(error) = client.delete_memory(request).await { return failed_status(error); }
        }
        let observed_at = entry.sources.iter().map(|e| e.ts).reduce(f64::max).and_then(timestamp);
        let ingest = wire::IngestTextRequest {
            context: Some(context(&namespace)), external_id,
            source_type: "qq_inner_agent_memory".into(), text: entry.text,
            person_ids: vec![], participants: vec![], observed_at,
            valid_from: None, valid_to: entry.expires.and_then(timestamp),
            tags: entry.keywords, metadata: Some(metadata), entities: vec![], relations: vec![],
            respect_filter: Some(false), relation_extraction: wire::RelationExtractionMode::Disabled as i32,
        };
        let request = match self.request(ingest) { Ok(request) => request, Err(error) => return failed(&error.code, replacing) };
        match client.ingest_text(request).await {
            Ok(reply) if !reply.get_ref().stored_ids.is_empty() || !reply.get_ref().skipped_ids.is_empty() => PortReply::Ok(()),
            Ok(_) => failed("memory_not_stored", true),
            Err(error) => failed_status(error),
        }
    }

    async fn read(&self, query: RetrievalQuery) -> PortReply<Vec<MemoryHit>> {
        if query.limit == 0 || query.limit > 100 { return failed("invalid_retrieval_limit", false); }
        let namespace = Self::namespace(&query.chat, &query.subject);
        let mut client = wire::memory_service_client::MemoryServiceClient::new(self.channel.clone());
        let request = wire::SearchMemoryRequest {
            context: Some(context(&namespace)), query: query.text.clone().unwrap_or_default(),
            limit: Some(query.limit as u32),
            mode: if query.text.as_deref().is_some_and(|s| !s.is_empty()) { wire::SearchMode::Search as i32 } else { wire::SearchMode::Time as i32 },
            shared_conversation_ids: vec![], person_id: String::new(),
            time_start: None, time_end: None, respect_filter: Some(true),
        };
        let request = match self.request(request) { Ok(request) => request, Err(error) => return failed(&error.code, false) };
        match client.search_memory(request).await {
            Ok(reply) => {
                let response = reply.into_inner();
                let mut hits = Vec::new();
                for hit in response.hits {
                    // Search hits contain retrieval scores but intentionally
                    // omit the stored custom metadata. Read the record by its
                    // scoped server ID before reconstructing neutral evidence.
                    let get = wire::GetMemoryRequest {
                        context: Some(context(&namespace)),
                        selector: Some(wire::get_memory_request::Selector::MemoryId(hit.memory_id)),
                    };
                    let request = match self.request(get) { Ok(request) => request, Err(error) => return failed(&error.code, false) };
                    let record = match client.get_memory(request).await {
                        Ok(reply) => reply.into_inner().memory,
                        Err(error) => return failed_status(error),
                    };
                    let Some(record) = record else { return failed("missing_memory_record", false); };
                    let Some(key_json) = string_field(record.metadata.as_ref(), "key_json") else { return failed("missing_memory_metadata", false); };
                    let Some(sources_json) = string_field(record.metadata.as_ref(), "sources_json") else { return failed("missing_memory_metadata", false); };
                    let Ok(key) = serde_json::from_str::<MemoryKey>(key_json) else { return failed("invalid_memory_metadata", false); };
                    let Ok(sources) = serde_json::from_str::<Vec<Evidence>>(sources_json) else { return failed("invalid_memory_metadata", false); };
                    if key.chat != query.chat || key.subject != query.subject { return failed("memory_scope_violation", false); }
                    if !query.layers.is_empty() && !query.layers.contains(&key.layer) { continue; }
                    hits.push(MemoryHit { key, text: hit.content, score: hit.score, sources });
                }
                PortReply::Ok(hits)
            }
            Err(error) => failed_status(error),
        }
    }

    async fn remove(&self, key: &MemoryKey) -> PortReply<()> {
        let namespace = Self::namespace(&key.chat, &key.subject);
        let mut client = wire::memory_service_client::MemoryServiceClient::new(self.channel.clone());
        let request = wire::DeleteMemoryRequest {
            context: Some(context(&namespace)),
            selector: Some(wire::delete_memory_request::Selector::ExternalId(Self::external_id(key))),
            reason: "memory_forget".into(),
        };
        let request = match self.request(request) { Ok(request) => request, Err(error) => return failed(&error.code, false) };
        match client.delete_memory(request).await {
            Ok(_) => PortReply::Ok(()), Err(error) => failed_status(error),
        }
    }
}

impl MemoryPort for AmemorixPort {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> MemoryCapabilities { self.capabilities.lock().unwrap().clone() }
    fn upsert<'a>(&'a self, entry: MemoryEntry) -> BoxFuture<'a, PortReply<()>> { Box::pin(self.write(entry)) }
    fn forget<'a>(&'a self, key: &'a MemoryKey) -> BoxFuture<'a, PortReply<()>> { Box::pin(self.remove(key)) }
    fn retrieve<'a>(&'a self, query: RetrievalQuery) -> BoxFuture<'a, PortReply<Vec<MemoryHit>>> { Box::pin(self.read(query)) }
    fn reset<'a>(&'a self, _: MemoryScope) -> BoxFuture<'a, PortReply<()>> {
        Box::pin(async { failed("memory_scope_reset_unimplemented", false) })
    }
}

fn context(namespace: &str) -> wire::RequestContext {
    wire::RequestContext { namespace_id: namespace.into(), ..Default::default() }
}
fn timestamp(seconds: f64) -> Option<Timestamp> {
    if !seconds.is_finite() || seconds <= 0. { return None; }
    let whole = seconds.floor();
    if whole < i64::MIN as f64 || whole > i64::MAX as f64 { return None; }
    Some(Timestamp { seconds: whole as i64, nanos: ((seconds - whole) * 1e9).round() as i32 })
}
fn proto_string(value: String) -> ProtoValue { ProtoValue { kind: Some(Kind::StringValue(value)) } }
fn proto_number(value: f64) -> ProtoValue { ProtoValue { kind: Some(Kind::NumberValue(value)) } }
fn string_field<'a>(value: Option<&'a Struct>, field: &str) -> Option<&'a str> {
    match value?.fields.get(field)?.kind.as_ref()? { Kind::StringValue(s) => Some(s), _ => None }
}
fn failed<T>(code: &str, uncertain: bool) -> PortReply<T> {
    PortReply::Failed { code: code.into(), uncertain }
}
fn failed_status<T>(status: Status) -> PortReply<T> {
    failed(&format!("memory_grpc_{}", status.code()), matches!(status.code(), Code::DeadlineExceeded | Code::Unknown))
}
