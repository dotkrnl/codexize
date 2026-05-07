use super::*;

fn sample_cached_model() -> CachedModel {
    CachedModel {
        vendor: VendorKind::Codex,
        name: "gpt-5.5".to_string(),
        overall_score: 88.4,
        current_score: 86.2,
        standard_error: 2.9,
        axes: vec![
            ("correctness".to_string(), 90.0),
            ("debugging".to_string(), 82.0),
        ],
        axis_provenance: BTreeMap::new(),
        ipbr_phase_scores: IpbrPhaseScores::default(),
        score_source: ScoreSource::None,
        ipbr_row_matched: false,
        quota_percent: Some(73),
        quota_resets_at: None,
        display_order: 2,
        fallback_from: Some("gpt-5".to_string()),
        ipbr_match_key: None,
        route_underlying_vendor: None,
    }
}

#[test]
fn cached_model_axis_returns_matching_value() {
    let model = sample_cached_model();

    assert_eq!(model.axis("correctness"), Some(90.0));
}

#[test]
fn cached_model_axis_returns_none_for_missing_key() {
    let model = sample_cached_model();

    assert_eq!(model.axis("safety"), None);
}

#[test]
fn cached_model_clone_and_fields_remain_accessible() {
    let model = sample_cached_model();
    let cloned = model.clone();

    assert_eq!(cloned, model);
    assert_eq!(cloned.vendor, VendorKind::Codex);
    assert_eq!(cloned.name, "gpt-5.5");
    assert_eq!(cloned.overall_score, 88.4);
    assert_eq!(cloned.current_score, 86.2);
    assert_eq!(cloned.standard_error, 2.9);
    assert_eq!(cloned.quota_percent, Some(73));
    assert_eq!(cloned.display_order, 2);
    assert_eq!(cloned.fallback_from.as_deref(), Some("gpt-5"));
}

#[test]
fn new_ipbr_fields_default_to_unscored_and_unmatched() {
    let model = sample_cached_model();

    assert_eq!(model.ipbr_phase_scores, IpbrPhaseScores::default());
    assert_eq!(model.ipbr_phase_scores.idea, None);
    assert_eq!(model.ipbr_phase_scores.planning, None);
    assert_eq!(model.ipbr_phase_scores.build, None);
    assert_eq!(model.ipbr_phase_scores.review, None);
    assert_eq!(model.score_source, ScoreSource::None);
    assert!(!model.ipbr_row_matched);
    assert_eq!(model.ipbr_match_key, None);
    assert_eq!(model.route_underlying_vendor, None);
}

#[test]
fn score_source_default_is_none_not_ipbr() {
    // The default MUST be a non-`Ipbr` value so freshly-constructed
    // entries cannot be mistaken for ipbr-authoritative data.
    let source = ScoreSource::default();
    assert_eq!(source, ScoreSource::None);
    assert_ne!(source, ScoreSource::Ipbr);
}
