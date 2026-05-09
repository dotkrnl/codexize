//! Free-models sub-panel widget.
//!
//! Operators declare additional, externally-funded providers for already
//! ipbr-supported models in a `[[free_models]]` config section. The sub-panel
//! offers three controls:
//!
//! - **Mapped model** — a multi-check selection box over ipbr canonical names.
//!   Checking N rows produces N saved entries on commit, one per row, sharing
//!   the single CLI/model_name pair.
//! - **CLI** — single-choice selection over `CliKind` variants.
//! - **Model name** — trimmed, non-empty text input passed verbatim to the CLI.
//!
//! Existing entries render in a list above the editor with a soft-warning
//! suffix `(no matching ipbr row)` for entries whose `mapped_into` does not
//! match any row in the loaded universe; the entry stays editable.

use crate::data::config::Config;
use crate::selection::{CliKind, FreeModelEntry};

/// Editor state for adding new `[[free_models]]` entries via the TUI. The
/// state is intentionally trivial — no pending IO, no diff tracking — so the
/// commit path is just `commit(&mut Config) -> usize` returning the number of
/// new entries appended. The interactive key wiring lives in `mod.rs`; the
/// editor type is exercised end-to-end through tests in this module so the
/// commit semantics stay covered even before keys are bound.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct FreeModelsEditor {
    /// All ipbr canonical names available to map into. The set is captured
    /// once when the editor opens; the operator's terminal need not refresh
    /// when the universe assembles new rows mid-edit.
    pub(crate) available_models: Vec<String>,
    /// Parallel-with-`available_models`: `true` when that model row is
    /// checked. On commit, every checked row produces a `[[free_models]]`
    /// entry sharing `cli` and `model_name`.
    pub(crate) checked: Vec<bool>,
    pub(crate) cli: CliKind,
    pub(crate) model_name: String,
}

#[allow(dead_code)]
impl FreeModelsEditor {
    pub(crate) fn new(available_models: Vec<String>) -> Self {
        let checked = vec![false; available_models.len()];
        Self {
            available_models,
            checked,
            cli: CliKind::Opencode,
            model_name: String::new(),
        }
    }

    /// Toggle the checked state of the row at `index`. Out-of-bounds is a
    /// no-op so the editor never panics on a stale cursor.
    pub(crate) fn toggle(&mut self, index: usize) {
        if let Some(slot) = self.checked.get_mut(index) {
            *slot = !*slot;
        }
    }

    /// Append one `FreeModelEntry` per checked row, sharing the editor's
    /// `cli` and `model_name`. Returns the count appended. Trimmed empty
    /// `model_name` or zero checked rows produces no entries (validation
    /// catches both at the panel level).
    pub(crate) fn commit(&self, config: &mut Config) -> usize {
        let trimmed = self.model_name.trim();
        if trimmed.is_empty() {
            return 0;
        }
        let to_add: Vec<FreeModelEntry> = self
            .available_models
            .iter()
            .zip(self.checked.iter())
            .filter(|(_, checked)| **checked)
            .map(|(name, _)| FreeModelEntry {
                mapped_into: name.clone(),
                cli: self.cli,
                model_name: trimmed.to_string(),
            })
            .collect();
        let count = to_add.len();
        if count == 0 {
            return 0;
        }
        let mut existing = config.free_models.value().clone();
        existing.extend(to_add);
        config.free_models = crate::data::config::schema::Override::explicit(existing);
        count
    }
}

/// One rendered row in the existing-entries list. The trailing warning is
/// `Some(...)` exactly when `mapped_into` does not match any row in the
/// supplied `known_models` set; callers render the warning suffix in dim
/// styling so the soft-warning channel from `assemble_universe` reaches the
/// operator without blocking saves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryRow {
    pub(crate) mapped_into: String,
    pub(crate) cli: CliKind,
    pub(crate) model_name: String,
    pub(crate) warning: Option<&'static str>,
}

pub(crate) const UNMATCHED_SUFFIX: &str = "(no matching ipbr row)";

pub(crate) fn entry_rows(config: &Config, known_models: &[String]) -> Vec<EntryRow> {
    config
        .free_models
        .value()
        .iter()
        .map(|entry| {
            let warning = if known_models.iter().any(|name| name == &entry.mapped_into) {
                None
            } else {
                Some(UNMATCHED_SUFFIX)
            };
            EntryRow {
                mapped_into: entry.mapped_into.clone(),
                cli: entry.cli,
                model_name: entry.model_name.clone(),
                warning,
            }
        })
        .collect()
}

/// Single-line text representation of an entry row used both by the list
/// renderer and by tests that scan for `(no matching ipbr row)`.
pub(crate) fn format_entry_line(row: &EntryRow) -> String {
    let mut text = format!(
        "{} · [{}] · {}",
        row.mapped_into,
        row.cli.as_str(),
        row.model_name
    );
    if let Some(suffix) = row.warning {
        text.push(' ');
        text.push_str(suffix);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::config::Config;

    fn empty_config() -> Config {
        Config::baked_defaults()
    }

    #[test]
    fn multi_check_commit_writes_one_entry_per_checked_row() {
        let mut editor =
            FreeModelsEditor::new(vec!["model-a".into(), "model-b".into(), "model-c".into()]);
        editor.toggle(0);
        editor.toggle(2);
        editor.cli = CliKind::Opencode;
        editor.model_name = "shared-name".into();

        let mut config = empty_config();
        let added = editor.commit(&mut config);

        assert_eq!(added, 2);
        let saved = config.free_models.value();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].mapped_into, "model-a");
        assert_eq!(saved[0].model_name, "shared-name");
        assert_eq!(saved[0].cli, CliKind::Opencode);
        assert_eq!(saved[1].mapped_into, "model-c");
        assert_eq!(saved[1].model_name, "shared-name");
    }

    #[test]
    fn commit_with_no_checked_rows_produces_no_entries() {
        let mut editor = FreeModelsEditor::new(vec!["model-a".into()]);
        editor.model_name = "ignored".into();

        let mut config = empty_config();
        assert_eq!(editor.commit(&mut config), 0);
        assert!(config.free_models.value().is_empty());
    }

    #[test]
    fn commit_with_empty_model_name_is_rejected() {
        let mut editor = FreeModelsEditor::new(vec!["model-a".into()]);
        editor.toggle(0);
        editor.model_name = "   ".into();

        let mut config = empty_config();
        assert_eq!(editor.commit(&mut config), 0);
        assert!(config.free_models.value().is_empty());
    }

    #[test]
    fn unmatched_mapped_into_renders_no_matching_ipbr_row_suffix() {
        let mut editor = FreeModelsEditor::new(vec!["claude-opus-4-7".into()]);
        editor.toggle(0);
        editor.cli = CliKind::Claude;
        editor.model_name = "my-opus".into();
        let mut config = empty_config();
        editor.commit(&mut config);

        // Add a stray entry whose mapped_into does NOT exist in the universe.
        let mut entries = config.free_models.value().clone();
        entries.push(FreeModelEntry {
            mapped_into: "ghost-model".into(),
            cli: CliKind::Codex,
            model_name: "missing".into(),
        });
        config.free_models = crate::data::config::schema::Override::explicit(entries);

        let rows = entry_rows(&config, &["claude-opus-4-7".to_string()]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].warning, None);
        assert_eq!(rows[1].warning, Some(UNMATCHED_SUFFIX));

        let line = format_entry_line(&rows[1]);
        assert!(
            line.contains(UNMATCHED_SUFFIX),
            "unmatched entry line must carry the soft warning: {line}"
        );
        assert!(line.starts_with("ghost-model · [codex] · missing"));
    }
}
