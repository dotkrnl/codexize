use super::*;
use crate::adapters::EffortLevel;
use crate::state::{LaunchModes, PipelineItem, PipelineItemStatus, SectionPart};

#[test]
fn coder_run_captures_iteration_loop_task_round_stage_path() {
    let mut state = SessionState::new("path-capture".to_string());
    state.current_phase = Phase::ImplementationRound(9);
    state.builder.pipeline_items.push(PipelineItem {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(4),
        round: Some(9),
        status: PipelineItemStatus::Running,
        title: Some("Extract UI".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 1,
    });
    let run_id = state.next_agent_run_id();
    let id = start_agent_run_with_id(
        &mut state,
        run_id,
        "coder".to_string(),
        Some(4),
        9,
        1,
        "claude-opus-4.7".to_string(),
        "claude".to_string(),
        "[Round 9 Coder]".to_string(),
        EffortLevel::Tough,
        crate::data::config::schema::EffortMapping::default(),
        false,
        LaunchModes::default(),
    );
    let run = state.agent_runs.iter().find(|r| r.id == id).expect("run");
    assert_eq!(
        run.section_path.as_deref(),
        Some(
            &[
                SectionPart::Iteration(1),
                SectionPart::Loop,
                SectionPart::Task(4),
                SectionPart::Round { n: 9, attempt: 1 },
                SectionPart::Stage("coder".to_string()),
            ][..]
        )
    );
}

#[test]
fn simplifier_run_captures_iteration_simplification_round_stage_path() {
    let mut state = SessionState::new("simpl-capture".to_string());
    state.current_phase = Phase::Simplification(9);
    state.builder.pipeline_items.push(PipelineItem {
        id: 1,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: Some(9),
        status: PipelineItemStatus::Approved,
        title: Some("Some task".to_string()),
        mode: None,
        trigger: None,
        interactive: None,
        iteration: 1,
    });
    let run_id = state.next_agent_run_id();
    let id = start_agent_run_with_id(
        &mut state,
        run_id,
        "simplifier".to_string(),
        None,
        9,
        2,
        "claude-opus-4.6".to_string(),
        "claude".to_string(),
        "[Simplifier]".to_string(),
        EffortLevel::Normal,
        crate::data::config::schema::EffortMapping::default(),
        false,
        LaunchModes::default(),
    );
    let run = state.agent_runs.iter().find(|r| r.id == id).expect("run");
    assert_eq!(
        run.section_path.as_deref(),
        Some(
            &[
                SectionPart::Iteration(1),
                SectionPart::Simplification,
                SectionPart::Round { n: 9, attempt: 2 },
                SectionPart::Stage("simplifier".to_string()),
            ][..]
        )
    );
}

#[test]
fn brainstorm_run_captures_brainstorm_stage_path() {
    let mut state = SessionState::new("brainstorm".to_string());
    state.current_phase = Phase::BrainstormRunning;
    let run_id = state.next_agent_run_id();
    let id = start_agent_run_with_id(
        &mut state,
        run_id,
        "brainstorm".to_string(),
        None,
        0,
        1,
        "x".to_string(),
        "y".to_string(),
        "[Brainstorm]".to_string(),
        EffortLevel::Normal,
        crate::data::config::schema::EffortMapping::default(),
        false,
        LaunchModes::default(),
    );
    let run = state.agent_runs.iter().find(|r| r.id == id).expect("run");
    assert_eq!(
        run.section_path.as_deref(),
        Some(
            &[
                SectionPart::Brainstorm,
                SectionPart::Stage("brainstorm".to_string()),
            ][..]
        )
    );
}

#[test]
fn test_try_parse_toml_artifact_missing_file() {
    let result = try_parse_toml_artifact::<toml::Value>(Path::new("/nonexistent/path.toml"));
    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(msg.contains("missing or unreadable"));
}

#[test]
fn test_try_parse_toml_artifact_malformed() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(&path, "this is not { valid toml").unwrap();
    let result = try_parse_toml_artifact::<toml::Value>(&path);
    assert!(result.is_err());
    let msg = format!("{:#}", result.unwrap_err());
    assert!(msg.contains("unparseable TOML"));
}

#[test]
fn test_try_parse_toml_artifact_valid() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("ok.toml");
    std::fs::write(&path, "status = \"approved\"\nsummary = \"good\"").unwrap();
    let val: toml::Value = try_parse_toml_artifact(&path).unwrap();
    assert_eq!(val.get("status").and_then(|v| v.as_str()), Some("approved"));
    assert_eq!(val.get("summary").and_then(|v| v.as_str()), Some("good"));
}

