use super::*;
use crate::data::config::schema::EffortMapping;
use crate::selection::types::{Candidate, CliKind};

/// Build a single-candidate `CachedModel` with the given per-tuple
/// flags. The `is_*_eligible` helpers under test are flag-driven now,
/// so each test sets just the bits it cares about.
fn model_with_candidate_flags(
    vendor: SubscriptionKind,
    name: &str,
    cheap: bool,
    tough: bool,
    effort: bool,
) -> CachedModel {
    let mut model = sample_cached_model();
    model.subscription = vendor;
    model.name = name.to_string();
    let cli = vendor.direct_cli().unwrap_or(CliKind::Opencode);
    model.candidates = vec![Candidate {
        subscription: vendor,
        cli,
        launch_name: name.to_string(),
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free: false,
        official: vendor != SubscriptionKind::Direct,
        quota_disabled: false,
        cheap_eligible: cheap,
        tough_eligible: tough,
        effort_eligible: effort,
        effort_mapping: EffortMapping::default(),
        quota_failed: false,
    }];
    model.selected_candidate = Some(0);
    model
}

fn sample_cached_model() -> CachedModel {
    CachedModel {
        subscription: SubscriptionKind::Codex,
        name: "gpt-5.5".to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 0,
        candidates: Vec::new(),
        selected_candidate: None,
    }
}

#[test]
fn is_cheap_eligible_reads_selected_candidate_flag() {
    let cheap = model_with_candidate_flags(
        SubscriptionKind::Claude,
        "claude-opus-4.7",
        true,
        false,
        false,
    );
    assert!(is_cheap_eligible(&cheap));

    let not_cheap = model_with_candidate_flags(
        SubscriptionKind::Gemini,
        "gemini-2.5-flash",
        false,
        false,
        false,
    );
    assert!(!is_cheap_eligible(&not_cheap));
}

#[test]
fn is_tough_eligible_reads_selected_candidate_flag() {
    let tough = model_with_candidate_flags(SubscriptionKind::Kimi, "kimi-k2", false, true, false);
    assert!(is_tough_eligible(&tough));

    let not_tough =
        model_with_candidate_flags(SubscriptionKind::Codex, "gpt-5", false, false, false);
    assert!(!is_tough_eligible(&not_tough));
}

#[test]
fn is_effort_eligible_reads_selected_candidate_flag() {
    let yes = model_with_candidate_flags(
        SubscriptionKind::Gemini,
        "gemini-2.5-pro",
        false,
        false,
        true,
    );
    assert!(is_effort_eligible(&yes));

    let no = model_with_candidate_flags(
        SubscriptionKind::Claude,
        "claude-sonnet-4-6",
        false,
        false,
        false,
    );
    assert!(!is_effort_eligible(&no));
}

#[test]
fn eligibility_helpers_return_false_when_no_selected_candidate() {
    let mut model = sample_cached_model();
    model.candidates.clear();
    model.selected_candidate = None;
    assert!(!is_cheap_eligible(&model));
    assert!(!is_tough_eligible(&model));
    assert!(!is_effort_eligible(&model));
}

#[test]
fn subscription_kind_to_str_round_trips_known_values() {
    assert_eq!(
        subscription_kind_to_str(SubscriptionKind::OpencodeGo),
        "opencode-go"
    );
    assert_eq!(subscription_kind_to_str(SubscriptionKind::Direct), "direct");
    assert_eq!(subscription_kind_to_str(SubscriptionKind::Claude), "claude");
}
