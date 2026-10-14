#[path = "../../../../../plugins/null/adapter.rs"]
pub mod null;
#[path = "../../../../../plugins/onebot/adapter.rs"]
pub mod onebot;
#[path = "../../../../../plugins/a_memorix/adapter.rs"]
pub mod amemorix;
#[path = "../../../../../plugins/claude/adapter.rs"]
pub mod claude;
#[path = "../../../../../plugins/model_http.rs"]
pub mod model_http;
#[path = "../../../../../plugins/ollama/adapter.rs"]
pub mod ollama;
#[path = "../../../../../plugins/openai/adapter.rs"]
pub mod openai;
#[path = "../../../../../plugins/sillytavern/adapter.rs"]
pub mod sillytavern;

pub(super) use super::{ports, types};