/// Run `f` with a private `CODEXIZE_ROOT` so `execute_transition`'s
/// implicit `SessionState::save` writes into a temp directory that gets
/// cleaned up.
fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock();
    let temp = tempfile::TempDir::new().unwrap();
    let prev = std::env::var_os("CODEXIZE_ROOT");
    // SAFETY: `set_var`/`remove_var` are not thread-safe on *nix; the
    // `test_fs_lock` mutex serializes every test that touches the env.
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
fn force_ship_rejected_without_final_validation_origin() {
    with_temp_root(|| {
        let mut state = SessionState::new("force-ship-recovery".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::BuilderRecovery);
        let err = execute_transition(&mut state, Phase::Done).expect_err("expected guard failure");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("force-ship"),
            "guard error must mention force-ship: {msg}"
        );
        assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
    });
}

#[test]
fn force_ship_rejected_when_block_origin_missing() {
    with_temp_root(|| {
        let mut state = SessionState::new("force-ship-missing".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = None;
        let err = execute_transition(&mut state, Phase::Done).expect_err("expected guard failure");
        assert!(format!("{err:#}").contains("force-ship"));
    });
}

#[test]
fn force_ship_allowed_with_final_validation_origin() {
    with_temp_root(|| {
        let mut state = SessionState::new("force-ship-ok".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::FinalValidation);
        execute_transition(&mut state, Phase::Done).expect("force-ship must succeed");
        assert_eq!(state.current_phase, Phase::Done);
        assert!(state.block_origin.is_none());
    });
}

#[test]
fn block_with_origin_sets_field_and_transitions() {
    with_temp_root(|| {
        let mut state = SessionState::new("block-helper".to_string());
        state.current_phase = Phase::PlanReviewRunning;
        block_with_origin(&mut state, BlockOrigin::PlanReview).expect("block transition succeeds");
        assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(state.block_origin, Some(BlockOrigin::PlanReview));
    });
}

#[test]
fn leaving_block_clears_origin() {
    with_temp_root(|| {
        let mut state = SessionState::new("leave-block".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::Brainstorm);
        execute_transition(&mut state, Phase::BrainstormRunning).expect("rewind succeeds");
        assert_eq!(state.current_phase, Phase::BrainstormRunning);
        assert!(state.block_origin.is_none(), "origin must clear on leave");
    });
}

#[test]
fn final_validation_round_trip_through_execute_transition() {
    with_temp_root(|| {
        let mut state = SessionState::new("fv-round-trip".to_string());
        state.current_phase = Phase::ReviewRound(2);
        execute_transition(&mut state, Phase::Simplification(2)).unwrap();
        execute_transition(&mut state, Phase::FinalValidation(2)).unwrap();
        assert_eq!(state.current_phase, Phase::FinalValidation(2));
        execute_transition(&mut state, Phase::Done).unwrap();
        assert_eq!(state.current_phase, Phase::Done);
    });
}

#[test]
fn enter_final_validation_increments_attempts_for_first_three_entries() {
    with_temp_root(|| {
        let mut state = SessionState::new("fv-cap-increment".to_string());
        assert_eq!(state.validation_attempts, 0);

        state.current_phase = Phase::ReviewRound(1);
        execute_transition(&mut state, Phase::Simplification(1)).unwrap();
        let outcome = enter_final_validation(&mut state, 1).unwrap();
        assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 1 });
        assert_eq!(state.current_phase, Phase::FinalValidation(1));
        assert_eq!(state.validation_attempts, 1);

        execute_transition(&mut state, Phase::ImplementationRound(2)).unwrap();
        execute_transition(&mut state, Phase::ReviewRound(2)).unwrap();
        execute_transition(&mut state, Phase::Simplification(2)).unwrap();
        let outcome = enter_final_validation(&mut state, 2).unwrap();
        assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 2 });
        assert_eq!(state.validation_attempts, 2);

        execute_transition(&mut state, Phase::ImplementationRound(3)).unwrap();
        execute_transition(&mut state, Phase::ReviewRound(3)).unwrap();
        execute_transition(&mut state, Phase::Simplification(3)).unwrap();
        let outcome = enter_final_validation(&mut state, 3).unwrap();
        assert_eq!(outcome, FinalValidationEntry::Entered { attempt: 3 });
        assert_eq!(state.validation_attempts, 3);
        assert_eq!(state.current_phase, Phase::FinalValidation(3));
    });
}

