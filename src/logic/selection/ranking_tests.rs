use super::*;
use crate::selection::{Candidate, CliKind};

fn sample_cached_model() -> CachedModel {
    CachedModel {
        subscription: SubscriptionKind::Codex,
        name: "gpt-5.5".to_string(),
        ipbr_stage_scores: crate::selection::IpbrStageScores::default(),
        score_source: crate::selection::ScoreSource::None,
        candidates: Vec::new(),
        selected_candidate: None,
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 1,
    }
}

fn ipbr_model(name: &str, score: f64, quota_percent: Option<u8>) -> CachedModel {
    let candidate = Candidate {
        subscription: SubscriptionKind::Codex,
        cli: CliKind::Codex,
        launch_name: name.to_string(),
        quota_percent,
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free: false,
        official: true,
        quota_disabled: false,
        cheap_eligible: true,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    };
    CachedModel {
        name: name.to_string(),
        ipbr_stage_scores: crate::selection::IpbrStageScores {
            idea: Some(score + 1.0),
            planning: Some(score + 2.0),
            build: Some(score),
            review: Some(score + 3.0),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        candidates: vec![candidate],
        selected_candidate: Some(0),
        quota_percent,
        ..sample_cached_model()
    }
}

#[test]
fn stage_rank_score_maps_each_stage_to_ipbr_field() {
    let model = CachedModel {
        ipbr_stage_scores: crate::selection::IpbrStageScores {
            idea: Some(11.0),
            planning: Some(22.0),
            build: Some(33.0),
            review: Some(44.0),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        ..sample_cached_model()
    };

    assert_eq!(stage_rank_score(&model, SelectionStage::Idea), Some(11.0));
    assert_eq!(
        stage_rank_score(&model, SelectionStage::Planning),
        Some(22.0)
    );
    assert_eq!(stage_rank_score(&model, SelectionStage::Build), Some(33.0));
    assert_eq!(stage_rank_score(&model, SelectionStage::Review), Some(44.0));
}

#[test]
fn stage_rank_score_returns_none_when_stage_score_or_ipbr_source_missing() {
    let missing_stage = CachedModel {
        ipbr_stage_scores: crate::selection::IpbrStageScores {
            build: None,
            ..crate::selection::IpbrStageScores::default()
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        ..sample_cached_model()
    };
    let unranked = CachedModel {
        ipbr_stage_scores: crate::selection::IpbrStageScores {
            build: Some(99.0),
            ..crate::selection::IpbrStageScores::default()
        },
        score_source: crate::selection::ScoreSource::None,
        ..sample_cached_model()
    };

    assert_eq!(
        stage_rank_score(&missing_stage, SelectionStage::Build),
        None
    );
    assert_eq!(stage_rank_score(&unranked, SelectionStage::Build), None);
}

#[test]
fn candidate_pool_weights_softmax_matches_pairwise_calibration() {
    let high = ipbr_model("high", 90.0, Some(80));
    let gap_5_low = ipbr_model("gap-5-low", 85.0, Some(80));
    let gap_15_low = ipbr_model("gap-15-low", 75.0, Some(80));

    let gap_5_weights = candidate_pool_weights(&[&high, &gap_5_low], SelectionStage::Build);
    let gap_5_low_share = gap_5_weights[1] / gap_5_weights.iter().sum::<f64>();
    assert!(
        (0.25..=0.30).contains(&gap_5_low_share),
        "5-point gap lower-score share should be 25-30%, got {gap_5_low_share}"
    );

    let gap_15_weights = candidate_pool_weights(&[&high, &gap_15_low], SelectionStage::Build);
    let gap_15_low_share = gap_15_weights[1] / gap_15_weights.iter().sum::<f64>();
    assert!(
        (0.06..=0.08).contains(&gap_15_low_share),
        "15-point gap lower-score share should be 6-8%, got {gap_15_low_share}"
    );
}

#[test]
fn relative_quota_factor_uses_smooth_deficit_curve() {
    assert_eq!(relative_quota_factor(20), 1.0);
    assert!((relative_quota_factor(30) - 0.55).abs() <= 0.03);
    assert_eq!(relative_quota_factor(40), 0.10);
    assert_eq!(relative_quota_factor(80), 0.10);
}

#[test]
fn candidate_pool_weights_keeps_unknown_quota_selectable_as_effective_30() {
    let known_best = ipbr_model("known-best", 90.0, Some(50));
    let unknown = ipbr_model("unknown", 90.0, None);
    let exhausted = ipbr_model("exhausted", 90.0, Some(0));

    let weights =
        candidate_pool_weights(&[&known_best, &unknown, &exhausted], SelectionStage::Build);

    assert!(weights[0] > 0.0);
    assert!(weights[1] > 0.0);
    assert_eq!(weights[2], 0.0);
    assert!((weights[0] - weights[1]).abs() < 1e-9);
}

#[test]
fn candidate_pool_weights_all_unknown_quota_has_uniform_quota_factor() {
    let a = ipbr_model("a", 90.0, None);
    let b = ipbr_model("b", 90.0, None);
    let weights = candidate_pool_weights(&[&a, &b], SelectionStage::Build);

    assert!((weights[0] - weights[1]).abs() < 1e-9);
    assert!(weights.iter().all(|weight| *weight > 0.0));
}
