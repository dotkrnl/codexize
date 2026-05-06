use super::*;
use std::fs;

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

fn assert_topic_shape(topic: &str) {
    assert_eq!(topic.len(), 32, "16 random bytes encoded as hex");
    assert!(
        topic.bytes().all(|b| b.is_ascii_hexdigit()),
        "topic is URL-safe hex: {topic}"
    );
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
