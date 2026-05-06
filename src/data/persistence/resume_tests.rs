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
