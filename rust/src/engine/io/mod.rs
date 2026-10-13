//! Neutral I/O contracts. No runtime adapter is connected at P0.

pub mod ports;
pub mod registry;
pub mod router;
pub mod types;

pub use ports::{ChatPort, DbPort, ExternalPort, MemoryPort, ModelPort, PersonaPort};
pub use registry::PortRegistry;
pub use router::ChatRouter;
pub use types::*;
