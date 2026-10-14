use super::model_http::{HttpModel, Wire};
use super::types::IoError;

pub fn adapter(name: &str, base: &str, context_window_tokens: Option<u32>) -> Result<HttpModel, IoError> {
    HttpModel::new(Wire::Ollama, name, base, None, context_window_tokens)
}
