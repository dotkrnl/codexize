use std::{fs, path::Path};

fn production_source_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            production_source_files(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tests_"))
        {
            files.push(path);
        }
    }
}

fn production_prefix(contents: &str) -> &str {
    let mut offset = 0;
    let mut cfg_test_start = None;
    for line in contents.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "#[cfg(test)]" {
            cfg_test_start = Some(offset);
        } else if let Some(start) = cfg_test_start {
            if trimmed.starts_with("#[") {
                // Test modules may carry lint attributes between #[cfg(test)] and mod tests.
            } else if trimmed.starts_with("mod ") {
                let split_at = if start > 0 && contents.as_bytes()[start - 1] == b'\n' {
                    start - 1
                } else {
                    start
                };
                return &contents[..split_at];
            } else {
                cfg_test_start = None;
            }
        }
        offset += line.len();
    }
    contents
}

#[test]
fn production_prefix_keeps_cfg_test_imports() {
    let contents = "use crate::state::SessionState;\n#[cfg(test)]\nuse crate::app::state::ModelRefreshState;\nfn production() {}\n#[cfg(test)]\nmod tests {}";

    assert!(production_prefix(contents).contains("fn production() {}"));
}

#[test]
fn production_prefix_stops_before_cfg_test_module() {
    let contents = "fn production() {}\n#[cfg(test)]\nmod tests {\n    fn fixture() {}\n}";

    assert_eq!(production_prefix(contents), "fn production() {}");
}

#[test]
fn production_prefix_stops_before_cfg_test_module_with_attributes() {
    let contents =
        "fn production() {}\n#[cfg(test)]\n#[allow(clippy::items_after_test_module)]\nmod tests {}";

    assert_eq!(production_prefix(contents), "fn production() {}");
}

#[test]
fn production_source_has_no_bare_unwrap_calls() {
    let mut files = Vec::new();
    production_source_files(Path::new("src"), &mut files);

    let mut violations = Vec::new();
    for path in files {
        let contents = fs::read_to_string(&path).expect("read source file");
        for (line_idx, line) in production_prefix(&contents).lines().enumerate() {
            if line.contains(".unwrap()") {
                violations.push(format!("{}:{}", path.display(), line_idx + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "bare unwrap() in production source:\n{}",
        violations.join("\n")
    );
}
