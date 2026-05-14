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
    match state.current_phase {
        Phase::RepoStateUpdateRunning => {
            // Not safely resumable: partial writes to spec.md/plan.md may have
            // occurred. Revert to WaitingToImplement so the scheduler restarts
            // the update from current inputs on the next eligible tick.
            let _ =
                state.log_event("resume: reverting RepoStateUpdateRunning to WaitingToImplement");
            state.current_phase = Phase::WaitingToImplement;
            if let Err(e) = state.save() {
                tracing::warn!("resume: failed to save after phase revert: {e}");
            }
        }
        Phase::WaitingToImplement => {
            // Idle phase; leave as-is and let the scheduler re-evaluate on the
            // next tick.
            let _ = state.log_event("resume: WaitingToImplement left idle");
        }
        Phase::SkipToImplPending => {
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
                    if let Err(e) = state.save() {
                        tracing::warn!("resume: failed to save skip-to-impl artifact state: {e}");
                    }
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
                    state.pending_decisions.skip_to_impl = None;
                    state.current_phase = Phase::SpecReviewRunning;
                    if let Err(e) = state.save() {
                        tracing::warn!("resume: failed to save after skip-to-impl fallback: {e}");
                    }
                }
                Err(err) => {
                    let _ = state.log_event(format!(
                        "resume: skip_to_impl artifact malformed, falling through to SpecReviewRunning: {err:#}"
                    ));
                    state.skip_to_impl_rationale = None;
                    state.skip_to_impl_kind = None;
                    state.pending_decisions.skip_to_impl = None;
                    state.current_phase = Phase::SpecReviewRunning;
                    if let Err(e) = state.save() {
                        tracing::warn!("resume: failed to save after malformed artifact fallback: {e}");
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}
#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
