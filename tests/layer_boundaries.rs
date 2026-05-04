use std::process::Command;

#[test]
fn layer_boundary_command_accepts_current_layout() {
    let output = Command::new("scripts/check-layers.sh")
        .output()
        .expect("run layer-boundary check");

    assert!(
        output.status.success(),
        "layer-boundary check failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn layer_boundary_command_rejects_banned_layer_references() {
    let root = tempfile::tempdir().expect("create temp repo");
    let data_dir = root.path().join("src/data");
    std::fs::create_dir_all(&data_dir).expect("create data layer");
    std::fs::write(data_dir.join("bad.rs"), "use ratatui::Frame;\n").expect("write fixture");

    let output = Command::new("scripts/check-layers.sh")
        .arg(root.path())
        .output()
        .expect("run layer-boundary check");

    assert!(
        !output.status.success(),
        "layer-boundary check unexpectedly accepted banned data reference"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("ratatui"),
        "stderr should name the banned token, got:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
