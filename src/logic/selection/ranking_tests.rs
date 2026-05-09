use super::*;
use std::collections::BTreeMap;

fn sample_cached_model() -> CachedModel {
    CachedModel {
        vendor: SubscriptionKind::Codex,
        name: "gpt-5.5".to_string(),
        overall_score: 88.0,
        current_score: 86.0,
        standard_error: 2.0,
        axes: vec![
            ("correctness".to_string(), 0.9),
            ("debugging".to_string(), 0.85),
            ("codequality".to_string(), 0.88),
            ("safety".to_string(), 0.87),
        ],
        axis_provenance: BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        ipbr_match_key: None,
        candidates: Vec::new(),
        selected_candidate: None,
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order: 1,
        fallback_from: None,
    }
}

fn ipbr_model(name: &str, score: f64, quota_percent: Option<u8>) -> CachedModel {
    CachedModel {
        name: name.to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            idea: Some(score + 1.0),
            planning: Some(score + 2.0),
            build: Some(score),
            review: Some(score + 3.0),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        ipbr_row_matched: true,
        quota_percent,
        ..sample_cached_model()
    }
}

#[test]
fn phase_rank_score_maps_each_phase_to_ipbr_field() {
    let model = CachedModel {
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            idea: Some(11.0),
            planning: Some(22.0),
            build: Some(33.0),
            review: Some(44.0),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        ipbr_row_matched: true,
        ..sample_cached_model()
    };

    assert_eq!(phase_rank_score(&model, SelectionPhase::Idea), Some(11.0));
    assert_eq!(
        phase_rank_score(&model, SelectionPhase::Planning),
        Some(22.0)
    );
    assert_eq!(phase_rank_score(&model, SelectionPhase::Build), Some(33.0));
    assert_eq!(phase_rank_score(&model, SelectionPhase::Review), Some(44.0));
}

#[test]
fn phase_rank_score_returns_none_when_phase_score_or_ipbr_source_missing() {
    let missing_phase = CachedModel {
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            build: None,
            ..crate::selection::IpbrPhaseScores::default()
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        ipbr_row_matched: true,
        ..sample_cached_model()
    };
    let cosmetic_only = CachedModel {
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            build: Some(99.0),
            ..crate::selection::IpbrPhaseScores::default()
        },
        score_source: crate::selection::ScoreSource::Aistupidlevel,
        ipbr_row_matched: false,
        ..sample_cached_model()
    };

    assert_eq!(
        phase_rank_score(&missing_phase, SelectionPhase::Build),
        None
    );
    assert_eq!(
        phase_rank_score(&cosmetic_only, SelectionPhase::Build),
        None
    );
}

#[test]
fn candidate_pool_weights_softmax_matches_pairwise_calibration() {
    let high = ipbr_model("high", 90.0, Some(80));
    let gap_5_low = ipbr_model("gap-5-low", 85.0, Some(80));
    let gap_15_low = ipbr_model("gap-15-low", 75.0, Some(80));

    let gap_5_weights = candidate_pool_weights(&[&high, &gap_5_low], SelectionPhase::Build);
    let gap_5_low_share = gap_5_weights[1] / gap_5_weights.iter().sum::<f64>();
    assert!(
        (0.25..=0.30).contains(&gap_5_low_share),
        "5-point gap lower-score share should be 25-30%, got {gap_5_low_share}"
    );

    let gap_15_weights = candidate_pool_weights(&[&high, &gap_15_low], SelectionPhase::Build);
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
        candidate_pool_weights(&[&known_best, &unknown, &exhausted], SelectionPhase::Build);

    assert!(weights[0] > 0.0);
    assert!(weights[1] > 0.0);
    assert_eq!(weights[2], 0.0);
    assert!((weights[0] - weights[1]).abs() < 1e-9);
}

#[test]
fn candidate_pool_weights_all_unknown_quota_has_uniform_quota_factor() {
    let a = ipbr_model("a", 90.0, None);
    let b = ipbr_model("b", 90.0, None);
    let weights = candidate_pool_weights(&[&a, &b], SelectionPhase::Build);

    assert!((weights[0] - weights[1]).abs() < 1e-9);
    assert!(weights.iter().all(|weight| *weight > 0.0));
}

