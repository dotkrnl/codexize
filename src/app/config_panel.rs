use crate::app::keys::{UiKey, UiKeyCode};
use crate::app_runtime::commands::{ConfigPanelCommand, CursorMove, InputCommand};
use crate::app_runtime::views::config_panel::{
    ConfigFieldView, ConfigPanelView, ConfigSectionView,
};
use crate::data::config::{
    Config, Override,
    loader::load_from_path,
    mutate,
    schema::{LogLevel, NtfyDetailMode, ShellPolicy},
};
use crate::data::notifications;
use anyhow::{Result, anyhow};
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

pub(crate) mod providers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelOutcome {
    KeepOpen,
    Close,
    Saved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SectionLookup {
    Exact(&'static str),
    UniquePrefix(&'static str),
    Ambiguous(Vec<&'static str>),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Bool,
    Enum(&'static [&'static str]),
    Integer { min: u64 },
    String,
    List,
    Map,
    ReadOnly,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FieldMeta {
    pub(crate) section: &'static str,
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: FieldKind,
    pub(crate) description: &'static str,
    pub(crate) secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchState {
    pub(crate) query: String,
    pub(crate) results: Vec<usize>,
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Editing {
    Integer,
    String,
    Choice {
        key: &'static str,
        options: Vec<String>,
        selected: usize,
    },
    AddProvider(providers::ProvidersEditor),
    ProviderDetail {
        cursor: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToggleField {
    Enabled,
    Official,
    Free,
    QuotaDisabled,
    Cheap,
    Tough,
    Effort,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderToggle {
    pub(crate) label: &'static str,
    pub(crate) description: &'static str,
    pub(crate) field: ToggleField,
    pub(crate) baked_locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConflictBanner {
    MtimeAdvanced,
    ExitPrompt { selected: usize },
    RegenerateTopicPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigPanelState {
    pub(crate) config: Config,
    pub(crate) path: PathBuf,
    pub(crate) opened_mtime: Option<SystemTime>,
    pub(crate) selected_section: usize,
    pub(crate) selected_field: usize,
    pub(crate) status: String,
    pub(crate) editing: Option<Editing>,
    pub(crate) edit_buffer: String,
    pub(crate) reveal_topic: bool,
    pub(crate) conflict: Option<ConflictBanner>,
    pub(crate) dirty: bool,
    pub(crate) save_error: Option<String>,
    pub(crate) searching: Option<SearchState>,
    pub(crate) providers_cursor: usize,
    // TUI-private layout state.
    pub(crate) providers_scroll: Cell<usize>,
    pub(crate) providers_body_h: Cell<usize>,
    pub(crate) folded_vendors: std::collections::BTreeSet<String>,
}

pub(crate) const FIELDS: &[FieldMeta] = &[
    FieldMeta {
        section: "notifications",
        key: "ntfy.enabled",
        label: "Notifications",
        kind: FieldKind::Bool,
        description: "Send ntfy alerts when a topic is set.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.server",
        label: "Server URL",
        kind: FieldKind::String,
        description: "Base ntfy URL. Must start with http:// or https://.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.topic",
        label: "Topic",
        kind: FieldKind::String,
        description: "Ntfy topic. Empty disables alerts; r reveals it, R regenerates it.",
        secret: true,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.detail_mode",
        label: "Detail",
        kind: FieldKind::Enum(NtfyDetailMode::variants()),
        description: "Alert body mode. detailed includes context; minimal is shorter.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.retry_attempts",
        label: "Retry attempts",
        kind: FieldKind::Integer { min: 1 },
        description: "Notification send attempts. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.retry_delay_ms",
        label: "Retry delay (ms)",
        kind: FieldKind::Integer { min: 0 },
        description: "Delay between notification retries.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.http_timeout_secs",
        label: "HTTP timeout (s)",
        kind: FieldKind::Integer { min: 1 },
        description: "Ntfy request timeout. Minimum: 1 second.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.body_max_bytes",
        label: "Body bytes",
        kind: FieldKind::Integer { min: 256 },
        description: "Maximum alert body size. Minimum: 256 bytes.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.excerpt_max_chars",
        label: "Excerpt chars",
        kind: FieldKind::Integer { min: 32 },
        description: "Maximum context excerpt length. Minimum: 32 characters.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "ntfy.created_at",
        label: "Ntfy created",
        kind: FieldKind::ReadOnly,
        description: "Read-only timestamp set when the topic is first generated.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "ntfy.updated_at",
        label: "Ntfy updated",
        kind: FieldKind::ReadOnly,
        description: "Read-only timestamp set when the topic changes.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.events.stage_wait",
        label: "Stage-wait alerts",
        kind: FieldKind::Bool,
        description: "Alert when a pipeline stage pauses for the operator.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.events.interactive_wait",
        label: "Input-wait alerts",
        kind: FieldKind::Bool,
        description: "Alert when an interactive run waits for input.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.events.pipeline_done",
        label: "Done alerts",
        kind: FieldKind::Bool,
        description: "Alert when the pipeline finishes.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.shell_policy",
        label: "Shell policy",
        kind: FieldKind::Enum(ShellPolicy::variants()),
        description: "Default ACP shell access: full-access or allowlist.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.shell_allowlist",
        label: "Shell allowlist",
        kind: FieldKind::List,
        description: "Allowed shell commands. Edit entries with the CLI.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.enforce_readonly_workspace",
        label: "Read-only workspace",
        kind: FieldKind::Bool,
        description: "Limit ACP writes to the allowed paths.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.allowed_write_paths",
        label: "Allowed write paths",
        kind: FieldKind::List,
        description: "Workspace paths ACP agents may write. Edit entries with the CLI.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.install.claude_acp_root",
        label: "Claude ACP root",
        kind: FieldKind::String,
        description: "Local Claude ACP install root. $HOME and ~/ expand at load.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.install.prefer_local_claude_acp",
        label: "Prefer local Claude",
        kind: FieldKind::Bool,
        description: "Use the local Claude ACP server before the global command.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "runner.full_review_interval",
        label: "Full review cadence",
        kind: FieldKind::Integer { min: 1 },
        description: "Run full alignment review every N review rounds. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.cache_root",
        label: "Cache root",
        kind: FieldKind::String,
        description: "Cache file root. $HOME and ~/ expand at load.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.sessions_root",
        label: "Sessions root",
        kind: FieldKind::String,
        description: "Session artifact root. $HOME and ~/ expand at load.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.runs_root",
        label: "Runs root",
        kind: FieldKind::String,
        description: "Reserved run root. No current subsystem reads it directly.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.memory_root",
        label: "Memory root",
        kind: FieldKind::String,
        description: "Project memory root. $HOME and ~/ expand at load.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "ui.prefer_split_on_open",
        label: "Open split view",
        kind: FieldKind::Bool,
        description: "Open run output in the split transcript.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "ui.colon_palette.show_help",
        label: "Palette help row",
        kind: FieldKind::Bool,
        description: "Show help text in the command palette.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "ui.footer.show_keys",
        label: "Footer key hints",
        kind: FieldKind::Bool,
        description: "Show keyboard hints in the footer.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "diagnostics.log_level",
        label: "Log level",
        kind: FieldKind::Enum(LogLevel::variants()),
        description: "Default log level; RUST_LOG takes precedence.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "diagnostics.json_logs",
        label: "JSON logs",
        kind: FieldKind::Bool,
        description: "Write logs as JSON unless the environment overrides diagnostics.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "memory.enabled",
        label: "Project memory",
        kind: FieldKind::Bool,
        description: "Include project memory in prompts.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "memory.max_topics_per_read",
        label: "Memory topic limit",
        kind: FieldKind::Integer { min: 1 },
        description: "Maximum memory topics per prompt. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "memory.journal_retention_months",
        label: "Journal retention",
        kind: FieldKind::Integer { min: 1 },
        description: "Months of memory journals to keep. Older journals prune at launch.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.agents.claude.env",
        label: "Claude env",
        kind: FieldKind::Map,
        description: "Environment variables for Claude ACP. Edit entries with the CLI.",
        secret: false,
    },
];

pub(crate) const SECTIONS: &[&str] = &["general", "models", "notifications", "agents", "system"];
pub(crate) const SECTION_ALIASES: &[(&str, &str)] = &[
    ("general", "general"),
    ("common", "general"),
    ("settings", "general"),
    ("ui", "general"),
    ("ui.footer", "general"),
    ("runner", "general"),
    ("models", "models"),
    ("model", "models"),
    ("providers", "models"),
    ("provider", "models"),
    ("notifications", "notifications"),
    ("notification", "notifications"),
    ("ntfy", "notifications"),
    ("ntfy.events", "notifications"),
    ("agents", "agents"),
    ("agent", "agents"),
    ("acp", "agents"),
    ("acp.policy", "agents"),
    ("acp.install", "agents"),
    ("acp.agents", "agents"),
    ("acp.agents.claude", "agents"),
    ("system", "system"),
    ("advanced", "system"),
    ("paths", "system"),
    ("diagnostics", "system"),
    ("memory", "system"),
    ("meta", "system"),
    ("ui.colon_palette", "system"),
];

pub(crate) fn is_providers_section(section: &str) -> bool {
    section == "models"
}

pub(crate) fn section_title(section: &str) -> &'static str {
    match section {
        "general" => "General",
        "models" => "Models",
        "notifications" => "Notifications",
        "agents" => "Agents",
        "system" => "System",
        _ => "Settings",
    }
}

pub(crate) fn resolve_section_name(name: &str) -> Option<&'static str> {
    let normalized = name.trim().to_ascii_lowercase();
    SECTION_ALIASES
        .iter()
        .find_map(|(alias, section)| (*alias == normalized).then_some(*section))
}

pub(crate) fn section_prefix_matches(prefix: &str) -> Vec<&'static str> {
    let normalized = prefix.trim().to_ascii_lowercase();
    let mut matches = Vec::new();
    for (alias, section) in SECTION_ALIASES {
        if alias.starts_with(&normalized) && !matches.contains(section) {
            matches.push(*section);
        }
    }
    matches
}

pub(crate) fn field_indices_for(section: &str) -> Vec<usize> {
    FIELDS
        .iter()
        .enumerate()
        .filter(|(_, meta)| meta.section == section)
        .map(|(idx, _)| idx)
        .collect()
}

pub(crate) fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = (current as isize + delta) % len as isize;
    if next < 0 {
        (next + len as isize) as usize
    } else {
        next as usize
    }
}

pub(crate) const PROVIDER_TOGGLES: &[ProviderToggle] = &[
    ProviderToggle {
        label: "Enabled",
        description: "Offer this provider when picking a model.",
        field: ToggleField::Enabled,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Official",
        description: "Mark as the vendor's official route.",
        field: ToggleField::Official,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Free",
        description: "Treat as free-tier with 100% quota weight.",
        field: ToggleField::Free,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Ignore quota",
        description: "Skip quota checks when scheduling this provider.",
        field: ToggleField::QuotaDisabled,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Cheap mode",
        description: "Use this provider in cheap-mode rotation.",
        field: ToggleField::Cheap,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Tough mode",
        description: "Use this provider for tough reasoning loops.",
        field: ToggleField::Tough,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Set effort",
        description: "Pass the configured effort value at launch.",
        field: ToggleField::Effort,
        baked_locked: false,
    },
];

pub(crate) const EXIT_OPTIONS: &[(ExitChoice, &str, char, &str)] = &[
    (
        ExitChoice::Save,
        "Save & close",
        's',
        "write the staged changes and close the panel",
    ),
    (
        ExitChoice::Discard,
        "Discard & close",
        'd',
        "drop unsaved changes and close",
    ),
    (
        ExitChoice::Cancel,
        "Cancel",
        'c',
        "keep editing without saving or closing",
    ),
];

pub(crate) const EXIT_DEFAULT_SELECTION: usize = 2;

pub(crate) fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// First toggle index that is editable for the provider type.
pub(crate) fn first_toggle_index(_is_baked: bool) -> usize {
    0
}

pub(crate) fn step_toggle(current: usize, is_baked: bool, delta: isize) -> usize {
    if PROVIDER_TOGGLES.is_empty() {
        return 0;
    }
    let len = PROVIDER_TOGGLES.len();
    let mut next = wrap_index(current, len, delta);
    // Skip baked-locked rows when navigating a built-in provider so the cursor
    // never lands on a no-op step. The list always contains at least one
    // unlocked row (`Enabled`), so the loop terminates.
    if is_baked {
        for _ in 0..len {
            if !PROVIDER_TOGGLES[next].baked_locked {
                break;
            }
            next = wrap_index(next, len, delta);
        }
    }
    next
}

pub(crate) fn lookup_section(arg: &str) -> SectionLookup {
    let needle = arg.trim().to_ascii_lowercase();
    if let Some(name) = resolve_section_name(&needle) {
        return SectionLookup::Exact(name);
    }
    let matches = section_prefix_matches(&needle);
    match matches.len() {
        0 => SectionLookup::Unknown,
        1 => SectionLookup::UniquePrefix(matches[0]),
        _ => SectionLookup::Ambiguous(matches),
    }
}

pub(crate) fn value_bool(o: &Override<bool>) -> String {
    if *o.value() { "true" } else { "false" }.to_string()
}

pub(crate) fn format_list(list: &[String]) -> String {
    if list.is_empty() {
        "(empty)".to_string()
    } else {
        list.join(", ")
    }
}

pub(crate) fn format_map(map: &std::collections::BTreeMap<String, String>) -> String {
    if map.is_empty() {
        "(empty)".to_string()
    } else {
        map.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub(crate) fn source_override<T>(o: &Override<T>) -> &'static str {
    if o.is_explicit() { "override" } else { "(def)" }
}

/// Translate a TUI-shaped key into the corresponding config-panel command.
///
/// This bridges the legacy key path (still exercised by test code) into the
/// typed `ConfigPanelCommand` surface.  Production input now crosses the seam
/// as `ConfigPanelCommand` directly (see `ui/tui.rs`).
pub(crate) fn config_panel_key_to_command(key: UiKey) -> ConfigPanelCommand {
    use crate::app_runtime::commands::CursorMove;

    if key.ctrl {
        return match key.code {
            UiKeyCode::Char('s') => ConfigPanelCommand::Save,
            UiKeyCode::Char('c') => ConfigPanelCommand::Cancel,
            UiKeyCode::Char('d') => ConfigPanelCommand::HalfPageDown,
            UiKeyCode::Char('u') => ConfigPanelCommand::HalfPageUp,
            UiKeyCode::Char('i') => ConfigPanelCommand::NextSection,
            UiKeyCode::Char('h') => ConfigPanelCommand::Edit(InputCommand::Backspace),
            UiKeyCode::Char(c) => ConfigPanelCommand::Edit(InputCommand::InsertText(c.to_string())),
            _ => ConfigPanelCommand::Edit(InputCommand::InsertText(String::new())),
        };
    }

    match key.code {
        UiKeyCode::Esc | UiKeyCode::Char('q') => ConfigPanelCommand::Close,
        UiKeyCode::Up | UiKeyCode::Char('k') => ConfigPanelCommand::MoveUp,
        UiKeyCode::Down | UiKeyCode::Char('j') => ConfigPanelCommand::MoveDown,
        UiKeyCode::Left | UiKeyCode::Char('h') => ConfigPanelCommand::PrevSection,
        UiKeyCode::Right | UiKeyCode::Char('l') => ConfigPanelCommand::NextSection,
        UiKeyCode::Enter => ConfigPanelCommand::Activate,
        UiKeyCode::Char(' ') => ConfigPanelCommand::Toggle,
        UiKeyCode::Char('n') => ConfigPanelCommand::AddProvider,
        UiKeyCode::Char('x') => ConfigPanelCommand::DeleteProvider,
        UiKeyCode::Char('d') => ConfigPanelCommand::DeleteEntry,
        UiKeyCode::Char('r') => ConfigPanelCommand::ToggleSecretReveal,
        UiKeyCode::Char('R') => ConfigPanelCommand::RemoveSavedSecret,
        UiKeyCode::Tab => ConfigPanelCommand::NextSection,
        UiKeyCode::BackTab => ConfigPanelCommand::PrevSection,
        UiKeyCode::Char('[') => ConfigPanelCommand::PrevSectionBracket,
        UiKeyCode::Char(']') => ConfigPanelCommand::NextSectionBracket,
        UiKeyCode::Char('g') => ConfigPanelCommand::JumpTop,
        UiKeyCode::Char('G') => ConfigPanelCommand::JumpBottom,
        UiKeyCode::PageDown => ConfigPanelCommand::HalfPageDown,
        UiKeyCode::PageUp => ConfigPanelCommand::HalfPageUp,
        UiKeyCode::Char('/') => ConfigPanelCommand::Edit(InputCommand::InsertText("/".to_string())),
        UiKeyCode::Backspace => ConfigPanelCommand::Edit(InputCommand::Backspace),
        UiKeyCode::Delete => ConfigPanelCommand::Edit(InputCommand::DeleteForward),
        UiKeyCode::Home => ConfigPanelCommand::Edit(InputCommand::MoveCursor(CursorMove::Home)),
        UiKeyCode::End => ConfigPanelCommand::Edit(InputCommand::MoveCursor(CursorMove::End)),
        UiKeyCode::Char(c) => ConfigPanelCommand::Edit(InputCommand::InsertText(c.to_string())),
        _ => ConfigPanelCommand::Edit(InputCommand::InsertText(String::new())),
    }
}

impl ConfigPanelState {
    pub(crate) fn current_view(&self) -> ConfigPanelView {
        let mut sections = Vec::with_capacity(SECTIONS.len());
        for &section in SECTIONS {
            let mut fields = Vec::new();
            for idx in field_indices_for(section) {
                let meta = FIELDS[idx];
                let value = self.value_for(&meta);
                fields.push(ConfigFieldView {
                    label: Arc::from(meta.label),
                    value: Arc::from(value),
                    description: Arc::from(meta.description),
                    is_secret: meta.secret,
                });
            }
            sections.push(ConfigSectionView {
                title: Arc::from(section_title(section)),
                fields: Arc::from(fields),
            });
        }

        ConfigPanelView {
            is_open: true,
            is_searching: self.searching.is_some(),
            is_editing: self.editing.is_some(),
            sections: Arc::from(sections),
            selected_section_index: self.selected_section,
            selected_field_index: self.selected_field,
        }
    }

    pub(crate) fn handle_command(&mut self, cmd: ConfigPanelCommand) -> PanelOutcome {
        if let Some(mut search) = self.searching.take() {
            let mapped = match &cmd {
                ConfigPanelCommand::Edit(input_cmd) => Some(input_cmd.clone()),
                ConfigPanelCommand::Close => Some(InputCommand::Cancel),
                ConfigPanelCommand::Activate => Some(InputCommand::Submit),
                ConfigPanelCommand::MoveUp => Some(InputCommand::MoveCursor(CursorMove::LineUp)),
                ConfigPanelCommand::MoveDown => {
                    Some(InputCommand::MoveCursor(CursorMove::LineDown))
                }
                _ => None,
            };
            if let Some(input_cmd) = mapped {
                // Cancel and Submit close the search overlay; keep the state
                // alive for all other commands.
                let closes_overlay =
                    matches!(input_cmd, InputCommand::Cancel | InputCommand::Submit);
                let outcome = self.handle_search_command(&mut search, input_cmd);
                if !closes_overlay {
                    self.searching = Some(search);
                }
                return outcome;
            }
            self.searching = Some(search);
        }

        if matches!(cmd, ConfigPanelCommand::Save) {
            self.save(false);
            let saved =
                self.editing.is_none() && self.conflict.is_none() && self.save_error.is_none();
            return if saved {
                PanelOutcome::Saved
            } else {
                PanelOutcome::KeepOpen
            };
        }
        if matches!(cmd, ConfigPanelCommand::Cancel) {
            return PanelOutcome::Close;
        }

        if let Some(conflict) = self.conflict.clone()
            && let Some(outcome) = self.handle_banner_command(conflict, cmd.clone())
        {
            return outcome;
        }

        if let Some(editing) = self.editing.take() {
            let mapped = match &cmd {
                ConfigPanelCommand::Edit(input_cmd) => Some(input_cmd.clone()),
                ConfigPanelCommand::Close => Some(InputCommand::Cancel),
                ConfigPanelCommand::Activate => Some(InputCommand::Submit),
                ConfigPanelCommand::MoveDown | ConfigPanelCommand::NextSection => {
                    Some(InputCommand::MoveCursor(CursorMove::LineDown))
                }
                ConfigPanelCommand::MoveUp | ConfigPanelCommand::PrevSection => {
                    Some(InputCommand::MoveCursor(CursorMove::LineUp))
                }
                _ => None,
            };
            if let Some(input_cmd) = mapped {
                self.handle_edit_command(editing, input_cmd);
                return PanelOutcome::KeepOpen;
            }
            self.editing = Some(editing);
        }

        match cmd {
            ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                if text == "/" && self.searching.is_none() && self.editing.is_none() =>
            {
                self.open_search();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::Close => {
                if self.dirty {
                    self.conflict = Some(ConflictBanner::ExitPrompt {
                        selected: EXIT_DEFAULT_SELECTION,
                    });
                    self.status = "unsaved changes — pick an exit option".to_string();
                    PanelOutcome::KeepOpen
                } else {
                    PanelOutcome::Close
                }
            }
            ConfigPanelCommand::MoveUp => {
                self.move_field(-1);
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::MoveDown => {
                self.move_field(1);
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::PrevSection => {
                self.move_section(-1);
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::NextSection => {
                self.move_section(1);
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::PrevSectionBracket => {
                self.selected_section = 0;
                self.select_first_field_in_current_section();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::NextSectionBracket => {
                self.selected_section = SECTIONS.len().saturating_sub(1);
                self.select_first_field_in_current_section();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::Activate => {
                if is_providers_section(self.current_section()) {
                    self.activate_provider_line();
                } else {
                    self.activate_field();
                }
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::Toggle => {
                if is_providers_section(self.current_section()) {
                    self.activate_provider_line();
                } else if let Some(meta) = self.current_meta().copied()
                    && matches!(meta.kind, FieldKind::Bool)
                {
                    self.flip_bool(&meta);
                }
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::AddProvider if is_providers_section(self.current_section()) => {
                self.open_add_provider_editor();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::DeleteProvider if is_providers_section(self.current_section()) => {
                self.remove_selected_provider();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::HalfPageDown => {
                if is_providers_section(self.current_section()) {
                    self.move_providers_page(1);
                }
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::HalfPageUp => {
                if is_providers_section(self.current_section()) {
                    self.move_providers_page(-1);
                }
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::DeleteEntry => {
                self.reset_field();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::ToggleSecretReveal
                if self.current_meta().is_some_and(|m| m.key == "ntfy.topic") =>
            {
                self.reveal_topic = !self.reveal_topic;
                self.status = if self.reveal_topic {
                    "topic revealed".to_string()
                } else {
                    "topic hidden".to_string()
                };
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::RemoveSavedSecret
                if self.current_meta().is_some_and(|m| m.key == "ntfy.topic") =>
            {
                self.conflict = Some(ConflictBanner::RegenerateTopicPrompt);
                self.status = "regenerate topic? y/n".to_string();
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::JumpTop => {
                if is_providers_section(self.current_section()) {
                    self.providers_cursor = 0;
                    self.providers_scroll.set(0);
                    self.move_field(1);
                    self.move_field(-1);
                } else {
                    self.select_first_field_in_current_section();
                }
                PanelOutcome::KeepOpen
            }
            ConfigPanelCommand::JumpBottom => {
                if is_providers_section(self.current_section()) {
                    let len = providers::get_lines(&self.config, &self.folded_vendors).len();
                    self.providers_cursor = len.saturating_sub(1);
                } else if let Some(last) = field_indices_for(self.current_section()).last() {
                    self.selected_field = *last;
                }
                PanelOutcome::KeepOpen
            }
            _ => PanelOutcome::KeepOpen,
        }
    }
    pub(crate) fn open_at(config: &Config, path: PathBuf, initial_section: Option<&str>) -> Self {
        let opened_mtime = mtime(&path);
        let selected_section = initial_section
            .and_then(resolve_section_name)
            .and_then(|name| SECTIONS.iter().position(|s| *s == name))
            .unwrap_or(0);
        let folded_vendors = providers::all_vendors(config);
        let mut state = Self {
            config: config.clone(),
            path,
            opened_mtime,
            selected_section,
            selected_field: 1,
            status: "config open".to_string(),
            editing: None,
            edit_buffer: String::new(),
            reveal_topic: false,
            conflict: None,
            dirty: false,
            save_error: None,
            searching: None,
            providers_cursor: 0,
            providers_scroll: Cell::new(0),
            providers_body_h: Cell::new(0),
            folded_vendors,
        };
        state.select_first_field_in_current_section();
        state
    }

    pub(crate) fn current_section_name(&self) -> &'static str {
        self.current_section()
    }

    pub(crate) fn current_section(&self) -> &'static str {
        SECTIONS
            .get(self.selected_section)
            .copied()
            .unwrap_or(SECTIONS[0])
    }

    pub(crate) fn current_meta(&self) -> Option<&'static FieldMeta> {
        FIELDS.get(self.selected_field)
    }

    pub(crate) fn select_first_field_in_current_section(&mut self) {
        if let Some(idx) = field_indices_for(self.current_section()).first() {
            self.selected_field = *idx;
        }
    }

    fn move_section(&mut self, delta: isize) {
        self.selected_section = wrap_index(self.selected_section, SECTIONS.len(), delta);
        self.providers_scroll.set(0);
        self.select_first_field_in_current_section();
    }

    fn move_field(&mut self, delta: isize) {
        let section = self.current_section();
        if is_providers_section(section) {
            let lines = providers::get_lines(&self.config, &self.folded_vendors);
            if !lines.is_empty() {
                let mut next = wrap_index(self.providers_cursor, lines.len(), delta);
                for _ in 0..lines.len() {
                    if !matches!(
                        lines.get(next),
                        Some(providers::ProvidersLine::ModelHeader { .. })
                    ) {
                        break;
                    }
                    next = wrap_index(next, lines.len(), delta);
                }
                self.providers_cursor = next;
            }
            return;
        }
        let fields = field_indices_for(section);
        if fields.is_empty() {
            return;
        }
        let pos = fields
            .iter()
            .position(|idx| *idx == self.selected_field)
            .unwrap_or(0);
        let next = wrap_index(pos, fields.len(), delta);
        self.selected_field = fields[next];
    }

    fn move_providers_page(&mut self, delta: isize) {
        let lines = providers::get_lines(&self.config, &self.folded_vendors);
        if lines.is_empty() {
            return;
        }
        let body_h = self.providers_body_h.get().max(1);
        let step = ((body_h as isize) / 2).max(1) * delta.signum();
        for _ in 0..step.unsigned_abs() {
            let before = self.providers_cursor;
            self.move_field(delta.signum());
            if self.providers_cursor == before {
                break;
            }
        }
    }

    fn clamp_provider_cursor(&mut self) {
        let len = providers::get_lines(&self.config, &self.folded_vendors).len();
        if len == 0 {
            self.providers_cursor = 0;
        } else {
            self.providers_cursor = self.providers_cursor.min(len - 1);
        }
    }

    fn open_search(&mut self) {
        let mut state = SearchState {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
        };
        Self::recompute_search_results(&mut state);
        self.status = "search · type to filter".to_string();
        self.searching = Some(state);
    }

    fn recompute_search_results(state: &mut SearchState) {
        let needle = state.query.to_lowercase();
        state.results = FIELDS
            .iter()
            .enumerate()
            .filter(|(_, meta)| {
                if needle.is_empty() {
                    return true;
                }
                meta.key.to_lowercase().contains(&needle)
                    || meta.label.to_lowercase().contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect();
        if state.selected >= state.results.len() {
            state.selected = state.results.len().saturating_sub(1);
        }
    }

    fn handle_search_command(
        &mut self,
        search: &mut SearchState,
        cmd: InputCommand,
    ) -> PanelOutcome {
        match cmd {
            InputCommand::Cancel => {
                self.status = "search cancelled".to_string();
                self.searching = None;
                return PanelOutcome::KeepOpen;
            }
            InputCommand::Submit => {
                if let Some(idx) = search.results.get(search.selected).copied() {
                    let meta = FIELDS[idx];
                    if let Some(section_idx) = SECTIONS.iter().position(|s| *s == meta.section) {
                        self.selected_section = section_idx;
                    }
                    self.selected_field = idx;
                    self.status = format!("jumped to {}", meta.key);
                } else {
                    self.status = "no match".to_string();
                }
                self.searching = None;
                return PanelOutcome::KeepOpen;
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineUp)
            | InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::Left)
                if !search.results.is_empty() =>
            {
                search.selected = wrap_index(search.selected, search.results.len(), -1);
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineDown)
            | InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::Right)
                if !search.results.is_empty() =>
            {
                search.selected = wrap_index(search.selected, search.results.len(), 1);
            }
            InputCommand::Backspace => {
                search.query.pop();
                Self::recompute_search_results(search);
            }
            InputCommand::InsertText(text) => {
                search.query.push_str(&text);
                Self::recompute_search_results(search);
            }
            _ => {}
        }
        PanelOutcome::KeepOpen
    }

    fn execute_exit_choice(&mut self, choice: ExitChoice) -> PanelOutcome {
        match choice {
            ExitChoice::Save => {
                self.conflict = None;
                self.save(false);
                let saved = self.editing.is_none()
                    && self.conflict.is_none()
                    && self.save_error.is_none()
                    && !self.dirty;
                if saved {
                    PanelOutcome::Close
                } else {
                    PanelOutcome::KeepOpen
                }
            }
            ExitChoice::Discard => {
                self.dirty = false;
                self.conflict = None;
                PanelOutcome::Close
            }
            ExitChoice::Cancel => {
                self.conflict = None;
                self.status = "kept editing".to_string();
                PanelOutcome::KeepOpen
            }
        }
    }

    fn handle_banner_command(
        &mut self,
        banner: ConflictBanner,
        cmd: ConfigPanelCommand,
    ) -> Option<PanelOutcome> {
        match banner {
            ConflictBanner::MtimeAdvanced => match cmd {
                ConfigPanelCommand::Activate => {
                    match load_from_path(&self.path) {
                        Ok(config) => {
                            self.config = config;
                            self.opened_mtime = mtime(&self.path);
                            self.dirty = false;
                            self.status = "reloaded config".to_string();
                            self.save_error = None;
                        }
                        Err(err) => self.save_error = Some(err.to_string()),
                    }
                    self.conflict = None;
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Save => {
                    self.conflict = None;
                    self.save(true);
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Cancel | ConfigPanelCommand::Close => {
                    self.conflict = None;
                    self.status = "kept editing".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Edit(InputCommand::InsertText(ref text)) if text == "r" => {
                    match load_from_path(&self.path) {
                        Ok(config) => {
                            self.config = config;
                            self.opened_mtime = mtime(&self.path);
                            self.dirty = false;
                            self.status = "reloaded config".to_string();
                            self.save_error = None;
                        }
                        Err(err) => self.save_error = Some(err.to_string()),
                    }
                    self.conflict = None;
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Edit(InputCommand::InsertText(ref text)) if text == "o" => {
                    self.conflict = None;
                    self.save(true);
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                    if text == "c" || text == "q" =>
                {
                    self.conflict = None;
                    self.status = "kept editing".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                _ => Some(PanelOutcome::KeepOpen),
            },
            ConflictBanner::ExitPrompt { selected } => {
                let len = EXIT_OPTIONS.len();
                match cmd {
                    ConfigPanelCommand::MoveUp => {
                        let next = wrap_index(selected, len, -1);
                        self.conflict = Some(ConflictBanner::ExitPrompt { selected: next });
                        Some(PanelOutcome::KeepOpen)
                    }
                    ConfigPanelCommand::MoveDown => {
                        let next = wrap_index(selected, len, 1);
                        self.conflict = Some(ConflictBanner::ExitPrompt { selected: next });
                        Some(PanelOutcome::KeepOpen)
                    }
                    ConfigPanelCommand::Activate => {
                        Some(self.execute_exit_choice(EXIT_OPTIONS[selected].0))
                    }
                    ConfigPanelCommand::Save => Some(self.execute_exit_choice(ExitChoice::Save)),
                    ConfigPanelCommand::DeleteEntry => {
                        Some(self.execute_exit_choice(ExitChoice::Discard))
                    }
                    ConfigPanelCommand::Cancel | ConfigPanelCommand::Close => {
                        Some(self.execute_exit_choice(ExitChoice::Cancel))
                    }
                    ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                        if text == "s" || text == "S" =>
                    {
                        Some(self.execute_exit_choice(ExitChoice::Save))
                    }
                    ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                        if text == "d" || text == "D" =>
                    {
                        Some(self.execute_exit_choice(ExitChoice::Discard))
                    }
                    ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                        if text == "c" || text == "C" || text == "q" =>
                    {
                        Some(self.execute_exit_choice(ExitChoice::Cancel))
                    }
                    _ => Some(PanelOutcome::KeepOpen),
                }
            }
            ConflictBanner::RegenerateTopicPrompt => match cmd {
                ConfigPanelCommand::Activate => {
                    match notifications::generate_topic()
                        .map_err(|err| anyhow!("{err:#}"))
                        .and_then(|topic| self.set_value("ntfy.topic", &topic))
                    {
                        Ok(()) => self.status = "regenerated ntfy.topic".to_string(),
                        Err(err) => self.status = err.to_string(),
                    }
                    self.conflict = None;
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Cancel | ConfigPanelCommand::Close => {
                    self.conflict = None;
                    self.status = "unchanged".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                    if text == "y" || text == "Y" =>
                {
                    match notifications::generate_topic()
                        .map_err(|err| anyhow!("{err:#}"))
                        .and_then(|topic| self.set_value("ntfy.topic", &topic))
                    {
                        Ok(()) => self.status = "regenerated ntfy.topic".to_string(),
                        Err(err) => self.status = err.to_string(),
                    }
                    self.conflict = None;
                    Some(PanelOutcome::KeepOpen)
                }
                ConfigPanelCommand::Edit(InputCommand::InsertText(ref text))
                    if text == "n" || text == "N" || text == "q" =>
                {
                    self.conflict = None;
                    self.status = "unchanged".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                _ => Some(PanelOutcome::KeepOpen),
            },
        }
    }

    fn handle_edit_command(&mut self, editing: Editing, cmd: InputCommand) {
        match editing {
            Editing::AddProvider(mut editor) => {
                if self.handle_add_provider_command(&mut editor, cmd) {
                    self.editing = Some(Editing::AddProvider(editor));
                }
                return;
            }
            Editing::ProviderDetail { cursor } => {
                self.handle_provider_detail_command(cursor, cmd);
                return;
            }
            Editing::Choice {
                key,
                options,
                selected,
            } => {
                self.handle_choice_command(key, options, selected, cmd);
                return;
            }
            _ => {
                self.editing = Some(editing);
            }
        }

        match cmd {
            InputCommand::Cancel => {
                self.editing = None;
                self.status = "edit cancelled".to_string();
            }
            InputCommand::Submit => {
                self.accept_edit();
            }
            InputCommand::Backspace => {
                self.edit_buffer.pop();
            }
            InputCommand::InsertText(text) => {
                if matches!(self.editing, Some(Editing::Integer)) {
                    for c in text.chars() {
                        if c.is_ascii_digit() {
                            self.edit_buffer.push(c);
                        }
                    }
                } else {
                    self.edit_buffer.push_str(&text);
                }
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineUp)
            | InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::Left) => {
                if matches!(self.editing, Some(Editing::Integer)) {
                    self.nudge_integer(1);
                }
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineDown)
            | InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::Right) => {
                if matches!(self.editing, Some(Editing::Integer)) {
                    self.nudge_integer(-1);
                }
            }
            _ => {}
        }
    }

    fn handle_add_provider_command(
        &mut self,
        editor: &mut providers::ProvidersEditor,
        cmd: InputCommand,
    ) -> bool {
        if let Some(target) = editor.open_dropdown {
            let options = editor.dropdown_options(target);
            let len = options.len();
            match cmd {
                InputCommand::Cancel => editor.close_dropdown(),
                InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineUp)
                    if len > 0 =>
                {
                    editor.dropdown_cursor = wrap_index(editor.dropdown_cursor, len, -1);
                }
                InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineDown)
                    if len > 0 =>
                {
                    editor.dropdown_cursor = wrap_index(editor.dropdown_cursor, len, 1);
                }
                InputCommand::Submit => editor.commit_dropdown(),
                _ => {}
            }
            return true;
        }

        match cmd {
            InputCommand::Cancel => {
                self.editing = None;
                self.status = "add cancelled".to_string();
                false
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineUp) => {
                editor.focus = editor.focus.prev();
                true
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineDown) => {
                editor.focus = editor.focus.next();
                true
            }
            InputCommand::Submit => match editor.focus {
                providers::AddProviderField::Model
                | providers::AddProviderField::Subscription
                | providers::AddProviderField::Cli => {
                    editor.open_dropdown(editor.focus);
                    true
                }
                providers::AddProviderField::Official => {
                    editor.official = !editor.official;
                    true
                }
                providers::AddProviderField::Free => {
                    editor.free = !editor.free;
                    true
                }
                providers::AddProviderField::LaunchName => {
                    if editor.commit(&mut self.config) {
                        self.dirty = true;
                        self.status = "provider added".to_string();
                        self.editing = None;
                        false
                    } else {
                        self.status =
                            "invalid provider data (duplicate or empty fields)".to_string();
                        true
                    }
                }
            },
            InputCommand::InsertText(text) => {
                if matches!(editor.focus, providers::AddProviderField::LaunchName) {
                    editor.launch_name.push_str(&text);
                } else if text == " " {
                    match editor.focus {
                        providers::AddProviderField::Model
                        | providers::AddProviderField::Subscription
                        | providers::AddProviderField::Cli => editor.open_dropdown(editor.focus),
                        providers::AddProviderField::Official => editor.official = !editor.official,
                        providers::AddProviderField::Free => editor.free = !editor.free,
                        providers::AddProviderField::LaunchName => editor.launch_name.push(' '),
                    }
                }
                true
            }
            InputCommand::Backspace => {
                if matches!(editor.focus, providers::AddProviderField::LaunchName) {
                    editor.launch_name.pop();
                }
                true
            }
            _ => true,
        }
    }

    fn handle_choice_command(
        &mut self,
        field_key: &'static str,
        options: Vec<String>,
        selected: usize,
        cmd: InputCommand,
    ) {
        match cmd {
            InputCommand::Cancel => {
                self.editing = None;
                self.status = "edit cancelled".to_string();
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineUp) => {
                let next = wrap_index(selected, options.len(), -1);
                self.set_choice_selected(next);
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineDown) => {
                let next = wrap_index(selected, options.len(), 1);
                self.set_choice_selected(next);
            }
            InputCommand::Submit => {
                let value = options.get(selected).cloned().unwrap_or_default();
                match self.set_value(field_key, &value) {
                    Ok(()) => {
                        self.status = format!("set {field_key} to {value}");
                        self.editing = None;
                    }
                    Err(err) => {
                        self.status = err.to_string();
                        self.editing = None;
                    }
                }
            }
            _ => {}
        }
    }

    fn set_choice_selected(&mut self, next: usize) {
        if let Some(Editing::Choice { selected, .. }) = self.editing.as_mut() {
            *selected = next;
        }
    }

    fn open_choice(&mut self, meta: &FieldMeta, variants: &[&'static str]) {
        let options: Vec<String> = variants.iter().map(|s| (*s).to_string()).collect();
        let current = self.value_for(meta);
        let selected = options.iter().position(|v| v == &current).unwrap_or(0);
        self.editing = Some(Editing::Choice {
            key: meta.key,
            options,
            selected,
        });
        self.status = format!("choose {}", meta.key);
    }

    fn open_add_provider_editor(&mut self) {
        let merged =
            crate::logic::selection::baked::merge_with_overrides(self.config.providers.value());
        let mut available: Vec<(String, String)> = Vec::new();
        for entry in &merged {
            let vendor_label =
                crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription)
                    .to_string();
            let pair = (vendor_label, entry.model.clone());
            if !available.contains(&pair) {
                available.push(pair);
            }
        }

        self.editing = Some(Editing::AddProvider(providers::ProvidersEditor::new(
            available,
        )));
        self.status = "add model provider".to_string();
    }

    fn remove_selected_provider(&mut self) {
        let lines = providers::get_lines(&self.config, &self.folded_vendors);
        let Some(providers::ProvidersLine::Provider {
            entry, is_baked, ..
        }) = lines.get(self.providers_cursor)
        else {
            self.status = "select a model provider first".to_string();
            return;
        };

        let identity = entry.identity();
        let mut existing = self.config.providers.value().clone();
        let before = existing.len();
        existing.retain(|candidate| candidate.identity() != identity);
        if existing.len() < before {
            self.config.providers = Override::explicit(existing);
            self.dirty = true;
            self.status = if *is_baked {
                "removed custom override; built-in provider restored".to_string()
            } else {
                "deleted custom provider".to_string()
            };
            self.clamp_provider_cursor();
            return;
        }

        if *is_baked {
            let mut disabled = entry.clone();
            disabled.enabled = false;
            self.upsert_provider_override(disabled);
            self.dirty = true;
            self.status = "built-in provider disabled".to_string();
        } else {
            self.status = "provider already absent".to_string();
        }
        self.clamp_provider_cursor();
    }

    fn upsert_provider_override(&mut self, updated: crate::data::config::schema::ProviderEntry) {
        let mut existing = self.config.providers.value().clone();
        if let Some(pos) = existing
            .iter()
            .position(|entry| entry.identity() == updated.identity())
        {
            existing[pos] = updated;
        } else {
            existing.push(updated);
        }
        self.config.providers = Override::explicit(existing);
    }

    pub(crate) fn activate_provider_line(&mut self) {
        let lines = providers::get_lines(&self.config, &self.folded_vendors);
        let Some(line) = lines.get(self.providers_cursor) else {
            return;
        };

        match line {
            providers::ProvidersLine::VendorHeader { vendor, folded } => {
                let vendor = vendor.clone();
                let was_folded = *folded;
                if was_folded {
                    self.folded_vendors.remove(&vendor);
                    self.status = format!("expanded {vendor}");
                } else {
                    self.folded_vendors.insert(vendor.clone());
                    self.status = format!("folded {vendor}");
                }
                let new_lines = providers::get_lines(&self.config, &self.folded_vendors);
                if let Some(idx) = new_lines.iter().position(|l| {
                    matches!(
                        l,
                        providers::ProvidersLine::VendorHeader { vendor: v, .. }
                            if v == &vendor
                    )
                }) {
                    self.providers_cursor = idx;
                }
            }
            providers::ProvidersLine::ModelHeader { .. } => {}
            providers::ProvidersLine::Provider {
                entry, is_baked, ..
            } => {
                let entry = entry.clone();
                let is_baked = *is_baked;
                self.editing = Some(Editing::ProviderDetail {
                    cursor: first_toggle_index(is_baked),
                });
                self.status = format!("editing {} · {}", entry.cli.as_str(), entry.launch_name);
            }
            providers::ProvidersLine::AddAction => {
                self.open_add_provider_editor();
            }
        }
    }

    fn toggle_provider_property(&mut self, idx: usize) {
        let lines = providers::get_lines(&self.config, &self.folded_vendors);
        let Some(providers::ProvidersLine::Provider {
            entry, is_baked, ..
        }) = lines.get(self.providers_cursor)
        else {
            return;
        };
        let mut updated = entry.clone();
        let is_baked = *is_baked;
        let toggle = match PROVIDER_TOGGLES.get(idx) {
            Some(t) => *t,
            None => return,
        };
        if toggle.baked_locked && is_baked {
            self.status = format!("{} is set by the built-in entry", toggle.label);
            return;
        }
        match toggle.field {
            ToggleField::Enabled => updated.enabled = !updated.enabled,
            ToggleField::Official => updated.official = !updated.official,
            ToggleField::Free => updated.free = !updated.free,
            ToggleField::QuotaDisabled => updated.quota_disabled = !updated.quota_disabled,
            ToggleField::Cheap => updated.cheap_eligible = !updated.cheap_eligible,
            ToggleField::Tough => updated.tough_eligible = !updated.tough_eligible,
            ToggleField::Effort => updated.effort_eligible = !updated.effort_eligible,
        }
        self.upsert_provider_override(updated);
        self.dirty = true;
        self.status = format!("{} toggled", toggle.label);
    }

    fn handle_provider_detail_command(&mut self, cursor: usize, cmd: InputCommand) {
        let lines = providers::get_lines(&self.config, &self.folded_vendors);
        let is_baked = matches!(
            lines.get(self.providers_cursor),
            Some(providers::ProvidersLine::Provider { is_baked: true, .. })
        );
        match cmd {
            InputCommand::Cancel => {
                self.editing = None;
                self.status = "closed details".to_string();
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineUp) => {
                let next = step_toggle(cursor, is_baked, -1);
                self.editing = Some(Editing::ProviderDetail { cursor: next });
            }
            InputCommand::MoveCursor(crate::app_runtime::commands::CursorMove::LineDown) => {
                let next = step_toggle(cursor, is_baked, 1);
                self.editing = Some(Editing::ProviderDetail { cursor: next });
            }
            InputCommand::Submit => {
                self.toggle_provider_property(cursor);
                self.editing = Some(Editing::ProviderDetail { cursor });
            }
            InputCommand::InsertText(text) if text == " " => {
                self.toggle_provider_property(cursor);
                self.editing = Some(Editing::ProviderDetail { cursor });
            }
            _ => {}
        }
    }

    fn flip_bool(&mut self, meta: &FieldMeta) {
        let current = self.value_for(meta);
        let next = if current == "true" { "false" } else { "true" };
        match self.set_value(meta.key, next) {
            Ok(()) => self.status = format!("set {} to {next}", meta.key),
            Err(err) => self.status = err.to_string(),
        }
    }

    fn activate_field(&mut self) {
        let Some(meta) = self.current_meta().copied() else {
            return;
        };
        match meta.kind {
            FieldKind::Bool => self.flip_bool(&meta),
            FieldKind::Enum(variants) => self.open_choice(&meta, variants),
            FieldKind::Integer { .. } => {
                let value = self.value_for(&meta);
                self.edit_buffer = value;
                self.editing = Some(Editing::Integer);
                self.status = format!("editing {}", meta.key);
            }
            FieldKind::String => {
                let value = self.value_for(&meta);
                self.edit_buffer = value;
                self.editing = Some(Editing::String);
                self.status = format!("editing {}", meta.key);
            }
            FieldKind::List | FieldKind::Map | FieldKind::ReadOnly => {
                self.status = "read-only".to_string();
            }
        }
    }

    fn accept_edit(&mut self) {
        let Some(meta) = self.current_meta().copied() else {
            return;
        };
        if let Some(reason) = self.edit_error_for(meta, &self.edit_buffer) {
            self.status = reason;
            return;
        }
        let value = self.edit_buffer.clone();
        match self.set_value(meta.key, &value) {
            Ok(()) => {
                self.status = format!("set {} to {value}", meta.key);
                self.editing = None;
            }
            Err(err) => {
                self.status = err.to_string();
            }
        }
    }

    fn reset_field(&mut self) {
        let Some(meta) = self.current_meta().copied() else {
            return;
        };
        if matches!(meta.kind, FieldKind::ReadOnly) {
            self.status = "read-only".to_string();
            return;
        }
        if self.source_for(&meta) == "(def)" {
            self.status = "unchanged".to_string();
            return;
        }
        match mutate::unset_value(&mut self.config, meta.key) {
            Ok(()) => {
                self.dirty = true;
                self.status = format!("reset {} to default", meta.label);
            }
            Err(err) => self.status = err.to_string(),
        }
    }

    fn save(&mut self, overwrite: bool) {
        self.save_error = None;
        if self.editing.is_some() {
            self.accept_edit();
        }
        if self.editing.is_some() {
            return;
        }
        if let Some(reason) = self.current_validation_error() {
            self.status = reason;
            return;
        }
        if !overwrite && self.opened_mtime != mtime(&self.path) {
            self.conflict = Some(ConflictBanner::MtimeAdvanced);
            self.status = "config changed on disk".to_string();
            return;
        }
        match crate::data::config::save_atomic_to(&self.path, &self.config) {
            Ok(()) => {
                self.opened_mtime = mtime(&self.path);
                self.dirty = false;
                self.status = "saved · in effect immediately".to_string();
            }
            Err(err) => {
                self.save_error = Some(err.to_string());
                self.status = "save failed".to_string();
            }
        }
    }

    fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        mutate::set_value(&mut self.config, key, value).map_err(|err| anyhow!(err.to_string()))?;
        self.dirty = true;
        Ok(())
    }

    fn nudge_integer(&mut self, delta: i64) {
        let min = self
            .current_meta()
            .and_then(|m| match m.kind {
                FieldKind::Integer { min } => Some(min as i64),
                _ => None,
            })
            .unwrap_or(0);
        let current = self.edit_buffer.parse::<i64>().unwrap_or(min);
        self.edit_buffer = current.saturating_add(delta).max(min).to_string();
    }

    pub(crate) fn section_override_count(&self, section: &str) -> usize {
        let field_count = FIELDS
            .iter()
            .filter(|meta| meta.section == section && self.source_for(meta) == "override")
            .count();
        if section == "models" && self.config.providers.is_explicit() {
            field_count + self.config.providers.value().len().max(1)
        } else {
            field_count
        }
    }

    pub(crate) fn value_for(&self, meta: &FieldMeta) -> String {
        match meta.key {
            "meta.version" => self.config.meta.version.to_string(),
            "ntfy.enabled" => value_bool(&self.config.ntfy.enabled),
            "ntfy.server" => self.config.ntfy.server.value().clone(),
            "ntfy.topic" => self.config.ntfy.topic.value().clone(),
            "ntfy.detail_mode" => self.config.ntfy.detail_mode.value().as_str().to_string(),
            "ntfy.retry_attempts" => self.config.ntfy.retry_attempts.value().to_string(),
            "ntfy.retry_delay_ms" => self.config.ntfy.retry_delay_ms.value().to_string(),
            "ntfy.http_timeout_secs" => self.config.ntfy.http_timeout_secs.value().to_string(),
            "ntfy.body_max_bytes" => self.config.ntfy.body_max_bytes.value().to_string(),
            "ntfy.excerpt_max_chars" => self.config.ntfy.excerpt_max_chars.value().to_string(),
            "ntfy.created_at" => self
                .config
                .ntfy
                .created_at
                .value()
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_default(),
            "ntfy.updated_at" => self
                .config
                .ntfy
                .updated_at
                .value()
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_default(),
            "ntfy.events.stage_wait" => value_bool(&self.config.ntfy.events.stage_wait),
            "ntfy.events.interactive_wait" => value_bool(&self.config.ntfy.events.interactive_wait),
            "ntfy.events.pipeline_done" => value_bool(&self.config.ntfy.events.pipeline_done),
            "acp.policy.shell_policy" => self
                .config
                .acp
                .policy
                .shell_policy
                .value()
                .as_str()
                .to_string(),
            "acp.policy.shell_allowlist" => {
                format_list(self.config.acp.policy.shell_allowlist.value())
            }
            "acp.policy.enforce_readonly_workspace" => {
                value_bool(&self.config.acp.policy.enforce_readonly_workspace)
            }
            "acp.policy.allowed_write_paths" => {
                format_list(self.config.acp.policy.allowed_write_paths.value())
            }
            "acp.install.claude_acp_root" => {
                self.config.acp.install.claude_acp_root.value().clone()
            }
            "acp.install.prefer_local_claude_acp" => {
                value_bool(&self.config.acp.install.prefer_local_claude_acp)
            }
            "runner.full_review_interval" => {
                self.config.runner.full_review_interval.value().to_string()
            }
            "paths.cache_root" => self.config.paths.cache_root.value().clone(),
            "paths.sessions_root" => self.config.paths.sessions_root.value().clone(),
            "paths.runs_root" => self.config.paths.runs_root.value().clone(),
            "paths.memory_root" => self.config.paths.memory_root.value().clone(),
            "ui.prefer_split_on_open" => value_bool(&self.config.ui.prefer_split_on_open),
            "ui.colon_palette.show_help" => value_bool(&self.config.ui.colon_palette.show_help),
            "ui.footer.show_keys" => value_bool(&self.config.ui.footer.show_keys),
            "diagnostics.log_level" => self
                .config
                .diagnostics
                .log_level
                .value()
                .as_str()
                .to_string(),
            "diagnostics.json_logs" => value_bool(&self.config.diagnostics.json_logs),
            "memory.enabled" => value_bool(&self.config.memory.enabled),
            "memory.max_topics_per_read" => {
                self.config.memory.max_topics_per_read.value().to_string()
            }
            "memory.journal_retention_months" => self
                .config
                .memory
                .journal_retention_months
                .value()
                .to_string(),
            "acp.agents.claude.env" => format_map(self.config.acp.agents.claude.env.value()),
            _ => String::new(),
        }
    }

    pub(crate) fn source_for(&self, meta: &FieldMeta) -> &'static str {
        match meta.key {
            "meta.version" => "(def)",
            "ntfy.enabled" => source_override(&self.config.ntfy.enabled),
            "ntfy.server" => source_override(&self.config.ntfy.server),
            "ntfy.topic" => source_override(&self.config.ntfy.topic),
            "ntfy.detail_mode" => source_override(&self.config.ntfy.detail_mode),
            "ntfy.retry_attempts" => source_override(&self.config.ntfy.retry_attempts),
            "ntfy.retry_delay_ms" => source_override(&self.config.ntfy.retry_delay_ms),
            "ntfy.http_timeout_secs" => source_override(&self.config.ntfy.http_timeout_secs),
            "ntfy.body_max_bytes" => source_override(&self.config.ntfy.body_max_bytes),
            "ntfy.excerpt_max_chars" => source_override(&self.config.ntfy.excerpt_max_chars),
            "ntfy.created_at" => source_override(&self.config.ntfy.created_at),
            "ntfy.updated_at" => source_override(&self.config.ntfy.updated_at),
            "ntfy.events.stage_wait" => source_override(&self.config.ntfy.events.stage_wait),
            "ntfy.events.interactive_wait" => {
                source_override(&self.config.ntfy.events.interactive_wait)
            }
            "ntfy.events.pipeline_done" => source_override(&self.config.ntfy.events.pipeline_done),
            "acp.policy.shell_policy" => source_override(&self.config.acp.policy.shell_policy),
            "acp.policy.shell_allowlist" => {
                source_override(&self.config.acp.policy.shell_allowlist)
            }
            "acp.policy.enforce_readonly_workspace" => {
                source_override(&self.config.acp.policy.enforce_readonly_workspace)
            }
            "acp.policy.allowed_write_paths" => {
                source_override(&self.config.acp.policy.allowed_write_paths)
            }
            "acp.install.claude_acp_root" => {
                source_override(&self.config.acp.install.claude_acp_root)
            }
            "acp.install.prefer_local_claude_acp" => {
                source_override(&self.config.acp.install.prefer_local_claude_acp)
            }
            "runner.full_review_interval" => {
                source_override(&self.config.runner.full_review_interval)
            }
            "paths.cache_root" => source_override(&self.config.paths.cache_root),
            "paths.sessions_root" => source_override(&self.config.paths.sessions_root),
            "paths.runs_root" => source_override(&self.config.paths.runs_root),
            "paths.memory_root" => source_override(&self.config.paths.memory_root),
            "ui.prefer_split_on_open" => source_override(&self.config.ui.prefer_split_on_open),
            "ui.colon_palette.show_help" => {
                source_override(&self.config.ui.colon_palette.show_help)
            }
            "ui.footer.show_keys" => source_override(&self.config.ui.footer.show_keys),
            "diagnostics.log_level" => source_override(&self.config.diagnostics.log_level),
            "diagnostics.json_logs" => source_override(&self.config.diagnostics.json_logs),
            "memory.enabled" => source_override(&self.config.memory.enabled),
            "memory.max_topics_per_read" => {
                source_override(&self.config.memory.max_topics_per_read)
            }
            "memory.journal_retention_months" => {
                source_override(&self.config.memory.journal_retention_months)
            }
            "acp.agents.claude.env" => source_override(&self.config.acp.agents.claude.env),
            _ => "(def)",
        }
    }

    pub(crate) fn current_validation_error(&self) -> Option<String> {
        if let Some(meta) = self.current_meta()
            && let Some(reason) = self.edit_error_for(*meta, &self.edit_buffer)
        {
            return Some(format!("cannot save: {reason}"));
        }
        None
    }

    pub(crate) fn edit_error_for(&self, meta: FieldMeta, value: &str) -> Option<String> {
        self.editing.as_ref()?;
        match meta.kind {
            FieldKind::Integer { min } => match value.parse::<u64>() {
                Ok(parsed) if parsed < min => Some(format!("{} must be >= {min}", meta.key)),
                Ok(_) => None,
                Err(_) => Some(format!("{} must be an integer", meta.key)),
            },
            FieldKind::String if meta.key == "ntfy.server" => {
                if value.starts_with("http://") || value.starts_with("https://") {
                    None
                } else {
                    Some("ntfy.server must start with http:// or https://".to_string())
                }
            }
            _ => None,
        }
    }
}

pub(crate) fn can_open(width: u16) -> bool {
    width >= 50
}

pub(crate) fn terminal_too_narrow_message() -> &'static str {
    "terminal too narrow (need ≥50 cols)"
}

#[cfg(test)]
pub(crate) fn field_index_for_test(key: &str) -> usize {
    FIELDS.iter().position(|f| f.key == key).expect("field key")
}

#[cfg(test)]
impl ConfigPanelState {
    pub(crate) fn set_focus_for_test(&mut self, field_idx: usize) {
        self.selected_field = field_idx;
        self.selected_section = SECTIONS
            .iter()
            .position(|s| *s == FIELDS[field_idx].section)
            .expect("section for field");
    }

    pub(crate) fn set_edit_buffer_for_test(&mut self, buffer: String) {
        self.edit_buffer = buffer;
    }

    pub(crate) fn expand_all_vendors_for_test(&mut self) {
        self.folded_vendors.clear();
    }
}
