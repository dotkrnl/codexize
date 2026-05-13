use super::*;
use crate::selection::{Candidate, CliKind};

fn ipbr_model(vendor: SubscriptionKind, name: &str, score: f64, quota: Option<u8>) -> CachedModel {
    let candidate = Candidate {
        subscription: vendor,
        cli: vendor.direct_cli().unwrap_or(CliKind::Codex),
        launch_name: name.to_string(),
        quota_percent: quota,
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
        subscription: vendor,
        name: name.to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            idea: Some(score),
            planning: Some(score),
            build: Some(score),
            review: Some(score),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        candidates: vec![candidate],
        selected_candidate: Some(0),
        quota_percent: quota,
        quota_resets_at: None,
        display_order: 0,
    }
}

fn unscored_model(vendor: SubscriptionKind, name: &str, display_order: usize) -> CachedModel {
    let candidate = Candidate {
        subscription: vendor,
        cli: vendor.direct_cli().unwrap_or(CliKind::Codex),
        launch_name: name.to_string(),
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order,
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
        subscription: vendor,
        name: name.to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        candidates: vec![candidate],
        selected_candidate: Some(0),
        quota_percent: Some(80),
        quota_resets_at: None,
        display_order,
    }
}

#[test]
fn visible_models_keeps_models_above_pool_weight_threshold() {
    // Three Claude models with bunched phase scores share the pool roughly
    // evenly (each weight ≫ 10%), while a much lower-scored peer's weight
    // collapses below the visibility threshold. The per-vendor floor still
    // admits one Claude row, but it picks the strongest, not the bottom.
    let models = vec![
        ipbr_model(SubscriptionKind::Claude, "claude-a", 95.0, Some(80)),
        ipbr_model(SubscriptionKind::Claude, "claude-b", 94.0, Some(80)),
        ipbr_model(SubscriptionKind::Claude, "claude-c", 93.0, Some(80)),
        ipbr_model(SubscriptionKind::Claude, "claude-d", 10.0, Some(100)),
    ];
    let visible = visible_models(&models);

    assert!(visible.contains("claude-a"));
    assert!(visible.contains("claude-b"));
    assert!(visible.contains("claude-c"));
    assert!(
        !visible.contains("claude-d"),
        "a model whose pool weight stays below 10% should not be visible"
    );
}

#[test]
fn visible_models_backfills_missing_vendors_by_build_rank() {
    let models = vec![
        ipbr_model(SubscriptionKind::Claude, "claude-top", 95.0, Some(80)),
        ipbr_model(SubscriptionKind::Codex, "codex-top", 95.0, Some(80)),
        ipbr_model(SubscriptionKind::Gemini, "gemini-top", 95.0, Some(80)),
        // Two Kimi models — the per-vendor floor must pick the higher
        // ipbr Build score, not the lower `display_order`.
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                idea: Some(40.0),
                planning: Some(40.0),
                build: Some(40.0),
                review: Some(40.0),
            },
            display_order: 0,
            ..ipbr_model(SubscriptionKind::Kimi, "kimi-weak", 40.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                idea: Some(60.0),
                planning: Some(60.0),
                build: Some(60.0),
                review: Some(60.0),
            },
            display_order: 5,
            ..ipbr_model(SubscriptionKind::Kimi, "kimi-strong", 60.0, Some(80))
        },
    ];
    let visible = visible_models(&models);

    assert!(
        visible.contains("kimi-strong"),
        "vendor floor should pick the highest ipbr Build score"
    );
    assert!(
        !visible.contains("kimi-weak"),
        "display_order should not promote the weaker peer"
    );
}

#[test]
fn visible_models_unscored_provider_model_remains_via_vendor_backfill() {
    // Spec: provider-backed models stay visible even with no ipbr
    // score. The backfill rule is the visibility safety net.
    let models = vec![
        ipbr_model(SubscriptionKind::Claude, "claude-top", 95.0, Some(80)),
        ipbr_model(SubscriptionKind::Codex, "codex-top", 95.0, Some(80)),
        ipbr_model(SubscriptionKind::Gemini, "gemini-top", 95.0, Some(80)),
        unscored_model(SubscriptionKind::Kimi, "kimi-cli-only", 0),
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
            ..ipbr_model(SubscriptionKind::Claude, "top", 95.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(50.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(SubscriptionKind::Codex, "mid", 50.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(10.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(SubscriptionKind::Gemini, "low", 10.0, Some(80))
        },
    ];
    let ranks = phase_rank(&models, SelectionPhase::Build);

    assert_eq!(ranks.len(), 3);
    assert_eq!(ranks["top"], 1);
    assert_eq!(ranks["mid"], 2);
    assert_eq!(ranks["low"], 3);
}

#[test]
fn phase_rank_omits_unscored_models() {
    // Unscored models render as unranked: they must not appear in the
    // rank map at all (callers treat absence as "no rank for this phase").
    let models = vec![
        ipbr_model(SubscriptionKind::Codex, "ranked", 80.0, Some(80)),
        unscored_model(SubscriptionKind::Gemini, "gemini-2.5-pro", 0),
    ];
    let ranks = phase_rank(&models, SelectionPhase::Build);

    assert_eq!(ranks.len(), 1);
    assert_eq!(ranks["ranked"], 1);
    assert!(!ranks.contains_key("gemini-2.5-pro"));
}

#[test]
fn phase_rank_dense_after_tie() {
    let models = vec![
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(90.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(SubscriptionKind::Claude, "tie-a", 90.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(90.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(SubscriptionKind::Codex, "tie-b", 90.0, Some(80))
        },
        CachedModel {
            ipbr_phase_scores: crate::selection::IpbrPhaseScores {
                build: Some(50.0),
                ..crate::selection::IpbrPhaseScores::default()
            },
            ..ipbr_model(SubscriptionKind::Gemini, "lower", 50.0, Some(80))
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

    let unscored = vec![unscored_model(SubscriptionKind::Claude, "a", 0)];
    assert!(phase_rank(&unscored, SelectionPhase::Build).is_empty());
}

