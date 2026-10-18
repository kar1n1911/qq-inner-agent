//! 示例插件源码——**不参与构建**，仅用于教程展示。
//!
//! 这份文件不接入 `engine/io/adapters/mod.rs`，因此不会被编译，也不会进入
//! 生产二进制。它的作用是让读者看到一个 ChatPort 插件的完整形状：清单、
//! 诚实的能力声明、类型化状态、命令路径、以及运行时可 attach 的 sink。
//!
//! 它不与任何真实协议通信。`feed` 用来把一个"对端发来的消息"直接投进 sink，
//! 便于测试或演示观察运行时如何消费入站事件。

use super::{ports::ChatPort, types::*};
use futures_util::future::BoxFuture;
use std::sync::Mutex;
use tokio::sync::mpsc;

pub struct EchoChat {
    manifest: PortManifest,
    state: PortState,
    sink: Mutex<Option<mpsc::Sender<PortEvent>>>,
    sent: Mutex<Vec<(String, String)>>,
    received: Mutex<u64>,
}

impl EchoChat {
    pub fn new(name: &str) -> Result<Self, IoError> {
        if name.is_empty() || name.contains(':') {
            return Err(IoError::new("invalid_port_manifest"));
        }
        Ok(Self {
            manifest: PortManifest {
                name: name.into(),
                kind: PortKind::Chat,
                transport: TransportKind::InProcess,
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![],
                dependencies: vec![],
                config_fields: vec![],
            },
            state: PortState {
                connected: true,
                online: true,
                self_id: Some("example".into()),
                degraded: None,
            },
            sink: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
            received: Mutex::new(0),
        })
    }

    /// 把一条文本当作"对端发来的入站消息"投进已 attach 的 sink。
    pub fn feed(&self, chat: ChatAddress, text: &str, received_at: f64) -> Result<(), IoError> {
        let guard = self.sink.lock().map_err(|_| IoError::new("sink_poisoned"))?;
        let sink = guard.as_ref().ok_or_else(|| IoError::new("sink_not_attached"))?;
        let mut count = self.received.lock().map_err(|_| IoError::new("counter_poisoned"))?;
        *count += 1;
        let incoming = Incoming {
            port: self.manifest.name.clone(),
            chat,
            message_id: format!("example-{}", *count),
            self_id: "example".into(),
            ts: received_at,
            received_at,
            sender: Sender {
                id: "example-peer".into(),
                name: "示例对端".into(),
                card: None,
                role: None,
                title: None,
                is_robot: None,
            },
            segments: vec![Segment::Text(text.into())],
            reply: ReplyObservation::NoReply,
            self_message: false,
            raw: None,
        };
        sink.try_send(PortEvent::Incoming(Box::new(incoming)))
            .map_err(|_| IoError::new("incoming_sink_unavailable"))
    }

    /// 已发送的文本，形如 `(chat, text)`。
    pub fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().map(|rows| rows.clone()).unwrap_or_default()
    }
}

impl ChatPort for EchoChat {
    fn manifest(&self) -> &PortManifest {
        &self.manifest
    }

    fn capabilities(&self) -> Capabilities {
        // 只发文本：没有 media、notices、history。声明永远不跑到实现前面。
        Capabilities { mention: true, reply: true, ..Capabilities::default() }
    }

    fn state(&self) -> PortState {
        self.state.clone()
    }

    fn deliver<'a>(&'a self, command: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(async move {
            match command {
                Command::Send { chat, segments, .. } => {
                    let text: String = segments
                        .into_iter()
                        .filter_map(|segment| match segment {
                            Segment::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect();
                    let Ok(mut sent) = self.sent.lock() else {
                        return PortReply::Failed {
                            code: "state_poisoned".into(),
                            uncertain: false,
                        };
                    };
                    let message_id = format!("echo-{}", sent.len() + 1);
                    sent.push((format!("{}:{}", chat.kind.as_str(), chat.id), text));
                    PortReply::Ok(CommandResult::Sent { message_id })
                }
                Command::Health { .. } => PortReply::Ok(CommandResult::Health(HealthInfo {
                    connected: self.state.connected,
                    online: self.state.online,
                })),
                // 没实现的命令必须说"不支持"，不是"失败"。
                _ => PortReply::Unsupported {
                    capability: Capability::Custom("example_echo_command".into()),
                },
            }
        })
    }

    fn attach(&self, sink: mpsc::Sender<PortEvent>) -> Result<(), IoError> {
        let mut guard = self.sink.lock().map_err(|_| IoError::new("sink_poisoned"))?;
        if guard.is_some() {
            return Err(IoError::new("sink_already_attached"));
        }
        *guard = Some(sink);
        Ok(())
    }
}
