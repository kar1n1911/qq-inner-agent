#[path = "../../../../../mod/null/adapter.rs"]
pub mod null;
#[path = "../../../../../mod/onebot/adapter.rs"]
pub mod onebot;
#[path = "../../../../../mod/a_memorix/adapter.rs"]
pub mod amemorix;
#[path = "../../../../../mod/claude/adapter.rs"]
pub mod claude;
#[path = "../../../../../mod/model_http.rs"]
pub mod model_http;
#[path = "../../../../../mod/ollama/adapter.rs"]
pub mod ollama;
#[path = "../../../../../mod/openai/adapter.rs"]
pub mod openai;
#[path = "../../../../../mod/sillytavern/adapter.rs"]
pub mod sillytavern;

pub(super) use super::{ports, types};
