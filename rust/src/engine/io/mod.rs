//! Neutral I/O aggregation. Legacy runtime call sites remain untouched at P1.

pub mod adapters;
pub mod factory;
pub mod ports;
pub mod plugin_list;
pub mod registry;
pub mod router;
pub mod types;

use std::sync::Arc;

pub use ports::{ChatPort, DbPort, ExternalPort, MemoryPort, ModelPort, PersonaPort};
pub use registry::PortRegistry;
pub use router::ChatRouter;
pub use types::*;

use adapters::null::{NullChat, NullDb, NullExternal, NullMemory, NullModel, NullPersona};

/// The six faces share one entry. P1 selects one instance per non-chat face;
/// multi-instance failover/fusion/fan-out is reserved for later phases.
pub struct Io<D: DbPort> {
    registry: PortRegistry<D>,
    model: Arc<dyn ModelPort>,
    memory: Arc<dyn MemoryPort>,
    external: Arc<dyn ExternalPort>,
    persona: Arc<dyn PersonaPort>,
}

impl<D: DbPort> Io<D> {
    /// Chat, model, memory, and the in-process DB are mandatory. External and
    /// persona are optional, but absence installs explicit-failure adapters.
    pub fn compose(
        registry: PortRegistry<D>,
        model_name: &str,
        memory_name: &str,
        external_name: Option<&str>,
        persona_name: Option<&str>,
    ) -> Result<Self, IoError> {
        if registry.chat.is_empty() {
            return Err(IoError::new("missing_chat_port"));
        }
        let model = registry
            .model(model_name)
            .cloned()
            .ok_or_else(|| IoError::new("missing_model_port"))?;
        let memory = registry
            .memory(memory_name)
            .cloned()
            .ok_or_else(|| IoError::new("missing_memory_port"))?;
        let external = match external_name {
            Some(name) => registry
                .external(name)
                .cloned()
                .ok_or_else(|| IoError::new("unknown_port"))?,
            None => Arc::new(NullExternal::new("null-external")),
        };
        let persona = match persona_name {
            Some(name) => registry
                .persona(name)
                .cloned()
                .ok_or_else(|| IoError::new("unknown_port"))?,
            None => Arc::new(NullPersona::new("null-persona")),
        };
        Ok(Self {
            registry,
            model,
            memory,
            external,
            persona,
        })
    }

    pub fn chat(&self) -> &ChatRouter {
        &self.registry.chat
    }
    pub fn model(&self) -> &dyn ModelPort {
        self.model.as_ref()
    }
    pub fn db(&self) -> &D {
        self.registry.db.as_ref()
    }
    pub fn memory(&self) -> &dyn MemoryPort {
        self.memory.as_ref()
    }
    pub fn external(&self) -> &dyn ExternalPort {
        self.external.as_ref()
    }
    pub fn persona(&self) -> &dyn PersonaPort {
        self.persona.as_ref()
    }
}

impl Io<NullDb> {
    /// All six faces are present, and every attempted operation fails visibly.
    pub fn null() -> Self {
        let mut registry = PortRegistry::new(Arc::new(NullDb));
        registry
            .register_chat(Arc::new(NullChat::new("null-chat")))
            .expect("valid null chat manifest");
        registry
            .register_model(Arc::new(NullModel::new("null-model")))
            .expect("valid null model manifest");
        registry
            .register_memory(Arc::new(NullMemory::new("null-memory")))
            .expect("valid null memory manifest");
        Self::compose(registry, "null-model", "null-memory", None, None)
            .expect("complete null registry")
    }
}
