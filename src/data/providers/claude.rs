use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::process::Command;

use super::{LiveModel, build_http_client, fetch_json_response, run_provider_warmup};

const BASE_URL: &str = "https://api.anthropic.com";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const BETA_HEADER: &str = "oauth-2025-04-20";

pub async fn load_live_models_async() -> Result<Vec<LiveModel>> {
    dummy_invoke()?;
    let token = resolve_access_token()?;
    let org_id = resolve_org_id()?;
    let payload = fetch_usage_payload(&token, &org_id).await?;
    live_models_from_payload(&payload)
}

fn live_models_from_payload(payload: &Value) -> Result<Vec<LiveModel>> {
    let object = payload
        .as_object()
        .context("Claude usage response was not an object")?;

    let mut min_remaining: Option<u8> = None;
    let mut earliest_reset: Option<DateTime<Utc>> = None;

    for (_name, value) in object {
        let Some(obj) = value.as_object() else {
            continue;
        };
        // Skip billing caps (extra_usage) — they have a "currency" field
        if obj.contains_key("currency") {
            continue;
        }
        let Some(utilization) = obj.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        let remaining = (100.0 - utilization).round().clamp(0.0, 100.0) as u8;
        min_remaining = Some(min_remaining.map_or(remaining, |prev| prev.min(remaining)));

        if let Some(ts) = obj.get("resets_at").and_then(Value::as_str)
            && let Ok(dt) = DateTime::parse_from_rfc3339(ts)
        {
            let dt_utc = dt.with_timezone(&Utc);
            earliest_reset = Some(earliest_reset.map_or(dt_utc, |earliest| earliest.min(dt_utc)));
        }
    }

    let Some(remaining) = min_remaining else {
        bail!("Claude usage response had no utilization windows");
    };

    Ok(vec![LiveModel {
        name: "claude-shared".to_string(),
        quota_percent: Some(remaining),
        quota_resets_at: earliest_reset,
    }])
}

fn dummy_invoke() -> Result<()> {
    run_provider_warmup(
        "Claude",
        "claude",
        &["--dangerously-skip-permissions"],
        "/stats\n/exit\n",
        &[("CLAUDE_CODE_DISABLE_LEGACY_MODEL_REMAP", "1")],
    )
}

fn resolve_access_token() -> Result<String> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .context("failed to read Claude keychain credentials")?;
    if !output.status.success() {
        bail!("Claude keychain credential lookup failed");
    }

    let payload: Value = serde_json::from_slice(&output.stdout)
        .context("Claude keychain payload was not valid JSON")?;
    payload
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .and_then(|oauth| oauth.get("accessToken"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("Claude keychain payload did not include accessToken")
}

fn resolve_org_id() -> Result<String> {
    let output = Command::new("claude")
        .args(["auth", "status"])
        .env("CLAUDE_CODE_DISABLE_LEGACY_MODEL_REMAP", "1")
        .output()
        .context("failed to run `claude auth status`")?;
    if !output.status.success() {
        bail!("`claude auth status` failed");
    }

    let payload: Value =
        serde_json::from_slice(&output.stdout).context("Claude auth status was not valid JSON")?;
    payload
        .get("orgId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .context("Claude auth status did not include orgId")
}

async fn fetch_usage_payload(token: &str, org_id: &str) -> Result<Value> {
    let client = build_http_client(5)?;

    let request = client
        .get(format!("{BASE_URL}/api/oauth/usage"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "claude-code/2.1.118")
        .header("x-organization-uuid", org_id)
        .header("anthropic-beta", BETA_HEADER)
        .header("anthropic-version", "2023-06-01");

    fetch_json_response(request, "Claude").await
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
