use crate::warmup;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

const DEFAULT_USAGE_BASE_URL: &str = "https://api.kimi.com/coding/v1";
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct LiveModel {
    pub name: String,
    pub quota_percent: Option<u8>,
}

pub fn load_live_models() -> Result<Vec<LiveModel>> {
    dummy_invoke()?;
    let api_key = resolve_api_key()?;
    let payload = fetch_usage_payload(&api_key)?;
    let mut models = BTreeMap::<String, Option<u8>>::new();

    if let Some(usage) = payload.get("usage").and_then(Value::as_object) {
        models.insert("kimi-latest".to_string(), usage_remaining_percent(usage));
    }

    if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
        for item in limits {
            let detail = item.get("detail").unwrap_or(item);
            let Some(detail) = detail.as_object() else {
                continue;
            };
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
        .map(|(name, quota_percent)| LiveModel {
            name,
            quota_percent,
        })
        .collect())
}

fn dummy_invoke() -> Result<()> {
    warmup::run(warmup::WarmupSpec {
        program: "kimi",
        args: &["--yolo"],
        script: "/usage\n/exit\n",
        env: &[("KIMI_CLI_NO_AUTO_UPDATE", "1")],
        settle_timeout: Duration::from_secs(2),
    })
    .context("Kimi dummy invoke failed")
}

fn resolve_api_key() -> Result<String> {
    if let Ok(value) = env::var("KIMI_API_KEY") {
        return Ok(value);
    }

    let oauth_file = home_dir()?.join(".kimi/credentials/kimi-code.json");
    if oauth_file.is_file() {
        let text = fs::read_to_string(&oauth_file)
            .with_context(|| format!("failed to read {}", oauth_file.display()))?;
        let payload: Value = serde_json::from_str(&text)
            .with_context(|| format!("failed to parse {}", oauth_file.display()))?;
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
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to build Kimi HTTP client")?;

    client
        .get(&usage_url)
        .bearer_auth(api_key)
        .send()
        .and_then(|response| response.error_for_status())
        .context("Kimi usage request failed")?
        .json::<Value>()
        .context("Kimi usage response was not valid JSON")
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
        .or_else(|| read_f64(data.get("remaining")).map(|remaining| limit - remaining))?;
    let remaining = if limit <= 0.0 {
        0.0
    } else {
        ((limit - used) / limit * 100.0).clamp(0.0, 100.0)
    };
    Some(remaining.round() as u8)
}

fn read_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse().ok(),
        _ => None,
    }
}

fn home_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(Path::new(&home).to_path_buf())
}
