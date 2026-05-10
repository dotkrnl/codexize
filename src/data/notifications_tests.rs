use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn generated_topics_are_opaque_url_safe_and_unprefixed() {
    let first = generate_topic().expect("generate topic");
    let second = generate_topic().expect("generate topic");

    assert_ne!(first, second);
    assert_topic_shape(&first);
    assert!(!first.starts_with("codexize"));
}

#[test]
fn notification_dedupe_is_process_local_and_suppresses_same_marker() {
    let context = NotificationContext {
        session_id: "session-a".to_string(),
        session_label: "Session A".to_string(),
        stage: "brainstorm".to_string(),
        task_id: None,
        round: Some(1),
        attempt: Some(1),
        run_id: Some(7),
        last_live_summary: None,
        last_agent_response: None,
    };
    let marker = InteractiveWaitMarker {
        run_id: 7,
        message_index: 3,
    };
    let mut first_runtime = NotificationRuntime::enabled_for_test();

    first_runtime.emit_interactive_wait(
        crate::state::Phase::BrainstormRunning,
        context.clone(),
        marker,
    );
    first_runtime.emit_interactive_wait(
        crate::state::Phase::BrainstormRunning,
        context.clone(),
        marker,
    );

    assert_eq!(first_runtime.events().len(), 1);

    let mut restarted_runtime = NotificationRuntime::enabled_for_test();
    restarted_runtime.emit_interactive_wait(
        crate::state::Phase::BrainstormRunning,
        context,
        marker,
    );

    assert_eq!(restarted_runtime.events().len(), 1);
    assert_eq!(
        restarted_runtime.events()[0].dedupe_key,
        first_runtime.events()[0].dedupe_key
    );
}

#[test]
fn formatter_uses_prose_and_attaches_last_agent_response_for_interactive_wait() {
    let mut event = sample_event(
        NotificationEventKind::InputNeeded,
        NotificationReason::InteractiveRunWait,
        crate::state::Phase::BrainstormRunning,
    );
    event.context.last_agent_response = Some(
        "Should I keep going on the implementation plan, or focus on tests first?".to_string(),
    );
    event.context.last_live_summary = Some("Should not surface here".to_string());

    let detailed = format_ntfy_message(&event, NtfyDetailMode::Detailed, 4096, 600);

    assert_eq!(detailed.title, "codexize: agent is waiting on you");
    assert!(
        detailed
            .body
            .contains("brainstorm agent on \"Readable Session\" is waiting on a reply")
    );
    assert!(detailed.body.contains("Last response:"));
    assert!(
        detailed
            .body
            .contains("Should I keep going on the implementation plan")
    );
    assert!(
        !detailed.body.contains("Last activity:"),
        "interactive wait must surface the agent response, not the live summary"
    );
    assert!(
        !detailed.body.contains("Should not surface here"),
        "interactive wait should ignore last_live_summary"
    );
    assert!(detailed.body.len() <= 4096);

    let minimal = format_ntfy_message(&event, NtfyDetailMode::Minimal, 4096, 600);

    assert_eq!(minimal.title, "codexize: agent is waiting on you");
    assert!(
        minimal
            .body
            .contains("brainstorm agent on \"Readable Session\" is waiting on a reply")
    );
    assert!(
        !minimal.body.contains("Last response:"),
        "minimal mode should drop the excerpt line"
    );
    assert!(minimal.body.len() < detailed.body.len());
}

