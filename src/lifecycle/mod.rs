//! Lifecycle module — agent-lifecycle FSM, slim phase, stage trait, and scheduler.
//!
//! Provides the runtime lifecycle management that drives the App alongside the
//! legacy [`crate::state::Phase`] bridge:
//! - [`phase`] — the slim, round-aware [`Phase`] enum.
//! - [`stage_id`] — lifecycle-internal [`StageId`] (distinct from the UI's
//!   `view::StageId`; 14 pipeline-stage variants vs. the UI's 9 modal ones).
//! - [`spec`] — [`StageSpec`] and [`ActiveRun`].
//! - [`fsm`] — runtime FSM ([`AgentState`], [`Fsm`], outcomes).
//! - [`pending`] — [`PendingDecisions`] replacing the old `*Paused`/`*Pending`
//!   `Phase` variants.
//! - [`stage`] — the [`Stage`] trait and [`StageRegistry`].
//! - [`stages`] — concrete `Stage` impls (one per [`StageId`]).
//! - [`stage_id::stage_id_for_run`] — best-effort stage id from legacy run
//!   record fields.
pub mod fsm;
pub mod ops;
pub mod pending;
pub mod phase;
pub mod scheduler;
pub mod spec;
pub mod stage;
pub mod stage_id;
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
pub use phase::Phase;
pub use spec::{ActiveRun, StageSpec};
pub use scheduler::{BlockReason, Scheduler, TickInput, TickOutcome};
pub use stage::{RunHistoryEntry, Stage, StageCtx, StageRegistry, SuccessOutcome, WorkUnit};
pub use stage_id::StageId;
pub use stages::default_registry;
pub use stage_id::stage_id_for_run;
