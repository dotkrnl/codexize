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
        ipbr_match_key: None,
    }
}

fn sample_cached_model() -> CachedModel {
    CachedModel {
        vendor: SubscriptionKind::Codex,
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
        ipbr_match_key: None,
        candidates: Vec::new(),
        selected_candidate: None,
    }
}

#[test]
fn is_effort_capable_only_claude_and_codex() {
    assert!(is_effort_capable(SubscriptionKind::Claude));
    assert!(is_effort_capable(SubscriptionKind::Codex));
    assert!(!is_effort_capable(SubscriptionKind::Gemini));
    assert!(!is_effort_capable(SubscriptionKind::Kimi));
}

#[test]
fn is_effort_capable_includes_opencode_route_vendor() {
    assert!(is_effort_capable(SubscriptionKind::OpencodeGo));
}

#[test]
fn is_cheap_eligible_matches_budget_subset() {
    let cases = [
        (SubscriptionKind::Claude, "claude-opus-4-7", false),
        (SubscriptionKind::Claude, "claude-sonnet-4-6", true),
        (SubscriptionKind::Claude, "claude-haiku-4-5", true),
        (SubscriptionKind::Codex, "gpt-5.5", true),
        (SubscriptionKind::Kimi, "kimi-k2", true),
        (SubscriptionKind::Gemini, "gemini-2.5-pro", false),
        (SubscriptionKind::Gemini, "gemini-2.5-flash", true),
        (SubscriptionKind::Gemini, "gemini-nano", true),
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
fn opencode_tough_eligibility_uses_underlying_model_identity() {
    let mut opus = sample_cached_model();
    opus.vendor = SubscriptionKind::OpencodeGo;
    opus.name = "opencode/claude-opus-4.7".to_string();
    assert!(is_tough_eligible(&opus));

    let mut sonnet = opus.clone();
    sonnet.name = "opencode/claude-sonnet-4.6".to_string();
    assert!(!is_tough_eligible(&sonnet));
}

#[test]
fn opencode_cheap_eligibility_uses_underlying_model_identity() {
    let mut flash = sample_cached_model();
    flash.vendor = SubscriptionKind::OpencodeGo;
    flash.name = "opencode/gemini-2.5-flash".to_string();
    assert!(is_cheap_eligible(&flash));

    let mut pro = flash.clone();
    pro.name = "opencode/gemini-2.5-pro".to_string();
    assert!(!is_cheap_eligible(&pro));
}

#[test]
fn str_to_vendor_round_trips_known_values() {
    assert_eq!(str_to_vendor("claude"), Some(SubscriptionKind::Claude));
    assert_eq!(str_to_vendor("codex"), Some(SubscriptionKind::Codex));
    assert_eq!(str_to_vendor("gemini"), Some(SubscriptionKind::Gemini));
    assert_eq!(str_to_vendor("kimi"), Some(SubscriptionKind::Kimi));
    assert_eq!(
        str_to_vendor("opencode"),
        Some(SubscriptionKind::OpencodeGo)
    );
    assert_eq!(
        vendor_kind_to_str(SubscriptionKind::OpencodeGo),
        "opencode-go"
    );
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
        Some(SubscriptionKind::Claude)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("gpt-5.5", "")),
        Some(SubscriptionKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("o1-mini", "")),
        Some(SubscriptionKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("gemini-2.5-pro", "")),
        Some(SubscriptionKind::Gemini)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("kimi-k2", "")),
        Some(SubscriptionKind::Kimi)
    );
    assert_ne!(
        vendor_for_dashboard_model(&dashboard_model("opencode/claude-opus-4.7", "")),
        Some(SubscriptionKind::OpencodeGo),
        "opencode is a route vendor and must not be inferred from model names"
    );
}

#[test]
fn vendor_for_dashboard_model_falls_back_to_vendor_field() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "anthropic")),
        Some(SubscriptionKind::Claude)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "openai")),
        Some(SubscriptionKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "google")),
        Some(SubscriptionKind::Gemini)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("model-x", "moonshotai")),
        Some(SubscriptionKind::Kimi)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("claude-opus-4.7", "opencode")),
        Some(SubscriptionKind::OpencodeGo)
    );
}

#[test]
fn vendor_for_dashboard_model_uses_name_substring_heuristics_for_unknown_vendor() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("legacy-opus", "unknown")),
        Some(SubscriptionKind::Claude)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("foo-davinci", "unknown")),
        Some(SubscriptionKind::Codex)
    );
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("ada-palm", "unknown")),
        Some(SubscriptionKind::Gemini)
    );
}

#[test]
fn vendor_for_dashboard_model_returns_none_when_nothing_matches() {
    assert_eq!(
        vendor_for_dashboard_model(&dashboard_model("strange-model", "unknown-vendor")),
        None
    );
}
