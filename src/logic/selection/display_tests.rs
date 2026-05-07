use super::*;

fn ipbr_model(vendor: VendorKind, name: &str, score: f64, quota: Option<u8>) -> CachedModel {
    CachedModel {
        vendor,
        name: name.to_string(),
        overall_score: 85.0,
        current_score: 85.0,
        standard_error: 2.0,
        axes: Vec::new(),
        axis_provenance: std::collections::BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            idea: Some(score),
            planning: Some(score),
            build: Some(score),
            review: Some(score),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        ipbr_row_matched: true,
        ipbr_match_key: Some(name.to_string()),
        route_underlying_vendor: None,
        route_provider: None,
        quota_percent: quota,
        quota_resets_at: None,
        display_order: 0,
        fallback_from: None,
    }
}

fn unscored_model(vendor: VendorKind, name: &str, display_order: usize) -> CachedModel {
    CachedModel {
        vendor,
        name: name.to_string(),
        overall_score: 85.0,
        current_score: 85.0,
        standard_error: 2.0,
        axes: Vec::new(),
        axis_provenance: std::collections::BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        ipbr_row_matched: false,
        ipbr_match_key: None,
        route_underlying_vendor: None,
        route_provider: None,
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order,
        fallback_from: None,
    }
}

#[test]
fn visible_models_includes_top_3_per_phase_by_ipbr_score() {
    let models = vec![
        ipbr_model(VendorKind::Claude, "claude-a", 95.0, Some(80)),
        ipbr_model(VendorKind::Claude, "claude-b", 90.0, Some(80)),
        ipbr_model(VendorKind::Claude, "claude-c", 85.0, Some(80)),
        // Lowest-scored Claude model — outside the top-3 union for any
        // phase, even though quota is healthy.
        ipbr_model(VendorKind::Claude, "claude-d", 10.0, Some(100)),
    ];
    let visible = visible_models(&models);

    assert!(visible.contains("claude-a"));
    assert!(visible.contains("claude-b"));
    assert!(visible.contains("claude-c"));
    assert!(
        !visible.contains("claude-d"),
        "lowest-scored model should not enter top-3 across any phase"
    );
}

#[test]
fn visible_models_backfills_missing_vendors_via_display_order() {
    let models = vec![
        ipbr_model(VendorKind::Claude, "claude-top", 95.0, Some(80)),
        ipbr_model(VendorKind::Codex, "codex-top", 95.0, Some(80)),
        ipbr_model(VendorKind::Gemini, "gemini-top", 95.0, Some(80)),
        // Two unscored Kimi models: backfill must pick the one with the
        // lower `display_order`, ignoring cosmetic `current_score`.
        CachedModel {
            current_score: 60.0,
            display_order: 0,
            ..unscored_model(VendorKind::Kimi, "kimi-first", 0)
        },
        CachedModel {
            current_score: 99.0,
            display_order: 5,
            ..unscored_model(VendorKind::Kimi, "kimi-later", 5)
        },
    ];
    let visible = visible_models(&models);

    assert!(
        visible.contains("kimi-first"),
        "backfill should follow inventory display_order"
    );
    assert!(
        !visible.contains("kimi-later"),
        "cosmetic current_score must not promote a later inventory entry"
    );
}

#[test]
fn visible_models_inventory_only_model_remains_via_vendor_backfill() {
    // Spec: inventory/CLI-visible models stay visible even with no ipbr
    // score. The backfill rule is the visibility safety net.
    let models = vec![
        ipbr_model(VendorKind::Claude, "claude-top", 95.0, Some(80)),
        ipbr_model(VendorKind::Codex, "codex-top", 95.0, Some(80)),
        ipbr_model(VendorKind::Gemini, "gemini-top", 95.0, Some(80)),
        unscored_model(VendorKind::Kimi, "kimi-cli-only", 0),
    ];
    let visible = visible_models(&models);

    assert!(visible.contains("kimi-cli-only"));
}

#[test]
fn phase_rank_orders_by_ipbr_phase_score_descending() {
    let models = vec![
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(95.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(VendorKind::Claude, "top", 95.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(50.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(VendorKind::Codex, "mid", 50.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(10.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(VendorKind::Gemini, "low", 10.0, Some(80))
        },
    ];
    let ranks = phase_rank(&models, SelectionPhase::Build);

    assert_eq!(ranks.len(), 3);
    assert_eq!(ranks["top"], 1);
    assert_eq!(ranks["mid"], 2);
    assert_eq!(ranks["low"], 3);
}

#[test]
fn phase_rank_omits_unscored_and_non_ipbr_models() {
    // Unscored / cosmetic-only models render as unranked: they must
    // not appear in the rank map at all (callers treat absence as
    // "no rank for this phase").
    let mut cosmetic_only = unscored_model(VendorKind::Claude, "cosmetic", 0);
    cosmetic_only.score_source = crate::selection::ScoreSource::Aistupidlevel;
    cosmetic_only.ipbr_phase_scores = crate::selection::IpbrPhaseScores {
        build: Some(99.0),
        ..crate::selection::IpbrPhaseScores::default()
    };

    let models = vec![
        ipbr_model(VendorKind::Codex, "ranked", 80.0, Some(80)),
        unscored_model(VendorKind::Gemini, "inventory-only", 0),
        cosmetic_only,
    ];
    let ranks = phase_rank(&models, SelectionPhase::Build);

    assert_eq!(ranks.len(), 1);
    assert_eq!(ranks["ranked"], 1);
    assert!(!ranks.contains_key("inventory-only"));
    assert!(!ranks.contains_key("cosmetic"));
}

#[test]
fn phase_rank_dense_after_tie() {
    let models = vec![
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(90.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(VendorKind::Claude, "tie-a", 90.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(90.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(VendorKind::Codex, "tie-b", 90.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(50.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(VendorKind::Gemini, "lower", 50.0, Some(80))
        },
    ];
    let ranks = phase_rank(&models, SelectionPhase::Build);

    assert_eq!(ranks["tie-a"], 1);
    assert_eq!(ranks["tie-b"], 1);
    assert_eq!(ranks["lower"], 2);
}

#[test]
fn phase_rank_empty_when_no_models_or_no_scores() {
    assert!(phase_rank(&[], SelectionPhase::Build).is_empty());

    let unscored = vec![unscored_model(VendorKind::Claude, "a", 0)];
    assert!(phase_rank(&unscored, SelectionPhase::Build).is_empty());
}

#[test]
fn visible_models_empty_input() {
    assert!(visible_models(&[]).is_empty());
}
