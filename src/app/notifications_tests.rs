use crate::app::test_support::{mk_app, with_temp_root};
use crate::data::config::schema::NtfyDetailMode;
use crate::data::config::view::NtfyEventsView;
use crate::data::notifications::{
    NotificationEventKind, NotificationParams, NotificationReason, NotificationRuntime,
};
use crate::state::{
    BlockOrigin, LaunchModes, Message, MessageKind, MessageSender, Phase, RunRecord, RunStatus,
    SessionState,
};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn state_in_phase(phase: Phase) -> SessionState {
    static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let mut state = SessionState::new(format!("notify-session-{id}"));
    state.current_phase = phase;
    state.title = Some("Readable Session".to_string());
    state
}

fn unique_state_in_phase(phase: Phase, label: &str) -> SessionState {
    let mut state = SessionState::new(format!(
        "notify-{label}-{}",
        chrono::Utc::now().timestamp_nanos_opt().expect("timestamp")
    ));
    state.current_phase = phase;
    state.title = Some("Readable Session".to_string());
    state
}

fn app_in_phase(phase: Phase) -> crate::app::App {
    let mut app = mk_app(state_in_phase(phase));
    app.enable_notifications_for_test();
    app
}

fn running_run(id: u64, stage: &str, interactive: bool) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "codex-latest".to_string(),
        vendor: "openai".to_string(),
        window_name: format!("[{stage}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: LaunchModes {
            interactive,
            ..LaunchModes::default()
        },
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

#[test]
fn waiting_phase_transitions_emit_input_needed_events() {
    with_temp_root(|| {
        let cases = [
            (
                Phase::BrainstormRunning,
                Phase::BlockedNeedsUser,
                Some(BlockOrigin::Brainstorm),
                "brainstorm",
            ),
            (
                Phase::SpecReviewRunning,
                Phase::SpecReviewPaused,
                None,
                "spec-review",
            ),
            (
                Phase::PlanReviewRunning,
                Phase::PlanReviewPaused,
                None,
                "plan-review",
            ),
            (
                Phase::BrainstormRunning,
                Phase::SkipToImplPending,
                None,
                "skip-to-impl",
            ),
            (
                Phase::BrainstormRunning,
                Phase::GitGuardPending,
                None,
                "git-guard",
            ),
            (
                Phase::FinalValidation(1),
                Phase::DreamingPending,
                None,
                "dreaming",
            ),
        ];

        for (from, to, block_origin, expected_stage) in cases {
            let mut app = app_in_phase(from);
            app.state.block_origin = block_origin;

            app.transition_to_phase(to).expect("transition succeeds");

            let events = app.notification_events_for_test();
            assert_eq!(events.len(), 1, "{to:?} should emit once");
            assert_eq!(events[0].kind, NotificationEventKind::InputNeeded);
            assert_eq!(events[0].reason, NotificationReason::PhaseWait);
            assert_eq!(events[0].phase, to);
            assert_eq!(events[0].context.stage, expected_stage);
            assert_eq!(events[0].context.session_label, "Readable Session");
        }
    });
}

#[test]
fn done_transition_emits_pipeline_done_event() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::FinalValidation(1));

        app.transition_to_phase(Phase::Done)
            .expect("done transition succeeds");

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, NotificationEventKind::PipelineDone);
        assert_eq!(events[0].phase, Phase::Done);
        assert_eq!(events[0].context.stage, "pipeline");
    });
}

#[test]
fn repeated_ticks_in_same_waiting_phase_do_not_emit_duplicates() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::SpecReviewRunning);
        app.transition_to_phase(Phase::SpecReviewPaused)
            .expect("pause transition succeeds");

        app.runtime_tick_after_data_drain();
        app.runtime_tick_after_data_drain();

        assert_eq!(app.notification_events_for_test().len(), 1);
    });
}

#[test]
fn re_entering_waiting_phase_emits_a_new_event() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::SpecReviewRunning);

        app.transition_to_phase(Phase::SpecReviewPaused)
            .expect("first pause succeeds");
        app.transition_to_phase(Phase::SpecReviewRunning)
            .expect("resume succeeds");
        app.transition_to_phase(Phase::SpecReviewPaused)
            .expect("second pause succeeds");

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 2);
        assert_ne!(events[0].dedupe_key, events[1].dedupe_key);
    });
}

