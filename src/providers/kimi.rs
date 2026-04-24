use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs,
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{LiveModel, build_http_client, home_dir, parse_json_response, send_request};

const DEFAULT_USAGE_BASE_URL: &str = "https://api.kimi.com/coding/v1";

pub fn load_live_models() -> Result<Vec<LiveModel>> {
    let api_key = resolve_api_key()?;
    let payload = fetch_usage_payload(&api_key)?;
    let mut models = BTreeMap::<String, Option<u8>>::new();

    if let Some(usage) = payload.get("usage").and_then(Value::as_object) {
        models.insert("kimi-latest".to_string(), usage_remaining_percent(usage));
    }

    if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
        for item in limits {
            let detail = item.get("detail").unwrap_or(item);
            let Some(detail) = detail.as_object() else { continue };
            let name = detail
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| detail.get("title").and_then(Value::as_str))
                .unwrap_or("kimi")
                .to_ascii_lowercase();
            models.insert(name, usage_remaining_percent(detail));
        }
    }

    if models.is_empty() {
        bail!("Kimi usage response had no usage limits");
    }

    Ok(models
        .into_iter()
        .map(|(name, quota_percent)| LiveModel { name, quota_percent })
        .collect())
}

fn resolve_api_key() -> Result<String> {
    if let Ok(value) = env::var("KIMI_API_KEY") {
        return Ok(value);
    }

    let creds_file = home_dir()?.join(".kimi/credentials/kimi-code.json");
    if creds_file.is_file() {
        let text = fs::read_to_string(&creds_file)
            .with_context(|| format!("failed to read {}", creds_file.display()))?;
        let payload: Value = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse {}", creds_file.display()))?;

        // Refresh the token if expired or within 60s of expiry
        let expires_at = payload.get("expires_at").and_then(Value::as_f64).unwrap_or(0.0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        if expires_at > 0.0 && expires_at < now + 60.0 {
            // Token is expired or nearly expired — run kimi briefly to trigger
            // a credential refresh. Close stdin so kimi sees EOF and exits;
            // kill after 10s if it somehow keeps running.
            if let Ok(mut child) = Command::new("kimi")
                .args(["--yolo", "--print"])
                .env("KIMI_CLI_NO_AUTO_UPDATE", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }

            // Re-read after potential refresh
            if let Ok(refreshed) = fs::read_to_string(&creds_file) {
                if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&refreshed) {
                    if let Some(token) = obj.get("access_token").and_then(Value::as_str) {
                        return Ok(token.to_string());
                    }
                }
            }
        }

        if let Some(token) = payload.get("access_token").and_then(Value::as_str) {
            return Ok(token.to_string());
        }
    }

    let config_file = home_dir()?.join(".kimi/config.toml");
    let text = fs::read_to_string(&config_file)
        .with_context(|| format!("failed to read {}", config_file.display()))?;
    let payload: toml::Value = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", config_file.display()))?;
    let providers = payload
        .get("providers")
        .and_then(toml::Value::as_table)
        .context("Kimi config did not include providers")?;
    for provider in providers.values() {
        if let Some(api_key) = provider.get("api_key").and_then(toml::Value::as_str) {
            return Ok(api_key.to_string());
        }
    }

    bail!("no Kimi API key found")
}

fn fetch_usage_payload(api_key: &str) -> Result<Value> {
    let base_url =
        env::var("KIMI_CODE_BASE_URL").unwrap_or_else(|_| DEFAULT_USAGE_BASE_URL.to_string());
    let usage_url = format!("{}/usages", base_url.trim_end_matches('/'));
    let client = build_http_client(5)?;

    let request = client
        .get(&usage_url)
        .bearer_auth(api_key);

    parse_json_response(send_request(request, "Kimi")?, "Kimi")
}

fn usage_remaining_percent(data: &serde_json::Map<String, Value>) -> Option<u8> {
    if let Some(value) = read_f64(
        data.get("remaining_percent")
            .or_else(|| data.get("remainingPercent")),
    ) {
        return Some(value.round().clamp(0.0, 100.0) as u8);
    }

    let limit = read_f64(data.get("limit"))?;
    let used = read_f64(data.get("used"))
        .or_else(|| read_f64(data.get("remaining")).map(|r| limit - r))?;
    let remaining = if limit <= 0.0 {
        0.0
    } else {
        ((limit - used) / limit * 100.0).clamp(0.0, 100.0)
    };
    Some(remaining.round() as u8)
}

fn read_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
}
