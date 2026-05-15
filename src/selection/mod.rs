//! Flat `crate::selection::*` public surface for app/runtime callers.
pub use crate::logic::selection::selection::{
    SelectionOutcome, SelectionWarning, pick_for_stage, pick_for_stage_with_effort,
    select_excluding, select_for_review, select_for_review_with_effort,
};
pub use crate::logic::selection::{
    CachedModel, Candidate, CliKind, IpbrStageScores, ModelRow, QuotaError, ScoreSource,
    SubscriptionKind,
};
pub use crate::logic::selection::{config, display, ranking, subscription, types};
pub use config::*;
