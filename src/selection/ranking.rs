use super::config::{SELECTION_CONFIG, SelectionPhase};
use super::types::{CachedModel, VendorKind};
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
    selection_events().lock().unwrap().clone()
}

#[cfg(test)]
fn clear_selection_events() {
    selection_events().lock().unwrap().clear();
}

fn record_zero_as_missing(axis: &str, phase: &str) {
    eprintln!("codexize: selection.zero_as_missing axis={axis} phase={phase}");
    selection_events()
        .lock()
        .unwrap()
        .push(SelectionEvent::ZeroAsMissing {
            axis: axis.to_string(),
            phase: phase.to_string(),
        });
}

const ZERO_THRESHOLD: f64 = 1e-9;

/// Linear variance-penalty factor (0..1) applied once, outside the role
/// score exponent, so a noisy reading doesn't get cubed into oblivion.
fn variance_factor(standard_error: f64) -> f64 {
    let standard_error = standard_error.max(0.0);
    if standard_error == 0.0 {
        return 1.0;
    }
    let cfg = &SELECTION_CONFIG;
    let mut penalty = standard_error * cfg.std_err_penalty_multiplier;
    if standard_error >= cfg.high_variance_std_err {
        penalty += cfg.high_variance_extra_penalty;
    }
    (1.0 - penalty / 100.0).clamp(0.0, 1.0)
}

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
/// Used to apply version penalties during probability calculation.
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

/// Consolidated probability calculation for CachedModel.
/// Combines quota weight, role weight with exponent, variance factor,
/// version penalty, vendor bias, and flash-tier penalty.
pub fn selection_probability(
    model: &CachedModel,
    phase: SelectionPhase,
    version_index: &VersionIndex,
) -> f64 {
    let cfg = &SELECTION_CONFIG;

    // Quota weight (concave curve, 1.0 at soft threshold)
    let quota = model.quota_percent.unwrap_or(50) as f64;
    let quota_weight = cfg.quota_weight(quota);
    if quota_weight <= 0.0 {
        return 0.0;
    }

    // Role score: extract axis values, compute mean (arithmetic for Idea, geometric for others)
    let axis_score = compute_axis_score(model, phase.axes(), phase) / 100.0;
    let role_weight = axis_score
        .max(cfg.min_role_score_weight)
        .powi(cfg.role_score_exponent);

    // Variance factor (linear penalty for standard error)
    let variance_factor = variance_factor(model.standard_error);

    // Version penalty (per-step multiplier based on version rank)
    let version_rank = version_index.version_rank(model.vendor, &model.name);
    let version_penalty = if phase.is_interactive() {
        cfg.version_penalty_per_step_interactive
            .powi(version_rank as i32)
    } else {
        cfg.version_penalty_per_step_headless
            .powi(version_rank as i32)
    };

    // Vendor bias (optional multiplier for specific vendor/phase combinations)
    let vendor_bias = cfg.vendor_bias(model.vendor, &model.name, phase);

    // Flash-tier penalty (aggressive derank for flash/nano models)
    let flash_penalty = if is_flash_tier(model) {
        cfg.flash_tier_penalty
    } else {
        1.0
    };

    quota_weight * role_weight * variance_factor * version_penalty * vendor_bias * flash_penalty
}

/// Extract and aggregate axis scores for the given phase.
/// Mirrors the logic in raw_axis_score but works with CachedModel.
/// Axis values at or below `ZERO_THRESHOLD` are treated identically to
/// missing axes: both use the `overall_score / 100` backfill.
fn compute_axis_score(model: &CachedModel, axes: &[&str], phase: SelectionPhase) -> f64 {
    let mut values: Vec<f64> = axes
        .iter()
        .filter_map(|axis| model.axis(axis).filter(|v| *v > ZERO_THRESHOLD))
        .collect();

    // Backfill missing-or-zero axes with overall_score / 100
    while values.len() < axes.len() && !axes.is_empty() {
        values.push(model.overall_score / 100.0);
    }

    if values.is_empty() {
        return model.overall_score.clamp(0.0, 100.0);
    }

    // Idea uses arithmetic mean (rewards breadth), others use geometric (punishes inconsistency)
    let score = match phase {
        SelectionPhase::Idea => values.iter().sum::<f64>() / values.len() as f64 * 100.0,
        SelectionPhase::Planning | SelectionPhase::Build | SelectionPhase::Review => {
            let floor = SELECTION_CONFIG.min_role_score_weight;
            let log_sum: f64 = values.iter().map(|v| v.max(floor).ln()).sum();
            (log_sum / values.len() as f64).exp() * 100.0
        }
    };
    score.clamp(0.0, 100.0)
}

