use codexize::logic::selection::{
    SelectionPhase,
    selection::pick_for_phase_with_seed,
    types::{CachedModel, IpbrPhaseScores, ScoreSource, SubscriptionKind},
};

fn sample_model(vendor: SubscriptionKind, name: &str, quota: u8) -> CachedModel {
    CachedModel {
        subscription: vendor,
        name: name.to_string(),
        ipbr_phase_scores: IpbrPhaseScores {
            idea: Some(85.0),
            planning: Some(85.0),
            build: Some(85.0),
            review: Some(85.0),
        },
        score_source: ScoreSource::Ipbr,
        ipbr_row_matched: true,
        ipbr_match_key: Some(name.to_string()),
        candidates: Vec::new(),
        selected_candidate: None,
        quota_percent: Some(quota),
        quota_resets_at: None,
        display_order: 0,
    }
}

#[test]
fn pick_for_phase_with_seed_is_deterministic_without_clock_access() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "high", 80),
        sample_model(SubscriptionKind::Codex, "low", 1),
    ];
    let chosen = pick_for_phase_with_seed(&models, SelectionPhase::Build, None, 1)
        .expect("should pick a model");
    assert_eq!(chosen.name, "high");
}
