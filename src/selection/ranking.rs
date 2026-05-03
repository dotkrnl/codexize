use super::config::SelectionPhase;
use super::types::{CachedModel, ScoreSource, VendorKind};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionEvent {
    ZeroAsMissing { axis: String, phase: String },
}

fn selection_events() -> &'static Mutex<Vec<SelectionEvent>> {
    static EVENTS: OnceLock<Mutex<Vec<SelectionEvent>>> = OnceLock::new();
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn selection_events_snapshot() -> Vec<SelectionEvent> {
    // SAFETY: `selection_events()` guards a `Vec<SelectionEvent>` whose
    // only mutators are `push`/`clear` — neither can panic — so the mutex
    // poison branch is only defensive for future mutators.
    selection_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone()
}

#[cfg(test)]
fn clear_selection_events() {
    // SAFETY: see `selection_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    selection_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clear();
}

fn record_zero_as_missing(axis: &str, phase: &str) {
    // SAFETY: see `selection_events_snapshot` — the guarded `Vec` has no
    // panicking mutators, so the mutex cannot be poisoned here.
    selection_events()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(SelectionEvent::ZeroAsMissing {
            axis: axis.to_string(),
            phase: phase.to_string(),
        });
}

const ZERO_THRESHOLD: f64 = 1e-9;
const RANK_SOFTMAX_TEMPERATURE: f64 = 5.5;
const UNKNOWN_QUOTA_PERCENT: u8 = 30;

pub type CandidateRef<'a> = &'a CachedModel;

pub(crate) fn extract_version(name: &str) -> Option<(u32, u32)> {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = i - start;
            if run > 2 {
                continue;
            }
            let major: u32 = name[start..i].parse().ok()?;

            if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'.') {
                let j = i + 1;
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    let mut k = j;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k - j <= 2 {
                        let minor: u32 = name[j..k].parse().ok()?;
                        return Some((major, minor));
                    }
                }
            }
            return Some((major, 0));
        } else {
            i += 1;
        }
    }
    None
}

/// Per-vendor version ranking built once from the final assembled model set.
/// Retained for callers that still thread a version index through selection.
/// Ranking and pool weights do not apply version penalties.
#[derive(Debug, Clone)]
pub struct VersionIndex {
    per_vendor: BTreeMap<VendorKind, Vec<(u32, u32)>>,
}

impl VersionIndex {
    /// Returns the version rank for the given model (0 = newest, 1 = second-newest, etc.).
    /// Models with no parseable version or only one version in their vendor get rank 0.
    pub fn version_rank(&self, vendor: VendorKind, name: &str) -> usize {
        let Some(unique) = self.per_vendor.get(&vendor) else {
            return 0;
        };
        if unique.len() <= 1 {
            return 0;
        }
        extract_version(name)
            .and_then(|v| unique.iter().position(|u| *u == v))
            .unwrap_or(0)
    }
}

/// Builds a version index from the final assembled model set.
/// Must be called after synthesis and collapse steps complete.
pub fn build_version_index(models: &[CachedModel]) -> VersionIndex {
    let mut per_vendor: BTreeMap<VendorKind, Vec<(u32, u32)>> = BTreeMap::new();

    // Collect all versions per vendor
    for model in models {
        if let Some(version) = extract_version(&model.name) {
            per_vendor.entry(model.vendor).or_default().push(version);
        }
    }

    // Sort newest-first and deduplicate
    for unique in per_vendor.values_mut() {
        unique.sort_unstable_by(|a, b| b.cmp(a));
        unique.dedup();
    }

    VersionIndex { per_vendor }
}

/// Return the authoritative ipbr rank score for `phase`, if this model has one.
pub fn phase_rank_score(model: &CachedModel, phase: SelectionPhase) -> Option<f64> {
    if model.score_source != ScoreSource::Ipbr {
        return None;
    }

    match phase {
        SelectionPhase::Idea => model.ipbr_phase_scores.idea,
        SelectionPhase::Planning => model.ipbr_phase_scores.planning,
        SelectionPhase::Build => model.ipbr_phase_scores.build,
        SelectionPhase::Review => model.ipbr_phase_scores.review,
    }
}

