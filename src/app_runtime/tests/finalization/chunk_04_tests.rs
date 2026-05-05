use super::*;

#[test]
fn final_validation_missing_verdict_fails_closed_to_blocked() {
    with_temp_root(|| {
        let session_id = "final-validation-missing-verdict";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(1);
        let run = RunRecord {
            id: 10,
            stage: "final-validation".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[FinalValidation]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(
            app.state.block_origin,
            Some(crate::state::BlockOrigin::FinalValidation)
        );
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .contains("artifact_missing"),
            "missing final validation verdict must fail closed"
        );
    });
}

#[test]
fn final_validation_invalid_verdict_fails_closed_to_blocked() {
    with_temp_root(|| {
        let session_id = "final-validation-invalid-verdict";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(
            artifacts.join("final_validation_1.toml"),
            r#"status = "goal_met"
summary = "claims success despite declaring a gap"
findings = ["checked workspace status"]

[[gaps]]
description = "this is invalid for goal_met"
checked = ["artifacts/spec.md"]
"#,
        )
        .expect("verdict");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(1);
        let run = RunRecord {
            id: 11,
            stage: "final-validation".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            vendor: "test-vendor".to_string(),
            window_name: "[FinalValidation]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(
            app.state.block_origin,
            Some(crate::state::BlockOrigin::FinalValidation)
        );
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("artifact_invalid:"),
            "invalid final validation verdict must fail closed"
        );
    });
}

#[test]
fn simplifier_simplified_status_transitions_to_final_validation() {
    with_temp_root(|| {
        let session_id = "simplifier-simplified-to-validation";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("simplification.toml"),
            r#"status = "simplified"
summary = "Renamed two helpers; inlined a single-use function."
commits = ["abc123"]
files_touched = ["src/foo.rs"]
"#,
        )
        .expect("simplification toml");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        let run = make_simplifier_run(7, 1, 1);
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::FinalValidation(1));
        // Each FinalValidation entry consumes one validation attempt; this is
        // the first validation entry, so the counter must read 1.
        assert_eq!(app.state.validation_attempts, 1);
    });
}

#[test]
fn simplifier_no_changes_status_transitions_to_final_validation() {
    with_temp_root(|| {
        let session_id = "simplifier-no-changes";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("simplification.toml"),
            r#"status = "no_changes"
summary = "Round diff already tight; nothing worth touching."
"#,
        )
        .expect("simplification toml");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        let run = make_simplifier_run(8, 1, 1);
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::FinalValidation(1));
    });
}

#[test]
fn simplifier_skipped_status_transitions_to_final_validation() {
    with_temp_root(|| {
        let session_id = "simplifier-skipped";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("simplification.toml"),
            r#"status = "skipped"
summary = "Docs-only round; no source diff to simplify."
"#,
        )
        .expect("simplification toml");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        let run = make_simplifier_run(9, 1, 1);
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        assert_eq!(app.state.current_phase, Phase::FinalValidation(1));
    });
}

#[test]
fn simplifier_missing_toml_records_artifact_missing_failure() {
    with_temp_root(|| {
        let session_id = "simplifier-missing-toml";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        // Deliberately do NOT write simplification.toml.

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        let run = make_simplifier_run(10, 1, 1);
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        // Without prior model retries available, maybe_auto_retry routes the
        // run through BlockOrigin::for_stage("simplifier") = Simplification.
        let last = app.state.agent_runs.last().expect("run record");
        assert_eq!(last.status, RunStatus::Failed);
        assert!(
            last.error
                .as_deref()
                .unwrap_or_default()
                .contains("artifact_missing"),
            "missing simplification TOML must surface as artifact_missing"
        );
        // FinalValidation must not advance — the simplifier never reported
        // a valid verdict for this round.
        assert_ne!(app.state.current_phase, Phase::FinalValidation(1));
        assert_eq!(app.state.validation_attempts, 0);
    });
}

#[test]
fn simplifier_invalid_toml_records_artifact_invalid_failure() {
    with_temp_root(|| {
        let session_id = "simplifier-invalid-toml";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("simplification.toml"),
            // Unknown status — schema violation.
            r#"status = "approved"
summary = "wrong status name"
"#,
        )
        .expect("simplification toml");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        let run = make_simplifier_run(11, 1, 1);
        state.agent_runs.push(run.clone());

        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        write_finish_stamp_for_run(&app, &run, 0, "");

        app.poll_agent_run();

        let last = app.state.agent_runs.last().expect("run record");
        assert_eq!(last.status, RunStatus::Failed);
        assert!(
            last.error
                .as_deref()
                .unwrap_or_default()
                .starts_with("artifact_invalid:"),
            "schema-violation simplification TOML must surface as artifact_invalid: {:?}",
            last.error
        );
        assert_eq!(app.state.validation_attempts, 0);
    });
}

#[test]
fn force_ship_is_denied_from_simplification_block() {
    with_temp_root(|| {
        let session_id = "simplifier-force-ship-denial";
        // Reach a Simplification block by exhausting the per-round attempt
        // cap — the same path operators encounter in production.
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::Simplification(1);
        state.block_origin = Some(crate::state::BlockOrigin::Simplification);
        // Drop into the block directly, bypassing transition validation
        // (the runtime force-ship guard's only input is `block_origin`).
        state.current_phase = Phase::BlockedNeedsUser;

        // The runtime guard says: a BlockedNeedsUser → Done transition is
        // only legal when block_origin = FinalValidation. Confirm Simplification
        // does NOT unlock force-ship.
        let result = session_state::transitions::execute_transition(&mut state, Phase::Done);
        assert!(
            result.is_err(),
            "force-ship from a Simplification block must be denied"
        );
        assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(
            state.block_origin,
            Some(crate::state::BlockOrigin::Simplification)
        );
    });
}
