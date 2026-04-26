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
    live_models_from_payload(&payload)
}

fn live_models_from_payload(payload: &Value) -> Result<Vec<LiveModel>> {
    let object = payload
        .as_object()
        .context("Claude usage response was not an object")?;

    let mut min_remaining: Option<u8> = None;

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
    }

    let Some(remaining) = min_remaining else {
        bail!("Claude usage response had no utilization windows");
    };

    Ok(vec![LiveModel {
        name: "claude-shared".to_string(),
        quota_percent: Some(remaining),
    }])
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn min_across_windows() {
        let payload = json!({
            "five_hour": { "utilization": 17.0, "resets_at": "2026-04-26T12:00:00Z" },
            "seven_day": { "utilization": 32.0, "resets_at": "2026-04-30T00:00:00Z" },
            "seven_day_sonnet": { "utilization": 7.0, "resets_at": "2026-04-30T00:00:00Z" }
        });
        let models = live_models_from_payload(&payload).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "claude-shared");
        // min(100-17, 100-32, 100-7) = min(83, 68, 93) = 68
        assert_eq!(models[0].quota_percent, Some(68));
    }

    #[test]
    fn extra_usage_excluded() {
        let payload = json!({
            "five_hour": { "utilization": 10.0, "resets_at": "..." },
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 5000,
                "used_credits": 3175.0,
                "utilization": 63.5,
                "currency": "USD"
            }
        });
        let models = live_models_from_payload(&payload).unwrap();
        assert_eq!(models[0].quota_percent, Some(90));
    }

    #[test]
    fn null_values_skipped() {
        let payload = json!({
            "five_hour": { "utilization": 20.0, "resets_at": "..." },
            "seven_day_opus": null,
            "iguana_necktie": null
        });
        let models = live_models_from_payload(&payload).unwrap();
        assert_eq!(models[0].quota_percent, Some(80));
    }

    #[test]
    fn all_null_returns_error() {
        let payload = json!({
            "seven_day_opus": null,
            "iguana_necktie": null
        });
        assert!(live_models_from_payload(&payload).is_err());
    }

    #[test]
    fn zero_utilization_contributes_100() {
        let payload = json!({
            "five_hour": { "utilization": 50.0 },
            "seven_day_omelette": { "utilization": 0.0, "resets_at": null }
        });
        let models = live_models_from_payload(&payload).unwrap();
        // min(50, 100) = 50
        assert_eq!(models[0].quota_percent, Some(50));
    }
}
