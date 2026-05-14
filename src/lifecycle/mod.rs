//! Lifecycle module — new agent-lifecycle types introduced in Step 1.
//!
//! Nothing in this module is wired up yet. The types live alongside the
//! existing pipeline/phase/run plumbing in [`crate::state`] and
//! [`crate::app`]; the cutover that replaces those code paths happens in a
//! later step. Importing this module must not change behavior.
//!
//! The submodules are organized by concern:
//! - [`phase`] — the slim, round-aware [`Phase`] enum.
//! - [`stage_id`] — lifecycle-internal [`StageId`] (distinct from the UI's
//!   `view::StageId`; 14 pipeline-stage variants vs. the UI's 9 modal ones).
//! - [`spec`] — [`StageSpec`] and [`ActiveRun`].
//! - [`fsm`] — runtime FSM ([`AgentState`], [`Fsm`], outcomes).
//! - [`pending`] — [`PendingDecisions`] replacing the old `*Paused`/`*Pending`
//!   `Phase` variants.
//! - [`stage`] — the [`Stage`] trait and [`StageRegistry`].
//! - [`stages`] — concrete `Stage` impls (one per [`StageId`]). Step 2.
//! - [`persist`] — V2 persistence shapes (added alongside the existing
//!   `SessionState`/`RunRecord` types, not replacing them yet).
pub mod fsm;
pub mod pending;
pub mod persist;
pub mod phase;
pub mod spec;
pub mod stage;
pub mod stage_id;
pub mod stages;

pub use fsm::{
    AfterStop, AgentState, CancelledBy, FinalizedRun, Fsm, FsmError, Outcome, StopResolution,
};
pub use pending::{
    DreamingData, GitGuardData, PendingDecisions, PlanApprovalData, SkipToImplData,
    SpecApprovalData,
};
pub use persist::{RunRecordV2, SessionFileV2};
pub use phase::Phase;
pub use spec::{ActiveRun, StageSpec};
pub use stage::{RunHistoryEntry, Stage, StageCtx, StageRegistry, SuccessOutcome, WorkUnit};
pub use stage_id::StageId;
