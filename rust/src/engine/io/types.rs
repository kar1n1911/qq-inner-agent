//! Serializable values shared by the six ports (plan §12, X1).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatKind {
    Group,
    Private,
}

impl ChatKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Private => "private",
        }
    }
}

/// Always three components; the opaque ID may itself contain colons.
/// P2c must migrate `outbound_message_facts`, `message_identity_audit`, and both
/// source/destination addresses in the two dual-address media tables as well as
/// other chat-bearing tables. This type does not perform that migration.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ChatAddress {
    pub kind: ChatKind,
    pub port: String,
    pub id: String,
}

impl ChatAddress {
    pub fn new(
        kind: ChatKind,
        port: impl Into<String>,
        id: impl Into<String>,
    ) -> Result<Self, IoError> {
        let address = Self {
            kind,
            port: port.into(),
            id: id.into(),
        };
        if address.port.is_empty() || address.port.contains(':') || address.id.is_empty() {
            return Err(IoError::new("invalid_chat_address"));
        }
        Ok(address)
    }
}

impl FromStr for ChatAddress {
    type Err = IoError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.splitn(3, ':');
        let kind = match parts.next() {
            Some("group") => ChatKind::Group,
            Some("private") => ChatKind::Private,
            _ => return Err(IoError::new("invalid_chat_address")),
        };
        let port = parts
            .next()
            .ok_or_else(|| IoError::new("invalid_chat_address"))?;
        let id = parts
            .next()
            .ok_or_else(|| IoError::new("invalid_chat_address"))?;
        Self::new(kind, port, id)
    }
}

impl fmt::Display for ChatAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.kind.as_str(), self.port, self.id)
    }
}

