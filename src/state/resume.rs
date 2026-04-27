use super::{Phase, SessionState, session_dir};
use crate::artifacts::{ArtifactKind, SkipToImplProposal};

/// Errors that can occur when attempting to resume a session.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ResumeError {
    InvalidState(String),
    CorruptedArtifacts(Vec<String>),
    ActiveSessionConflict(String),
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeError::InvalidState(msg) => write!(f, "Cannot resume: {msg}"),
            ResumeError::CorruptedArtifacts(paths) => {
                write!(f, "Corrupted artifacts: {}", paths.join(", "))
            }
            ResumeError::ActiveSessionConflict(msg) => {
                write!(f, "Active session conflict: {msg}")
            }
        }
    }
}

impl std::error::Error for ResumeError {}

/// Check whether the current state can be safely resumed.
pub fn can_resume(state: &SessionState) -> Result<(), ResumeError> {
    match state.current_phase {
        Phase::Done => {
            return Err(ResumeError::InvalidState(
                "Cannot resume a completed session".to_string(),
            ));
        }
        Phase::IdeaInput => {
            return Err(ResumeError::InvalidState(
                "No work to resume from IdeaInput phase".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

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
mod tests {
    use super::*;
    use crate::artifacts::SkipProposalStatus;
    use std::fs;

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("CODEXIZE_ROOT");

        // SAFETY: env mutation is serialized by `test_fs_lock`.
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
                None => std::env::remove_var("CODEXIZE_ROOT"),
            }
        }
        result.unwrap()
    }

    #[test]
    fn resume_skip_to_impl_pending_with_overlength_proposal_keeps_modal() {
        with_temp_root(|| {
            let session_id = "resume-skip-overlength";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::SkipToImplPending;

            let session_dir = session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            fs::create_dir_all(&artifacts).expect("mk artifacts dir");

            let rationale = "x".repeat(520);
            let proposal_toml = format!(
                "proposed = true\nstatus = \"nothing_to_do\"\nrationale = \"{}\"\n",
                rationale
            );
            fs::write(artifacts.join("skip_proposal.toml"), proposal_toml)
                .expect("write skip proposal");

            resume_session(&mut state).expect("resume should succeed");

            assert_eq!(state.current_phase, Phase::SkipToImplPending);
            assert_eq!(
                state.skip_to_impl_kind,
                Some(SkipProposalStatus::NothingToDo)
            );
            let stored_rationale = state
                .skip_to_impl_rationale
                .expect("rationale should be set");
            assert_eq!(stored_rationale.chars().count(), 500);
        });
    }
}