#[test]
fn formatter_attaches_last_live_summary_for_phase_wait_and_pipeline_done() {
    let mut spec_review_event = sample_event(
        NotificationEventKind::InputNeeded,
        NotificationReason::PhaseWait,
        crate::state::Phase::SpecReviewPaused,
    );
    spec_review_event.context.last_live_summary =
        Some("drafted §3 about caching layer invariants".to_string());

    let detailed = format_ntfy_message(&spec_review_event, NtfyDetailMode::Detailed, 4096, 600);
    assert_eq!(detailed.title, "codexize: spec ready for review");
    assert!(
        detailed
            .body
            .contains("Spec review is paused on \"Readable Session\"")
    );
    assert!(
        detailed
            .body
            .contains("Last activity: drafted §3 about caching layer invariants")
    );

    let mut done_event = sample_event(
        NotificationEventKind::PipelineDone,
        NotificationReason::PhaseWait,
        crate::state::Phase::Done,
    );
    done_event.context.last_live_summary = Some("ran final validation, all green".to_string());

    let done = format_ntfy_message(&done_event, NtfyDetailMode::Detailed, 4096, 600);
    assert_eq!(done.title, "codexize: pipeline finished");
    assert!(
        done.body
            .contains("Pipeline finished on \"Readable Session\"")
    );
    assert!(
        done.body
            .contains("Last activity: ran final validation, all green")
    );
}

#[test]
fn formatter_excerpts_long_context_lines() {
    let mut event = sample_event(
        NotificationEventKind::InputNeeded,
        NotificationReason::InteractiveRunWait,
        crate::state::Phase::BrainstormRunning,
    );
    event.context.last_agent_response = Some("x".repeat(5_000));

    let formatted = format_ntfy_message(&event, NtfyDetailMode::Detailed, 4096, 600);

    assert!(formatted.body.len() <= 4096);
    assert!(
        formatted.body.contains("..."),
        "long agent responses must be truncated with an ellipsis"
    );
}

#[test]
fn formatter_normalizes_title_and_truncates_body_on_utf8_boundaries() {
    let mut event = sample_event(
        NotificationEventKind::PipelineDone,
        NotificationReason::PhaseWait,
        crate::state::Phase::Done,
    );
    event.context.session_label = "title ".repeat(2000);

    let message = format_ntfy_message(&event, NtfyDetailMode::Detailed, 4096, 600);

    assert_eq!(message.title, "codexize: pipeline finished");
    assert!(message.title.is_ascii());
    assert!(message.body.len() <= 4096);
    assert!(
        message.body.ends_with("..."),
        "truncation marker should be preserved"
    );
    assert!(std::str::from_utf8(message.body.as_bytes()).is_ok());
}

#[tokio::test]
async fn publisher_posts_to_configured_endpoint_with_formatted_title_and_body() {
    let (server, requests) = MockNtfyServer::spawn(vec![MockResponse::ok()]).await;
    let mut params = test_params(NtfyDetailMode::Detailed, &server.url());
    params.retry_attempts = 1;
    params.retry_delay_ms = 0;
    let mut runtime = NotificationRuntime::from_params_for_test(params);
    let context = sample_context("session-a", "brainstorm");

    runtime.emit_phase_wait(
        crate::state::Phase::SpecReviewPaused,
        NotificationContext {
            last_live_summary: None,
            ..context
        },
    );
    assert_eq!(runtime.pending_sends_for_test(), 1);
    let _ = runtime.drain_pending_sends(Duration::from_secs(2)).await;

    let reqs = requests.await.expect("server task");
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/topic-test");
    assert_eq!(
        reqs[0].header("title").as_deref(),
        Some("codexize: spec ready for review")
    );
}

#[test]
fn event_gates_suppress_disabled_events() {
    let params = NotificationParams {
        enabled: true,
        server: "https://ntfy.sh".to_string(),
        topic: "test-topic".to_string(),
        detail_mode: NtfyDetailMode::Minimal,
        body_max_bytes: 4096,
        excerpt_max_chars: 600,
        retry_attempts: 1,
        retry_delay_ms: 0,
        http_timeout_secs: 5,
        events: NtfyEventsView {
            phase_wait: false,
            interactive_wait: false,
            pipeline_done: true,
        },
    };
    let mut runtime = NotificationRuntime::from_params_for_test(params);
    let context = sample_context("session-gate", "brainstorm");

    // phase_wait and interactive_wait are disabled
    runtime.emit_phase_wait(crate::state::Phase::SpecReviewPaused, context.clone());
    runtime.emit_interactive_wait(
        crate::state::Phase::BrainstormRunning,
        context.clone(),
        InteractiveWaitMarker {
            run_id: 1,
            message_index: 0,
        },
    );

    // pipeline_done is enabled
    runtime.emit_pipeline_done(crate::state::Phase::Done, context);

    assert_eq!(runtime.events().len(), 1);
    assert_eq!(
        runtime.events()[0].kind,
        NotificationEventKind::PipelineDone
    );
}

