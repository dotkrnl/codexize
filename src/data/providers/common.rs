use crate::data::warmup;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::{Client, RequestBuilder, Response};
use serde_json::{Map, Value};
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
/// Parse common absolute reset timestamp shapes from provider JSON.
pub(crate) fn parse_reset_time(value: Option<&Value>) -> Option<DateTime<Utc>> {
    match value? {
        Value::String(raw) => parse_reset_string(raw),
        Value::Number(number) => number.as_f64().and_then(parse_epoch_timestamp),
        _ => None,
    }
}
/// Extract a reset timestamp from a provider quota/window object.
pub(crate) fn reset_time_from_object(object: &Map<String, Value>) -> Option<DateTime<Utc>> {
    const ABSOLUTE_KEYS: &[&str] = &[
        "resets_at",
        "resetsAt",
        "reset_at",
        "resetAt",
        "reset_time",
        "resetTime",
        "next_reset_at",
        "nextResetAt",
        "window_reset_at",
        "windowResetAt",
    ];
    for key in ABSOLUTE_KEYS {
        if let Some(parsed) = parse_reset_time(object.get(*key)) {
            return Some(parsed);
        }
    }
    const RELATIVE_KEYS: &[&str] = &[
        "reset_after_seconds",
        "resetAfterSeconds",
        "seconds_until_reset",
        "secondsUntilReset",
        "retry_after_seconds",
        "retryAfterSeconds",
    ];
    for key in RELATIVE_KEYS {
        if let Some(seconds) = object.get(*key).and_then(number_or_numeric_string) {
            let whole_seconds = seconds.max(0.0).round() as i64;
            return Some(Utc::now() + ChronoDuration::seconds(whole_seconds));
        }
    }
    None
}
fn parse_reset_string(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| raw.parse::<f64>().ok().and_then(parse_epoch_timestamp))
}
fn parse_epoch_timestamp(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let seconds = if value > 1_000_000_000_000.0 {
        value / 1000.0
    } else {
        value
    };
    let mut secs = seconds.trunc() as i64;
    let mut nanos = ((seconds.fract()) * 1_000_000_000.0).round() as u32;
    if nanos >= 1_000_000_000 {
        secs = secs.checked_add(1)?;
        nanos -= 1_000_000_000;
    }
    DateTime::from_timestamp(secs, nanos)
}
fn number_or_numeric_string(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.parse().ok(),
        _ => None,
    }
}
/// Return the user's home directory.
pub fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).to_path_buf())
}
#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
