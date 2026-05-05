// Re-organized from tests_lifecycle.rs — see commit history.

use super::tree::node_at_path;
use super::*;
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    selection::{self},
    state::{
        self as session_state, Message, MessageKind, MessageSender, PendingGuardDecision, Phase,
        PipelineItem, PipelineItemStatus, RunRecord, RunStatus, SessionState,
    },
};

pub(super) fn make_non_interactive_run(id: u64, window_name: &str) -> RunRecord {
    RunRecord {
        id,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "m".to_string(),
        vendor: "v".to_string(),
        window_name: window_name.to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes {
            interactive: false,
            ..Default::default()
        },
        hostname: None,
        mount_device_id: None,
    }
}

pub(super) fn app_waiting_on_agent_exit(session_id: &str) -> (App, String) {
    let mut state = SessionState::new(session_id.to_string());
    state.current_phase = Phase::BrainstormRunning;
    let mut run = make_brainstorm_run(7);
    run.window_name = format!("[Brainstorm {session_id}]");
    run.modes.interactive = true;
    let window_name = run.window_name.clone();
    let model = run.model.clone();
    let vendor = run.vendor.clone();
    state.agent_runs.push(run);
    crate::runner::request_run_label_interactive_input_for_test(&window_name);
    let mut app = idle_app(state);
    app.current_run_id = Some(7);
    app.messages.push(Message {
        ts: chrono::Utc::now(),
        run_id: 7,
        kind: MessageKind::AgentText,
        sender: MessageSender::Agent { model, vendor },
        text: "Done. Enter /exit if there are no further requests.".to_string(),
    });
    (app, window_name)
}

pub(super) const WATCHDOG_TEST_PROMPT_BODY: &str =
    "Original coder prompt — keep this file current until you exit.";

pub(super) fn write_watchdog_test_prompt(session_id: &str, name: &str) -> std::path::PathBuf {
    let dir = session_state::session_dir(session_id).join("prompts");
    std::fs::create_dir_all(&dir).expect("prompts dir");
    let path = dir.join(name);
    std::fs::write(&path, WATCHDOG_TEST_PROMPT_BODY).expect("write prompt");
    path
}

pub(super) fn install_watchdog_run(
    app: &mut App,
    run_id: u64,
    window_name: &str,
    prompt_path: std::path::PathBuf,
    effort: EffortLevel,
) {
    app.watchdog.register(
        run_id,
        effort,
        window_name.to_string(),
        prompt_path,
        std::time::Instant::now(),
    );
}

pub(super) fn fast_forward_idle(app: &mut App, run_id: u64, shift: Duration) {
    let state = app
        .watchdog
        .get_mut(run_id)
        .expect("watchdog state registered");
    state.last_live_summary_event = std::time::Instant::now() - shift - Duration::from_millis(1);
}

mod chunk_00_tests;
mod chunk_01_tests;
mod chunk_02_tests;
mod chunk_03_tests;
mod chunk_04_tests;
mod chunk_05_tests;
mod chunk_06_tests;