fn assert_topic_shape(topic: &str) {
    assert_eq!(topic.len(), 32, "16 random bytes encoded as hex");
    assert!(
        topic.bytes().all(|b| b.is_ascii_hexdigit()),
        "topic is URL-safe hex: {topic}"
    );
}

fn test_params(detail_mode: NtfyDetailMode, server: &str) -> NotificationParams {
    NotificationParams {
        enabled: true,
        server: server.to_string(),
        topic: "topic-test".to_string(),
        detail_mode,
        body_max_bytes: 4096,
        excerpt_max_chars: 600,
        retry_attempts: 3,
        retry_delay_ms: 250,
        http_timeout_secs: 10,
        events: NtfyEventsView {
            phase_wait: true,
            interactive_wait: true,
            pipeline_done: true,
        },
    }
}

fn sample_context(session_id: &str, stage: &str) -> NotificationContext {
    NotificationContext {
        session_id: session_id.to_string(),
        session_label: "Readable Session".to_string(),
        stage: stage.to_string(),
        task_id: Some(3),
        round: Some(2),
        attempt: Some(1),
        run_id: Some(7),
        last_live_summary: None,
        last_agent_response: None,
    }
}

fn sample_event(
    kind: NotificationEventKind,
    reason: NotificationReason,
    phase: crate::state::Phase,
) -> NotificationEvent {
    let context = sample_context("session-a", "brainstorm");
    NotificationEvent {
        kind,
        reason,
        phase,
        dedupe_key: NotificationDedupeKey::PipelineDone {
            session_id: context.session_id.clone(),
            occurrence: 1,
        },
        context,
    }
}

fn format_ntfy_message(
    event: &NotificationEvent,
    detail_mode: NtfyDetailMode,
    body_max_bytes: u64,
    excerpt_max_chars: u32,
) -> NtfyMessage {
    let title = normalize_header_title(&prose_title(event));
    let include_context = matches!(detail_mode, NtfyDetailMode::Detailed);
    let body = prose_body(event, include_context, excerpt_max_chars);
    NtfyMessage {
        title,
        body: truncate_body(body, body_max_bytes),
    }
}

#[derive(Debug)]
struct MockNtfyServer {
    url: String,
}

impl MockNtfyServer {
    async fn spawn(
        responses: Vec<MockResponse>,
    ) -> (Self, tokio::task::JoinHandle<Vec<RecordedRequest>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let request = read_request(&mut stream).await;
                if let Some(delay) = response.delay {
                    tokio::time::sleep(delay).await;
                }
                let body = response.body;
                let wire = format!(
                    "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    body.len(),
                    body
                );
                stream.write_all(wire.as_bytes()).await.expect("write");
                requests.push(request);
            }
            requests
        });
        (Self { url }, task)
    }

    fn url(&self) -> String {
        self.url.clone()
    }
}

#[derive(Debug)]
struct MockResponse {
    status: u16,
    body: String,
    delay: Option<Duration>,
}

impl MockResponse {
    fn ok() -> Self {
        Self::status(200)
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            body: "mock".to_string(),
            delay: None,
        }
    }
}

#[derive(Debug)]
struct RecordedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> RecordedRequest {
    let mut buf = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 1024];
        let n = stream.read(&mut chunk).await.expect("read");
        assert!(n > 0, "request stream closed before headers");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = headers.split("\r\n");
    let request_line = lines.next().expect("request line");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().expect("method").to_string();
    let path = request_parts.next().expect("path").to_string();
    let header_pairs: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_string(), value.trim().to_string()))
        .collect();
    let content_length = header_pairs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let mut chunk = [0_u8; 1024];
        let n = stream.read(&mut chunk).await.expect("read body");
        assert!(n > 0, "request stream closed before body");
        buf.extend_from_slice(&chunk[..n]);
    }
    RecordedRequest {
        method,
        path,
        headers: header_pairs,
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}
