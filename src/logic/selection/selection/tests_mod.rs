use super::*;
use crate::selection::types::{Candidate, CliKind, IpbrStageScores, ScoreSource};

/// Derive cheap/tough/effort eligibility flags from a model name, used
/// by the `sample_model` fixture to seed a single Candidate that
/// `is_cheap_eligible` / `is_tough_eligible` can read. Mirrors the
/// pre-task-2 vendor.rs heuristics so the existing test contract keeps
/// asserting the same eligibility outcomes — the heuristic now lives in
/// fixture code instead of production selection logic.
fn eligibility_for_name(vendor: SubscriptionKind, name: &str) -> (bool, bool) {
    let lower = name.to_lowercase();
    let cheap = match vendor {
        SubscriptionKind::Claude => !lower.contains("opus"),
        SubscriptionKind::Codex | SubscriptionKind::Kimi => true,
        SubscriptionKind::Gemini => lower.contains("flash") || lower.contains("nano"),
        SubscriptionKind::OpencodeGo | SubscriptionKind::Direct => true,
    };
    let tough = match vendor {
        SubscriptionKind::Claude => lower.contains("opus"),
        SubscriptionKind::Codex => true,
        SubscriptionKind::Kimi
        | SubscriptionKind::Gemini
        | SubscriptionKind::OpencodeGo
        | SubscriptionKind::Direct => false,
    };
    (cheap, tough)
}

fn sample_model(vendor: SubscriptionKind, name: &str, quota: u8) -> CachedModel {
    sample_model_with_score(vendor, name, quota, 85.0)
}

