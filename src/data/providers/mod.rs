mod common;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

pub use common::{build_http_client, home_dir, parse_json_response, percent_to_u8, send_request};
pub(crate) use common::{fetch_json_response, run_provider_warmup};

/// A live model with its current quota status.
#[derive(Debug, Clone)]
pub struct LiveModel {
    pub name: String,
    pub quota_percent: Option<u8>,
    pub quota_resets_at: Option<chrono::DateTime<chrono::Utc>>,
}
