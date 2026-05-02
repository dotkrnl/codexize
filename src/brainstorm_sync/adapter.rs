//! Vendor adapter renderer for the brainstorming package.
//!
//! Each ACP vendor needs the same upstream `skills/brainstorming/` workflow,
//! but wrapped with a small adapter preamble that names the vendor and tells
//! the agent to defer to codexize's launch-time brainstorm prompt for paths,
//! the no-VCS rule, live summaries, and `/exit` semantics. The spec
//! deliberately keeps per-vendor content thin: one shared renderer + a
//! per-vendor wording map, not hand-authored package bodies.
//!
//! The renderer copies the upstream package tree into the target directory
//! and rewrites `SKILL.md` to include the vendor preamble after any upstream
//! YAML frontmatter. Other files (e.g. `references/`) are copied as-is so
//! the upstream workflow keeps working without per-vendor patching.

use super::metadata::vendor_key;
use crate::selection::VendorKind;
use anyhow::{Context, Result, anyhow};
use std::path::Path;

/// Required filename inside any rendered brainstorming package. The installer
/// uses this to validate staged output before swapping it into place.
pub const SKILL_FILE: &str = "SKILL.md";

/// Human-readable vendor name used in preamble copy.
fn vendor_display_name(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Codex => "Codex",
        VendorKind::Claude => "Claude",
        VendorKind::Gemini => "Gemini",
        VendorKind::Kimi => "Kimi",
    }
}

/// One-line invocation hint per vendor. Kept short so it doesn't compete with
/// codexize's launch-time prompt — that prompt remains the source of truth
/// for session-specific instructions.
fn vendor_invocation_hint(vendor: VendorKind) -> &'static str {
    match vendor {
        VendorKind::Codex => {
            "When the operator launches a brainstorm session, run this skill end to end."
        }
        VendorKind::Claude => {
            "When the operator launches a brainstorm session, follow this skill via the Skill tool."
        }
        VendorKind::Gemini => {
            "When the operator launches a brainstorm session, activate this skill and follow it through."
        }
        VendorKind::Kimi => {
            "When the operator launches a brainstorm session, follow this skill end to end."
        }
    }
}

/// Build the adapter preamble for `vendor`. Always ends with a blank line so
/// inserting it into the upstream body produces well-formed Markdown.
pub fn vendor_preamble(vendor: VendorKind) -> String {
    let name = vendor_display_name(vendor);
    let key = vendor_key(vendor);
    let hint = vendor_invocation_hint(vendor);
    format!(
        "<!-- codexize: brainstorming adapter for {key} -->\n\
         # Brainstorming ({name} adapter)\n\
         \n\
         This package wraps the upstream Superpowers `skills/brainstorming` \
         workflow for {name}. {hint}\n\
         \n\
         Defer to codexize's generated brainstorm prompt for session-specific \
         paths, the no-VCS rule, live summary updates, and `/exit` completion \
         wording. Do not invent those values from this file.\n\
         \n\
         ---\n\
         \n"
    )
}

/// Render the brainstorming package for `vendor` into `out_dir`.
///
/// `upstream_pkg_dir` must be the upstream `skills/brainstorming/` directory
/// (not the repo root). The renderer:
///
/// * Copies every regular file/subdirectory from the upstream package into
///   `out_dir` (creating parents as needed). Existing contents under
///   `out_dir` are left untouched — callers stage into a fresh temp dir.
/// * Reads `SKILL.md` from the upstream package and writes
///   `<out_dir>/SKILL.md` with the vendor preamble inserted after upstream
///   YAML frontmatter when present, preserving loader-required first bytes.
///
/// Errors when the upstream `SKILL.md` is missing — the installer treats
/// that as a hard abort because the spec requires upstream
/// `skills/brainstorming/` to be present before any vendor is replaced.
pub fn render_package(upstream_pkg_dir: &Path, vendor: VendorKind, out_dir: &Path) -> Result<()> {
    if !upstream_pkg_dir.is_dir() {
        return Err(anyhow!(
            "upstream brainstorming directory does not exist: {}",
            upstream_pkg_dir.display()
        ));
    }
    let upstream_skill = upstream_pkg_dir.join(SKILL_FILE);
    let upstream_body = std::fs::read_to_string(&upstream_skill).with_context(|| {
        format!(
            "failed to read upstream {} at {}",
            SKILL_FILE,
            upstream_skill.display()
        )
    })?;

    std::fs::create_dir_all(out_dir).with_context(|| {
        format!(
            "failed to create adapter output directory {}",
            out_dir.display()
        )
    })?;

    copy_tree(upstream_pkg_dir, out_dir).with_context(|| {
        format!(
            "failed to copy upstream package from {} to {}",
            upstream_pkg_dir.display(),
            out_dir.display()
        )
    })?;

    let wrapped = wrap_skill_body(&upstream_body, vendor);
    let skill_out = out_dir.join(SKILL_FILE);
    std::fs::write(&skill_out, wrapped)
        .with_context(|| format!("failed to write adapter {}", skill_out.display()))?;

    Ok(())
}

fn wrap_skill_body(upstream_body: &str, vendor: VendorKind) -> String {
    let preamble = vendor_preamble(vendor);
    if let Some(frontmatter_end) = frontmatter_end(upstream_body) {
        let (frontmatter, body) = upstream_body.split_at(frontmatter_end);
        format!("{frontmatter}\n{preamble}{body}")
    } else {
        format!("{preamble}{upstream_body}")
    }
}