/// Like [`sample_model`] but lets the caller pin the ipbr stage score.
/// Useful for tests that want to differentiate models by rank without
/// touching quota.
fn sample_model_with_score(
    vendor: SubscriptionKind,
    name: &str,
    quota: u8,
    score: f64,
) -> CachedModel {
    let (cheap_eligible, tough_eligible) = eligibility_for_name(vendor, name);
    let cli = vendor.direct_cli().unwrap_or(CliKind::Opencode);
    let candidate = Candidate {
        subscription: vendor,
        cli,
        launch_name: name.to_string(),
        quota_percent: Some(quota),
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free: false,
        official: vendor != SubscriptionKind::Direct,
        quota_disabled: false,
        cheap_eligible,
        tough_eligible,
        effort_eligible: matches!(vendor, SubscriptionKind::Claude | SubscriptionKind::Codex),
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    };
    CachedModel {
        subscription: vendor,
        name: name.to_string(),
        ipbr_stage_scores: IpbrStageScores {
            idea: Some(score),
            planning: Some(score),
            build: Some(score),
            review: Some(score),
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
fn pick_for_stage_returns_none_for_empty() {
    let result = pick_for_stage(&[], SelectionStage::Build, None);
    assert!(result.is_none());
}

#[test]
fn pick_for_stage_low_quota_loses_to_high_quota_via_pool_factor() {
    // High-quota model has quota_factor 1.0; low-quota has factor 0.10
    // (deficit 79 → ≥40 floor). Both have the same ipbr score, so the
    // weighted sample with seed=1 deterministically chooses "high".
    let models = vec![
        sample_model(SubscriptionKind::Claude, "high", 80),
        sample_model(SubscriptionKind::Codex, "low", 1),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        pick_for_stage(&models, SelectionStage::Build, None).expect("should pick high-quota model");
    assert_eq!(chosen.name, "high");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_for_stage_excludes_known_zero_quota() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "exhausted", 0),
        sample_model(SubscriptionKind::Codex, "available", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_stage(&models, SelectionStage::Build, None)
        .expect("non-exhausted candidate exists");
    assert_eq!(chosen.name, "available");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_for_stage_excludes_models_missing_stage_score() {
    let mut models = vec![
        sample_model(SubscriptionKind::Claude, "ranked", 80),
        sample_model(SubscriptionKind::Codex, "missing-build", 80),
    ];
    // Strip the Build stage score from the second model — it should be
    // unselectable for Build but its presence in the slice must not
    // poison the pool.
    models[1].ipbr_stage_scores.build = None;
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        pick_for_stage(&models, SelectionStage::Build, None).expect("ranked candidate exists");
    assert_eq!(chosen.name, "ranked");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_for_stage_unknown_quota_remains_selectable() {
    // None quota → effective 30 inside the pool scorer. With only one
    // candidate the effective-30 baseline still produces a non-zero weight.
    let mut model = sample_model(SubscriptionKind::Claude, "unknown-quota", 0);
    model.quota_percent = None;
    // Mirror the row's "unknown" state on the per-tuple Candidate — the
    // sampler now reads the row's max effective quota across enabled
    // providers, so a stale `Some(0)` on the candidate would wrongly
    // mark this row as exhausted instead of unknown.
    model.candidates[0].quota_percent = None;
    let models = vec![model];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_stage(&models, SelectionStage::Build, None)
        .expect("unknown quota stays selectable");
    assert_eq!(chosen.name, "unknown-quota");
    assert_eq!(chosen.quota_percent, None);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_for_stage_returns_none_when_pool_empty_after_exclusions() {
    // Every candidate is either exhausted or unranked for the stage;
    // the function must surface the no-eligible-model condition rather
    // than fall back to unscored data.
    let mut unranked = sample_model(SubscriptionKind::Claude, "unranked", 80);
    unranked.ipbr_stage_scores = IpbrStageScores::default();
    unranked.score_source = ScoreSource::None;
    let models = vec![
        sample_model(SubscriptionKind::Claude, "exhausted", 0),
        unranked,
    ];
    let chosen = pick_for_stage(&models, SelectionStage::Build, None);
    assert!(chosen.is_none());
}

#[test]
fn pick_for_stage_respects_vendor_filter() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-model", 80),
        sample_model(SubscriptionKind::Codex, "codex-model", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_stage(
        &models,
        SelectionStage::Build,
        Some(SubscriptionKind::Claude),
    )
    .expect("should pick claude");
    assert_eq!(chosen.subscription, SubscriptionKind::Claude);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_prefers_fresh_vendor() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-1", 80),
        sample_model(SubscriptionKind::Codex, "codex-1", 80),
    ];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-1".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        select_for_review(&models, &used_vendors, &used_models).expect("should pick fresh vendor");
    assert_eq!(chosen.subscription, SubscriptionKind::Codex);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_falls_back_to_unused_model_same_vendor() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-1", 80),
        sample_model(SubscriptionKind::Claude, "claude-2", 80),
    ];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-1".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        select_for_review(&models, &used_vendors, &used_models).expect("should pick unused model");
    assert_eq!(chosen.name, "claude-2");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_returns_none_when_all_used() {
    let models = vec![sample_model(SubscriptionKind::Claude, "claude-1", 80)];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-1".to_string())];

    let chosen = select_for_review(&models, &used_vendors, &used_models);
    assert!(chosen.is_none());
}

#[test]
fn select_excluding_excludes_listed_models() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "excluded", 80),
        sample_model(SubscriptionKind::Codex, "included", 80),
    ];
    let excluded = vec![(SubscriptionKind::Claude, "excluded".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_excluding(&models, SelectionStage::Build, &excluded, None)
        .expect("should pick non-excluded");
    assert_eq!(chosen.name, "included");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_excluding_returns_none_when_all_excluded() {
    let models = vec![sample_model(SubscriptionKind::Claude, "model-1", 80)];
    let excluded = vec![(SubscriptionKind::Claude, "model-1".to_string())];

    let chosen = select_excluding(&models, SelectionStage::Build, &excluded, None);
    assert!(chosen.is_none());
}

#[test]
fn weighted_sample_returns_none_for_empty() {
    let candidates: Vec<(&CachedModel, f64)> = vec![];
    assert!(weighted_sample(&candidates, 1).is_none());
}

#[test]
fn weighted_sample_uses_weights_for_random() {
    let m1 = sample_model(SubscriptionKind::Claude, "high-weight", 80);
    let m2 = sample_model(SubscriptionKind::Codex, "low-weight", 80);
    let candidates = vec![(&m1, 1000.0), (&m2, 1.0)];

    let mut high_count = 0;
    for seed in 1..100_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = weighted_sample(&candidates, seed).expect("should pick");
        if chosen.name == "high-weight" {
            high_count += 1;
        }
    }

    assert!(high_count > 90);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pool_pick_sampler_dominates_for_free_row_over_lower_fetched_quota() {
    // Spec §"Selection algorithm" (operator decision, 2026-05-09): the
    // sampler's quota input is the row's effective quota across enabled
    // providers. With dashboard scores tied at 80.0, the free row's
    // effective quota of 100 vastly outweighs the fetched row's 40 — the
    // sampler should pick the free row in nearly every seed.
    let mut free_row = sample_model_with_score(SubscriptionKind::OpencodeGo, "free-row", 0, 80.0);
    free_row.candidates[0].free = true;
    free_row.candidates[0].official = false;
    free_row.candidates[0].quota_percent = None;
    free_row.quota_percent = Some(100);
    let fetched_row = sample_model_with_score(SubscriptionKind::Codex, "fetched-row", 40, 80.0);

    let models = vec![free_row, fetched_row];
    let mut free_count = 0;
    for seed in 1..200_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = pick_for_stage(&models, SelectionStage::Build, None).expect("non-empty pool");
        if chosen.name == "free-row" {
            free_count += 1;
        }
    }
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);

    assert!(
        free_count > 180,
        "free row should dominate the sampler when its row-level effective_quota=100 \
         vs fetched-row's 40 (got {free_count}/199)"
    );
}

#[test]
fn pool_pick_sampler_dominates_for_quota_disabled_row_over_lower_fetched_quota() {
    // `quota_disabled = true` forces 100%; same sampler-dominance
    // contract as the free-row test above.
    let mut disabled_row =
        sample_model_with_score(SubscriptionKind::Codex, "disabled-row", 0, 80.0);
    disabled_row.candidates[0].quota_disabled = true;
    disabled_row.candidates[0].quota_percent = None;
    disabled_row.quota_percent = Some(100);
    let fetched_row = sample_model_with_score(SubscriptionKind::Claude, "fetched-row", 30, 80.0);

    let models = vec![disabled_row, fetched_row];
    let mut disabled_count = 0;
    for seed in 1..200_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = pick_for_stage(&models, SelectionStage::Build, None).expect("non-empty pool");
        if chosen.name == "disabled-row" {
            disabled_count += 1;
        }
    }
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);

    assert!(
        disabled_count > 180,
        "quota_disabled row should dominate the sampler at 100% headroom \
         (got {disabled_count}/199)"
    );
}

fn opus_sonnet_codex_kimi() -> Vec<CachedModel> {
    vec![
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 80),
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(SubscriptionKind::Claude, "claude-haiku-4-5", 80),
        sample_model(SubscriptionKind::Codex, "gpt-5.5", 80),
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
    ]
}

#[test]
fn pick_with_effort_normal_does_not_filter_ineligible_models() {
    let models = vec![
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5", 80),
    ];
    let chosen = pick_for_stage_with_effort(
        &models,
        SelectionStage::Build,
        None,
        EffortLevel::Normal,
        false,
    )
    .expect("Normal must pick from non-empty slice");
    assert!(matches!(
        chosen.subscription,
        SubscriptionKind::Kimi | SubscriptionKind::Gemini
    ));
}

#[test]
fn pick_with_effort_low_does_not_use_tough_filter() {
    let models = vec![
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5", 80),
    ];
    let chosen = pick_for_stage_with_effort(
        &models,
        SelectionStage::Build,
        None,
        EffortLevel::Low,
        false,
    )
    .expect("Low effort must use the non-tough path until cheap filtering is wired");
    assert!(matches!(
        chosen.subscription,
        SubscriptionKind::Kimi | SubscriptionKind::Gemini
    ));
}

#[test]
fn pick_with_effort_cheap_filters_to_budget_subset() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5-pro", 80),
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5-flash", 80),
    ];
    for seed in 1..100_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = pick_for_stage_with_effort(
            &models,
            SelectionStage::Build,
            None,
            EffortLevel::Tough,
            true,
        )
        .expect("cheap candidate exists");
        assert_eq!(chosen.warning, None);
        assert!(
            matches!(
                chosen.model.name.as_str(),
                "claude-sonnet-4-6" | "gemini-2.5-flash"
            ),
            "cheap selection must not pick {:?} {}",
            chosen.model.subscription,
            chosen.model.name
        );
    }
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_cheap_fallback_warns_when_eligible_quota_empty() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 0),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5-flash", 0),
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        pick_for_stage_with_effort(&models, SelectionStage::Build, None, EffortLevel::Low, true)
            .expect("full-pool fallback must yield a candidate");
    assert_eq!(chosen.model.name, "claude-opus-4.7");
    assert_eq!(
        chosen.warning,
        Some(SelectionWarning::CheapFallback {
            stage: SelectionStage::Build,
            reason: "no_eligible_with_quota",
        })
    );
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_tough_only_picks_eligible() {
    let models = opus_sonnet_codex_kimi();
    for seed in 1..200_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = pick_for_stage_with_effort(
            &models,
            SelectionStage::Build,
            None,
            EffortLevel::Tough,
            false,
        )
        .expect("eligible candidate exists");
        assert!(
            (chosen.subscription == SubscriptionKind::Claude && chosen.name.contains("opus"))
                || chosen.subscription == SubscriptionKind::Codex,
            "tough must never pick {:?} {}",
            chosen.subscription,
            chosen.name
        );
    }
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_tough_falls_back_to_kimi_gemini() {
    let models = vec![
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_stage_with_effort(
        &models,
        SelectionStage::Build,
        None,
        EffortLevel::Tough,
        false,
    )
    .expect("degraded fallback must yield a candidate");
    assert!(matches!(
        chosen.subscription,
        SubscriptionKind::Kimi | SubscriptionKind::Gemini
    ));
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_tough_falls_back_to_sonnet_haiku() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(SubscriptionKind::Claude, "claude-haiku-4-5", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_stage_with_effort(
        &models,
        SelectionStage::Build,
        None,
        EffortLevel::Tough,
        false,
    )
    .expect("degraded fallback must yield a candidate");
    assert_eq!(chosen.subscription, SubscriptionKind::Claude);
    assert!(!chosen.name.contains("opus"));
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_normal_can_pick_ineligible_vendor() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 80),
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
    ];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-opus-4.7".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        EffortLevel::Normal,
        false,
    )
    .expect("Normal review picks fresh Kimi");
    assert_eq!(chosen.subscription, SubscriptionKind::Kimi);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_low_can_pick_ineligible_vendor() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 80),
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
    ];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-opus-4.7".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        EffortLevel::Low,
        false,
    )
    .expect("Low review effort must use the non-tough path");
    assert_eq!(chosen.subscription, SubscriptionKind::Kimi);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_cheap_reuses_used_eligible_before_expensive_fresh_model() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5-pro", 80),
    ];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-sonnet-4-6".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        select_for_review_with_effort(&models, &used_vendors, &used_models, EffortLevel::Low, true)
            .expect("cheap reviewer should reuse the only eligible model");
    assert_eq!(chosen.warning, None);
    assert_eq!(chosen.model.name, "claude-sonnet-4-6");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_cheap_fallback_warns_when_eligible_quota_empty() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 0),
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 0),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5-pro", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(&models, &[], &[], EffortLevel::Low, true)
        .expect("full-pool fallback must yield a reviewer");
    assert_eq!(chosen.model.name, "gemini-2.5-pro");
    assert_eq!(
        chosen.warning,
        Some(SelectionWarning::CheapFallback {
            stage: SelectionStage::Review,
            reason: "no_eligible_with_quota",
        })
    );
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_tough_reuses_opus_over_fresh_sonnet() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 80),
        sample_model(SubscriptionKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
    ];
    let used_vendors = vec![SubscriptionKind::Claude];
    let used_models = vec![(SubscriptionKind::Claude, "claude-opus-4.7".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        EffortLevel::Tough,
        false,
    )
    .expect("eligibility-dominated reuse expected");
    assert_eq!(chosen.subscription, SubscriptionKind::Claude);
    assert_eq!(chosen.name, "claude-opus-4.7");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_tough_degrades_when_no_eligible_remain() {
    let models = vec![
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(&models, &[], &[], EffortLevel::Tough, false)
        .expect("degraded fallback must yield a candidate");
    assert!(matches!(
        chosen.subscription,
        SubscriptionKind::Kimi | SubscriptionKind::Gemini
    ));
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_tough_degrades_when_eligible_have_zero_probability() {
    let models = vec![
        sample_model(SubscriptionKind::Claude, "claude-opus-4.7", 0),
        sample_model(SubscriptionKind::Codex, "gpt-5.5", 0),
        sample_model(SubscriptionKind::Kimi, "kimi-k2", 80),
        sample_model(SubscriptionKind::Gemini, "gemini-2.5", 80),
    ];
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(&models, &[], &[], EffortLevel::Tough, false)
        .expect("degraded fallback must yield an available candidate");
    assert!(
        matches!(
            chosen.subscription,
            SubscriptionKind::Kimi | SubscriptionKind::Gemini
        ),
        "exhausted tough-eligible model was selected: {:?} {}",
        chosen.subscription,
        chosen.name
    );
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}
