//! Persisting resume flow that reads optional artifacts and logs progress.

use crate::artifacts::{ArtifactKind, SkipToImplProposal};
use crate::logic::pipeline::phase::Phase;
use crate::state::{ResumeError, SessionState, can_resume, session_dir};

/// Resume a session, logging the resumption event.
pub fn resume_session(state: &mut SessionState) -> Result<(), ResumeError> {
    can_resume(state)?;
    state
        .log_event("resuming session")
        .map_err(|e| ResumeError::InvalidState(format!("Failed to log resume event: {e}")))?;

    if state.current_phase == Phase::SkipToImplPending {
        let path = session_dir(&state.session_id)
            .join("artifacts")
            .join(ArtifactKind::SkipToImpl.filename());
        match SkipToImplProposal::read_from_path(&path) {
            Ok((Some(p), warnings)) if p.proposed => {
                for w in warnings {
                    let _ = state.log_event(format!("resume: skip_proposal.toml: {w}"));
                }
                state.skip_to_impl_rationale = Some(p.rationale);
                state.skip_to_impl_kind = Some(p.status);
            }
            Ok((_, warnings)) => {
                for w in warnings {
                    let _ = state.log_event(format!("resume: skip_proposal.toml: {w}"));
                }
                let _ = state.log_event(
                    "resume: skip_to_impl artifact missing or not proposed, falling through to SpecReviewRunning",
                );
                state.skip_to_impl_rationale = None;
                state.skip_to_impl_kind = None;
                state.current_phase = Phase::SpecReviewRunning;
                let _ = state.save();
            }
            Err(err) => {
                let _ = state.log_event(format!(
                    "resume: skip_to_impl artifact malformed, falling through to SpecReviewRunning: {err:#}"
                ));
                state.skip_to_impl_rationale = None;
                state.skip_to_impl_kind = None;
                state.current_phase = Phase::SpecReviewRunning;
                let _ = state.save();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
