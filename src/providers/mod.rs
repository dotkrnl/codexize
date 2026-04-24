use anyhow::{Context, Result};
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod kimi;

/// A live model with its current quota status.
#[derive(Debug, Clone)]
pub struct LiveModel {
    pub name: String,
    pub quota_percent: Option<u8>,
}

/// Build an HTTP client with the given timeout.
pub fn build_http_client(timeout_secs: u64) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("failed to build HTTP client")
}

/// Send a request and return the response, checking for HTTP errors.
pub fn send_request(
    request: reqwest::blocking::RequestBuilder,
    provider: &str,
) -> Result<Response> {
    request
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("{provider} request failed"))
}

/// Parse a JSON response body, returning a descriptive error on failure.
pub fn parse_json_response(response: Response, provider: &str) -> Result<Value> {
    response
        .json::<Value>()
        .with_context(|| format!("{provider} response was not valid JSON"))
}

/// Convert a percentage value to a u8 clamped to 0–100.
pub fn percent_to_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}

/// Return the user's home directory.
pub fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).to_path_buf())
}
