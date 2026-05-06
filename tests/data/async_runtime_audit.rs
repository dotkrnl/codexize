use std::{fs, path::Path};

const BANNED_PRODUCT_PATTERNS: &[&str] = &[
    "std::sync::mpsc",
    "use std::sync::mpsc",
    "std::thread::spawn",
    "thread::spawn",
    "std::thread::sleep",
    "thread::sleep",
    "OnceLock<Mutex<HashMap",
    "AtomicBool",
    "reqwest::blocking",
];

#[test]
fn product_code_does_not_reintroduce_sync_runtime_primitives() {
    let mut hits = Vec::new();
    visit_rs_files(Path::new("src"), &mut |path| {
        if is_test_path(path) {
            return;
        }
        let source = fs::read_to_string(path).unwrap();
        for (line_idx, line) in source.lines().enumerate() {
            for pattern in BANNED_PRODUCT_PATTERNS {
                if line.contains(pattern) {
                    hits.push(format!(
                        "{}:{} contains `{}`",
                        path.display(),
                        line_idx + 1,
                        pattern
                    ));
                }
            }
        }
    });

    assert!(
        hits.is_empty(),
        "sync runtime primitives remain in product code:\n{}",
        hits.join("\n")
    );
}

fn visit_rs_files(path: &Path, f: &mut impl FnMut(&Path)) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            visit_rs_files(&entry.unwrap().path(), f);
        }
        return;
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
        f(path);
    }
}

fn is_test_path(path: &Path) -> bool {
    if path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy();
        text == "tests"
    }) {
        return true;
    }
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name == "tests_mod.rs"
        || file_name.ends_with("_tests.rs")
        || (file_name.starts_with("chunk_") && file_name.ends_with("tests.rs"))
}

#[test]
fn test_path_filter_only_excludes_known_test_shapes() {
    assert!(is_test_path(Path::new("src/app/tests_mod.rs")));
    assert!(is_test_path(Path::new(
        "src/app_runtime/tests/lifecycle/mod.rs"
    )));
    assert!(is_test_path(Path::new(
        "src/app_runtime/tests/lifecycle/chunk_00_tests.rs"
    )));
    assert!(is_test_path(Path::new("src/ui/footer/keymap_tests.rs")));
    assert!(!is_test_path(Path::new("src/testsupport/runtime.rs")));
    assert!(!is_test_path(Path::new("src/app/tests_helpers.rs")));
}
