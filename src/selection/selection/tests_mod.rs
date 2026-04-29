use super::super::ranking::build_version_index;
use super::*;

fn sample_model(vendor: VendorKind, name: &str, quota: u8) -> CachedModel {
    CachedModel {
        vendor,
        name: name.to_string(),
        overall_score: 85.0,
        current_score: 85.0,
        standard_error: 2.0,
        axes: vec![
            ("codequality".to_string(), 0.85),
            ("correctness".to_string(), 0.85),
            ("debugging".to_string(), 0.85),
            ("safety".to_string(), 0.85),
        ],
        axis_provenance: std::collections::BTreeMap::new(),
        quota_percent: Some(quota),
        display_order: 0,
        fallback_from: None,
    }
}

#[test]
fn pick_for_phase_returns_none_for_empty() {
    let index = build_version_index(&[]);
    let result = pick_for_phase(&[], SelectionPhase::Build, None, &index);
    assert!(result.is_none());
}

#[test]
fn pick_for_phase_applies_relative_cutoff() {
    let models = vec![
        sample_model(VendorKind::Claude, "high", 80),
        sample_model(VendorKind::Codex, "low", 1), // Very low quota
    ];
    let index = build_version_index(&models);

    // With cutoff ratio 1/3 and quota=1, the low-quota model should be excluded
    // quota_weight(1) ≈ 0.0016, quota_weight(80) = 1.0, ratio < 1/3
    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_phase(&models, SelectionPhase::Build, None, &index)
        .expect("should pick high-quota model");
    assert_eq!(chosen.name, "high");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_for_phase_respects_vendor_filter() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-model", 80),
        sample_model(VendorKind::Codex, "codex-model", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_phase(
        &models,
        SelectionPhase::Build,
        Some(VendorKind::Claude),
        &index,
    )
    .expect("should pick claude");
    assert_eq!(chosen.vendor, VendorKind::Claude);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_for_phase_uses_unfiltered_max_for_cutoff() {
    // High-prob Claude model sets the cutoff bar, even when filtering to Codex
    let models = vec![
        sample_model(VendorKind::Claude, "high-claude", 90),
        sample_model(VendorKind::Codex, "medium-codex", 50),
        sample_model(VendorKind::Codex, "low-codex", 10),
    ];
    let index = build_version_index(&models);

    // Filter to Codex only — cutoff still based on Claude's high prob
    let chosen = pick_for_phase(
        &models,
        SelectionPhase::Build,
        Some(VendorKind::Codex),
        &index,
    );

    // medium-codex should survive cutoff; low-codex should not
    assert!(chosen.is_some());
    assert_eq!(chosen.unwrap().vendor, VendorKind::Codex);
}

#[test]
fn select_for_review_prefers_fresh_vendor() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-1", 80),
        sample_model(VendorKind::Codex, "codex-1", 80),
    ];
    let index = build_version_index(&models);

    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-1".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review(&models, &used_vendors, &used_models, &index)
        .expect("should pick fresh vendor");
    assert_eq!(chosen.vendor, VendorKind::Codex);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_falls_back_to_unused_model_same_vendor() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-1", 80),
        sample_model(VendorKind::Claude, "claude-2", 80),
    ];
    let index = build_version_index(&models);

    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-1".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review(&models, &used_vendors, &used_models, &index)
        .expect("should pick unused model");
    assert_eq!(chosen.name, "claude-2");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_returns_none_when_all_used() {
    let models = vec![sample_model(VendorKind::Claude, "claude-1", 80)];
    let index = build_version_index(&models);

    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-1".to_string())];

    let chosen = select_for_review(&models, &used_vendors, &used_models, &index);
    assert!(chosen.is_none());
}

