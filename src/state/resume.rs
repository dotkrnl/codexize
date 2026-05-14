//! Pure pre-resume validation for [`SessionState`].
//!
//! The IO-side of resume (logging the resume event and reading
//! `skip_proposal.toml`) lives in [`crate::data::persistence::resume`].
use crate::logic::pipeline::stage::Stage;
use crate::state::SessionState;
/// Errors that can occur when attempting to resume a session.
#[derive(Debug, thiserror::Error)]
pub enum ResumeError {
    #[error("Cannot resume: {0}")]
    InvalidState(String),
}
/// Check whether the current state can be safely resumed.
pub fn can_resume(state: &SessionState) -> Result<(), ResumeError> {
    match state.current_stage {
        Stage::Done => Err(ResumeError::InvalidState(
            "Cannot resume a completed session".to_string(),
        )),
        Stage::Cancelled => Err(ResumeError::InvalidState(
            "Cannot resume a cancelled session".to_string(),
        )),
        Stage::IdeaInput => Err(ResumeError::InvalidState(
            "No work to resume from IdeaInput stage".to_string(),
        )),
        _ => Ok(()),
    }
}
