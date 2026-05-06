use super::*;
use std::collections::BTreeMap;

fn dashboard_model(name: &str, vendor: &str) -> dashboard::DashboardModel {
    dashboard::DashboardModel {
        name: name.to_string(),
        vendor: vendor.to_string(),
        overall_score: 0.0,
        current_score: 0.0,
        standard_error: 0.0,
        axes: Vec::new(),
        axis_provenance: BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        display_order: 0,
        fallback_from: None,
    }
}

fn sample_cached_model() -> CachedModel {
    CachedModel {
        vendor: VendorKind::Codex,
        name: "gpt-5.5".to_string(),
        overall_score: 85.0,
        current_score: 85.0,
        standard_error: 2.0,
        axes: Vec::new(),
        axis_provenance: BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 0,
        fallback_from: None,
    }
}

#[test]
fn is_effort_capable_only_claude_and_codex() {
    assert!(is_effort_capable(VendorKind::Claude));
    assert!(is_effort_capable(VendorKind::Codex));
    assert!(!is_effort_capable(VendorKind::Gemini));
    assert!(!is_effort_capable(VendorKind::Kimi));
}

#[test]
fn is_cheap_eligible_matches_budget_subset() {
    let cases = [
        (VendorKind::Claude, "claude-opus-4-7", false),
        (VendorKind::Claude, "claude-sonnet-4-6", true),
        (VendorKind::Claude, "claude-haiku-4-5", true),
        (VendorKind::Codex, "gpt-5.5", true),
        (VendorKind::Kimi, "kimi-k2", true),
        (VendorKind::Gemini, "gemini-2.5-pro", false),
        (VendorKind::Gemini, "gemini-2.5-flash", true),
        (VendorKind::Gemini, "gemini-nano", true),
    ];

    for (vendor, name, expected) in cases {
        let mut model = sample_cached_model();
        model.vendor = vendor;
        model.name = name.to_string();
        assert_eq!(
            is_cheap_eligible(&model),
            expected,
            "{vendor:?} {name} eligibility"
        );
    }
}

#[test]
fn str_to_vendor_round_trips_known_values() {
    assert_eq!(str_to_vendor("claude"), Some(VendorKind::Claude));
    assert_eq!(str_to_vendor("codex"), Some(VendorKind::Codex));
    assert_eq!(str_to_vendor("gemini"), Some(VendorKind::Gemini));
    assert_eq!(str_to_vendor("kimi"), Some(VendorKind::Kimi));
}

#[test]
fn str_to_vendor_rejects_unknown_and_alias_strings() {
    assert_eq!(str_to_vendor(""), None);
    assert_eq!(str_to_vendor("anthropic"), None);
    assert_eq!(str_to_vendor("openai"), None);
    assert_eq!(str_to_vendor("Claude"), None);
}

#[test]
fn vendor_for_dashboard_model_matches_name_prefixes() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("claude-sonnet-4", "")),
        Some(VendorKind::Claude)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("gpt-5.5", "")),
        Some(VendorKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("o1-mini", "")),
        Some(VendorKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("gemini-2.5-pro", "")),
        Some(VendorKind::Gemini)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("kimi-k2", "")),
        Some(VendorKind::Kimi)
    );
}

#[test]
fn vendor_for_dashboard_model_falls_back_to_vendor_field() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "anthropic")),
        Some(VendorKind::Claude)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "openai")),
        Some(VendorKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "google")),
        Some(VendorKind::Gemini)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "moonshotai")),
        Some(VendorKind::Kimi)
    );
}

#[test]
fn vendor_for_dashboard_model_uses_name_substring_heuristics_for_unknown_vendor() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("legacy-opus", "unknown")),
        Some(VendorKind::Claude)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("foo-davinci", "unknown")),
        Some(VendorKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("ada-palm", "unknown")),
        Some(VendorKind::Gemini)
    );
}

#[test]
fn vendor_for_dashboard_model_returns_none_when_nothing_matches() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("strange-model", "unknown-vendor")),
        None
    );
}
