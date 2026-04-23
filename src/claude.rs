use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{process::Command, time::Duration};

const BASE_URL: &str = "https://api.anthropic.com";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const BETA_HEADER: &str = "oauth-2025-04-20";
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct LiveModel {
    pub name: String,
    pub quota_percent: Option<u8>,
}

pub fn load_live_models() -> Result<Vec<LiveModel>> {
    dummy_invoke()?;
    let token = resolve_access_token()?;
    let org_id = resolve_org_id()?;
    let payload = fetch_usage_payload(&token, &org_id)?;

    let mut models = Vec::new();
    let object = payload
        .as_object()
        .context("Claude usage response was not an object")?;

    for (name, value) in object {
        let Some(utilization) = value.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        let remaining = (100.0 - utilization).round().clamp(0.0, 100.0) as u8;
        models.push(LiveModel {
            name: name.to_ascii_lowercase(),
            quota_percent: Some(remaining),
        });
    }

    if models.is_empty() {
        bail!("Claude usage response had no utilization windows");
    }

    Ok(models)
}

fn dummy_invoke() -> Result<()> {
    let output = Command::new("claude")
        .args(["auth", "status"])
        .env("CLAUDE_CODE_DISABLE_LEGACY_MODEL_REMAP", "1")
        .output()
        .context("failed to run Claude dummy invoke")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = first_line(&stderr)
            .or_else(|| first_line(&stdout))
            .unwrap_or("unknown error");
        bail!("Claude dummy invoke failed: {detail}");
    }
    Ok(())
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

fn fetch_usage_payload(token: &str, org_id: &str) -> Result<Value> {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to build Claude HTTP client")?;

    client
        .get(format!("{BASE_URL}/api/oauth/usage"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "claude-code/2.1.118")
        .header("x-organization-uuid", org_id)
        .header("anthropic-beta", BETA_HEADER)
        .header("anthropic-version", "2023-06-01")
        .send()
        .and_then(|response| response.error_for_status())
        .context("Claude usage request failed")?
        .json::<Value>()
        .context("Claude usage response was not valid JSON")
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}
