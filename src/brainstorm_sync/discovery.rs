//! Vendor eligibility narrowing and brainstorming-package target discovery.
//!
//! Eligibility: a vendor is eligible when codexize can currently launch it
//! through the ACP configuration *or* metadata records a previously managed
//! install for it. This keeps fallback packages stable across restarts even
//! if a CLI temporarily disappears.
//!
//! Target discovery: each eligible vendor is resolved to either a native
//! plugin path or a codexize-managed fallback under
//! `~/.codexize/skills/vendor/<vendor>/brainstorming/`. Native discovery is
//! conservative and marker-driven; the spec only commits to Codex
//! `~/.codex/superpowers/skills/brainstorming/` and `~/.codex/skills/brainstorming/`
//! as native candidates, gated on a recognizable `SKILL.md` marker.

use super::metadata::{BrainstormMetadata, InstallMode, vendor_key};
use crate::selection::VendorKind;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Resolved install target for a vendor's brainstorming package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorTarget {
    pub vendor: VendorKind,
    pub mode: InstallMode,
    /// Directory that does (or will) contain the brainstorming package.
    /// For native installs this is the existing plugin directory; for
    /// fallback installs this is the codexize-managed location.
    pub path: PathBuf,
}

/// Compute eligible vendors from the current ACP-launchable set plus any
/// vendor with a recorded brainstorming install. Order is deterministic
/// (BTreeSet) so plan ordering is stable across runs.
pub fn eligible_vendors(
    acp_available: &BTreeSet<VendorKind>,
    metadata: &BrainstormMetadata,
) -> BTreeSet<VendorKind> {
    let mut out: BTreeSet<VendorKind> = acp_available.iter().copied().collect();
    out.extend(metadata.recorded_vendors());
    out
}

/// Codexize-managed fallback path for `vendor` under `codexize_home`
/// (typically `$HOME/.codexize`). Stable so prompt generation and metadata
/// can refer to it without re-discovering.
pub fn fallback_path(codexize_home: &Path, vendor: VendorKind) -> PathBuf {
    codexize_home
        .join("skills")
        .join("vendor")
        .join(vendor_key(vendor))
        .join("brainstorming")
}

/// Discover the install target for `vendor`.
///
/// Native candidates are only accepted when a marker proves the directory
/// is the correct brainstorming package: a readable `SKILL.md` whose
/// content references brainstorming. The spec requires high confidence; a
/// guessed home-directory path is not enough on its own. If discovery
/// fails, the vendor falls back to the codexize-managed path.
pub fn discover_target(home: &Path, codexize_home: &Path, vendor: VendorKind) -> VendorTarget {
    if let Some(native) = discover_native(home, vendor) {
        return VendorTarget {
            vendor,
            mode: InstallMode::Native,
            path: native,
        };
    }
    VendorTarget {
        vendor,
        mode: InstallMode::Fallback,
        path: fallback_path(codexize_home, vendor),
    }
}

/// Discover all eligible vendors at once, preserving the eligibility set's
/// order.
pub fn discover_targets(
    home: &Path,
    codexize_home: &Path,
    eligible: &BTreeSet<VendorKind>,
) -> BTreeMap<VendorKind, VendorTarget> {
    eligible
        .iter()
        .map(|v| (*v, discover_target(home, codexize_home, *v)))
        .collect()
}

fn discover_native(home: &Path, vendor: VendorKind) -> Option<PathBuf> {
    match vendor {
        VendorKind::Codex => discover_codex_native(home),
        // Spec: Claude/Gemini/Kimi native roots are fallback-only for the
        // first pass unless an existing resolver finds a concrete marker.
        // Don't guess.
        VendorKind::Claude | VendorKind::Gemini | VendorKind::Kimi => None,
    }
}

fn discover_codex_native(home: &Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".codex")
            .join("superpowers")
            .join("skills")
            .join("brainstorming"),
        home.join(".codex").join("skills").join("brainstorming"),
    ];
    candidates
        .into_iter()
        .find(|candidate| has_brainstorming_marker(candidate))
}

