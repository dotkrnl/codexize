use super::*;

fn sample_cached_model() -> CachedModel {
    CachedModel {
        subscription: SubscriptionKind::Codex,
        name: "gpt-5.5".to_string(),
        ipbr_phase_scores: IpbrPhaseScores::default(),
        score_source: ScoreSource::None,
        quota_percent: Some(73),
        quota_resets_at: None,
        display_order: 2,
        candidates: Vec::new(),
        selected_candidate: None,
    }
}

#[test]
fn new_ipbr_fields_default_to_unscored() {
    let model = sample_cached_model();

    assert_eq!(model.ipbr_phase_scores, IpbrPhaseScores::default());
    assert_eq!(model.ipbr_phase_scores.idea, None);
    assert_eq!(model.ipbr_phase_scores.planning, None);
    assert_eq!(model.ipbr_phase_scores.build, None);
    assert_eq!(model.ipbr_phase_scores.review, None);
    assert_eq!(model.score_source, ScoreSource::None);
}

#[test]
fn score_source_default_is_none_not_ipbr() {
    // The default MUST be a non-`Ipbr` value so freshly-constructed
    // entries cannot be mistaken for ipbr-authoritative data.
    let source = ScoreSource::default();
    assert_eq!(source, ScoreSource::None);
    assert_ne!(source, ScoreSource::Ipbr);
}

fn sample_candidate() -> Candidate {
    Candidate {
        subscription: SubscriptionKind::Claude,
        cli: CliKind::Claude,
        launch_name: "claude-opus-4.7".to_string(),
        quota_percent: Some(60),
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free: false,
        official: true,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    }
}

#[test]
fn effective_quota_returns_raw_when_quota_known() {
    let candidate = sample_candidate();
    assert_eq!(candidate.effective_quota(), Some(60));
    assert_eq!(candidate.effective_quota_for_tiebreak(), 60);
}

#[test]
fn effective_quota_treats_free_as_full_capacity() {
    let mut candidate = sample_candidate();
    candidate.free = true;
    candidate.quota_percent = Some(5);
    assert_eq!(candidate.effective_quota(), Some(100));
}

#[test]
fn effective_quota_treats_quota_disabled_as_full_capacity() {
    let mut candidate = sample_candidate();
    candidate.quota_disabled = true;
    candidate.quota_percent = Some(5);
    assert_eq!(candidate.effective_quota(), Some(100));
}

#[test]
fn effective_quota_assumes_50_for_failed_subscription_with_unknown_quota() {
    // Spec §quota-failure plumbing: a candidate whose subscription
    // failed its quota fetch and has no fallback quota row is treated
    // as 50% so it stays selectable instead of being downranked to
    // unknown.
    let mut candidate = sample_candidate();
    candidate.quota_percent = None;
    candidate.quota_failed = true;
    assert_eq!(candidate.effective_quota(), Some(50));
}

#[test]
fn effective_quota_prefers_known_quota_over_failure_assumption() {
    // A successful per-model fetch beats the subscription-level
    // failure marker — the failure assumption is a *floor* for
    // unknown values, not an override.
    let mut candidate = sample_candidate();
    candidate.quota_percent = Some(72);
    candidate.quota_failed = true;
    assert_eq!(candidate.effective_quota(), Some(72));
}

#[test]
fn effective_quota_for_tiebreak_collapses_unknown_to_zero() {
    let mut candidate = sample_candidate();
    candidate.quota_percent = None;
    candidate.quota_failed = false;
    assert_eq!(candidate.effective_quota(), None);
    assert_eq!(candidate.effective_quota_for_tiebreak(), 0);
}
