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
    contents
        .split_once("\n#[cfg(test)]")
        .map(|(prefix, _)| prefix)
        .unwrap_or(contents)
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
