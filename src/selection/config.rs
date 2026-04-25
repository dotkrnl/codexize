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
            SelectionPhase::Idea => {
                &["complexity", "edgecases", "contextawareness", "taskcompletion"]
            }
            SelectionPhase::Planning => {
                &["correctness", "complexity", "edgecases", "stability"]
            }
            SelectionPhase::Build => &["codequality", "correctness", "debugging", "safety"],
            SelectionPhase::Review => {
                &["correctness", "debugging", "edgecases", "safety", "stability"]
            }
        }
    }

    /// Interactive phases put the user in the loop and expose model
    /// staleness directly; non-interactive phases run headless in rounds.
    pub fn is_interactive(self) -> bool {
        matches!(self, SelectionPhase::Idea | SelectionPhase::Planning)
    }
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
    min_role_score_weight: 0.05,
    version_penalty_per_step_interactive: 1.0 / 3.0,
    version_penalty_per_step_headless: 1.0 / 2.0,
    high_variance_std_err: 5.0,
    high_variance_extra_penalty: 10.0,
    std_err_penalty_multiplier: 1.0,
    flash_tier_penalty: 0.05,
    vendor_phase_biases: &[
        (VendorKind::Claude, Some("opus"), SelectionPhase::Idea, 1.5),
        (VendorKind::Claude, Some("opus"), SelectionPhase::Planning, 1.5),
        (VendorKind::Codex, None, SelectionPhase::Review, 1.5),
    ],
};

impl SelectionConfig {
    pub fn vendor_bias(&self, vendor: VendorKind, name: &str, phase: SelectionPhase) -> f64 {
        self.vendor_phase_biases
            .iter()
            .find(|(v, name_match, p, _)| {
                *v == vendor
                    && *p == phase
                    && name_match.is_none_or(|needle| name.contains(needle))
            })
            .map(|(_, _, _, bias)| *bias)
            .unwrap_or(1.0)
    }

    /// Concave curve: reaches 1.0 at `quota_soft_threshold`, falls off
    /// quadratically to 0 at 0%. Above the threshold the weight stays 1.0.
    pub fn quota_weight(&self, quota_percent: f64) -> f64 {
        if quota_percent <= 0.0 {
            return 0.0;
        }
        let t = (quota_percent / self.quota_soft_threshold).min(1.0);
        1.0 - (1.0 - t).powi(2)
    }
}