#[test]
fn enter_final_validation_caps_fourth_entry_into_blocked() {
    with_temp_root(|| {
        let mut state = SessionState::new("fv-cap-block".to_string());
        state.validation_attempts = VALIDATION_ATTEMPT_CAP;
        state.current_phase = Phase::Simplification(4);

        let outcome = enter_final_validation(&mut state, 4).unwrap();

        assert_eq!(outcome, FinalValidationEntry::CapExceeded);
        assert_eq!(state.validation_attempts, VALIDATION_ATTEMPT_CAP);
        assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(state.block_origin, Some(BlockOrigin::FinalValidation));
    });
}

#[test]
fn block_origin_simplification_does_not_unlock_force_ship() {
    with_temp_root(|| {
        let mut state = SessionState::new("force-ship-simplification".to_string());
        state.current_phase = Phase::BlockedNeedsUser;
        state.block_origin = Some(BlockOrigin::Simplification);
        let err = execute_transition(&mut state, Phase::Done).expect_err("expected guard failure");
        assert!(format!("{err:#}").contains("force-ship"));
        assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
    });
}

#[test]
fn enter_simplification_increments_per_round_counter() {
    with_temp_root(|| {
        let mut state = SessionState::new("simplify-counter".to_string());
        state.current_phase = Phase::ReviewRound(1);
        let outcome = enter_simplification(&mut state, 1).unwrap();
        assert_eq!(outcome, SimplificationEntry::Entered { attempt: 1 });
        assert_eq!(state.current_phase, Phase::Simplification(1));
        assert_eq!(state.simplification_attempts.get(&1).copied(), Some(1));

        execute_transition(&mut state, Phase::ReviewRound(1)).unwrap();
        let outcome = enter_simplification(&mut state, 1).unwrap();
        assert_eq!(outcome, SimplificationEntry::Entered { attempt: 2 });
        assert_eq!(state.simplification_attempts.get(&1).copied(), Some(2));

        execute_transition(&mut state, Phase::FinalValidation(1)).unwrap();
        execute_transition(&mut state, Phase::ImplementationRound(2)).unwrap();
        execute_transition(&mut state, Phase::ReviewRound(2)).unwrap();
        let outcome = enter_simplification(&mut state, 2).unwrap();
        assert_eq!(outcome, SimplificationEntry::Entered { attempt: 1 });
        assert_eq!(state.simplification_attempts.get(&2).copied(), Some(1));
    });
}

#[test]
fn enter_simplification_caps_fourth_entry_into_blocked() {
    with_temp_root(|| {
        let mut state = SessionState::new("simplify-cap".to_string());
        state
            .simplification_attempts
            .insert(4, SIMPLIFICATION_ATTEMPT_CAP);
        state.current_phase = Phase::ReviewRound(4);

        let outcome = enter_simplification(&mut state, 4).unwrap();

        assert_eq!(outcome, SimplificationEntry::CapExceeded);
        assert_eq!(
            state.simplification_attempts.get(&4).copied(),
            Some(SIMPLIFICATION_ATTEMPT_CAP)
        );
        assert_eq!(state.current_phase, Phase::BlockedNeedsUser);
        assert_eq!(state.block_origin, Some(BlockOrigin::Simplification));
    });
}

#[test]
fn enter_simplification_rejects_illegal_source_phase() {
    with_temp_root(|| {
        let mut state = SessionState::new("simplify-illegal-source".to_string());
        state.current_phase = Phase::PlanningRunning;

        let err = enter_simplification(&mut state, 1).expect_err("must reject");
        assert!(format!("{err:#}").contains("Cannot transition"));
        assert!(!state.simplification_attempts.contains_key(&1));
        assert_eq!(state.current_phase, Phase::PlanningRunning);
    });
}

#[test]
fn enter_final_validation_rejects_illegal_source_phase() {
    with_temp_root(|| {
        let mut state = SessionState::new("fv-illegal-source".to_string());
        state.current_phase = Phase::IdeaInput;

        let err = enter_final_validation(&mut state, 1).expect_err("must reject");
        assert!(format!("{err:#}").contains("Cannot transition"));
        assert_eq!(state.validation_attempts, 0);
        assert_eq!(state.current_phase, Phase::IdeaInput);
    });
}
