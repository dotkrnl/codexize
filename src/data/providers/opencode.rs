//! Opencode provider IO: model enumeration and quota estimation.
//!
//! Quota cascade: opencode does not currently expose a Go-tier remaining
//! quota through a stable machine-readable surface, so we estimate from the
//! `opencode stats --days 30 --models` output (which lists per-model spend
//! the CLI itself computed) against the documented $60 Go API allowance.
//! Any cookie/login-based path is intentionally absent — operator override.
//!
//! Enumeration cascade: prefer `opencode models opencode --verbose`, fall
//! back to a small hardcoded snapshot when the local CLI is unavailable. No
//! HTTP catalog endpoint is hit because opencode.ai exposes no documented
//! unauthenticated catalog and authenticated cookie flows are out of scope.
use super::LiveModel;
use crate::logic::selection::types::VendorKind;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::process::Command;
/// Documented Go-tier API allowance the percentage scale is normalized
/// against. If opencode raises or splits the cap, this constant moves with it.
pub const GO_QUOTA_DOLLARS: f64 = 60.0;
const STATS_WINDOW_DAYS: u32 = 30;
const GO_PROVIDER_PREFIX: &str = "opencode-go/";
/// Quota map key shared by every opencode-routed model: a single Go-tier
/// pool funds them all, so the per-model heuristic in
/// `logic::selection::quota` will pick this single entry up regardless of
/// which opencode model the universe is asking about.
pub const SHARED_QUOTA_KEY: &str = "opencode-shared";
/// Metadata describing one model opencode advertises. The selection layer
/// later intersects these against ipbr to decide which actually surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeModelMeta {
    /// Bare model id (e.g. `gpt-5-nano`), as advertised by opencode.
    pub id: String,
    /// Provider id from the route header (typically `opencode`).
    pub provider_id: String,
    /// Pretty display name when the CLI provides one.
    pub display_name: Option<String>,
    /// `api.npm` field — used to infer the underlying vendor without
    /// relying on display-name heuristics.
    pub api_npm: Option<String>,
    /// Underlying provider this model is resold from, when inferable.
    pub underlying_vendor: Option<VendorKind>,
}
/// Live quota fetch entry point used by the data-side selection plumbing.
pub async fn load_live_models_async() -> Result<Vec<LiveModel>> {
    let stats_text = run_stats_command()?;
    quota_models_from_stats(&stats_text)
}
/// Pure quota construction from already-captured stats text. Splits out so
/// fixture tests can exercise the dollar→percent math without invoking the
/// `opencode` binary.
pub fn quota_models_from_stats(stats_text: &str) -> Result<Vec<LiveModel>> {
    let spent = extract_go_tier_spend(stats_text)?;
    let percent = remaining_percent_from_spend(spent);
    Ok(vec![LiveModel {
        name: SHARED_QUOTA_KEY.to_string(),
        quota_percent: Some(percent),
        quota_resets_at: None,
    }])
}
/// Convert a 30-day Go-tier dollar spend to remaining-quota percent against
/// the $60 cap, rounded and clamped to `0..=100`.
pub fn remaining_percent_from_spend(spent_dollars: f64) -> u8 {
    if !spent_dollars.is_finite() {
        return 0;
    }
    let remaining = (GO_QUOTA_DOLLARS - spent_dollars).max(0.0);
    let percent = (100.0 * remaining / GO_QUOTA_DOLLARS).clamp(0.0, 100.0);
    percent.round() as u8
}
/// Sum dollar amounts spent on `opencode-go/<model>` rows in the past 30 days
/// from the rendered `opencode stats` table. Returns an error when the table
/// is missing — i.e. the CLI did not include a MODEL USAGE section at all,
/// which means we have no way to even pin the spend at $0 with confidence.
/// A MODEL USAGE table that exists but contains no `opencode-go/` rows is
/// treated as $0 spent: stats listing other providers is still observable
/// usage data, just not Go-tier usage.
pub fn extract_go_tier_spend(stats_text: &str) -> Result<f64> {
    if !stats_text.contains("MODEL USAGE") {
        bail!("opencode stats did not include a MODEL USAGE section");
    }
    let mut total = 0.0f64;
    let mut in_go_block = false;
    for line in stats_text.lines() {
        let cleaned = strip_box_chars(line);
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(header) = model_header_token(trimmed) {
            in_go_block = header.starts_with(GO_PROVIDER_PREFIX);
            continue;
        }
        if in_go_block && trimmed.starts_with("Cost") {
            if let Some(amount) = parse_dollar_amount(trimmed) {
                total += amount;
            }
            in_go_block = false;
        }
    }
    Ok(total)
}
/// Enumerate models advertised by opencode for the `opencode` provider.
/// Cascades local CLI → hardcoded fallback. The cascade does not retry on
/// each call: a single CLI failure in this process drops us to the snapshot.
pub fn enumerate_models() -> Vec<OpencodeModelMeta> {
    enumerate_with_cli_text(run_models_command().ok().as_deref())
}
/// Enumeration with the CLI invocation factored out so cascade behavior is
/// fixture-testable: pass `None` to simulate CLI failure, or a captured
/// verbose output to simulate success.
pub fn enumerate_with_cli_text(cli_text: Option<&str>) -> Vec<OpencodeModelMeta> {
    if let Some(text) = cli_text {
        let parsed = parse_verbose_models(text);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    hardcoded_fallback_models()
}
/// Parse the multi-block output of `opencode models opencode --verbose`.
/// Each block is an `opencode/<id>` route header followed by a pretty-printed
/// JSON object. We rely on the JSON braces — not the route header — for
/// boundary detection so reordered or relabeled headers do not break parsing.
pub fn parse_verbose_models(text: &str) -> Vec<OpencodeModelMeta> {
    let bytes = text.as_bytes();
    let mut models = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && let Some(end) = find_matching_brace(bytes, i)
        {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes[i..=end])
                && let Some(meta) = model_meta_from_value(&value)
            {
                models.push(meta);
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    models
}
/// Static snapshot used when the local CLI cannot be reached. Refresh as
/// opencode adds models. Underlying vendors are best-effort: leave `None`
/// when the resale provenance is not obvious from the model id.
fn hardcoded_fallback_models() -> Vec<OpencodeModelMeta> {
    [
        ("big-pickle", None),
        ("gpt-5-nano", Some(VendorKind::Codex)),
        ("hy3-preview-free", None),
        ("minimax-m2.5-free", Some(VendorKind::Kimi)),
        ("nemotron-3-super-free", None),
    ]
    .into_iter()
    .map(|(id, underlying)| OpencodeModelMeta {
        id: id.to_string(),
        provider_id: "opencode".to_string(),
        display_name: None,
        api_npm: None,
        underlying_vendor: underlying,
    })
    .collect()
}
fn run_models_command() -> Result<String> {
    let output = Command::new("opencode")
        .args(["models", "opencode", "--verbose"])
        .output()
        .context("failed to invoke `opencode models opencode --verbose`")?;
    if !output.status.success() {
        bail!(
            "`opencode models opencode --verbose` exited with {:?}",
            output.status
        );
    }
    String::from_utf8(output.stdout).context("opencode models output was not UTF-8")
}
fn run_stats_command() -> Result<String> {
    let output = Command::new("opencode")
        .args([
            "stats",
            "--days",
            &STATS_WINDOW_DAYS.to_string(),
            "--models",
        ])
        .output()
        .context("failed to invoke `opencode stats`")?;
    if !output.status.success() {
        bail!("`opencode stats` exited with {:?}", output.status);
    }
    String::from_utf8(output.stdout).context("opencode stats output was not UTF-8")
}
fn model_meta_from_value(value: &Value) -> Option<OpencodeModelMeta> {
    let obj = value.as_object()?;
    let id = obj.get("id")?.as_str()?.to_string();
    let provider_id = obj.get("providerID")?.as_str()?.to_string();
    let display_name = obj
        .get("name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let api_npm = obj
        .get("api")
        .and_then(Value::as_object)
        .and_then(|api| api.get("npm"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let underlying_vendor = api_npm.as_deref().and_then(underlying_vendor_from_npm);
    Some(OpencodeModelMeta {
        id,
        provider_id,
        display_name,
        api_npm,
        underlying_vendor,
    })
}
fn underlying_vendor_from_npm(npm: &str) -> Option<VendorKind> {
    match npm {
        "@ai-sdk/anthropic" => Some(VendorKind::Claude),
        "@ai-sdk/openai" => Some(VendorKind::Codex),
        "@ai-sdk/google" => Some(VendorKind::Gemini),
        "@ai-sdk/moonshotai" | "@ai-sdk/moonshot" => Some(VendorKind::Kimi),
        _ => None,
    }
}
fn find_matching_brace(bytes: &[u8], start: usize) -> Option<usize> {
    debug_assert_eq!(bytes.get(start), Some(&b'{'));
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (offset, &c) in bytes[start..].iter().enumerate() {
        let i = start + offset;
        if escape {
            escape = false;
            continue;
        }
        if in_string {
            match c {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}
fn strip_box_chars(line: &str) -> String {
    line.chars()
        .filter(|c| !matches!(c, '│' | '|' | '├' | '┤' | '┌' | '┐' | '└' | '┘' | '─'))
        .collect()
}
/// Extract a leading `provider/model` token from a (boxchar-stripped, trimmed)
/// row, skipping non-header rows like `Total Cost`, `Cost  $1.23`, or the
/// section title `MODEL USAGE`.
fn model_header_token(trimmed: &str) -> Option<&str> {
    let token = trimmed.split_whitespace().next()?;
    if !token.contains('/') {
        return None;
    }
    if token.starts_with('$') {
        return None;
    }
    // Headers occupy their own row; metric lines have a trailing value.
    if trimmed.split_whitespace().count() != 1 {
        return None;
    }
    Some(token)
}
fn parse_dollar_amount(text: &str) -> Option<f64> {
    let dollar = text.rfind('$')?;
    let rest = &text[dollar + 1..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}
#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
