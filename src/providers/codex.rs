use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, env, fs, path::PathBuf};

use super::{
    LiveModel, build_http_client, fetch_json_response, home_dir, percent_to_u8, run_provider_warmup,
};

const DEFAULT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";

#[derive(Debug, Deserialize, Default)]
struct CodexConfig {
    model: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct AuthTokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct AuthFile {
    access_token: Option<String>,
    account_id: Option<String>,
    tokens: Option<AuthTokens>,
}

#[derive(Debug)]
struct UsageIdentity {
    access_token: String,
    account_id: Option<String>,
}

#[derive(Debug, Default)]
struct ModelQuota {
    remaining_min: Option<f64>,
}

pub fn load_live_models() -> Result<Vec<LiveModel>> {
    dummy_invoke()?;
    let config = read_config().unwrap_or_default();
    let default_model = config
        .model
        .unwrap_or_else(|| "gpt-5.4".to_string())
        .to_ascii_lowercase();
    let identity = resolve_usage_identity()?;
    let payload = fetch_usage_payload(&identity)?;

    let mut quotas = BTreeMap::<String, ModelQuota>::new();

    if let Some(rate_limit) = payload.get("rate_limit") {
        record_rate_limit(&mut quotas, &default_model, rate_limit);
    }

    if let Some(additional) = payload
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for item in additional {
            let name = item
                .get("limit_name")
                .and_then(Value::as_str)
                .or_else(|| item.get("metered_feature").and_then(Value::as_str))
                .unwrap_or("additional")
                .to_ascii_lowercase();
            if let Some(rate_limit) = item.get("rate_limit") {
                record_rate_limit(&mut quotas, &name, rate_limit);
            }
        }
    }

    if quotas.is_empty() {
        bail!("Codex usage response did not include any rate-limit windows");
    }

    let mut models = Vec::new();

    if let Some(quota) = quotas.remove(&default_model) {
        models.push(LiveModel {
            name: default_model.clone(),
            quota_percent: quota.remaining_min.map(percent_to_u8),
        });
    }

    for (name, quota) in quotas {
        models.push(LiveModel {
            name,
            quota_percent: quota.remaining_min.map(percent_to_u8),
        });
    }

    Ok(models)
}

fn dummy_invoke() -> Result<()> {
    run_provider_warmup("Codex", "codex", &[], "/status\n/exit\n", &[])
}

fn read_config() -> Result<CodexConfig> {
    let path = codex_home()?.join("config.toml");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
}

fn resolve_usage_identity() -> Result<UsageIdentity> {
    let access_token = env::var("CODEX_ACCESS_TOKEN").ok();
    let account_id = env::var("CODEX_ACCOUNT_ID")
        .ok()
        .or_else(|| env::var("CHATGPT_ACCOUNT_ID").ok());

    if let Some(access_token) = access_token {
        return Ok(UsageIdentity {
            access_token,
            account_id,
        });
    }

    let auth_path = env::var_os("CODEX_AUTH_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            codex_home()
                .unwrap_or_else(|_| PathBuf::from(".codex"))
                .join("auth.json")
        });
    let text = fs::read_to_string(&auth_path)
        .with_context(|| format!("failed to read {}", auth_path.display()))?;
    let auth: AuthFile = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", auth_path.display()))?;

    let access_token = auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.access_token.clone())
        .or(auth.access_token)
        .context("no Codex access token found in auth.json")?;

    let account_id = account_id
        .or_else(|| auth.tokens.and_then(|tokens| tokens.account_id))
        .or(auth.account_id);

    Ok(UsageIdentity {
        access_token,
        account_id,
    })
}

fn fetch_usage_payload(identity: &UsageIdentity) -> Result<Value> {
    let base_url = normalize_base_url(
        &env::var("CODEX_BASE_URL")
            .ok()
            .or_else(|| env::var("CODEX_CHATGPT_BASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_CHATGPT_BASE_URL.to_string()),
    );

    let usage_url = if base_url.contains("/backend-api") {
        let account_id = identity
            .account_id
            .as_deref()
            .context("no ChatGPT account id found for backend-api usage request")?;
        let _ = account_id;
        format!("{base_url}/wham/usage")
    } else {
        format!("{base_url}/api/codex/usage")
    };

    let client = build_http_client(5)?;

    let mut request = client
        .get(&usage_url)
        .bearer_auth(&identity.access_token)
        .header("Content-Type", "application/json");

    if let Some(account_id) = identity.account_id.as_deref() {
        request = request.header("chatgpt-account-id", account_id);
    }

    fetch_json_response(request, "Codex")
}

fn record_rate_limit(quotas: &mut BTreeMap<String, ModelQuota>, name: &str, rate_limit: &Value) {
    let quota = quotas.entry(name.to_string()).or_default();

    for key in ["primary_window", "secondary_window"] {
        let used_percent = rate_limit
            .get(key)
            .and_then(|window| {
                window
                    .get("used_percent")
                    .or_else(|| window.get("usedPercent"))
            })
            .and_then(Value::as_f64);

        if let Some(used_percent) = used_percent {
            let remaining = (100.0 - used_percent).clamp(0.0, 100.0);
            quota.remaining_min = Some(
                quota
                    .remaining_min
                    .map_or(remaining, |current| current.min(remaining)),
            );
        }
    }
}

fn normalize_base_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    match base {
        "https://chatgpt.com" | "https://chat.openai.com" => format!("{base}/backend-api"),
        _ => base.to_string(),
    }
}

fn codex_home() -> Result<PathBuf> {
    if let Some(path) = env::var_os("CODEX_HOME") {
        return Ok(PathBuf::from(path));
    }

    Ok(home_dir()?.join(".codex"))
}
