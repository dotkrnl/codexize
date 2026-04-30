use crate::adapters::EffortLevel;
use crate::app::split::SplitTarget;
use crate::app::test_harness::{key, mk_app};
use crate::state::{LaunchModes, Phase, RunRecord, RunStatus, SessionState};

#[test]
fn synchronize_split_target_force_opens_interactive_prompt() {
    let mut state = SessionState::new("force-open".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    let run = RunRecord {
        id: 42,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "m".to_string(),
        vendor: "v".to_string(),
        window_name: "test-run".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes {
            interactive: true,
            ..Default::default()
        },
        hostname: None,
        mount_device_id: None,
    };
    state.agent_runs.push(run);

    // Mock the run label to be waiting for input
    crate::runner::request_run_label_interactive_input_for_test("test-run");

    let mut app = mk_app(state);
    app.current_run_id = Some(42);

    assert!(app.split_target.is_none());
    assert!(!app.input_mode);

    app.synchronize_split_target();

    assert_eq!(app.split_target, Some(SplitTarget::Run(42)));
    assert!(app.input_mode);
}

#[test]
fn synchronize_split_target_force_opens_idea_input() {
    let mut state = SessionState::new("force-open-idea".to_string());
    state.current_phase = Phase::IdeaInput;

    let mut app = mk_app(state);

    assert!(app.split_target.is_none());

    app.synchronize_split_target();

    assert_eq!(app.split_target, Some(SplitTarget::Idea));
    assert!(app.input_mode);
}

#[test]
fn esc_in_idea_input_closes_split_but_sync_reopens() {
    let mut state = SessionState::new("esc-idea".to_string());
    state.current_phase = Phase::IdeaInput;

    let mut app = mk_app(state);
    app.synchronize_split_target();
    assert_eq!(app.split_target, Some(SplitTarget::Idea));
    assert!(app.input_mode);

    // Press Esc - should close split and exit input mode in one go
    app.handle_key(key(crossterm::event::KeyCode::Esc));
    assert!(app.split_target.is_none());
    assert!(!app.input_mode);

    // Next sync reopens it because we are still in IdeaInput phase
    app.synchronize_split_target();
    assert_eq!(app.split_target, Some(SplitTarget::Idea));
    assert!(app.input_mode);
}

#[test]
fn esc_in_interactive_prompt_closes_split_but_sync_reopens() {
    let mut state = SessionState::new("esc-interactive".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    let run = RunRecord {
        id: 42,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "m".to_string(),
        vendor: "v".to_string(),
        window_name: "test-run".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes {
            interactive: true,
            ..Default::default()
        },
        hostname: None,
        mount_device_id: None,
    };
    state.agent_runs.push(run);
    crate::runner::request_run_label_interactive_input_for_test("test-run");

    let mut app = mk_app(state);
    app.current_run_id = Some(42);
    app.synchronize_split_target();
    assert_eq!(app.split_target, Some(SplitTarget::Run(42)));
    assert!(app.input_mode);

    // Press Esc
    app.handle_key(key(crossterm::event::KeyCode::Esc));
    assert!(app.split_target.is_none());
    assert!(!app.input_mode);

    // Next sync reopens it
    app.synchronize_split_target();
    assert_eq!(app.split_target, Some(SplitTarget::Run(42)));
    assert!(app.input_mode);
}

#[test]
fn interactive_split_input_treats_colon_as_text_before_palette() {
    let mut state = SessionState::new("interactive-colon-text".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    let run = RunRecord {
        id: 42,
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        model: "m".to_string(),
        vendor: "v".to_string(),
        window_name: "test-run".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes {
            interactive: true,
            ..Default::default()
        },
        hostname: None,
        mount_device_id: None,
    };
    state.agent_runs.push(run);
    crate::runner::request_run_label_interactive_input_for_test("test-run");

    let mut app = mk_app(state);
    app.current_run_id = Some(42);
    app.synchronize_split_target();
    assert_eq!(app.split_target, Some(SplitTarget::Run(42)));
    assert!(app.input_mode);

    app.handle_key(key(crossterm::event::KeyCode::Char(':')));

    assert_eq!(app.input_buffer, ":");
    assert!(
        !app.palette.open,
        "interactive split input should keep ':' as editor text instead of opening the palette"
    );
}
