//! Persisting resume flow that reads optional artifacts and logs progress.
use crate::data::artifacts::{ArtifactKind, SkipToImplProposal};
use crate::logic::pipeline::stage::Stage;
use crate::state::{ResumeError, SessionState, can_resume, session_dir};
/// Resume a session, logging the resumption event.
pub fn resume_session(state: &mut SessionState) -> Result<(), ResumeError> {
    can_resume(state)?;
    state
        .log_event("resuming session")
        .map_err(|e| ResumeError::InvalidState(format!("Failed to log resume event: {e}")))?;
    match state.current_stage {
        Stage::RepoStateUpdateRunning => {
            // Not safely resumable: partial writes to spec.md/plan.md may have
            // occurred. Revert to WaitingToImplement so the scheduler restarts
            // the update from current inputs on the next eligible tick.
            let _ =
                state.log_event("resume: reverting RepoStateUpdateRunning to WaitingToImplement");
            state.current_stage = Stage::WaitingToImplement;
            if let Err(e) = state.save() {
                tracing::warn!("resume: failed to save after stage revert: {e}");
            }
        }
        Stage::WaitingToImplement => {
            // Idle stage; leave as-is and let the scheduler re-evaluate on the
            // next tick.
            let _ = state.log_event("resume: WaitingToImplement left idle");
        }
        Stage::SkipToImplPending => {
            let path = session_dir(&state.session_id)
                .join("artifacts")
                .join(ArtifactKind::SkipToImpl.filename());
            match SkipToImplProposal::read_from_path(&path) {
                Ok((Some(p), warnings)) if p.proposed => {
                    log_skip_proposal_warnings(state, warnings);
                    state.skip_to_impl_rationale = Some(p.rationale);
                    state.skip_to_impl_kind = Some(p.status);
                    if let Err(e) = state.save() {
                        tracing::warn!("resume: failed to save skip-to-impl artifact state: {e}");
                    }
                }
                Ok((Some(_), warnings)) => {
                    log_skip_proposal_warnings(state, warnings);
                    return Err(ResumeError::InvalidState(
                        "skip_proposal.toml must contain proposed = true while resuming SkipToImplPending"
                            .to_string(),
                    ));
                }
                Ok((None, warnings)) => {
                    log_skip_proposal_warnings(state, warnings);
                    return Err(ResumeError::InvalidState(
                        "skip_proposal.toml is required while resuming SkipToImplPending"
                            .to_string(),
                    ));
                }
                Err(err) => {
                    return Err(ResumeError::InvalidState(format!(
                        "invalid skip_proposal.toml while resuming SkipToImplPending: {err:#}"
                    )));
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn log_skip_proposal_warnings(state: &mut SessionState, warnings: Vec<String>) {
    for warning in warnings {
        let _ = state.log_event(format!("resume: skip_proposal.toml: {warning}"));
    }
}
#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