/// Return pool-scoped sampling weights in the same order as `candidates`.
pub fn candidate_pool_weights(candidates: &[CandidateRef<'_>], phase: SelectionPhase) -> Vec<f64> {
    // Keep output parallel to the input slice; ineligible candidates are "dropped" as zero weights.
    let mut weights = vec![0.0; candidates.len()];
    let mut survivors = Vec::new();

    for (index, model) in candidates.iter().enumerate() {
        let Some(score) = phase_rank_score(model, phase) else {
            continue;
        };
        let Some(effective_quota) = effective_quota_percent(model) else {
            continue;
        };

        survivors.push((index, score, effective_quota));
    }

    if survivors.is_empty() {
        return weights;
    }

    let max_score = survivors
        .iter()
        .map(|(_, score, _)| *score)
        .fold(f64::NEG_INFINITY, f64::max);
    let exp_weights: Vec<f64> = survivors
        .iter()
        .map(|(_, score, _)| ((score - max_score) / RANK_SOFTMAX_TEMPERATURE).exp())
        .collect();
    let exp_total: f64 = exp_weights.iter().sum();
    let pool_best_quota = survivors
        .iter()
        .map(|(_, _, quota)| *quota)
        .max()
        .unwrap_or(UNKNOWN_QUOTA_PERCENT);

    for ((index, _, quota), exp_weight) in survivors.iter().zip(exp_weights.iter()) {
        let rank_weight = exp_weight / exp_total;
        let quota_factor = relative_quota_factor(pool_best_quota - *quota);
        weights[*index] = rank_weight * quota_factor;
    }

    weights
}

fn effective_quota_percent(model: &CachedModel) -> Option<u8> {
    match model.quota_percent {
        Some(0) => None,
        Some(quota) => Some(quota),
        None => Some(UNKNOWN_QUOTA_PERCENT),
    }
}

fn relative_quota_factor(deficit: u8) -> f64 {
    if deficit <= 20 {
        return 1.0;
    }
    if deficit >= 40 {
        return 0.10;
    }

    let t = ((f64::from(deficit) - 20.0) / 20.0).clamp(0.0, 1.0);
    let smooth = t * t * (3.0 - 2.0 * t);
    1.0 - 0.90 * smooth
}

/// Compatibility wrapper for legacy single-model callers.
pub fn selection_probability(
    model: &CachedModel,
    phase: SelectionPhase,
    _version_index: &VersionIndex,
) -> f64 {
    if model.quota_percent == Some(0) {
        return 0.0;
    }

    phase_rank_score(model, phase).unwrap_or(0.0)
}

/// Stamp `fallback:overall` provenance on missing or zero-as-missing legacy
/// axes, and emit counter events for zero-as-missing substitutions.
pub fn stamp_selection_provenance(model: &mut CachedModel) {
    let mut seen = std::collections::HashSet::new();
    for phase in SelectionPhase::ALL {
        for &axis in phase.axes() {
            match model.axis(axis) {
                Some(v) if v <= ZERO_THRESHOLD => {
                    if seen.insert(axis) {
                        model
                            .axis_provenance
                            .insert(axis.to_string(), "fallback:overall".to_string());
                    }
                    record_zero_as_missing(axis, phase.name());
                }
                None => {
                    if seen.insert(axis) {
                        model
                            .axis_provenance
                            .insert(axis.to_string(), "fallback:overall".to_string());
                    }
                }
                _ => {
                    seen.insert(axis);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cached_model() -> CachedModel {
        CachedModel {
            vendor: VendorKind::Codex,
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
    fn build_version_index_ranks_newest_first() {
        let models = vec![
            CachedModel {
                name: "gpt-5.2".to_string(),
                vendor: VendorKind::Codex,
                ..sample_cached_model()
            },
            CachedModel {
                name: "gpt-5.5".to_string(),
                vendor: VendorKind::Codex,
                ..sample_cached_model()
            },
            CachedModel {
                name: "gpt-5.4".to_string(),
                vendor: VendorKind::Codex,
                ..sample_cached_model()
            },
        ];

        let index = build_version_index(&models);

        assert_eq!(index.version_rank(VendorKind::Codex, "gpt-5.5"), 0);
        assert_eq!(index.version_rank(VendorKind::Codex, "gpt-5.4"), 1);
        assert_eq!(index.version_rank(VendorKind::Codex, "gpt-5.2"), 2);
    }

    #[test]
    fn extract_version_parses_supported_model_names() {
        assert_eq!(extract_version("gpt-5.5"), Some((5, 5)));
        assert_eq!(extract_version("gpt-5.4"), Some((5, 4)));
        assert_eq!(extract_version("gpt-5.2"), Some((5, 2)));
        assert_eq!(extract_version("gemini-2.5-flash"), Some((2, 5)));
        assert_eq!(extract_version("gemini-3-pro-preview"), Some((3, 0)));
        assert_eq!(extract_version("gemini-3-flash-preview"), Some((3, 0)));
        assert_eq!(extract_version("claude-sonnet-4-6"), Some((4, 6)));
        assert_eq!(extract_version("gpt-4-turbo-2024-04-09"), Some((4, 0)));
    }

    #[test]
    fn build_version_index_isolates_per_vendor() {
        let models = vec![
            CachedModel {
                name: "gpt-5.5".to_string(),
                vendor: VendorKind::Codex,
                ..sample_cached_model()
            },
            CachedModel {
                name: "claude-sonnet-4-6".to_string(),
                vendor: VendorKind::Claude,
                ..sample_cached_model()
            },
        ];

        let index = build_version_index(&models);

        // Each vendor has only one version, so rank is 0
        assert_eq!(index.version_rank(VendorKind::Codex, "gpt-5.5"), 0);
        assert_eq!(
            index.version_rank(VendorKind::Claude, "claude-sonnet-4-6"),
            0
        );
    }

    #[test]
    fn version_index_returns_zero_for_single_version() {
        let models = vec![CachedModel {
            name: "gpt-5.5".to_string(),
            vendor: VendorKind::Codex,
            ..sample_cached_model()
        }];

        let index = build_version_index(&models);

        assert_eq!(index.version_rank(VendorKind::Codex, "gpt-5.5"), 0);
    }

    #[test]
    fn selection_probability_wrapper_uses_ipbr_phase_rank_only() {
        let index = build_version_index(&[]);
        let mut high_variance_old_flash = ipbr_model("gemini-2.5-flash", 90.0, Some(80));
        high_variance_old_flash.vendor = VendorKind::Gemini;
        high_variance_old_flash.standard_error = 99.0;
        let low_variance_pro = CachedModel {
            vendor: VendorKind::Gemini,
            name: "gemini-2.5-pro".to_string(),
            standard_error: 0.0,
            ..ipbr_model("gemini-2.5-pro", 80.0, Some(80))
        };

        let flash_prob =
            selection_probability(&high_variance_old_flash, SelectionPhase::Build, &index);
        let pro_prob = selection_probability(&low_variance_pro, SelectionPhase::Build, &index);

        assert_eq!(flash_prob, 90.0);
        assert_eq!(pro_prob, 80.0);
    }

    #[test]
    fn selection_probability_wrapper_excludes_zero_quota_and_unranked_models() {
        let index = build_version_index(&[]);
        let exhausted = ipbr_model("exhausted", 90.0, Some(0));
        let unranked = sample_cached_model();

        assert_eq!(
            selection_probability(&exhausted, SelectionPhase::Build, &index),
            0.0
        );
        assert_eq!(
            selection_probability(&unranked, SelectionPhase::Build, &index),
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
            .filter(|e| {
                matches!(e, SelectionEvent::ZeroAsMissing { axis, .. } if axis == "correctness")
            })
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
}
