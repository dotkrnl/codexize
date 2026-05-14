//! Session-state IO: load/save/log helpers and persisting transition wrappers.
//!
//! The pure pipeline state and pure mutators live in
//! [`crate::logic::pipeline`]. This module extends `SessionState` with
//! filesystem and process-side methods, and wraps the pure transition
//! mutators in routines that log + persist.
pub mod resume;
pub mod session;
pub mod transitions;
pub use resume::resume_session;
pub use transitions::{
    FinalValidationEntry, SimplificationEntry, block_with_origin, enter_final_validation,
    enter_simplification, execute_transition, finish_run_record, resume_running_runs,

};
