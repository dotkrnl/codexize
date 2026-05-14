//! Pending operator-decision state.
//!
//! Replaces the `*Paused` / `*Pending` variants on the old `Phase` enum. Each
//! field is `Some(_)` when the lifecycle is waiting on the operator for that
//! decision. The marker `*Data` structs are intentionally empty named-field
//! structs (for TOML serialization); they may carry originating-phase context
//! in a future migration step.
use super::phase::Phase;
use serde::{Deserialize, Serialize};

/// Operator decision payload for the git-guard modal (`HEAD` moved under
/// `GuardMode::AskOperator`).
///
/// Defined as an empty named-field struct (`{}`) rather than a unit struct
/// (`;`) so TOML can serialize it as an inline empty table — `toml-rs`
/// rejects unit-struct values entirely.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitGuardData {}

/// Operator decision payload for the spec-review pause.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecApprovalData {}

/// Operator decision payload for the plan-review pause.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanApprovalData {}

/// Operator decision payload for the "skip to implementation" modal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkipToImplData {}

/// Operator decision payload for the Dreaming-after-validation modal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DreamingData {}

/// Aggregate of every operator-decision slot the lifecycle can block on.
///
/// Defaults to "no decisions pending". The lifecycle code sets each slot to
/// `Some(_)` when it raises the corresponding modal and clears it back to
/// `None` once the operator decides.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingDecisions {
    pub git_guard: Option<GitGuardData>,
    pub spec_approval: Option<SpecApprovalData>,
    pub plan_approval: Option<PlanApprovalData>,
    pub skip_to_impl: Option<SkipToImplData>,
    pub dreaming: Option<DreamingData>,
}

impl PendingDecisions {
    /// True when no decision slot is populated. Used by the persistence
    /// layer's `skip_serializing_if` so a default `PendingDecisions` stays
    /// out of `session.toml` (avoiding fixture drift).
    pub fn is_empty(&self) -> bool {
        self.git_guard.is_none()
            && self.spec_approval.is_none()
            && self.plan_approval.is_none()
            && self.skip_to_impl.is_none()
            && self.dreaming.is_none()
    }

    /// True when any pending decision applies to `phase`.
    ///
    /// True when any pending decision slot is populated.
    ///
    /// NOTE(step-5): currently permissive — every set slot blocks regardless
    /// of the caller's phase. Will narrow once per-decision data carries the
    /// originating phase context.
    pub fn blocks(&self) -> bool {
        self.git_guard.is_some()
            || self.spec_approval.is_some()
            || self.plan_approval.is_some()
            || self.skip_to_impl.is_some()
            || self.dreaming.is_some()
    }

    /// Clear any pending decision whose originating [`Phase`] is strictly
    /// later than `target`. Used by `:rewind` so a decision raised at a
    /// phase the operator is rolling away from doesn't linger.
    ///
    /// The originating-phase map below mirrors where each modal is raised
    /// in the legacy code:
    /// - `spec_approval` and `skip_to_impl` are raised at [`Phase::Spec`].
    /// - `plan_approval` is raised at [`Phase::Plan`].
    /// - `dreaming` is raised at [`Phase::Finalization`].
    /// - `git_guard` is independent of pipeline phase (operator HEAD
    ///   moved); rewind preserves it.
    pub fn clear_after(&mut self, target: Phase) {
        // A decision is "after target" when its originating phase is
        // strictly greater than target. `Phase` ordering is partial — see
        // `Phase::partial_cmp` — so a `None` comparison (e.g. against
        // `Phase::Cancelled`) leaves the decision in place.
        let strictly_after = |origin: Phase| -> bool {
            matches!(origin.partial_cmp(&target), Some(std::cmp::Ordering::Greater))
        };
        if strictly_after(Phase::Spec) {
            self.spec_approval = None;
            self.skip_to_impl = None;
        }
        if strictly_after(Phase::Plan) {
            self.plan_approval = None;
        }
        if strictly_after(Phase::Finalization) {
            self.dreaming = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_does_not_block() {
        let pd = PendingDecisions::default();
        assert!(!pd.blocks());
    }

    #[test]
    fn any_some_field_blocks() {
        let cases: Vec<PendingDecisions> = vec![
            PendingDecisions {
                git_guard: Some(GitGuardData {}),
                ..Default::default()
            },
            PendingDecisions {
                spec_approval: Some(SpecApprovalData {}),
                ..Default::default()
            },
            PendingDecisions {
                plan_approval: Some(PlanApprovalData {}),
                ..Default::default()
            },
            PendingDecisions {
                skip_to_impl: Some(SkipToImplData {}),
                ..Default::default()
            },
            PendingDecisions {
                dreaming: Some(DreamingData {}),
                ..Default::default()
            },
        ];
        for pd in cases {
            assert!(pd.blocks());
        }
    }

    #[test]
    fn clear_after_drops_only_decisions_originating_past_target() {
        let mut pd = PendingDecisions {
            git_guard: Some(GitGuardData {}),
            spec_approval: Some(SpecApprovalData {}),
            plan_approval: Some(PlanApprovalData {}),
            skip_to_impl: Some(SkipToImplData {}),
            dreaming: Some(DreamingData {}),
        };
        // Rewinding to Plan keeps Plan-or-earlier decisions but drops the
        // later Finalization-origin dreaming modal.
        pd.clear_after(Phase::Plan);
        assert!(pd.git_guard.is_some());
        assert!(pd.spec_approval.is_some());
        assert!(pd.plan_approval.is_some());
        assert!(pd.skip_to_impl.is_some());
        assert!(pd.dreaming.is_none());
        // Rewinding further back to Idea drops the Spec-origin decisions too.
        pd.clear_after(Phase::Idea);
        assert!(pd.git_guard.is_some());
        assert!(pd.spec_approval.is_none());
        assert!(pd.plan_approval.is_none());
        assert!(pd.skip_to_impl.is_none());
    }
}
