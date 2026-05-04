//! Pure pre-resume validation for [`SessionState`].
//!
//! The IO-side of resume (logging the resume event and reading
//! `skip_proposal.toml`) lives in [`crate::data::persistence::resume`].

use crate::logic::pipeline::phase::Phase;
use crate::logic::pipeline::state::SessionState;

/// Errors that can occur when attempting to resume a session.
#[derive(Debug)]
pub enum ResumeError {
    InvalidState(String),
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeError::InvalidState(msg) => write!(f, "Cannot resume: {msg}"),
        }
    }
}

impl std::error::Error for ResumeError {}

/// Check whether the current state can be safely resumed.
pub fn can_resume(state: &SessionState) -> Result<(), ResumeError> {
    match state.current_phase {
        Phase::Done => Err(ResumeError::InvalidState(
            "Cannot resume a completed session".to_string(),
        )),
        Phase::IdeaInput => Err(ResumeError::InvalidState(
            "No work to resume from IdeaInput phase".to_string(),
        )),
        _ => Ok(()),
    }
}
