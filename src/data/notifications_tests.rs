use super::*;
use std::fs;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
#[serial_test::serial]
fn ntfy_config_env_override_isolated_from_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    let override_path = dir.path().join("override.toml");
    let home_path = dir.path().join("home").join(".codexize").join("ntfy.toml");
    // Environment mutation is process-global, so this serial test restores
    // both variables before returning to avoid leaking paths into other tests.
    let previous_override = std::env::var_os("CODEXIZE_NTFY_CONFIG");
    let previous_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("CODEXIZE_NTFY_CONFIG", &override_path);
        std::env::set_var("HOME", dir.path().join("home"));
    }

    let config = ensure_ntfy_config(false).expect("create override config");

    assert_topic_shape(&config.topic);
    assert!(override_path.exists());
    assert!(
        !home_path.exists(),
        "override must avoid real/default home path"
    );

    restore_env_var("CODEXIZE_NTFY_CONFIG", previous_override);
    restore_env_var("HOME", previous_home);
}

#[test]
fn missing_ntfy_config_disables_notifications() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ntfy.toml");

    assert!(load_ntfy_config_at(&path).is_none());
    assert!(!path.exists(), "load must not create config");
}

#[test]
fn invalid_ntfy_config_disables_notifications() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ntfy.toml");
    fs::write(&path, "not = [valid").expect("write invalid config");

    assert!(load_ntfy_config_at(&path).is_none());
}

#[test]
fn ensure_ntfy_config_creates_default_enabled_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("ntfy.toml");

    let config = ensure_ntfy_config_at(&path, false).expect("create config");

    assert_eq!(config.version, 1);
    assert_eq!(config.server, DEFAULT_NTFY_SERVER);
    assert!(config.enabled);
    assert_eq!(config.detail_mode, NtfyDetailMode::Detailed);
    assert_eq!(config.created_at, config.updated_at);
    assert_topic_shape(&config.topic);
    assert!(path.exists());
}

#[test]
fn ensure_ntfy_config_reuses_existing_topic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ntfy.toml");
    let first = ensure_ntfy_config_at(&path, false).expect("create config");

    let second = ensure_ntfy_config_at(&path, false).expect("reuse config");

    assert_eq!(second.topic, first.topic);
    assert_eq!(second.created_at, first.created_at);
    assert_eq!(second.updated_at, first.updated_at);
}

#[test]
fn ensure_ntfy_config_reset_rotates_topic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ntfy.toml");
    let first = ensure_ntfy_config_at(&path, false).expect("create config");

    let second = ensure_ntfy_config_at(&path, true).expect("reset config");

    assert_ne!(second.topic, first.topic);
    assert_eq!(second.created_at, first.created_at);
    assert!(second.updated_at >= first.updated_at);
    assert_topic_shape(&second.topic);
}

#[test]
fn load_ntfy_config_rejects_disabled_or_invalid_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ntfy.toml");
    fs::write(
        &path,
        r#"
version = 1
server = "https://ntfy.sh"
topic = "abc"
enabled = false
detail_mode = "detailed"
created_at = "2026-05-06T12:00:00Z"
updated_at = "2026-05-06T12:00:00Z"
"#,
    )
    .expect("write disabled config");
    assert!(load_ntfy_config_at(&path).is_none());

    fs::write(
        &path,
        r#"
version = 1
server = ""
topic = "../bad"
enabled = true
detail_mode = "verbose"
created_at = "not a timestamp"
updated_at = "2026-05-06T12:00:00Z"
"#,
    )
    .expect("write invalid config");
    assert!(load_ntfy_config_at(&path).is_none());
}

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
fn formatter_preserves_context_and_minimal_mode_reduces_detail() {
    let mut config = test_config(NtfyDetailMode::Detailed, "https://ntfy.example");
    let event = sample_event(
        NotificationEventKind::InputNeeded,
        NotificationReason::InteractiveRunWait,
        crate::state::Phase::BrainstormRunning,
    );

    let detailed = format_ntfy_message(&config, &event);

    assert_eq!(detailed.title, "codexize: input needed");
    assert!(detailed.body.contains("session: Readable Session"));
    assert!(detailed.body.contains("session_id: session-a"));
    assert!(detailed.body.contains("phase: Brainstorming"));
    assert!(detailed.body.contains("stage: brainstorm"));
    assert!(detailed.body.contains("run_id: 7"));
    assert!(
        detailed
            .body
            .contains("reason: interactive agent is waiting")
    );
    assert!(detailed.body.len() <= NTFY_BODY_MAX_BYTES);

    config.detail_mode = NtfyDetailMode::Minimal;
    let minimal = format_ntfy_message(&config, &event);

    assert_eq!(minimal.title, "codexize: input needed");
    assert!(minimal.body.len() < detailed.body.len());
    assert!(minimal.body.contains("input needed"));
    assert!(minimal.body.contains("session-a"));
    assert!(minimal.body.contains("brainstorm"));
    assert!(!minimal.body.contains("Readable Session"));
}

