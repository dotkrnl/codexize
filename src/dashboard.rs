use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;

pub const DASHBOARD_URL: &str = "https://aistupidlevel.info/dashboard/cached";
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct DashboardModel {
    pub name: String,
    pub vendor: String,
    pub overall_score: f64,
    pub current_score: f64,
    pub standard_error: f64,
    pub axes: Vec<(String, f64)>,
    pub display_order: usize,
}

pub fn load_models() -> Result<Vec<DashboardModel>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("failed to build dashboard HTTP client")?;
    let payload = client
        .get(DASHBOARD_URL)
        .send()
        .and_then(|response| response.error_for_status())
        .context("dashboard request failed")?
        .json::<Value>()
        .context("dashboard response was not valid JSON")?;
    parse_models(&payload)
}

fn parse_models(payload: &Value) -> Result<Vec<DashboardModel>> {
    let data = payload.get("data").unwrap_or(payload);
    let model_scores = data
        .get("modelScores")
        .or_else(|| payload.get("modelScores"))
        .and_then(Value::as_array)
        .context("dashboard payload missing modelScores")?;
    let history_map = data
        .get("historyMap")
        .or_else(|| payload.get("historyMap"))
        .and_then(Value::as_object);

    let mut models = Vec::with_capacity(model_scores.len());
    for (index, item) in model_scores.iter().enumerate() {
        let name = item
            .get("name")
            .or_else(|| item.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }

        let model_id = item
            .get("id")
            .map(value_to_string)
            .unwrap_or_default();
        let axes = history_map
            .and_then(|map| map.get(&model_id))
            .and_then(latest_axes)
            .unwrap_or_default();

        models.push(DashboardModel {
            name,
            vendor: item
                .get("vendor")
                .or_else(|| item.get("provider"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase(),
            overall_score: value_to_f64(item.get("score")).unwrap_or(0.0),
            current_score: value_to_f64(item.get("currentScore"))
                .or_else(|| value_to_f64(item.get("score")))
                .unwrap_or(0.0),
            standard_error: value_to_f64(
                item.get("standardError")
                    .or_else(|| item.get("standard_error")),
            )
            .unwrap_or(0.0),
            axes,
            display_order: index,
        });
    }

    if models.is_empty() {
        bail!("dashboard returned no models");
    }

    Ok(models)
}

fn latest_axes(value: &Value) -> Option<Vec<(String, f64)>> {
    let latest = value.as_array()?.last()?;
    let axes = latest.get("axes")?.as_object()?;
    Some(
        axes.iter()
            .map(|(key, value)| (key.to_ascii_lowercase(), value_to_f64(Some(value)).unwrap_or(0.0)))
            .collect(),
    )
}

fn value_to_f64(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse().ok(),
        Some(Value::Bool(boolean)) => Some(if *boolean { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
