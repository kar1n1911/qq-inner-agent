//! Chat routing by the explicit source/target port, without a default account.

use super::{ports::ChatPort, types::*};
use std::{collections::HashMap, sync::Arc};

#[derive(Default)]
pub struct ChatRouter {
    ports: HashMap<String, Arc<dyn ChatPort>>,
}

impl ChatRouter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, port: Arc<dyn ChatPort>) -> Result<(), IoError> {
        let manifest = port.manifest();
        if manifest.kind != PortKind::Chat
            || manifest.name.is_empty()
            || manifest.name.contains(':')
        {
            return Err(IoError::new("invalid_port_manifest"));
        }
        if self.ports.contains_key(&manifest.name) {
            return Err(IoError::new("duplicate_port"));
        }
        self.ports.insert(manifest.name.clone(), port);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn ChatPort>> {
        self.ports.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.ports.is_empty()
    }

    pub fn capabilities_for(&self, chat: &ChatAddress) -> Result<Capabilities, IoError> {
        self.get(&chat.port)
            .map(|port| port.capabilities())
            .ok_or_else(|| IoError::new("unknown_port"))
    }

    pub async fn deliver(&self, command: Command) -> PortReply<CommandResult> {
        let Some(port) = self.get(command.port()) else {
            return PortReply::Failed {
                code: "unknown_port".into(),
                uncertain: false,
            };
        };
        port.deliver(command).await
    }
}