#[test]
fn formatter_normalizes_title_and_truncates_body_on_utf8_boundaries() {
    let config = test_config(NtfyDetailMode::Detailed, "https://ntfy.example");
    let mut event = sample_event(
        NotificationEventKind::PipelineDone,
        NotificationReason::PhaseWait,
        crate::state::Phase::Done,
    );
    event.context.session_label = "title ".repeat(2000);

    let message = format_ntfy_message(&config, &event);

    assert_eq!(message.title, "codexize: pipeline done");
    assert!(message.title.is_ascii());
    assert!(message.body.len() <= NTFY_BODY_MAX_BYTES);
    assert!(
        message.body.ends_with("..."),
        "truncation marker should be preserved"
    );
    assert!(std::str::from_utf8(message.body.as_bytes()).is_ok());
}

#[tokio::test]
async fn publisher_posts_to_configured_endpoint_with_formatted_title_and_body() {
    let (server, requests) = MockNtfyServer::spawn(vec![MockResponse::ok()]).await;
    let config = test_config(NtfyDetailMode::Detailed, &server.url());
    let event = sample_event(
        NotificationEventKind::InputNeeded,
        NotificationReason::PhaseWait,
        crate::state::Phase::SpecReviewPaused,
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("client");

    send_ntfy_with_policy(
        &client,
        &config,
        &event,
        NtfyPublishPolicy::for_test(1, Duration::ZERO),
    )
    .await
    .expect("publish succeeds");

    let requests = requests.await.expect("server task");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/topic-test");
    assert_eq!(
        requests[0].header("title").as_deref(),
        Some("codexize: input needed")
    );
    assert!(requests[0].body.contains("session: Readable Session"));
}

#[tokio::test]
async fn publisher_retries_non_2xx_and_reports_final_failure() {
    let (server, requests) = MockNtfyServer::spawn(vec![
        MockResponse::status(503),
        MockResponse::status(503),
        MockResponse::ok(),
    ])
    .await;
    let config = test_config(NtfyDetailMode::Minimal, &server.url());
    let event = sample_event(
        NotificationEventKind::PipelineDone,
        NotificationReason::PhaseWait,
        crate::state::Phase::Done,
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("client");

    send_ntfy_with_policy(
        &client,
        &config,
        &event,
        NtfyPublishPolicy::for_test(3, Duration::ZERO),
    )
    .await
    .expect("eventually succeeds");
    assert_eq!(requests.await.expect("server task").len(), 3);

    let (server, _requests) =
        MockNtfyServer::spawn(vec![MockResponse::status(500), MockResponse::status(500)]).await;
    let mut config = config;
    config.server = server.url();
    let err = send_ntfy_with_policy(
        &client,
        &config,
        &event,
        NtfyPublishPolicy::for_test(2, Duration::ZERO),
    )
    .await
    .expect_err("final non-2xx is an error");
    assert!(
        format!("{err:#}").contains("500"),
        "error should include final HTTP status: {err:#}"
    );
}

#[tokio::test]
async fn runtime_drains_pending_sends_but_honors_timeout() {
    let (server, _requests) =
        MockNtfyServer::spawn(vec![MockResponse::delayed_ok(Duration::from_millis(50))]).await;
    let config = test_config(NtfyDetailMode::Minimal, &server.url());
    let mut runtime = NotificationRuntime::from_config_for_test(
        Some(config),
        NtfyPublishPolicy::for_test(1, Duration::ZERO),
    );

    runtime.emit_pipeline_done(
        crate::state::Phase::Done,
        sample_context("session-drain", "pipeline"),
    );
    assert_eq!(runtime.pending_sends_for_test(), 1);
    assert!(runtime.drain_pending_sends(Duration::from_secs(1)).await);
    assert_eq!(runtime.pending_sends_for_test(), 0);

    let (server, _requests) =
        MockNtfyServer::spawn(vec![MockResponse::delayed_ok(Duration::from_millis(300))]).await;
    let config = test_config(NtfyDetailMode::Minimal, &server.url());
    let mut runtime = NotificationRuntime::from_config_for_test(
        Some(config),
        NtfyPublishPolicy::for_test(1, Duration::ZERO),
    );
    runtime.emit_pipeline_done(
        crate::state::Phase::Done,
        sample_context("session-timeout", "pipeline"),
    );
    let started = std::time::Instant::now();

    assert!(!runtime.drain_pending_sends(Duration::from_millis(25)).await);
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "drain should remain bounded"
    );
}

fn assert_topic_shape(topic: &str) {
    assert_eq!(topic.len(), 32, "16 random bytes encoded as hex");
    assert!(
        topic.bytes().all(|b| b.is_ascii_hexdigit()),
        "topic is URL-safe hex: {topic}"
    );
}

fn test_config(detail_mode: NtfyDetailMode, server: &str) -> NtfyConfig {
    NtfyConfig {
        version: 1,
        server: server.to_string(),
        topic: "topic-test".to_string(),
        enabled: true,
        detail_mode,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
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

    fn delayed_ok(delay: Duration) -> Self {
        Self {
            delay: Some(delay),
            ..Self::ok()
        }
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
    body: String,
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
        body: String::from_utf8_lossy(&buf[body_start..body_start + content_length]).to_string(),
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn restore_env_var(key: &str, value: Option<std::ffi::OsString>) {
    // The caller is a serial test that owns these process-wide variables.
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
