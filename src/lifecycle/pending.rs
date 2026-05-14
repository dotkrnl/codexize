//! Pending operator-decision state.
//!
//! Replaces the `*Paused` / `*Pending` variants on the old `Phase` enum. Each
//! field is `Some(_)` when the lifecycle is waiting on the operator for that
//! decision. The marker `*Data` structs are intentionally empty in Step 1;
//! Step 5 will fill them in with the data currently carried inline on the
//! old [`crate::logic::pipeline::Phase`] variants.
use super::phase::Phase;
use serde::{Deserialize, Serialize};

/// Operator decision payload for the git-guard modal (`HEAD` moved under
/// `GuardMode::AskOperator`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitGuardData;

/// Operator decision payload for the spec-review pause.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecApprovalData;

/// Operator decision payload for the plan-review pause.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanApprovalData;

/// Operator decision payload for the "skip to implementation" modal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkipToImplData;

/// Operator decision payload for the Dreaming-after-validation modal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DreamingData;

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
    /// True when any pending decision applies to `phase`.
    ///
    /// The phase-applicability map is intentionally permissive in Step 1: a
    /// decision is "applicable" if it is set. Step 5 narrows this once the
    /// per-decision data carries the originating phase context.
    // TODO: Step 5 — once the *Data variants carry their originating phase,
    // narrow `applicable_at` to a real per-decision phase map.
    pub fn blocks(&self, phase: Phase) -> bool {
        let _ = phase;
        self.git_guard.is_some()
            || self.spec_approval.is_some()
            || self.plan_approval.is_some()
            || self.skip_to_impl.is_some()
            || self.dreaming.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_does_not_block() {
        let pd = PendingDecisions::default();
        assert!(!pd.blocks(Phase::Idea));
        assert!(!pd.blocks(Phase::Spec));
        assert!(!pd.blocks(Phase::Implementation(3)));
    }

    #[test]
    fn any_some_field_blocks() {
        let cases: Vec<PendingDecisions> = vec![
            PendingDecisions {
                git_guard: Some(GitGuardData),
                ..Default::default()
            },
            PendingDecisions {
                spec_approval: Some(SpecApprovalData),
                ..Default::default()
            },
            PendingDecisions {
                plan_approval: Some(PlanApprovalData),
                ..Default::default()
            },
            PendingDecisions {
                skip_to_impl: Some(SkipToImplData),
                ..Default::default()
            },
            PendingDecisions {
                dreaming: Some(DreamingData),
                ..Default::default()
            },
        ];
        for pd in cases {
            assert!(pd.blocks(Phase::Plan));
        }
    }
}
