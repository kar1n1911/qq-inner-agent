//! Six-face registration skeleton. Composition policy is deferred to P1.

use super::{ports::*, router::ChatRouter, types::*};
use std::{collections::HashMap, sync::Arc};

pub struct PortRegistry<D: DbPort> {
    pub chat: ChatRouter,
    pub db: Arc<D>,
    models: HashMap<String, Arc<dyn ModelPort>>,
    memories: HashMap<String, Arc<dyn MemoryPort>>,
    externals: HashMap<String, Arc<dyn ExternalPort>>,
    personas: HashMap<String, Arc<dyn PersonaPort>>,
}

impl<D: DbPort> PortRegistry<D> {
    pub fn new(db: Arc<D>) -> Self {
        Self {
            chat: ChatRouter::new(),
            db,
            models: HashMap::new(),
            memories: HashMap::new(),
            externals: HashMap::new(),
            personas: HashMap::new(),
        }
    }

    pub fn register_chat(&mut self, port: Arc<dyn ChatPort>) -> Result<(), IoError> {
        self.chat.register(port)
    }

    pub fn register_model(&mut self, port: Arc<dyn ModelPort>) -> Result<(), IoError> {
        Self::insert(
            &mut self.models,
            port.manifest().clone(),
            port,
            PortKind::Model,
        )
    }

    pub fn register_memory(&mut self, port: Arc<dyn MemoryPort>) -> Result<(), IoError> {
        Self::insert(
            &mut self.memories,
            port.manifest().clone(),
            port,
            PortKind::Memory,
        )
    }

    pub fn register_external(&mut self, port: Arc<dyn ExternalPort>) -> Result<(), IoError> {
        Self::insert(
            &mut self.externals,
            port.manifest().clone(),
            port,
            PortKind::External,
        )
    }

    pub fn register_persona(&mut self, port: Arc<dyn PersonaPort>) -> Result<(), IoError> {
        Self::insert(
            &mut self.personas,
            port.manifest().clone(),
            port,
            PortKind::Persona,
        )
    }

    fn insert<T: ?Sized>(
        map: &mut HashMap<String, Arc<T>>,
        manifest: PortManifest,
        port: Arc<T>,
        kind: PortKind,
    ) -> Result<(), IoError> {
        if manifest.kind != kind || manifest.name.is_empty() || manifest.name.contains(':') {
            return Err(IoError::new("invalid_port_manifest"));
        }
        if map.contains_key(&manifest.name) {
            return Err(IoError::new("duplicate_port"));
        }
        map.insert(manifest.name, port);
        Ok(())
    }

    pub fn model(&self, name: &str) -> Option<&Arc<dyn ModelPort>> {
        self.models.get(name)
    }
    pub fn memory(&self, name: &str) -> Option<&Arc<dyn MemoryPort>> {
        self.memories.get(name)
    }
    pub fn external(&self, name: &str) -> Option<&Arc<dyn ExternalPort>> {
        self.externals.get(name)
    }
    pub fn persona(&self, name: &str) -> Option<&Arc<dyn PersonaPort>> {
        self.personas.get(name)
    }
}
