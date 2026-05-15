use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn headless_emits_snapshot_on_startup() {
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
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    let first_line = lines
        .next()
        .expect("expected snapshot line from headless")
        .expect("failed to read stdout");

    assert!(
        first_line.contains(r#""Snapshot""#),
        "first line should contain Snapshot payload: {first_line}"
    );
    assert!(
        first_line.contains(r#""seq""#),
        "Snapshot should contain seq field: {first_line}"
    );

    let _ = child.kill();
    let _ = child.wait();
}
