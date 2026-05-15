use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn headless_emits_snapshot_and_handles_commands_and_sigint() {
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
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    reader.read_line(&mut line).expect("read snapshot line");

    assert!(
        line.contains(r#""Snapshot""#),
        "first line should contain Snapshot payload: {line}"
    );
    assert!(
        line.contains(r#""seq""#),
        "Snapshot should contain seq field: {line}"
    );

    // Send one valid AppCommand line
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, r#"{{"Global":"Quit"}}"#).expect("write valid command");
    stdin.flush().expect("flush stdin");
    // Keep stdin open until after SIGINT to avoid a race between EOF
    // (triggers non-zero "stdin closed" exit) and SIGINT (clean exit 0).
    // The headless frontend loops until shutdown, so the pipe must stay
    // open or the EOF path may win under load.

    // We cannot wait for a granular delta here, because the runtime update loop
    // isn't implemented in this task (run_frontend just blocks on frontend.run).
    // So we just send SIGINT.
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("failed to send SIGINT");

    let status = child.wait().expect("wait for child");
    assert!(status.success(), "child should exit with 0 on SIGINT");
}
