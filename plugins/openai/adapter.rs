use super::model_http::{HttpModel, Wire};
use super::types::IoError;

pub fn adapter(name: &str, base: &str, api_key: String, context_window_tokens: Option<u32>) -> Result<HttpModel, IoError> {
    HttpModel::new(Wire::OpenAi, name, base, Some(api_key), context_window_tokens)
}
