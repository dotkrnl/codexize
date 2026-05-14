use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_TERMS: [&str; 2] = [concat!("ph", "ase"), concat!("leg", "acy")];
const ALLOWED_EXTERNAL_LITERAL: &str = "CLAUDE_CODE_DISABLE_LEGACY_MODEL_REMAP";

#[test]
fn repository_uses_stage_terminology_only() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut findings = Vec::new();

    collect_findings(&root, &root, &mut findings).expect("scan repository");

    assert!(
        findings.is_empty(),
        "persisted terminology remains:\n{}",
        findings.join("\n")
    );
}

fn collect_findings(root: &Path, path: &Path, findings: &mut Vec<String>) -> std::io::Result<()> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if should_skip(rel) {
        return Ok(());
    }

    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            collect_findings(root, &entry.path(), findings)?;
        }
        return Ok(());
    }

    let Some(file_name) = rel.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let rel_display = rel.display().to_string();
    for term in FORBIDDEN_TERMS {
        if rel_display.to_ascii_lowercase().contains(term) {
            findings.push(format!("{rel_display}: path contains {term}"));
        }
    }

    if !is_scanned_file(file_name) {
        return Ok(());
    }

    let content = fs::read_to_string(path)?;
    for (idx, line) in content.lines().enumerate() {
        if line.contains(ALLOWED_EXTERNAL_LITERAL) {
            continue;
        }
        let line_lower = line.to_ascii_lowercase();
        for term in FORBIDDEN_TERMS {
            if line_lower.contains(term) {
                findings.push(format!("{rel_display}:{}: {line}", idx + 1));
            }
        }
    }

    Ok(())
}

fn should_skip(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        if name.starts_with('.') {
            return true;
        }
        matches!(
            name.as_ref(),
            ".git" | "target" | "Cargo.lock" | "tests/terminology.rs"
        )
    })
}

fn is_scanned_file(file_name: &str) -> bool {
    matches!(
        Path::new(file_name)
            .extension()
            .and_then(|ext| ext.to_str()),
        Some("rs" | "md" | "toml" | "snap")
    )
}