#[test]
fn select_excluding_excludes_listed_models() {
    let models = vec![
        sample_model(VendorKind::Claude, "excluded", 80),
        sample_model(VendorKind::Codex, "included", 80),
    ];
    let index = build_version_index(&models);

    let excluded = vec![(VendorKind::Claude, "excluded".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_excluding(&models, SelectionPhase::Build, &excluded, None, &index)
        .expect("should pick non-excluded");
    assert_eq!(chosen.name, "included");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_excluding_applies_diversity_bonus() {
    let models = vec![
        sample_model(VendorKind::Claude, "same-vendor", 80),
        sample_model(VendorKind::Codex, "other-vendor", 80),
    ];
    let index = build_version_index(&models);

    // Both have same quota, but Codex gets 1.3× diversity bonus
    // With bonus, Codex has 1.3× the probability, so should win most samples
    let mut codex_count = 0;
    for seed in 1000..1100_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chose_codex = select_excluding(
            &models,
            SelectionPhase::Build,
            &[],
            Some(VendorKind::Claude),
            &index,
        )
        .is_some_and(|chosen| chosen.vendor == VendorKind::Codex);
        if chose_codex {
            codex_count += 1;
        }
    }

    // Codex should win at least 50% of the time (actual ratio should be ~1.3:1 or 56.5%)
    assert!(codex_count > 50, "Codex won {} out of 100", codex_count);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_excluding_returns_none_when_all_excluded() {
    let models = vec![sample_model(VendorKind::Claude, "model-1", 80)];
    let index = build_version_index(&models);

    let excluded = vec![(VendorKind::Claude, "model-1".to_string())];

    let chosen = select_excluding(&models, SelectionPhase::Build, &excluded, None, &index);
    assert!(chosen.is_none());
}

#[test]
fn weighted_sample_returns_none_for_empty() {
    let candidates: Vec<(&CachedModel, f64)> = vec![];
    assert!(weighted_sample(&candidates).is_none());
}

#[test]
fn weighted_sample_returns_first_when_all_zero_weight() {
    let m1 = sample_model(VendorKind::Claude, "first", 80);
    let m2 = sample_model(VendorKind::Codex, "second", 80);
    let candidates = vec![(&m1, 0.0), (&m2, 0.0)];

    let chosen = weighted_sample(&candidates).expect("should pick first");
    assert_eq!(chosen.name, "first");
}

#[test]
fn weighted_sample_uses_weights_for_random() {
    let m1 = sample_model(VendorKind::Claude, "high-weight", 80);
    let m2 = sample_model(VendorKind::Codex, "low-weight", 80);
    let candidates = vec![(&m1, 1000.0), (&m2, 1.0)];

    // High weight should almost always win
    let mut high_count = 0;
    for seed in 1..100_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = weighted_sample(&candidates).expect("should pick");
        if chosen.name == "high-weight" {
            high_count += 1;
        }
    }

    assert!(high_count > 90); // Should win most of the time
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

fn opus_sonnet_codex_kimi() -> Vec<CachedModel> {
    vec![
        sample_model(VendorKind::Claude, "claude-opus-4-7", 80),
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(VendorKind::Claude, "claude-haiku-4-5", 80),
        sample_model(VendorKind::Codex, "gpt-5.5", 80),
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
    ]
}

#[test]
fn pick_with_effort_normal_does_not_filter_ineligible_models() {
    // With only ineligible models in the slice, Normal effort still
    // returns a candidate — proving the eligibility filter is not
    // applied. The Tough variant of this scenario exercises the
    // degraded-fallback path; here we want the *non-degraded* selection
    // to ignore eligibility entirely.
    let models = vec![
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5", 80),
    ];
    let index = build_version_index(&models);

    let chosen = pick_for_phase_with_effort(
        &models,
        SelectionPhase::Build,
        None,
        &index,
        EffortLevel::Normal,
        false,
    )
    .expect("Normal must pick from non-empty slice");
    assert!(matches!(
        chosen.vendor,
        VendorKind::Kimi | VendorKind::Gemini
    ));
}

#[test]
fn pick_with_effort_low_does_not_use_tough_filter() {
    let models = vec![
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5", 80),
    ];
    let index = build_version_index(&models);

    let chosen = pick_for_phase_with_effort(
        &models,
        SelectionPhase::Build,
        None,
        &index,
        EffortLevel::Low,
        false,
    )
    .expect("Low effort must use the non-tough path until cheap filtering is wired");
    assert!(matches!(
        chosen.vendor,
        VendorKind::Kimi | VendorKind::Gemini
    ));
}

#[test]
fn pick_with_effort_cheap_filters_to_budget_subset() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-opus-4-7", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5-pro", 80),
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5-flash", 80),
    ];
    let index = build_version_index(&models);

    for seed in 1..100_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = pick_for_phase_with_effort(
            &models,
            SelectionPhase::Build,
            None,
            &index,
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
            chosen.model.vendor,
            chosen.model.name
        );
    }
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_cheap_fallback_warns_when_eligible_quota_empty() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 0),
        sample_model(VendorKind::Gemini, "gemini-2.5-flash", 0),
        sample_model(VendorKind::Claude, "claude-opus-4-7", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_phase_with_effort(
        &models,
        SelectionPhase::Build,
        None,
        &index,
        EffortLevel::Low,
        true,
    )
    .expect("full-pool fallback must yield a candidate");
    assert_eq!(chosen.model.name, "claude-opus-4-7");
    assert_eq!(
        chosen.warning,
        Some(SelectionWarning::CheapFallback {
            phase: SelectionPhase::Build,
            reason: "no_eligible_with_quota",
        })
    );
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_tough_only_picks_eligible() {
    let models = opus_sonnet_codex_kimi();
    let index = build_version_index(&models);

    for seed in 1..200_u64 {
        TEST_SAMPLE_SEED.store(seed, AtomicOrdering::Relaxed);
        let chosen = pick_for_phase_with_effort(
            &models,
            SelectionPhase::Build,
            None,
            &index,
            EffortLevel::Tough,
            false,
        )
        .expect("eligible candidate exists");
        assert!(
            (chosen.vendor == VendorKind::Claude && chosen.name.contains("opus"))
                || chosen.vendor == VendorKind::Codex,
            "tough must never pick {:?} {}",
            chosen.vendor,
            chosen.name
        );
    }
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_tough_falls_back_to_kimi_gemini() {
    let models = vec![
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_phase_with_effort(
        &models,
        SelectionPhase::Build,
        None,
        &index,
        EffortLevel::Tough,
        false,
    )
    .expect("degraded fallback must yield a candidate");
    assert!(matches!(
        chosen.vendor,
        VendorKind::Kimi | VendorKind::Gemini
    ));
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn pick_with_effort_tough_falls_back_to_sonnet_haiku() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(VendorKind::Claude, "claude-haiku-4-5", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = pick_for_phase_with_effort(
        &models,
        SelectionPhase::Build,
        None,
        &index,
        EffortLevel::Tough,
        false,
    )
    .expect("degraded fallback must yield a candidate");
    assert_eq!(chosen.vendor, VendorKind::Claude);
    assert!(!chosen.name.contains("opus"));
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_normal_can_pick_ineligible_vendor() {
    // Under Normal effort, the reviewer can pick a Kimi model — proving
    // it is delegating to the original `select_for_review` rather than
    // running the tough-eligible filter.
    let models = vec![
        sample_model(VendorKind::Claude, "claude-opus-4-7", 80),
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
    ];
    let index = build_version_index(&models);
    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-opus-4-7".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        &index,
        EffortLevel::Normal,
        false,
    )
    .expect("Normal review picks fresh Kimi");
    assert_eq!(chosen.vendor, VendorKind::Kimi);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_low_can_pick_ineligible_vendor() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-opus-4-7", 80),
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
    ];
    let index = build_version_index(&models);
    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-opus-4-7".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        &index,
        EffortLevel::Low,
        false,
    )
    .expect("Low review effort must use the non-tough path");
    assert_eq!(chosen.vendor, VendorKind::Kimi);
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_cheap_reuses_used_eligible_before_expensive_fresh_model() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5-pro", 80),
    ];
    let index = build_version_index(&models);
    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-sonnet-4-6".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        &index,
        EffortLevel::Low,
        true,
    )
    .expect("cheap reviewer should reuse the only eligible model");
    assert_eq!(chosen.warning, None);
    assert_eq!(chosen.model.name, "claude-sonnet-4-6");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_cheap_fallback_warns_when_eligible_quota_empty() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 0),
        sample_model(VendorKind::Kimi, "kimi-k2", 0),
        sample_model(VendorKind::Gemini, "gemini-2.5-pro", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(&models, &[], &[], &index, EffortLevel::Low, true)
        .expect("full-pool fallback must yield a reviewer");
    assert_eq!(chosen.model.name, "gemini-2.5-pro");
    assert_eq!(
        chosen.warning,
        Some(SelectionWarning::CheapFallback {
            phase: SelectionPhase::Review,
            reason: "no_eligible_with_quota",
        })
    );
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_tough_reuses_opus_over_fresh_sonnet() {
    // Coder used Claude-opus and Codex (so both eligible options used);
    // sonnet is "fresh" but ineligible. Reviewer must reuse Claude-opus.
    // Codex is omitted entirely (exhausted upstream by quota filtering),
    // so the only eligible model is Claude-opus, which the coder used.
    // Sonnet is fresh-vendor + fresh-model but ineligible.
    let models = vec![
        sample_model(VendorKind::Claude, "claude-opus-4-7", 80),
        sample_model(VendorKind::Claude, "claude-sonnet-4-6", 80),
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
    ];
    let index = build_version_index(&models);
    let used_vendors = vec![VendorKind::Claude];
    let used_models = vec![(VendorKind::Claude, "claude-opus-4-7".to_string())];

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen = select_for_review_with_effort(
        &models,
        &used_vendors,
        &used_models,
        &index,
        EffortLevel::Tough,
        false,
    )
    .expect("eligibility-dominated reuse expected");
    assert_eq!(chosen.vendor, VendorKind::Claude);
    assert_eq!(chosen.name, "claude-opus-4-7");
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_tough_degrades_when_no_eligible_remain() {
    // Only Kimi/Gemini exist — no eligible model at all. Must still
    // launch via the unfiltered fallback.
    let models = vec![
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        select_for_review_with_effort(&models, &[], &[], &index, EffortLevel::Tough, false)
            .expect("degraded fallback must yield a candidate");
    assert!(matches!(
        chosen.vendor,
        VendorKind::Kimi | VendorKind::Gemini
    ));
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}

#[test]
fn select_for_review_tough_degrades_when_eligible_have_zero_probability() {
    let models = vec![
        sample_model(VendorKind::Claude, "claude-opus-4-7", 0),
        sample_model(VendorKind::Codex, "gpt-5.5", 0),
        sample_model(VendorKind::Kimi, "kimi-k2", 80),
        sample_model(VendorKind::Gemini, "gemini-2.5", 80),
    ];
    let index = build_version_index(&models);

    TEST_SAMPLE_SEED.store(1, AtomicOrdering::Relaxed);
    let chosen =
        select_for_review_with_effort(&models, &[], &[], &index, EffortLevel::Tough, false)
            .expect("degraded fallback must yield an available candidate");
    assert!(
        matches!(chosen.vendor, VendorKind::Kimi | VendorKind::Gemini),
        "exhausted tough-eligible model was selected: {:?} {}",
        chosen.vendor,
        chosen.name
    );
    TEST_SAMPLE_SEED.store(0, AtomicOrdering::Relaxed);
}
