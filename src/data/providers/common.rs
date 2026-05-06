use crate::data::warmup;
use anyhow::{Context, Result};
use reqwest::{Client, RequestBuilder, Response};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
// The current providers only converge on IO/transport scaffolding. Their
// credential lookup and payload normalization rules diverge immediately, so we
// intentionally stop at helper extraction instead of inventing a provider trait.
/// Build an HTTP client with the given timeout.
pub fn build_http_client(timeout_secs: u64) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("failed to build HTTP client")
}
/// Send a request and return the response, checking for HTTP errors.
pub async fn send_request(request: RequestBuilder, provider: &str) -> Result<Response> {
    request
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("{provider} request failed"))
}
/// Parse a JSON response body, returning a descriptive error on failure.
pub async fn parse_json_response(response: Response, provider: &str) -> Result<Value> {
    response
        .json::<Value>()
        .await
        .with_context(|| format!("{provider} response was not valid JSON"))
}
/// Send a request and parse the JSON response with provider-specific context.
pub(crate) async fn fetch_json_response(request: RequestBuilder, provider: &str) -> Result<Value> {
    parse_json_response(send_request(request, provider).await?, provider).await
}
/// Run a provider CLI briefly so later quota/status calls hit a warmed-up auth state.
pub(crate) fn run_provider_warmup(
    provider: &str,
    program: &str,
    args: &[&str],
    script: &str,
    env: &[(&str, &str)],
) -> Result<()> {
    warmup::run(warmup::WarmupSpec {
        program,
        args,
        script,
        env,
        settle_timeout: Duration::from_secs(2),
    })
    .with_context(|| format!("{provider} dummy invoke failed"))
}
/// Convert a percentage value to a u8 clamped to 0-100.
pub fn percent_to_u8(value: f64) -> u8 {
    value.round().clamp(0.0, 100.0) as u8
}
/// Return the user's home directory.
pub fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).to_path_buf())
}
#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
