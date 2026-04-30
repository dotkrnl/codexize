use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, env, fs};

use super::{LiveModel, build_http_client, fetch_json_response, home_dir, run_provider_warmup};

const QUOTA_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";

#[derive(Debug, Deserialize)]
struct OAuthCreds {
    access_token: Option<String>,
}

pub fn load_live_models() -> Result<Vec<LiveModel>> {
    dummy_invoke()?;
    let token = resolve_access_token()?;
    let project_id = resolve_project_id()?;
    let payload = fetch_usage_payload(&token, &project_id)?;
    live_models_from_payload(&payload)
}

fn live_models_from_payload(payload: &Value) -> Result<Vec<LiveModel>> {
    let buckets = payload
        .get("buckets")
        .and_then(Value::as_array)
        .context("Gemini usage response did not include buckets")?;

    let mut models = BTreeMap::<String, Option<u8>>::new();
    for bucket in buckets {
        let name = bucket
            .get("modelId")
            .and_then(Value::as_str)
            .or_else(|| bucket.get("name").and_then(Value::as_str))
            .unwrap_or("gemini")
            .to_ascii_lowercase();
        let remaining = bucket
            .get("remainingFraction")
            .or_else(|| bucket.get("remaining_fraction"))
            .and_then(Value::as_f64)
            .map(|value| (value * 100.0).round().clamp(0.0, 100.0) as u8);
        let entry = models.entry(name).or_insert(remaining);
        if let (Some(a), Some(b)) = (*entry, remaining) {
            *entry = Some(a.min(b));
        }
    }

    if models.is_empty() {
        bail!("Gemini quota response had no model buckets");
    }

    Ok(models
        .into_iter()
        .map(|(name, quota_percent)| LiveModel {
            name,
            quota_percent,
            quota_resets_at: None,
        })
        .collect())
}

fn dummy_invoke() -> Result<()> {
    run_provider_warmup("Gemini", "gemini", &["--yolo"], "/stats\n/exit\n", &[])
}

fn resolve_access_token() -> Result<String> {
    if let Ok(token) = env::var("GEMINI_ACCESS_TOKEN") {
        return Ok(token);
    }

    let path = home_dir()?.join(".gemini/oauth_creds.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let creds: OAuthCreds = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    creds
        .access_token
        .context("no Gemini access token found in oauth_creds.json")
}

fn resolve_project_id() -> Result<String> {
    if let Ok(project_id) = env::var("GEMINI_PROJECT_ID") {
        return Ok(project_id);
    }
    if let Ok(project_id) = env::var("GOOGLE_CLOUD_PROJECT") {
        return Ok(project_id);
    }

    let path = home_dir()?.join(".gemini/projects.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let payload: Value = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let projects = payload
        .get("projects")
        .and_then(Value::as_object)
        .context("Gemini projects.json did not include projects")?;
    let cwd = env::current_dir().context("failed to get current directory")?;
    let cwd_str = cwd.to_string_lossy().to_string();
    let cwd_resolved = cwd
        .canonicalize()
        .unwrap_or(cwd)
        .to_string_lossy()
        .to_string();

    projects
        .get(&cwd_str)
        .or_else(|| projects.get(&cwd_resolved))
        .and_then(Value::as_str)
        .or_else(|| projects.values().find_map(Value::as_str))
        .map(ToOwned::to_owned)
        .context("no Gemini project id found")
}

fn fetch_usage_payload(token: &str, project_id: &str) -> Result<Value> {
    let client = build_http_client(5)?;

    let request = client
        .post(QUOTA_ENDPOINT)
        .bearer_auth(token)
        .json(&json!({ "project": project_id }));

    let payload = fetch_json_response(request, "Gemini")?;
    if payload.get("error").is_some() {
        bail!("Gemini quota response contained an error");
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_buckets_are_min_d() {
        let payload = json!({
            "buckets": [
                { "modelId": "gemini-2.5-pro", "remainingFraction": 0.80 },
                { "modelId": "gemini-2.5-pro", "remainingFraction": 0.30 }
            ]
        });
        let models = live_models_from_payload(&payload).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "gemini-2.5-pro");
        // MIN(80, 30) = 30
        assert_eq!(models[0].quota_percent, Some(30));
    }

    #[test]
    fn single_bucket_preserved() {
        let payload = json!({
            "buckets": [
                { "modelId": "gemini-2.5-flash", "remainingFraction": 0.47 }
            ]
        });
        let models = live_models_from_payload(&payload).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "gemini-2.5-flash");
        assert_eq!(models[0].quota_percent, Some(47));
    }

    #[test]
    fn missing_buckets_returns_error() {
        let payload = json!({ "buckets": [] });
        assert!(live_models_from_payload(&payload).is_err());
    }
}
