//! Availability of the three faces that reach outside the process.
//!
//! A manifest or a successful construction says nothing about whether a face
//! can actually carry traffic, so the runtime reports the route it really
//! uses and a separate health verdict.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceKind {
    Chat,
    Model,
    Db,
}

impl InterfaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Model => "model",
            Self::Db => "db",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceRoute {
    Plugin,
    BuiltIn,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceHealth {
    Ready,
    Degraded,
    Unknown,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceScope {
    Full,
    DiagnosticOnly,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceAvailability {
    pub kind: InterfaceKind,
    pub route: InterfaceRoute,
    pub health: InterfaceHealth,
    pub reason: Option<String>,
    pub observed_at: f64,
    pub generation: String,
    pub fallback_from: Option<InterfaceRoute>,
    pub scope: InterfaceScope,
}

impl InterfaceAvailability {
    pub fn new(
        kind: InterfaceKind,
        route: InterfaceRoute,
        health: InterfaceHealth,
        scope: InterfaceScope,
        observed_at: f64,
        generation: String,
    ) -> Self {
        Self { kind, route, health, reason: None, observed_at, generation, fallback_from: None, scope }
    }
}
