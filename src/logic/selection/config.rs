use super::types::SubscriptionKind;
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::IntoStaticStr)]
pub enum SelectionStage {
    #[strum(to_string = "idea")]
    Idea,
    #[strum(to_string = "planning")]
    Planning,
    #[strum(to_string = "build")]
    Build,
    #[strum(to_string = "review")]
    Review,
}
impl SelectionStage {
    pub fn axes(self) -> &'static [&'static str] {
        match self {
            SelectionStage::Idea => &[
                "complexity",
                "edgecases",
                "contextawareness",
                "taskcompletion",
            ],
            SelectionStage::Planning => &["correctness", "complexity", "edgecases", "stability"],
            SelectionStage::Build => &["codequality", "correctness", "debugging", "safety"],
            SelectionStage::Review => &[
                "correctness",
                "debugging",
                "edgecases",
                "safety",
                "stability",
            ],
        }
    }
    /// Interactive stages put the user in the loop and expose model
    /// staleness directly; non-interactive stages run headless in rounds.
    pub fn is_interactive(self) -> bool {
        matches!(self, SelectionStage::Idea | SelectionStage::Planning)
    }
    pub fn name(self) -> &'static str {
        self.into()
    }
    pub const ALL: [SelectionStage; 4] = [
        SelectionStage::Idea,
        SelectionStage::Planning,
        SelectionStage::Build,
        SelectionStage::Review,
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
    /// interactive stages (Idea, Planning). Smaller values penalise older
    /// versions more aggressively.
    pub version_penalty_per_step_interactive: f64,
    /// Per-version-step multiplier for non-interactive stages (Build,
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
    /// to a model-name substring) is being considered for the given stage.
    pub vendor_stage_biases:
        &'static [(SubscriptionKind, Option<&'static str>, SelectionStage, f64)],
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
    vendor_stage_biases: &[
        (
            SubscriptionKind::Claude,
            Some("opus"),
            SelectionStage::Idea,
            1.5,
        ),
        (
            SubscriptionKind::Claude,
            Some("opus"),
            SelectionStage::Planning,
            1.5,
        ),
        (SubscriptionKind::Codex, None, SelectionStage::Review, 1.5),
    ],
};
impl SelectionConfig {
    pub fn vendor_bias(&self, vendor: SubscriptionKind, name: &str, stage: SelectionStage) -> f64 {
        self.vendor_stage_biases
            .iter()
            .find(|(v, name_match, p, _)| {
                *v == vendor && *p == stage && name_match.is_none_or(|needle| name.contains(needle))
            })
            .map_or(1.0, |(_, _, _, bias)| *bias)
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
#[path = "config_tests.rs"]
mod tests;
