use std::{fs, path::Path};

#[test]
fn adapter_and_provider_child_spawns_stay_in_runner() {
    for dir in ["src/adapters", "src/providers"] {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }

            let source = fs::read_to_string(&path).unwrap();
            assert!(
                !source.contains(".spawn("),
                "{} directly spawns a child process; route launch parameters through src/runner.rs",
                display_path(&path)
            );
        }
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
