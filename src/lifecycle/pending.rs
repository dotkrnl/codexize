//! Pending operator-decision state.
//!
//! Replaces the `*Paused` / `*Pending` variants on the old `Phase` enum. Each
//! field is `Some(_)` when the lifecycle is waiting on the operator for that
//! decision. The marker `*Data` structs are intentionally empty named-field
//! structs (for TOML serialization).
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

}
