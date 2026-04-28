use super::types::VendorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPhase {
    Idea,
    Planning,
    Build,
    Review,
}

impl SelectionPhase {
    pub fn axes(self) -> &'static [&'static str] {
        match self {
            SelectionPhase::Idea => &[
                "complexity",
                "edgecases",
                "contextawareness",
                "taskcompletion",
            ],
            SelectionPhase::Planning => &["correctness", "complexity", "edgecases", "stability"],
            SelectionPhase::Build => &["codequality", "correctness", "debugging", "safety"],
            SelectionPhase::Review => &[
                "correctness",
                "debugging",
                "edgecases",
                "safety",
                "stability",
            ],
        }
    }

    /// Interactive phases put the user in the loop and expose model
    /// staleness directly; non-interactive phases run headless in rounds.
    pub fn is_interactive(self) -> bool {
        matches!(self, SelectionPhase::Idea | SelectionPhase::Planning)
    }

    pub fn name(self) -> &'static str {
        match self {
            SelectionPhase::Idea => "idea",
            SelectionPhase::Planning => "planning",
            SelectionPhase::Build => "build",
            SelectionPhase::Review => "review",
        }
    }

    pub const ALL: [SelectionPhase; 4] = [
        SelectionPhase::Idea,
        SelectionPhase::Planning,
        SelectionPhase::Build,
        SelectionPhase::Review,
    ];
}

pub struct SelectionConfig {
    /// Exponent applied to the normalised role score (0..1). Higher values
    /// sharpen the preference for high-scoring models.
    pub role_score_exponent: i32,
    /// Quota percent (0..100) at or above which quota ceases to penalise
    /// the model. Below this, a concave curve falls off to 0 at 0%.
    pub quota_soft_threshold: f64,
    /// Lower bound on role score used before exponentiation so that weak
    /// models still retain a small chance.
    pub min_role_score_weight: f64,
    /// Minimum probability ratio (relative to the highest probability in a
    /// role) required to keep a candidate in the role-specific pool.
    pub min_selection_probability_ratio: f64,
    /// Multiplier applied per version step (newest = 0 steps) for
    /// interactive phases (Idea, Planning). Smaller values penalise older
    /// versions more aggressively.
    pub version_penalty_per_step_interactive: f64,
    /// Per-version-step multiplier for non-interactive phases (Build,
    /// Review) where stale models are less visible to the user.
    pub version_penalty_per_step_headless: f64,
    /// Standard error above which an extra flat penalty is applied.
    pub high_variance_std_err: f64,
    pub high_variance_extra_penalty: f64,
    pub std_err_penalty_multiplier: f64,
    /// Multiplicative penalty applied to flash-tier models (e.g. Gemini Flash/Nano)
    /// so they become last-resort fallbacks rather than regular candidates.
    pub flash_tier_penalty: f64,
    /// Multiplicative biases applied when a vendor (optionally restricted
    /// to a model-name substring) is being considered for the given phase.
    pub vendor_phase_biases: &'static [(VendorKind, Option<&'static str>, SelectionPhase, f64)],
}

pub const SELECTION_CONFIG: SelectionConfig = SelectionConfig {
    role_score_exponent: 3,
    quota_soft_threshold: 25.0,
    // Spec §4.2 / §7: with role_score_exponent=3 this caps a single weak
    // axis's penalty at (0.20/best)^3 instead of (0.05/best)^3, ~64× more
    // lenient, so one zero/missing axis can't single-handedly disable a model.
    min_role_score_weight: 0.20,
    min_selection_probability_ratio: 1.0 / 3.0,
    version_penalty_per_step_interactive: 1.0 / 3.0,
    version_penalty_per_step_headless: 2.0 / 3.0,
    high_variance_std_err: 5.0,
    high_variance_extra_penalty: 10.0,
    std_err_penalty_multiplier: 1.0,
    flash_tier_penalty: 0.05,
    vendor_phase_biases: &[
        (VendorKind::Claude, Some("opus"), SelectionPhase::Idea, 1.5),
        (
            VendorKind::Claude,
            Some("opus"),
            SelectionPhase::Planning,
            1.5,
        ),
        (VendorKind::Codex, None, SelectionPhase::Review, 1.5),
    ],
};