#[test]
fn same_wait_phase_in_later_stage_is_not_suppressed() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::BrainstormRunning);
        app.state.block_origin = Some(BlockOrigin::Brainstorm);

        app.transition_to_phase(Phase::BlockedNeedsUser)
            .expect("brainstorm block succeeds");
        app.transition_to_phase(Phase::PlanningRunning)
            .expect("resume into planning succeeds");
        app.state.block_origin = Some(BlockOrigin::Planning);
        app.transition_to_phase(Phase::BlockedNeedsUser)
            .expect("planning block succeeds");

        let stages: Vec<&str> = app
            .notification_events_for_test()
            .iter()
            .map(|event| event.context.stage.as_str())
            .collect();
        assert_eq!(stages, vec!["brainstorm", "planning"]);
    });
}

#[test]
fn interactive_wait_rising_edge_emits_once_until_next_prompt() {
    with_temp_root(|| {
        let mut state = state_in_phase(Phase::BrainstormRunning);
        state.agent_runs.push(running_run(7, "brainstorm", true));
        let mut app = mk_app(state);
        app.enable_notifications_for_test();
        app.current_run_id = Some(7);
        crate::runner::register_test_run_id("[brainstorm]", 7);
        crate::runner::request_run_label_active_for_test("[brainstorm]");

        app.runtime_tick_after_data_drain();
        assert!(app.notification_events_for_test().is_empty());

        crate::runner::request_run_label_interactive_input_for_test("[brainstorm]");
        app.messages.push(Message {
            ts: chrono::Utc::now(),
            run_id: 7,
            kind: MessageKind::AgentText,
            sender: MessageSender::Agent {
                model: "codex-latest".to_string(),
                vendor: "openai".to_string(),
            },
            text: "Need your input".to_string(),
        });
        app.runtime_tick_after_data_drain();
        app.runtime_tick_after_data_drain();

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, NotificationEventKind::InputNeeded);
        assert_eq!(events[0].reason, NotificationReason::InteractiveRunWait);
        assert_eq!(events[0].context.stage, "brainstorm");
        assert_eq!(events[0].context.run_id, Some(7));
    });
}

#[test]
fn phase_wait_event_carries_last_live_summary() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::SpecReviewRunning);
        app.live_summary_cached_text = "drafted §3 about caching layer invariants".to_string();

        app.transition_to_phase(Phase::SpecReviewPaused)
            .expect("transition succeeds");

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].context.last_live_summary.as_deref(),
            Some("drafted §3 about caching layer invariants"),
            "phase-wait notifications should surface the live summary"
        );
        assert!(
            events[0].context.last_agent_response.is_none(),
            "phase-wait notifications must not carry an agent response"
        );
    });
}

#[test]
fn modal_decision_phases_skip_live_summary() {
    // Skip-to-impl and git-guard prompts are modal decisions where the
    // live summary would just echo the prompt; the App should leave the
    // field empty so the body stays a single sentence.
    with_temp_root(|| {
        for phase in [Phase::SkipToImplPending, Phase::GitGuardPending] {
            let mut app = app_in_phase(Phase::BrainstormRunning);
            app.live_summary_cached_text = "should not surface here".to_string();

            app.transition_to_phase(phase).expect("transition succeeds");

            let events = app.notification_events_for_test();
            assert_eq!(events.len(), 1);
            assert!(
                events[0].context.last_live_summary.is_none(),
                "{phase:?} should not carry a live summary"
            );
        }
    });
}

#[test]
fn pipeline_done_event_carries_last_live_summary() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::FinalValidation(1));
        app.live_summary_cached_text = "ran final validation, all green".to_string();

        app.transition_to_phase(Phase::Done)
            .expect("done transition succeeds");

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].context.last_live_summary.as_deref(),
            Some("ran final validation, all green")
        );
    });
}

