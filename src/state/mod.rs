//! Flat `crate::state::*` public surface.
//!
//! Canonical state/session data, stage IO contracts, and resume validation
//! live here. The pipeline module only owns the phase graph and pure
//! transition mutators.
mod builder;
#[path = "resume.rs"]
mod resume_logic;
mod stage_io;
#[path = "types.rs"]
mod types_logic;
pub use crate::data::persistence::resume_session;
pub use crate::logic::pipeline::{FinishedRunRecord, Phase, TransitionError};
pub use builder::BuilderState;
pub use resume_logic::{ResumeError, can_resume};
pub use stage_io::{
    BRAINSTORM_IO, CODER_IO, PLAN_REVIEWER_IO, PLANNER_IO, RECOVERY_IO, RECOVERY_PLAN_REVIEWER_IO,
    RECOVERY_SHARDER_IO, REVIEWER_IO, SHARDER_IO, SIMPLIFIER_IO, SPEC_REVIEWER_IO, StageIO,
    stage_io, stage_io_with_mode,
};
#[cfg(test)]
pub(crate) use types_logic::test_fs_lock;
pub use types_logic::{
    BlockOrigin, DreamingDecision, DreamingDecisionKind, Event, LaunchModes, Message, MessageKind,
    MessageSender, Modes, Node, NodeKind, NodeStatus, PendingGuardDecision, PipelineItem,
    PipelineItemStatus, RunRecord, RunStatus, SectionPart, SessionState, codexize_root,
    session_dir,
};
pub(crate) use types_logic::{EventsFile, MessagesFile};
/// Compatibility module mirroring the pre-refactor `crate::state::transitions`
/// surface. Pure mutators are re-exported from
/// [`crate::logic::pipeline::transitions`]; persisting wrappers come from
/// [`crate::data::persistence::transitions`].
pub mod transitions {
    pub use crate::data::persistence::transitions::{
        FinalValidationEntry, SimplificationEntry, block_with_origin, enter_final_validation,
        enter_simplification, execute_transition, finish_run_record, resume_running_runs,
        start_agent_run, start_agent_run_with_id, try_parse_toml_artifact,
    };
    pub use crate::logic::pipeline::transitions::{
        FinishedRunRecord, SIMPLIFICATION_ATTEMPT_CAP, TransitionError, VALIDATION_ATTEMPT_CAP,
        append_final_validation_gap_tasks, append_refine_feedback, apply_revise_with_new_tasks,
        archive_session, clear_agent_error, clear_builder_recovery_context,
        clear_pending_guard_decision, clear_skip_to_impl_proposal, ensure_builder_task_for_round,
        increment_recovery_cycle_count, initialize_task_pipeline, load_task_titles_if_empty,
        mark_current_task_for_recovery, mark_latest_pipeline_stage_done,
        mark_latest_pipeline_stage_running, mark_task_status, prepare_new_session_for_brainstorm,
        queue_recovery_plan_review, queue_recovery_sharding, queue_recovery_stage,
        record_agent_error, record_brainstorm_launch, record_builder_recovery_context,
        record_builder_verdict, record_pending_guard_decision, record_session_title,
        record_skip_to_impl_proposal, replace_recovery_pipeline, reset_builder_after_rewind,
        reset_recovery_cycle_count, restore_archived_session, restore_guard_originating_phase,
        set_cheap_mode, set_phase_for_operator_retry, set_retry_reset_run_id_cutoff, set_yolo_mode,
        take_pending_guard_decision, take_pending_refine_feedback, validate_transition,
    };
    pub use crate::state::stage_io::{
        BRAINSTORM_IO, CODER_IO, PLAN_REVIEWER_IO, PLANNER_IO, RECOVERY_IO,
        RECOVERY_PLAN_REVIEWER_IO, RECOVERY_SHARDER_IO, REVIEWER_IO, SHARDER_IO, SIMPLIFIER_IO,
        SPEC_REVIEWER_IO, StageIO, stage_io, stage_io_with_mode,
    };
}
/// Compatibility module mirroring the pre-refactor `crate::state::resume`
/// surface.
pub mod resume {
    pub use crate::data::persistence::resume::resume_session;
    pub use crate::state::resume_logic::{ResumeError, can_resume};
}
#[cfg(test)]
mod tests_mod;
