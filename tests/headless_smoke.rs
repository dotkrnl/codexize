use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn next_stdout_line(
    child: &mut std::process::Child,
    lines: &mpsc::Receiver<String>,
    label: &str,
) -> String {
    match lines.recv_timeout(Duration::from_secs(5)) {
        Ok(line) => line,
        Err(err) => {
            let _ = child.kill();
            panic!("timed out waiting for {label}: {err}");
        }
    }
}

fn next_stdout_line_matching(
    child: &mut std::process::Child,
    lines: &mpsc::Receiver<String>,
    label: &str,
    predicate: impl Fn(&str) -> bool,
) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            let _ = child.kill();
            panic!("timed out waiting for {label}");
        }
        match lines.recv_timeout(deadline.saturating_duration_since(now)) {
            Ok(line) if predicate(&line) => return line,
            Ok(_) => {}
            Err(err) => {
                let _ = child.kill();
                panic!("timed out waiting for {label}: {err}");
            }
        }
    }
}

#[test]
fn headless_emits_snapshot_handles_errors_and_drives_stage_transition() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let codexize_root = dir.path().join(".codexize");
    std::fs::create_dir_all(&codexize_root).expect("create codexize_root");

    let mut child = Command::new(env!("CARGO_BIN_EXE_codexize"))
        .arg("headless")
        .env("CODEXIZE_ROOT", &codexize_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start codexize headless");

    let stdout = child.stdout.take().expect("stdout");
    let (line_tx, line_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    if line_tx.send(line).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });

    let line = next_stdout_line(&mut child, &line_rx, "snapshot line");

    assert!(
        line.contains(r#""Snapshot""#),
        "first line should contain Snapshot payload: {line}"
    );
    assert!(
        line.contains(r#""seq""#),
        "Snapshot should contain seq field: {line}"
    );

    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, r#"{{"not a valid command": 42}}"#).expect("write malformed command");
    stdin.flush().expect("flush malformed command");
    let error_line = next_stdout_line(&mut child, &line_rx, "parse-error event");
    assert!(
        error_line.contains(r#""Error""#),
        "malformed stdin should produce an Error payload: {error_line}"
    );

    writeln!(
        stdin,
        r#"{{"Session":["headless-smoke",{{"Stage":"Start"}}]}}"#
    )
    .expect("write stage command");
    stdin.flush().expect("flush stdin");
    let stage_line =
        next_stdout_line_matching(&mut child, &line_rx, "stage transition event", |line| {
            line.contains(r#""SessionChanged""#)
                && line.contains(r#""Stage""#)
                && line.contains(r#""BrainstormRunning""#)
        });
    assert!(stage_line.contains("headless-smoke"));

    // Keep stdin open until after SIGINT to avoid a race between EOF
    // (triggers non-zero "stdin closed" exit) and SIGINT (clean exit 0).
    // The headless frontend loops until shutdown, so the pipe must stay
    // open or the EOF path may win under load.
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("failed to send SIGINT");

    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child should exit with 0 on SIGINT");
}
