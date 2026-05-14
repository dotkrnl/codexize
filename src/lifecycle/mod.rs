//! Lifecycle module — agent-lifecycle FSM, lifecycle stage, stage driver, and scheduler.
//!
//! Provides the runtime lifecycle management that drives the App alongside the
//! [`crate::state::Stage`] bridge:
//! - [`stage_state`] — the compact, round-aware [`Stage`] enum.
//! - [`stage_id`] — lifecycle-internal [`StageId`] (distinct from the UI's
//!   `view::StageId`; 14 pipeline-stage variants vs. the UI's 9 modal ones).
//! - [`spec`] — [`StageSpec`] and [`ActiveRun`].
//! - [`fsm`] — runtime FSM ([`AgentState`], [`Fsm`], outcomes).
//! - [`pending`] — [`PendingDecisions`] for approval and operator-decision gates.
//! - [`stage`] — the [`StageDriver`] trait and [`StageRegistry`].
//! - [`stages`] — concrete stage-driver impls (one per [`StageId`]).
//! - [`stage_id::stage_id_for_run`] — best-effort stage id from run
//!   record fields.
pub mod fsm;
pub mod ops;
pub mod pending;
pub mod scheduler;
pub mod spec;
pub mod stage;
pub mod stage_id;
pub mod stage_state;
pub mod stages;

pub use fsm::{
    AfterStop, AgentState, CancelledBy, CleanupPlan, FinalizedRun, Fsm, FsmError, Outcome,
    StopResolution,
};
pub use ops::{LifecycleOps, OpAction, OpOutcome, OpsCtx, resolution_to_action};
pub use pending::{
    DreamingData, GitGuardData, PendingDecisions, PlanApprovalData, SkipToImplData,
    SpecApprovalData,
};
pub use scheduler::{BlockReason, Scheduler, TickInput, TickOutcome};
pub use spec::{ActiveRun, StageSpec};
pub use stage::{RunHistoryEntry, StageCtx, StageDriver, StageRegistry, SuccessOutcome, WorkUnit};
pub use stage_id::StageId;
pub use stage_id::stage_id_for_run;
pub use stage_state::Stage;
pub use stages::default_registry;
