//! Pipeline state, phase graph, transition rules, and pure helpers.
//!
//! The persistence-side `impl SessionState` block lives in
//! [`crate::data::persistence::session`]; only pure data and pure mutators
//! live here.
pub mod phase;
pub mod transitions;
pub use phase::Phase;
pub use transitions::{
    FinishedRunRecord, TransitionError, append_final_validation_gap_tasks, append_refine_feedback,
    apply_revise_with_new_tasks, archive_session, clear_agent_error,
    clear_builder_recovery_context, clear_pending_guard_decision, clear_skip_to_impl_proposal,
    ensure_builder_task_for_round, increment_recovery_cycle_count, initialize_task_pipeline,
    mark_current_task_for_recovery, mark_latest_pipeline_stage_done,
    mark_latest_pipeline_stage_running, mark_task_status, prepare_new_session_for_brainstorm,
    queue_recovery_plan_review, queue_recovery_sharding, queue_recovery_stage, record_agent_error,
    record_brainstorm_launch, record_builder_recovery_context, record_builder_verdict,
    record_pending_guard_decision, record_session_title, record_skip_to_impl_proposal,
    replace_recovery_pipeline, reset_builder_after_rewind, reset_recovery_cycle_count,
    restore_archived_session, restore_guard_originating_phase, set_cheap_mode,
    set_phase_for_operator_retry, set_retry_reset_run_id_cutoff, set_yolo_mode,
    take_pending_guard_decision, take_pending_refine_feedback, validate_transition,
};
