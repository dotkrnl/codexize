//! Flat `crate::state::*` public surface.
//!
//! Pipeline state lives canonically in [`crate::logic::pipeline`] (pure
//! data + mutators) and [`crate::data::persistence`] (filesystem-backed
//! save/load and persisting transition wrappers). This module flattens
//! both halves into the single `crate::state::{transitions, resume, …}`
//! shape consumed by `main.rs`, integration tests, and the future
//! server-mode binary — it is the intentional public API surface, not a
//! migration leftover. New logic/data callers should still prefer the
//! layered names above.

pub use crate::logic::pipeline::{
    BRAINSTORM_IO, BlockOrigin, BuilderState, CODER_IO, Event, FinishedRunRecord, LaunchModes,
    Message, MessageKind, MessageSender, Modes, Node, NodeKind, NodeStatus, PLAN_REVIEWER_IO,
    PLANNER_IO, PendingGuardDecision, Phase, PipelineItem, PipelineItemStatus, RECOVERY_IO,
    RECOVERY_PLAN_REVIEWER_IO, RECOVERY_SHARDER_IO, REVIEWER_IO, ResumeError, RunRecord, RunStatus,
    SHARDER_IO, SIMPLIFIER_IO, SPEC_REVIEWER_IO, SectionPart, SessionState, StageIO,
    TransitionError, can_resume, codexize_root, session_dir,
};

pub use crate::data::persistence::resume_session;

#[cfg(test)]
pub(crate) use crate::logic::pipeline::test_fs_lock;

/// Compatibility module mirroring the pre-refactor `crate::state::transitions`
/// surface. Pure mutators are re-exported from
/// [`crate::logic::pipeline::transitions`]; persisting wrappers come from
/// [`crate::data::persistence::transitions`].
pub mod transitions {
    pub use crate::data::persistence::transitions::{
        FinalValidationEntry, SimplificationEntry, block_with_origin, enter_final_validation,
        enter_simplification, execute_transition, finish_run_record, resume_running_runs,
        start_agent_run, try_parse_toml_artifact,
    };
    pub use crate::logic::pipeline::transitions::{
        BRAINSTORM_IO, CODER_IO, FinishedRunRecord, PLAN_REVIEWER_IO, PLANNER_IO, RECOVERY_IO,
        RECOVERY_PLAN_REVIEWER_IO, RECOVERY_SHARDER_IO, REVIEWER_IO, SHARDER_IO,
        SIMPLIFICATION_ATTEMPT_CAP, SIMPLIFIER_IO, SPEC_REVIEWER_IO, StageIO, TransitionError,
        VALIDATION_ATTEMPT_CAP, append_final_validation_gap_tasks, append_refine_feedback,
        apply_revise_with_new_tasks, archive_session, clear_agent_error,
        clear_builder_recovery_context, clear_pending_guard_decision, clear_skip_to_impl_proposal,
        ensure_builder_task_for_round, increment_recovery_cycle_count, initialize_task_pipeline,
        load_task_titles_if_empty, mark_current_task_for_recovery, mark_latest_pipeline_stage_done,
        mark_latest_pipeline_stage_running, mark_task_status, prepare_new_session_for_brainstorm,
        queue_recovery_plan_review, queue_recovery_sharding, queue_recovery_stage,
        record_agent_error, record_brainstorm_launch, record_builder_recovery_context,
        record_builder_verdict, record_pending_guard_decision, record_session_title,
        record_skip_to_impl_proposal, replace_recovery_pipeline, reset_builder_after_rewind,
        reset_recovery_cycle_count, restore_archived_session, restore_guard_originating_phase,
        set_cheap_mode, set_phase_for_operator_retry, set_retry_reset_run_id_cutoff, set_yolo_mode,
        stage_io, stage_io_with_mode, take_pending_guard_decision, take_pending_refine_feedback,
        validate_transition,
    };
}

/// Compatibility module mirroring the pre-refactor `crate::state::resume`
/// surface.
pub mod resume {
    pub use crate::data::persistence::resume::resume_session;
    pub use crate::logic::pipeline::resume::{ResumeError, can_resume};
}

#[cfg(test)]
mod tests_mod;
