use crate::warmup;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{process::Command, time::Duration};

use super::{LiveModel, build_http_client, parse_json_response, send_request};

const BASE_URL: &str = "https://api.anthropic.com";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const BETA_HEADER: &str = "oauth-2025-04-20";

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
    warmup::run(warmup::WarmupSpec {
        program: "claude",
        args: &["--dangerously-skip-permissions"],
        script: "/stats\n/exit\n",
        env: &[("CLAUDE_CODE_DISABLE_LEGACY_MODEL_REMAP", "1")],
        settle_timeout: Duration::from_secs(2),
    })
    .context("Claude dummy invoke failed")
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
    let client = build_http_client(5)?;

    let request = client
        .get(format!("{BASE_URL}/api/oauth/usage"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", "claude-code/2.1.118")
        .header("x-organization-uuid", org_id)
        .header("anthropic-beta", BETA_HEADER)
        .header("anthropic-version", "2023-06-01");

    parse_json_response(send_request(request, "Claude")?, "Claude")
}