impl TryFrom<String> for ChatAddress {
    type Error = IoError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ChatAddress> for String {
    fn from(value: ChatAddress) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Incoming {
    pub port: String,
    pub chat: ChatAddress,
    /// Protocol identifier only. Storage assigns the monotonic message_key.
    pub message_id: String,
    pub self_id: String,
    /// Protocol server timestamp. Invalid/missing values are rejected at ingress.
    pub ts: f64,
    /// Local receive time; never substituted for `ts`.
    pub received_at: f64,
    pub sender: Sender,
    pub segments: Vec<Segment>,
    pub self_message: bool,
    /// Adapter/audit use only. The kernel must not interpret this protocol JSON.
    pub raw: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PortEvent {
    Incoming(Incoming),
    Notice(Notice),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub kind: NoticeKind,
    pub chat: Option<ChatAddress>,
    pub actor: Option<String>,
    pub self_id: Option<String>,
    /// Protocol timestamp, never the local receive time.
    pub at: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum NoticeKind {
    GroupJoined,
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sender {
    pub id: String,
    pub name: String,
    pub card: Option<String>,
    pub role: Option<String>,
    pub title: Option<String>,
    pub is_robot: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MediaSrc {
    Url(String),
    LocalPath(String),
    Opaque(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    pub source: MediaSrc,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForwardNode {
    pub sender: Sender,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Segment {
    Text(String),
    Mention {
        id: String,
    },
    Reply {
        id: String,
    },
    Image {
        src: MediaSrc,
        summary: Option<String>,
    },
    Face {
        id: String,
    },
    Forward {
        id: Option<String>,
        nodes: Vec<ForwardNode>,
    },
    Card {
        title: Option<String>,
        desc: Option<String>,
        url: Option<String>,
    },
    Unknown {
        kind: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Send {
        chat: ChatAddress,
        segments: Vec<Segment>,
        reply_to: Option<String>,
    },
    SendMedia {
        chat: ChatAddress,
        media: MediaRef,
    },
    /// ACK means the target port confirmed the protocol recall action.
    /// A timeout/disconnect is `Failed { uncertain: true }`, never success.
    Recall {
        port: String,
        message_id: String,
    },
    FetchHistory {
        chat: ChatAddress,
        count: usize,
        before: Option<String>,
    },
    FetchForward {
        port: String,
        id: String,
    },
    FetchMembers {
        chat: ChatAddress,
    },
    FetchGroupInfo {
        chat: ChatAddress,
    },
    FetchFriendList {
        port: String,
    },
    FetchGroupList {
        port: String,
    },
    LoginInfo {
        port: String,
    },
    Health {
        port: String,
    },
    SetGroupCard {
        chat: ChatAddress,
        card: String,
    },
    SetProfile {
        port: String,
        nickname: Option<String>,
        signature: Option<String>,
    },
    SetAvatar {
        port: String,
        media: MediaRef,
    },
}

impl Command {
    pub fn port(&self) -> &str {
        match self {
            Self::Send { chat, .. }
            | Self::SendMedia { chat, .. }
            | Self::FetchHistory { chat, .. }
            | Self::FetchMembers { chat }
            | Self::FetchGroupInfo { chat }
            | Self::SetGroupCard { chat, .. } => &chat.port,
            Self::Recall { port, .. }
            | Self::FetchForward { port, .. }
            | Self::FetchFriendList { port }
            | Self::FetchGroupList { port }
            | Self::LoginInfo { port }
            | Self::Health { port }
            | Self::SetProfile { port, .. }
            | Self::SetAvatar { port, .. } => port,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupInfo {
    pub chat: ChatAddress,
    pub name: String,
    pub member_count: Option<u64>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub chat: ChatAddress,
    pub sender: Sender,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContactInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoginInfo {
    pub self_id: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthInfo {
    pub connected: bool,
    pub online: bool,
}

/// Success only. Unsupported/degraded/failed live in `PortReply<T>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CommandResult {
    Sent { message_id: String },
    History(Vec<Incoming>),
    Forward(Vec<ForwardNode>),
    Members(Vec<MemberInfo>),
    GroupInfo(GroupInfo),
    Friends(Vec<ContactInfo>),
    Groups(Vec<GroupInfo>),
    LoginInfo(LoginInfo),
    Health(HealthInfo),
    Ok,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PortKind {
    Chat,
    Model,
    Db,
    Memory,
    External,
    Persona,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransportKind {
    InProcess,
    Sidecar,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionReq {
    pub major: u16,
    pub min_minor: u16,
    pub max_minor: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostPermission {
    Http { origin: String },
    Spawn { executable: String },
    ReadFile { root: String },
    Log { max_bytes: usize },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortManifest {
    pub name: String,
    pub kind: PortKind,
    pub transport: TransportKind,
    pub core_api_version: VersionReq,
    /// Declared host activity for visibility and audit; an empty list is not a denial rule.
    pub host_permissions: Vec<HostPermission>,
    /// Declarative runtime services used by an optional capability.
    #[serde(default)]
    pub dependencies: Vec<PortDependency>,
    #[serde(default)]
    pub config_fields: Vec<PortConfigField>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortDependency {
    pub role: String,
    pub needed_for: String,
    pub required: bool,
}

/// A declarative configuration input, separate from host authorization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortConfigField {
    pub key: String,
    pub label: String,
    pub required: bool,
    pub secret: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortState {
    pub connected: bool,
    pub online: bool,
    pub self_id: Option<String>,
    pub degraded: Option<String>,
}

impl PortState {
    pub fn available() -> Self {
        Self { connected: true, online: true, self_id: None, degraded: None }
    }

    pub fn disconnected() -> Self {
        Self { connected: false, online: false, self_id: None, degraded: None }
    }

    pub fn is_available(&self) -> bool { self.connected && self.online }

    pub fn label(&self) -> &'static str {
        if !self.connected { "disconnected" } else if !self.online { "degraded" } else { "available" }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Capability {
    Mention,
    Reply,
    Media,
    Forward,
    GroupCard,
    Profile,
    Avatar,
    Signature,
    MemberTitle,
    MemberRole,
    IsRobot,
    History,
    GroupInfo,
    Members,
    JsonMode,
    ThinkingField,
    TruncationSignal,
    ListModels,
    Custom(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub mention: bool,
    pub reply: bool,
    pub media: bool,
    pub notices: bool,
    pub forward: bool,
    pub group_card: bool,
    pub profile: bool,
    pub avatar: bool,
    pub signature: bool,
    pub member_title: bool,
    pub member_role: bool,
    pub is_robot: bool,
    pub history: bool,
    pub group_info: bool,
    pub members: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub json_mode: bool,
    pub thinking_field: bool,
    pub truncation_signal: bool,
    pub list_models: bool,
    /// None means unknown; callers must not assume a window size.
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MemoryCapabilities {
    pub text_retrieval: bool,
    pub embeddings: bool,
    pub relation_extraction: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PortReply<T> {
    Ok(T),
    Unsupported { capability: Capability },
    Degraded { result: T, missing: Vec<Capability> },
    Failed { code: String, uncertain: bool },
}

impl<T> PortReply<T> {
    pub fn into_result(self) -> Result<T, IoError> {
        match self {
            Self::Ok(value) => Ok(value),
            Self::Unsupported { .. } => Err(IoError::new("capability_unsupported")),
            Self::Degraded { .. } => Err(IoError::new("port_reply_degraded")),
            Self::Failed { code, .. } => Err(IoError::new(code)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IoError {
    pub code: String,
}

impl IoError {
    pub fn new(code: impl Into<String>) -> Self {
        Self { code: code.into() }
    }
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for IoError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OutputFormat {
    Json,
    Text,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelRequest<'a> {
    pub instance: &'a str,
    pub system: &'a str,
    /// Neutral input: either a messages array or {messages, tools}. Message
    /// roles are user/assistant/tool; assistant.tool_calls holds ModelToolCall,
    /// tool messages carry ModelToolResult fields, and tools holds
    /// ModelToolDefinition. Adapters validate and translate these values.
    pub payload: &'a Value,
    pub format: OutputFormat,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelReply {
    pub text: String,
    /// An empty text is successful only when at least one structured call exists.
    #[serde(default)]
    pub tool_calls: Vec<ModelToolCall>,
}

/// Neutral schema for payload.tools entries and tool calls. `input_schema`
/// and `arguments` are JSON values, not provider-specific wire blocks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelToolCall {
    /// Some protocols identify a call only by tool name.
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

/// In payload.messages, a tool result uses role=tool and this shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelToolResult {
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModelDelta {
    Text(String),
    ToolCallStart { id: Option<String>, name: String },
    ToolCallArguments { id: Option<String>, fragment: String },
    ToolCallEnd { id: Option<String> },
    Done,
}

pub type ModelStream = tokio::sync::mpsc::Receiver<PortReply<ModelDelta>>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum MemorySubject {
    Group,
    Person(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum MemoryLayer {
    ShortTerm,
    LongTerm,
    Trait,
    Expression,
    Backstory,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct MemoryKey {
    pub chat: ChatAddress,
    pub subject: MemorySubject,
    pub layer: MemoryLayer,
    pub slot: String,
}

/// Internal message_key is the locator; protocol message_id is audit metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub chat: ChatAddress,
    pub message_key: String,
    pub message_id: Option<String>,
    pub ts: f64,
    pub received_at: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: MemoryKey,
    pub text: String,
    pub sources: Vec<Evidence>,
    pub confidence: f64,
    pub importance: f64,
    pub expires: Option<f64>,
    pub keywords: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryHit {
    pub key: MemoryKey,
    pub text: String,
    pub score: f64,
    pub sources: Vec<Evidence>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievalQuery {
    pub chat: ChatAddress,
    pub subject: MemorySubject,
    pub layers: Vec<MemoryLayer>,
    pub limit: usize,
    /// Optional semantic/text query; `None` requests scope-only recall.
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryScope {
    pub chat: ChatAddress,
    pub subject: MemorySubject,
    pub layers: Vec<MemoryLayer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FetchRequest {
    pub source: String,
    pub timeout_ms: u64,
    pub max_bytes: u64,
    pub max_redirects: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedItem {
    pub title: String,
    pub url: String,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PersonaSource {
    /// Only a human-confirmed import may set this initial persona; it is never
    /// part of the kernel's non-overridable base prompt.
    pub initial_persona: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub personality: Option<String>,
    pub scenario: Option<String>,
    pub examples: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PersonaCapabilities {
    pub supported: Vec<String>,
    pub unsupported: Vec<String>,
}