#[test]
fn interactive_wait_event_carries_last_agent_response() {
    // Distinct window-name / run-id from the rising-edge test above so the
    // process-global runner registry (`request_run_label_*`) cannot leak
    // between this test and the rising-edge case under cargo's parallel
    // runner.
    const RUN_ID: u64 = 71;
    const WINDOW: &str = "[brainstorm-context]";
    with_temp_root(|| {
        let mut state = state_in_phase(Phase::BrainstormRunning);
        let mut run = running_run(RUN_ID, "brainstorm", true);
        run.window_name = WINDOW.to_string();
        state.agent_runs.push(run);
        let mut app = mk_app(state);
        app.enable_notifications_for_test();
        app.current_run_id = Some(RUN_ID);
        crate::runner::register_test_run_id(WINDOW, RUN_ID);
        crate::runner::request_run_label_active_for_test(WINDOW);
        // Brief on the same run is the live summary; it must NOT be picked
        // up for an interactive-run wait — the agent's question is what the
        // operator wants to read.
        app.messages.push(Message {
            ts: chrono::Utc::now(),
            run_id: RUN_ID,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: "codex-latest".to_string(),
                vendor: "openai".to_string(),
            },
            text: "live summary that should not surface".to_string(),
        });
        app.messages.push(Message {
            ts: chrono::Utc::now(),
            run_id: RUN_ID,
            kind: MessageKind::AgentText,
            sender: MessageSender::Agent {
                model: "codex-latest".to_string(),
                vendor: "openai".to_string(),
            },
            text: "Should I keep going on the migration plan?".to_string(),
        });
        crate::runner::request_run_label_interactive_input_for_test(WINDOW);

        app.runtime_tick_after_data_drain();

        let events = app.notification_events_for_test();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, NotificationReason::InteractiveRunWait);
        assert_eq!(
            events[0].context.last_agent_response.as_deref(),
            Some("Should I keep going on the migration plan?")
        );
        assert!(
            events[0].context.last_live_summary.is_none(),
            "interactive-run waits must not carry a live summary"
        );
    });
}

#[test]
fn stage_starts_retries_and_mid_run_errors_do_not_emit_events() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::IdeaInput);

        app.transition_to_phase(Phase::BrainstormRunning)
            .expect("stage start succeeds");
        app.record_agent_error("mid-run warning");
        app.runtime_tick_after_data_drain();

        assert!(app.notification_events_for_test().is_empty());
    });
}

#[test]
fn notification_publish_failures_surface_warning_without_changing_phase() {
    with_temp_root(|| {
        let mut app = app_in_phase(Phase::SpecReviewPaused);
        app.notification_runtime
            .push_publish_failure_for_test("ntfy publish failed after 3 attempts: 503");

        app.poll_notification_reports();

        assert_eq!(app.state.current_phase, Phase::SpecReviewPaused);
        let events_path = crate::state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(events.contains("notification_publish_failed"));
        assert!(events.contains("503"));
        let status = app
            .status_line
            .borrow()
            .render()
            .expect("warning status")
            .to_string();
        assert!(status.contains("ntfy notification failed"));
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_drain_surfaces_pending_publish_failures_without_changing_phase() {
    // The tokio runtime forces this test to run on its own; we still need
    // a temp `CODEXIZE_ROOT` so events.toml lands in scratch, not the host
    // repo. with_temp_root takes the same fs lock as the sync siblings, so
    // ordering with them is preserved.
    let server = FailingNtfyServer::spawn(500).await;
    with_temp_root(|| {
        let mut app = mk_app(unique_state_in_phase(
            Phase::FinalValidation(1),
            "shutdown-drain",
        ));
        app.notification_runtime =
            NotificationRuntime::from_params_for_test(test_ntfy_params(&server.url()));

        app.transition_to_phase(Phase::Done)
            .expect("done transition succeeds");
        app.drain_notifications_for_shutdown();

        assert_eq!(app.state.current_phase, Phase::Done);
        let events_path = crate::state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(events.contains("notification_publish_failed"));
        assert!(events.contains("500"));
        let status = app
            .status_line
            .borrow()
            .render()
            .expect("warning status")
            .to_string();
        assert!(status.contains("ntfy notification failed"));
    });
}

fn test_ntfy_params(server: &str) -> NotificationParams {
    NotificationParams {
        enabled: true,
        server: server.to_string(),
        topic: "topic-test".to_string(),
        detail_mode: NtfyDetailMode::Minimal,
        body_max_bytes: 4096,
        excerpt_max_chars: 600,
        retry_attempts: 1,
        retry_delay_ms: 0,
        http_timeout_secs: 5,
        events: NtfyEventsView {
            phase_wait: true,
            interactive_wait: true,
            pipeline_done: true,
        },
    }
}

struct FailingNtfyServer {
    url: String,
}

impl FailingNtfyServer {
    async fn spawn(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_http_request(&mut stream).await;
            let body = "mock";
            let wire = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(wire.as_bytes()).await.expect("write");
        });
        Self { url }
    }

    fn url(&self) -> String {
        self.url.clone()
    }
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) {
    let mut buf = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let n = stream.read(&mut chunk).await.expect("read");
        assert!(n > 0, "request stream closed before headers");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = headers
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let mut chunk = [0_u8; 1024];
        let n = stream.read(&mut chunk).await.expect("read body");
        assert!(n > 0, "request stream closed before body");
        buf.extend_from_slice(&chunk[..n]);
    }
}