/// Stamp `fallback:overall` provenance on every axis that `compute_axis_score`
/// will backfill (missing or zero-as-missing), and emit counter events for
/// zero-as-missing substitutions. Must be called once per model before
/// selection probabilities are used.
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

fn is_flash_tier(model: &CachedModel) -> bool {
    model.vendor == VendorKind::Gemini
        && (model.name.contains("flash") || model.name.contains("nano"))
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
            quota_percent: Some(80),
            display_order: 1,
            fallback_from: None,
        }
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
    fn selection_probability_applies_quota_weight() {
        let index = build_version_index(&[]);
        let mut model_high_quota = sample_cached_model();
        model_high_quota.quota_percent = Some(80);
        let mut model_low_quota = sample_cached_model();
        model_low_quota.quota_percent = Some(5);

        let prob_high = selection_probability(&model_high_quota, SelectionPhase::Build, &index);
        let prob_low = selection_probability(&model_low_quota, SelectionPhase::Build, &index);

        assert!(prob_high > prob_low);
    }

    #[test]
    fn selection_probability_returns_zero_for_zero_quota() {
        let index = build_version_index(&[]);
        let mut model = sample_cached_model();
        model.quota_percent = Some(0);

        let prob = selection_probability(&model, SelectionPhase::Build, &index);

        assert_eq!(prob, 0.0);
    }

    #[test]
    fn selection_probability_applies_role_weight_exponent() {
        let index = build_version_index(&[]);
        let mut model_high_axis = sample_cached_model();
        model_high_axis.axes = vec![
            ("codequality".to_string(), 0.95),
            ("correctness".to_string(), 0.95),
            ("debugging".to_string(), 0.95),
            ("safety".to_string(), 0.95),
        ];
        let mut model_low_axis = sample_cached_model();
        model_low_axis.axes = vec![
            ("codequality".to_string(), 0.50),
            ("correctness".to_string(), 0.50),
            ("debugging".to_string(), 0.50),
            ("safety".to_string(), 0.50),
        ];

        let prob_high = selection_probability(&model_high_axis, SelectionPhase::Build, &index);
        let prob_low = selection_probability(&model_low_axis, SelectionPhase::Build, &index);

        // With exponent = 3, difference should be amplified
        assert!(prob_high > prob_low);
        let ratio = prob_high / prob_low;
        assert!(ratio > 5.0); // Should be significantly larger due to cubing
    }

    #[test]
    fn selection_probability_applies_variance_factor() {
        let index = build_version_index(&[]);
        let mut model_low_variance = sample_cached_model();
        model_low_variance.standard_error = 1.0;
        let mut model_high_variance = sample_cached_model();
        model_high_variance.standard_error = 8.0;

        let prob_low_var =
            selection_probability(&model_low_variance, SelectionPhase::Build, &index);
        let prob_high_var =
            selection_probability(&model_high_variance, SelectionPhase::Build, &index);

        assert!(prob_low_var > prob_high_var);
    }

    #[test]
    fn selection_probability_applies_version_penalty() {
        let models = vec![
            CachedModel {
                name: "gpt-5.5".to_string(),
                ..sample_cached_model()
            },
            CachedModel {
                name: "gpt-5.2".to_string(),
                ..sample_cached_model()
            },
        ];
        let index = build_version_index(&models);

        let prob_new = selection_probability(&models[0], SelectionPhase::Build, &index);
        let prob_old = selection_probability(&models[1], SelectionPhase::Build, &index);

        assert!(prob_new > prob_old);
    }

    #[test]
    fn selection_probability_uses_different_version_penalty_for_interactive() {
        let models = vec![
            CachedModel {
                name: "gpt-5.5".to_string(),
                ..sample_cached_model()
            },
            CachedModel {
                name: "gpt-5.2".to_string(),
                ..sample_cached_model()
            },
        ];
        let index = build_version_index(&models);

        let idea_new = selection_probability(&models[0], SelectionPhase::Idea, &index);
        let idea_old = selection_probability(&models[1], SelectionPhase::Idea, &index);
        let build_new = selection_probability(&models[0], SelectionPhase::Build, &index);
        let build_old = selection_probability(&models[1], SelectionPhase::Build, &index);

        // Interactive phases penalize old versions more aggressively
        let idea_ratio = idea_new / idea_old;
        let build_ratio = build_new / build_old;
        assert!(idea_ratio > build_ratio);
    }

    #[test]
    fn selection_probability_applies_vendor_bias() {
        let index = build_version_index(&[]);
        let claude_opus = CachedModel {
            vendor: VendorKind::Claude,
            name: "claude-opus-4-7".to_string(),
            ..sample_cached_model()
        };
        let codex_model = CachedModel {
            vendor: VendorKind::Codex,
            name: "gpt-5.5".to_string(),
            ..sample_cached_model()
        };

        // Claude Opus gets 1.5× bias for Idea phase (per SELECTION_CONFIG)
        let claude_idea = selection_probability(&claude_opus, SelectionPhase::Idea, &index);
        let codex_idea = selection_probability(&codex_model, SelectionPhase::Idea, &index);

        // Codex gets 1.5× bias for Review phase
        let claude_review = selection_probability(&claude_opus, SelectionPhase::Review, &index);
        let codex_review = selection_probability(&codex_model, SelectionPhase::Review, &index);

        // Verify bias is applied (exact ratio depends on other factors, but should be noticeable)
        assert!(claude_idea > codex_idea * 1.2); // Approximate check accounting for other factors
        assert!(codex_review > claude_review * 1.2);
    }

    #[test]
    fn selection_probability_applies_flash_tier_penalty() {
        let index = build_version_index(&[]);
        let flash_model = CachedModel {
            vendor: VendorKind::Gemini,
            name: "gemini-2.5-flash".to_string(),
            ..sample_cached_model()
        };
        let pro_model = CachedModel {
            vendor: VendorKind::Gemini,
            name: "gemini-2.5-pro".to_string(),
            ..sample_cached_model()
        };

        let flash_prob = selection_probability(&flash_model, SelectionPhase::Build, &index);
        let pro_prob = selection_probability(&pro_model, SelectionPhase::Build, &index);

        // Flash should be heavily penalized (0.05 multiplier)
        assert!(pro_prob > flash_prob * 10.0);
    }

    #[test]
    fn compute_axis_score_uses_arithmetic_mean_for_idea() {
        let model = CachedModel {
            axes: vec![
                ("complexity".to_string(), 0.8),
                ("edgecases".to_string(), 0.6),
            ],
            ..sample_cached_model()
        };

        let score = compute_axis_score(&model, SelectionPhase::Idea.axes(), SelectionPhase::Idea);

        // Should be (0.8 + 0.6) / 2 * 100 = 70.0 (plus backfilled values)
        // With 4 axes and 2 provided, backfill with overall_score/100 = 0.88
        // (0.8 + 0.6 + 0.88 + 0.88) / 4 * 100 = 79.0
        assert!((score - 79.0).abs() < 1.0);
    }

    #[test]
    fn compute_axis_score_uses_geometric_mean_for_build() {
        let model = CachedModel {
            axes: vec![
                ("codequality".to_string(), 0.9),
                ("correctness".to_string(), 0.9),
                ("debugging".to_string(), 0.1), // Weak axis should pull down geometric mean
                ("safety".to_string(), 0.9),
            ],
            ..sample_cached_model()
        };

        let score = compute_axis_score(&model, SelectionPhase::Build.axes(), SelectionPhase::Build);

        // Geometric mean with weak axis floored at min_role_score_weight
        assert!(score < 70.0);
    }

    #[test]
    fn is_flash_tier_detects_flash_models() {
        assert!(is_flash_tier(&CachedModel {
            vendor: VendorKind::Gemini,
            name: "gemini-2.5-flash".to_string(),
            ..sample_cached_model()
        }));
        assert!(is_flash_tier(&CachedModel {
            vendor: VendorKind::Gemini,
            name: "gemini-nano".to_string(),
            ..sample_cached_model()
        }));
        assert!(!is_flash_tier(&CachedModel {
            vendor: VendorKind::Gemini,
            name: "gemini-2.5-pro".to_string(),
            ..sample_cached_model()
        }));
        assert!(!is_flash_tier(&CachedModel {
            vendor: VendorKind::Claude,
            name: "claude-flash".to_string(), // Not Gemini
            ..sample_cached_model()
        }));
    }
}
