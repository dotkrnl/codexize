use codexize::logic::selection::{
    SelectionStage,
    selection::pick_for_stage_with_seed,
    types::{CachedModel, Candidate, CliKind, IpbrStageScores, ScoreSource, SubscriptionKind},
};

fn sample_model(vendor: SubscriptionKind, name: &str, quota: u8) -> CachedModel {
    let candidate = Candidate {
        subscription: vendor,
        cli: vendor.direct_cli().unwrap_or(CliKind::Codex),
        launch_name: name.to_string(),
        quota_percent: Some(quota),
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free: false,
        official: true,
        quota_disabled: false,
        cheap_eligible: true,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: codexize::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    };
    CachedModel {
        subscription: vendor,
        name: name.to_string(),
        ipbr_stage_scores: IpbrStageScores {
            idea: Some(85.0),
            planning: Some(85.0),
            build: Some(85.0),
            review: Some(85.0),
        },
        score_source: ScoreSource::Ipbr,
        candidates: vec![candidate],
        selected_candidate: Some(0),
        quota_percent: Some(quota),
        quota_resets_at: None,
        display_order: 0,
    }
}

#[test]
fn pick_for_stage_with_seed_is_deterministic_without_clock_access() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "high", 80),
        sample_model(SubscriptionKind::Codex, "low", 1),
    ];
    let chosen = pick_for_stage_with_seed(&models, SelectionStage::Build, None, 1)
        .expect("should pick a model");
    assert_eq!(chosen.name, "high");
}