impl SelectionConfig {
    pub fn vendor_bias(&self, vendor: VendorKind, name: &str, phase: SelectionPhase) -> f64 {
        self.vendor_phase_biases
            .iter()
            .find(|(v, name_match, p, _)| {
                *v == vendor && *p == phase && name_match.is_none_or(|needle| name.contains(needle))
            })
            .map(|(_, _, _, bias)| *bias)
            .unwrap_or(1.0)
    }

    /// Concave curve: reaches 1.0 at `quota_soft_threshold`, falls off
    /// quadratically to 0 at 0%. At and above the soft threshold this stays
    /// flat at 1.0 (no extra quota penalty).
    pub fn quota_weight(&self, quota_percent: f64) -> f64 {
        if quota_percent <= 0.0 {
            return 0.0;
        }
        let t = (quota_percent / self.quota_soft_threshold).min(1.0);
        1.0 - (1.0 - t).powi(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_bias_matches_opus_only_for_idea_and_planning() {
        let cfg = &SELECTION_CONFIG;
        assert_eq!(
            cfg.vendor_bias(VendorKind::Claude, "claude-opus-4", SelectionPhase::Idea),
            1.5
        );
        assert_eq!(
            cfg.vendor_bias(
                VendorKind::Claude,
                "claude-opus-4",
                SelectionPhase::Planning
            ),
            1.5
        );
        // Non-opus Claude variants do not match the substring needle, so
        // the bias falls back to the neutral 1.0.
        assert_eq!(
            cfg.vendor_bias(
                VendorKind::Claude,
                "claude-sonnet-4",
                SelectionPhase::Idea
            ),
            1.0
        );
    }

    #[test]
    fn vendor_bias_codex_review_uses_unrestricted_needle() {
        let cfg = &SELECTION_CONFIG;
        // The Codex Review entry has needle = None, so any model name
        // qualifies as long as the vendor + phase match.
        assert_eq!(
            cfg.vendor_bias(VendorKind::Codex, "gpt-5.5", SelectionPhase::Review),
            1.5
        );
        assert_eq!(
            cfg.vendor_bias(VendorKind::Codex, "o1-mini", SelectionPhase::Review),
            1.5
        );
        // Wrong phase: returns the neutral default.
        assert_eq!(
            cfg.vendor_bias(VendorKind::Codex, "gpt-5.5", SelectionPhase::Build),
            1.0
        );
    }

    #[test]
    fn vendor_bias_unknown_vendor_phase_combo_is_one() {
        let cfg = &SELECTION_CONFIG;
        // Gemini and Kimi have no vendor_phase_biases entries.
        assert_eq!(
            cfg.vendor_bias(VendorKind::Gemini, "gemini-2.5-pro", SelectionPhase::Idea),
            1.0
        );
        assert_eq!(
            cfg.vendor_bias(VendorKind::Kimi, "kimi-k2", SelectionPhase::Build),
            1.0
        );
    }

    #[test]
    fn quota_weight_zero_or_negative_is_zero() {
        let cfg = &SELECTION_CONFIG;
        assert_eq!(cfg.quota_weight(0.0), 0.0);
        assert_eq!(cfg.quota_weight(-5.0), 0.0);
    }

    #[test]
    fn quota_weight_at_or_above_soft_threshold_is_one() {
        let cfg = &SELECTION_CONFIG;
        let threshold = cfg.quota_soft_threshold;
        assert!((cfg.quota_weight(threshold) - 1.0).abs() < 1e-12);
        assert!((cfg.quota_weight(threshold * 4.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn quota_weight_is_concave_below_soft_threshold() {
        let cfg = &SELECTION_CONFIG;
        let threshold = cfg.quota_soft_threshold;
        let half = cfg.quota_weight(threshold / 2.0);
        // 1 - (1 - 0.5)^2 = 0.75
        assert!((half - 0.75).abs() < 1e-12, "quota_weight at half: {half}");
        let quarter = cfg.quota_weight(threshold / 4.0);
        // 1 - (1 - 0.25)^2 = 0.4375
        assert!(
            (quarter - 0.4375).abs() < 1e-12,
            "quota_weight at quarter: {quarter}"
        );
        // Strictly increasing on [0, threshold].
        assert!(quarter < half);
        assert!(half < cfg.quota_weight(threshold));
    }
}