fn frontmatter_end(body: &str) -> Option<usize> {
    if !body.starts_with("---\n") {
        return None;
    }
    let rest = &body[4..];
    rest.find("\n---\n")
        .map(|closing_start| 4 + closing_start + "\n---\n".len())
}

/// Recursive copy that follows the upstream tree shape. Symlinks are
/// resolved as their target file/directory (the upstream repo never relies on
/// link semantics for skill content), so the staged package never contains
/// dangling links once the upstream cache is cleaned up.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&to)?;
            copy_tree(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        } else if file_type.is_symlink() {
            let resolved = std::fs::metadata(&from)?;
            if resolved.is_dir() {
                std::fs::create_dir_all(&to)?;
                copy_tree(&from, &to)?;
            } else {
                std::fs::copy(&from, &to)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_upstream(text: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let pkg = dir.path();
        std::fs::write(pkg.join(SKILL_FILE), text).unwrap();
        let refs = pkg.join("references");
        std::fs::create_dir_all(&refs).unwrap();
        std::fs::write(refs.join("notes.md"), "support file body\n").unwrap();
        let nested = refs.join("deeper");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("inner.txt"), "deep\n").unwrap();
        dir
    }

    #[test]
    fn preamble_identifies_vendor_and_defers_to_codexize_prompt() {
        let body = vendor_preamble(VendorKind::Claude);
        assert!(body.contains("Claude adapter"), "{body}");
        assert!(
            body.contains("codexize's generated brainstorm prompt"),
            "{body}"
        );
        assert!(body.contains("/exit"), "{body}");
    }

    #[test]
    fn preamble_distinguishes_each_vendor() {
        let mut seen = std::collections::BTreeSet::new();
        for vendor in [
            VendorKind::Codex,
            VendorKind::Claude,
            VendorKind::Gemini,
            VendorKind::Kimi,
        ] {
            seen.insert(vendor_preamble(vendor));
        }
        assert_eq!(seen.len(), 4, "preambles should be unique per vendor");
    }

    #[test]
    fn render_package_writes_skill_md_with_preamble_and_upstream_body() {
        let upstream = make_upstream("# Brainstorming\n\nupstream body line\n");
        let out_root = TempDir::new().unwrap();
        let out = out_root.path().join("staged");
        render_package(upstream.path(), VendorKind::Codex, &out).unwrap();
        let skill = std::fs::read_to_string(out.join(SKILL_FILE)).unwrap();
        assert!(skill.starts_with("<!-- codexize: brainstorming adapter for codex -->"));
        assert!(skill.contains("upstream body line"));
        let preamble_marker = "---\n\n";
        let split = skill
            .find(preamble_marker)
            .expect("preamble separator missing");
        // Preamble appears before the upstream body.
        assert!(split < skill.find("upstream body line").unwrap());
    }

    #[test]
    fn render_package_preserves_frontmatter_as_first_bytes() {
        let upstream = make_upstream(
            "---\nname: brainstorming\ndescription: upstream skill\n---\n\n# Brainstorming\n",
        );
        let out_root = TempDir::new().unwrap();
        let out = out_root.path().join("staged");
        render_package(upstream.path(), VendorKind::Codex, &out).unwrap();
        let skill = std::fs::read_to_string(out.join(SKILL_FILE)).unwrap();

        assert!(
            skill.starts_with("---\nname: brainstorming\ndescription: upstream skill\n---\n\n"),
            "{skill}"
        );
        assert!(skill.contains("<!-- codexize: brainstorming adapter for codex -->"));
        assert!(
            skill.find("---\n\n").unwrap()
                < skill
                    .find("<!-- codexize: brainstorming adapter for codex -->")
                    .unwrap()
        );
    }

    #[test]
    fn render_package_copies_supporting_files() {
        let upstream = make_upstream("# Brainstorming\n");
        let out_root = TempDir::new().unwrap();
        let out = out_root.path().join("staged");
        render_package(upstream.path(), VendorKind::Gemini, &out).unwrap();
        assert!(out.join("references/notes.md").is_file());
        assert!(out.join("references/deeper/inner.txt").is_file());
        assert_eq!(
            std::fs::read_to_string(out.join("references/notes.md")).unwrap(),
            "support file body\n"
        );
    }

    #[test]
    fn render_package_errors_when_upstream_missing() {
        let out_root = TempDir::new().unwrap();
        let out = out_root.path().join("staged");
        let bogus = out_root.path().join("does-not-exist");
        let err = render_package(&bogus, VendorKind::Codex, &out).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn render_package_errors_when_skill_md_missing_in_upstream() {
        let dir = TempDir::new().unwrap();
        // Create dir but no SKILL.md.
        let out_root = TempDir::new().unwrap();
        let out = out_root.path().join("staged");
        let err = render_package(dir.path(), VendorKind::Kimi, &out).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(SKILL_FILE), "{msg}");
    }

    #[test]
    fn render_package_does_not_inject_vendor_into_other_files() {
        let upstream = make_upstream("# Brainstorming\nbody\n");
        let out_root = TempDir::new().unwrap();
        let out = out_root.path().join("staged");
        render_package(upstream.path(), VendorKind::Claude, &out).unwrap();
        // The vendor preamble belongs only in SKILL.md.
        let aux = std::fs::read_to_string(out.join("references/notes.md")).unwrap();
        assert!(!aux.contains("Claude adapter"));
    }
}