#[test]
fn phase_score_for_legacy_callers_returns_ipbr_phase_score() {
    let mut high_variance_old_flash = ipbr_model("gemini-2.5-flash", 90.0, Some(80));
    high_variance_old_flash.vendor = SubscriptionKind::Gemini;
    high_variance_old_flash.standard_error = 99.0;
    let low_variance_pro = CachedModel {
        vendor: SubscriptionKind::Gemini,
        name: "gemini-2.5-pro".to_string(),
        standard_error: 0.0,
        ..ipbr_model("gemini-2.5-pro", 80.0, Some(80))
    };

    let flash_score =
        phase_score_for_legacy_callers(&high_variance_old_flash, SelectionPhase::Build);
    let pro_score = phase_score_for_legacy_callers(&low_variance_pro, SelectionPhase::Build);

    assert_eq!(flash_score, 90.0);
    assert_eq!(pro_score, 80.0);
}

#[test]
fn phase_score_for_legacy_callers_excludes_zero_quota_and_unranked_models() {
    let exhausted = ipbr_model("exhausted", 90.0, Some(0));
    let unranked = sample_cached_model();

    assert_eq!(
        phase_score_for_legacy_callers(&exhausted, SelectionPhase::Build),
        0.0
    );
    assert_eq!(
        phase_score_for_legacy_callers(&unranked, SelectionPhase::Build),
        0.0
    );
}

fn selection_counter_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[test]
fn zero_as_missing_fires_counter_and_rewrites_provenance() {
    let _guard = selection_counter_lock();
    clear_selection_events();
    let mut model = CachedModel {
        axes: vec![
            ("codequality".to_string(), 0.85),
            ("correctness".to_string(), 0.0),
            ("debugging".to_string(), 0.85),
            ("safety".to_string(), 0.85),
        ],
        axis_provenance: BTreeMap::from([
            ("codequality".to_string(), "suite:deep".to_string()),
            ("correctness".to_string(), "suite:deep".to_string()),
            ("debugging".to_string(), "suite:deep".to_string()),
            ("safety".to_string(), "suite:deep".to_string()),
        ]),
        ..sample_cached_model()
    };
    stamp_selection_provenance(&mut model);
    let events = selection_events_snapshot();

    // correctness=0.0 appears in Planning, Build, Review → 3 events
    let correctness_events: Vec<_> = events
        .iter()
        .filter(
            |e| matches!(e, SelectionEvent::ZeroAsMissing { axis, .. } if axis == "correctness"),
        )
        .collect();
    assert_eq!(
        correctness_events.len(),
        3,
        "expected 3 events (Planning, Build, Review), got {correctness_events:?}"
    );

    // Each (axis, phase) combo fires exactly once
    assert!(events.iter().any(|e| matches!(
        e,
        SelectionEvent::ZeroAsMissing { axis, phase }
            if axis == "correctness" && phase == "build"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        SelectionEvent::ZeroAsMissing { axis, phase }
            if axis == "correctness" && phase == "planning"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        SelectionEvent::ZeroAsMissing { axis, phase }
            if axis == "correctness" && phase == "review"
    )));

    // Provenance rewritten
    assert_eq!(
        model.axis_provenance.get("correctness").map(String::as_str),
        Some("fallback:overall")
    );
    // Non-zero axes keep their original provenance
    assert_eq!(
        model.axis_provenance.get("codequality").map(String::as_str),
        Some("suite:deep")
    );
}

#[test]
fn truly_missing_axis_gets_fallback_overall_provenance() {
    let _guard = selection_counter_lock();
    clear_selection_events();
    let mut model = CachedModel {
        axes: vec![
            ("codequality".to_string(), 0.85),
            ("debugging".to_string(), 0.85),
            ("safety".to_string(), 0.85),
            // correctness entirely absent
        ],
        axis_provenance: BTreeMap::from([
            ("codequality".to_string(), "suite:deep".to_string()),
            ("debugging".to_string(), "suite:deep".to_string()),
            ("safety".to_string(), "suite:deep".to_string()),
        ]),
        ..sample_cached_model()
    };
    stamp_selection_provenance(&mut model);

    assert_eq!(
        model.axis_provenance.get("correctness").map(String::as_str),
        Some("fallback:overall")
    );
    // Truly-missing does NOT fire zero_as_missing counter
    let events = selection_events_snapshot();
    assert!(
        !events.iter().any(|e| matches!(
            e,
            SelectionEvent::ZeroAsMissing { axis, .. } if axis == "correctness"
        )),
        "truly-missing axis should not fire zero_as_missing"
    );
}