/// A directory qualifies as a native brainstorming package when it contains
/// a readable `SKILL.md` referencing the brainstorming workflow. Keeping the
/// marker check string-based (case-insensitive contains "brainstorm") avoids
/// brittle frontmatter parsing while still rejecting unrelated `SKILL.md`
/// files that happen to live nearby.
fn has_brainstorming_marker(dir: &Path) -> bool {
    let marker = dir.join("SKILL.md");
    let Ok(text) = std::fs::read_to_string(&marker) else {
        return false;
    };
    text.to_lowercase().contains("brainstorm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brainstorm_sync::metadata::VendorRecord;
    use tempfile::TempDir;

    fn write_marker(path: &Path, body: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn eligibility_is_acp_union_recorded() {
        let mut acp = BTreeSet::new();
        acp.insert(VendorKind::Codex);
        acp.insert(VendorKind::Claude);

        let mut metadata = BrainstormMetadata::default();
        metadata.set_vendor_record(
            VendorKind::Gemini,
            VendorRecord {
                installed_commit: "x".into(),
                path: PathBuf::from("/cached"),
                mode: InstallMode::Fallback,
            },
        );

        let eligible = eligible_vendors(&acp, &metadata);
        assert!(eligible.contains(&VendorKind::Codex));
        assert!(eligible.contains(&VendorKind::Claude));
        assert!(eligible.contains(&VendorKind::Gemini));
        assert!(!eligible.contains(&VendorKind::Kimi));
    }

    #[test]
    fn eligibility_excludes_unlaunchable_unmanaged_vendors() {
        let acp = BTreeSet::new();
        let metadata = BrainstormMetadata::default();
        let eligible = eligible_vendors(&acp, &metadata);
        assert!(eligible.is_empty());
    }

    #[test]
    fn codex_native_discovered_via_superpowers_skill_marker() {
        let home = TempDir::new().unwrap();
        let codexize_home = TempDir::new().unwrap();
        let native = home
            .path()
            .join(".codex")
            .join("superpowers")
            .join("skills")
            .join("brainstorming");
        write_marker(&native, "# Brainstorming\nworkflow body");

        let target = discover_target(home.path(), codexize_home.path(), VendorKind::Codex);
        assert_eq!(target.mode, InstallMode::Native);
        assert_eq!(target.path, native);
    }

    #[test]
    fn codex_native_discovered_via_legacy_skills_path() {
        let home = TempDir::new().unwrap();
        let codexize_home = TempDir::new().unwrap();
        let legacy = home
            .path()
            .join(".codex")
            .join("skills")
            .join("brainstorming");
        write_marker(&legacy, "# Brainstorming legacy");

        let target = discover_target(home.path(), codexize_home.path(), VendorKind::Codex);
        assert_eq!(target.mode, InstallMode::Native);
        assert_eq!(target.path, legacy);
    }

    #[test]
    fn codex_native_rejects_skill_without_brainstorm_marker() {
        let home = TempDir::new().unwrap();
        let codexize_home = TempDir::new().unwrap();
        let candidate = home
            .path()
            .join(".codex")
            .join("superpowers")
            .join("skills")
            .join("brainstorming");
        // SKILL.md exists but does not look like the brainstorming skill —
        // a guessed directory should not promote to native.
        write_marker(&candidate, "# debugging\nunrelated content");

        let target = discover_target(home.path(), codexize_home.path(), VendorKind::Codex);
        assert_eq!(target.mode, InstallMode::Fallback);
        assert_eq!(
            target.path,
            fallback_path(codexize_home.path(), VendorKind::Codex)
        );
    }

    #[test]
    fn codex_falls_back_when_no_marker_present() {
        let home = TempDir::new().unwrap();
        let codexize_home = TempDir::new().unwrap();
        let target = discover_target(home.path(), codexize_home.path(), VendorKind::Codex);
        assert_eq!(target.mode, InstallMode::Fallback);
        assert_eq!(
            target.path,
            codexize_home
                .path()
                .join("skills")
                .join("vendor")
                .join("codex")
                .join("brainstorming")
        );
    }

    #[test]
    fn other_vendors_always_use_fallback_in_first_pass() {
        let home = TempDir::new().unwrap();
        let codexize_home = TempDir::new().unwrap();

        for vendor in [VendorKind::Claude, VendorKind::Gemini, VendorKind::Kimi] {
            // Even with a plausibly populated home dir, claude/gemini/kimi
            // must not promote to native without a concrete resolver.
            let plausible = home
                .path()
                .join(format!(".{}", vendor_key(vendor)))
                .join("skills")
                .join("brainstorming");
            write_marker(&plausible, "# Brainstorming");

            let target = discover_target(home.path(), codexize_home.path(), vendor);
            assert_eq!(target.mode, InstallMode::Fallback, "{vendor:?}");
            assert_eq!(target.path, fallback_path(codexize_home.path(), vendor));
        }
    }

    #[test]
    fn discover_targets_covers_full_eligibility_set() {
        let home = TempDir::new().unwrap();
        let codexize_home = TempDir::new().unwrap();
        let mut eligible = BTreeSet::new();
        eligible.insert(VendorKind::Codex);
        eligible.insert(VendorKind::Claude);

        let targets = discover_targets(home.path(), codexize_home.path(), &eligible);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[&VendorKind::Codex].mode, InstallMode::Fallback);
        assert_eq!(targets[&VendorKind::Claude].mode, InstallMode::Fallback);
    }

    #[test]
    fn fallback_path_is_deterministic_per_vendor() {
        let home = Path::new("/h");
        assert_eq!(
            fallback_path(home, VendorKind::Codex),
            PathBuf::from("/h/skills/vendor/codex/brainstorming")
        );
        assert_eq!(
            fallback_path(home, VendorKind::Claude),
            PathBuf::from("/h/skills/vendor/claude/brainstorming")
        );
        assert_eq!(
            fallback_path(home, VendorKind::Gemini),
            PathBuf::from("/h/skills/vendor/gemini/brainstorming")
        );
        assert_eq!(
            fallback_path(home, VendorKind::Kimi),
            PathBuf::from("/h/skills/vendor/kimi/brainstorming")
        );
    }
}
