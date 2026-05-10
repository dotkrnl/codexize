use crate::data::{
    config::{
        Config,
        loader::load_from_path,
        mutate,
        schema::{LogLevel, NtfyDetailMode, Override, ShellPolicy},
    },
    notifications,
};
use crate::ui::chrome::{bottom_rule, top_rule_with_left_spans};
use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use itertools::Itertools;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) mod providers;

const MIN_WIDTH: u16 = 50;
const TAB_SEPARATOR: &str = "  ";
const LABEL_WIDTH: usize = 28;

// Pipeline-style palette: focus accent matches the pipeline focus glyph,
// override accent picks up the warning yellow used for waiting nodes.
const COLOR_FOCUS: Color = Color::Cyan;
const COLOR_OVERRIDE: Color = Color::Yellow;
const COLOR_DIM: Color = Color::DarkGray;
const COLOR_DANGER: Color = Color::Red;
const COLOR_OK: Color = Color::Green;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Bool,
    Enum(&'static [&'static str]),
    Integer { min: u64 },
    String,
    List,
    Map,
    ReadOnly,
}

#[derive(Debug, Clone, Copy)]
struct FieldMeta {
    section: &'static str,
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
    description: &'static str,
    secret: bool,
}

const FIELDS: &[FieldMeta] = &[
    FieldMeta {
        section: "system",
        key: "meta.version",
        label: "Config version",
        kind: FieldKind::ReadOnly,
        description: "Config schema version. This is stamped by the binary and cannot be edited.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.enabled",
        label: "Notifications",
        kind: FieldKind::Bool,
        description: "Enables notification delivery when a topic is configured.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.server",
        label: "Server URL",
        kind: FieldKind::String,
        description: "Base ntfy server URL. Must start with http:// or https://.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.topic",
        label: "Topic",
        kind: FieldKind::String,
        description: "Notification topic. Empty disables delivery; r reveals the full value and q regenerates it.",
        secret: true,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.detail_mode",
        label: "Message detail",
        kind: FieldKind::Enum(NtfyDetailMode::variants()),
        description: "Controls notification body length. Allowed: detailed, minimal. Default: detailed.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.retry_attempts",
        label: "Retry attempts",
        kind: FieldKind::Integer { min: 1 },
        description: "Number of notification send attempts. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.retry_delay_ms",
        label: "Retry delay",
        kind: FieldKind::Integer { min: 0 },
        description: "Delay between notification retries in milliseconds.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.http_timeout_secs",
        label: "HTTP timeout",
        kind: FieldKind::Integer { min: 1 },
        description: "HTTP timeout for ntfy requests. Minimum: 1 second.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.body_max_bytes",
        label: "Body limit",
        kind: FieldKind::Integer { min: 256 },
        description: "Maximum notification body size in bytes. Minimum: 256.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.excerpt_max_chars",
        label: "Excerpt limit",
        kind: FieldKind::Integer { min: 32 },
        description: "Maximum excerpt characters used in notifications. Minimum: 32.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "ntfy.created_at",
        label: "Notification config created",
        kind: FieldKind::ReadOnly,
        description: "Metadata timestamp maintained by config mutation paths.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "ntfy.updated_at",
        label: "Notification config updated",
        kind: FieldKind::ReadOnly,
        description: "Metadata timestamp maintained by config mutation paths.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.events.phase_wait",
        label: "Phase wait alerts",
        kind: FieldKind::Bool,
        description: "Notify when a phase is waiting.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.events.interactive_wait",
        label: "Input wait alerts",
        kind: FieldKind::Bool,
        description: "Notify when an interactive run is waiting for input.",
        secret: false,
    },
    FieldMeta {
        section: "notifications",
        key: "ntfy.events.pipeline_done",
        label: "Done alerts",
        kind: FieldKind::Bool,
        description: "Notify when the pipeline finishes.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.shell_policy",
        label: "Shell access",
        kind: FieldKind::Enum(ShellPolicy::variants()),
        description: "Default ACP shell policy. Allowed: full-access, allowlist.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.shell_allowlist",
        label: "Allowed shell commands",
        kind: FieldKind::List,
        description: "Read-only list in this panel; use the CLI for item-level edits.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.enforce_readonly_workspace",
        label: "Read-only workspaces",
        kind: FieldKind::Bool,
        description: "Force ACP runs to treat the workspace as read-only unless write paths are allowed.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.policy.allowed_write_paths",
        label: "Writable paths",
        kind: FieldKind::List,
        description: "Read-only list in this panel; use the CLI for item-level edits.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.install.claude_acp_root",
        label: "Claude ACP install root",
        kind: FieldKind::String,
        description: "Claude ACP installation root; $HOME and ~/ are expanded by the loader.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.install.prefer_local_claude_acp",
        label: "Prefer local Claude ACP",
        kind: FieldKind::Bool,
        description: "Use the locally installed Claude ACP server before falling back to the global command.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "runner.full_review_interval",
        label: "Review cadence",
        kind: FieldKind::Integer { min: 1 },
        description: "Run a full alignment review every N review rounds. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.cache_root",
        label: "Cache folder",
        kind: FieldKind::String,
        description: "Root for cache files. $HOME and ~/ are expanded at load.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.sessions_root",
        label: "Sessions folder",
        kind: FieldKind::String,
        description: "Root for session artifacts. $HOME and ~/ are expanded at load.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.runs_root",
        label: "Runs folder",
        kind: FieldKind::String,
        description: "Reserved top-level run root; no current subsystem consumes it directly.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "paths.memory_root",
        label: "Memory folder",
        kind: FieldKind::String,
        description: "Root for project memory files. $HOME and ~/ are expanded at load.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "ui.prefer_split_on_open",
        label: "Open runs in split view",
        kind: FieldKind::Bool,
        description: "Prefer the split transcript when opening run output.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "ui.colon_palette.show_help",
        label: "Palette help row",
        kind: FieldKind::Bool,
        description: "Show the command palette help row.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "ui.footer.show_keys",
        label: "Footer key hints",
        kind: FieldKind::Bool,
        description: "Show footer key hints.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "diagnostics.log_level",
        label: "Log level",
        kind: FieldKind::Enum(LogLevel::variants()),
        description: "Default log level. RUST_LOG still takes precedence.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "diagnostics.json_logs",
        label: "JSON logs",
        kind: FieldKind::Bool,
        description: "Emit JSON logs unless the environment overrides diagnostics.",
        secret: false,
    },
    FieldMeta {
        section: "general",
        key: "memory.enabled",
        label: "Project memory",
        kind: FieldKind::Bool,
        description: "Enable project memory prompt context.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "memory.max_topics_per_read",
        label: "Memory topics per prompt",
        kind: FieldKind::Integer { min: 1 },
        description: "Maximum memory topics read into one prompt. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "system",
        key: "memory.journal_retention_months",
        label: "Journal retention",
        kind: FieldKind::Integer { min: 1 },
        description: "Monthly memory journals older than this are pruned at launch. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "agents",
        key: "acp.agents.claude.env",
        label: "Claude environment",
        kind: FieldKind::Map,
        description: "Read-only map in this panel; use dotted CLI keys for env entries.",
        secret: false,
    },
];

const SECTIONS: &[&str] = &["general", "models", "notifications", "agents", "system"];
const SECTION_ALIASES: &[(&str, &str)] = &[
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

/// True when the section name owns the providers sub-panel rendering path
/// (a list-of-entries layout) rather than the default key/value field grid.
fn is_providers_section(section: &str) -> bool {
    section == "models"
}

fn section_title(section: &str) -> &'static str {
    match section {
        "general" => "Common",
        "models" => "Models",
        "notifications" => "Notifications",
        "agents" => "Agents",
        "system" => "System",
        _ => "Settings",
    }
}

fn resolve_section_name(name: &str) -> Option<&'static str> {
    let normalized = name.trim().to_ascii_lowercase();
    SECTION_ALIASES
        .iter()
        .find_map(|(alias, section)| (*alias == normalized).then_some(*section))
}

fn section_prefix_matches(prefix: &str) -> Vec<&'static str> {
    let normalized = prefix.trim().to_ascii_lowercase();
    let mut matches = Vec::new();
    for (alias, section) in SECTION_ALIASES {
        if alias.starts_with(&normalized) && !matches.contains(section) {
            matches.push(*section);
        }
    }
    matches
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchState {
    pub(crate) query: String,
    /// Field indices that match `query`. Recomputed on every keystroke
    /// so the result list and `selected` cursor stay in sync.
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
    /// Detail drawer for the provider currently under `providers_cursor`.
    /// `cursor` walks the toggleable property list (see [`PROVIDER_TOGGLES`]).
    ProviderDetail {
        cursor: usize,
    },
}

#[derive(Debug, Clone, Copy)]
enum ToggleField {
    Enabled,
    Official,
    Free,
    QuotaDisabled,
    Cheap,
    Tough,
    Effort,
}

#[derive(Debug, Clone, Copy)]
struct ProviderToggle {
    label: &'static str,
    /// Short helper line shown in the drawer when this row is focused.
    description: &'static str,
    field: ToggleField,
    /// True for fields whose value is owned by the baked entry; the drawer
    /// shows the live value but disallows toggling for built-in providers.
    baked_locked: bool,
}

const PROVIDER_TOGGLES: &[ProviderToggle] = &[
    ProviderToggle {
        label: "Enabled",
        description: "Whether this entry is offered when picking a model.",
        field: ToggleField::Enabled,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Official",
        description: "Marks the provider as the vendor's official endpoint.",
        field: ToggleField::Official,
        baked_locked: true,
    },
    ProviderToggle {
        label: "Free",
        description: "Marks usage as no-cost (display label only; does not affect billing).",
        field: ToggleField::Free,
        baked_locked: true,
    },
    ProviderToggle {
        label: "Ignore quota",
        description: "Skip quota accounting when scheduling this entry.",
        field: ToggleField::QuotaDisabled,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Cheap eligible",
        description: "Eligible for the [CHEAP] mode model rotation.",
        field: ToggleField::Cheap,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Tough eligible",
        description: "Eligible for harder reasoning loops.",
        field: ToggleField::Tough,
        baked_locked: false,
    },
    ProviderToggle {
        label: "Effort eligible",
        description: "Eligible for fixed-effort modes.",
        field: ToggleField::Effort,
        baked_locked: false,
    },
];

/// First toggle index that is editable for the provider type. Built-in entries
/// pin official/free, so the cursor lands on `Enabled` rather than a row the
/// user can't actually flip.
fn first_toggle_index(_is_baked: bool) -> usize {
    0
}

fn step_toggle(current: usize, is_baked: bool, delta: isize) -> usize {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConflictBanner {
    MtimeAdvanced,
    DiscardPrompt,
    RegenerateTopicPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelOutcome {
    KeepOpen,
    Close,
    Saved,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigPanelState {
    pub(crate) config: Config,
    pub(crate) path: PathBuf,
    opened_mtime: Option<SystemTime>,
    selected_section: usize,
    selected_field: usize,
    status: String,
    pub(crate) editing: Option<Editing>,
    edit_buffer: String,
    reveal_topic: bool,
    conflict: Option<ConflictBanner>,
    pub(crate) dirty: bool,
    save_error: Option<String>,
    pub(crate) read_only: bool,
    pub(crate) searching: Option<SearchState>,
    pub(crate) providers_cursor: usize,
    /// Top-of-viewport scroll offset for the providers list, kept in a Cell
    /// so the immutable render path can clamp it after seeing `body_h` —
    /// the cursor stays in view even as the user pages through dozens of
    /// rows. Reset to 0 on section switches and on cursor jumps.
    pub(crate) providers_scroll: Cell<usize>,
    /// Last observed body height for the providers section. Set by the
    /// render path so half-page key shortcuts (Ctrl-D / Ctrl-U / PgDown /
    /// PgUp) can move by a real viewport rather than guessing.
    pub(crate) providers_body_h: Cell<usize>,
}

impl ConfigPanelState {
    /// Pre-positions the panel on `initial_section` if the name resolves;
    /// otherwise falls back to the default section. Used by `:config`,
    /// `:config <section>`, and the App's last-viewed-section memory.
    pub(crate) fn open_at(
        config: &Config,
        path: PathBuf,
        read_only: bool,
        initial_section: Option<&str>,
    ) -> Self {
        let opened_mtime = mtime(&path);
        let selected_section = initial_section
            .and_then(resolve_section_name)
            .and_then(|name| SECTIONS.iter().position(|s| *s == name))
            .unwrap_or(0);
        let mut state = Self {
            config: config.clone(),
            path,
            opened_mtime,
            selected_section,
            selected_field: 1,
            status: if read_only {
                "read-only mode · press e to edit".to_string()
            } else {
                "config open".to_string()
            },
            editing: None,
            edit_buffer: String::new(),
            reveal_topic: false,
            conflict: None,
            dirty: false,
            save_error: None,
            read_only,
            searching: None,
            providers_cursor: 0,
            providers_scroll: Cell::new(0),
            providers_body_h: Cell::new(0),
        };
        state.select_first_field_in_current_section();
        state
    }

    pub(crate) fn current_section_name(&self) -> &'static str {
        self.current_section()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        if self.searching.is_some() {
            return self.handle_search_key(key);
        }
        if !self.read_only
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('s')
        {
            self.save(false);
            // Save is only successful when the inline-edit buffer (if any)
            // committed cleanly, the file write hit no IO error, and no
            // mtime conflict was detected. Anything else keeps the panel
            // open so the operator sees the diagnostic.
            let saved =
                self.editing.is_none() && self.conflict.is_none() && self.save_error.is_none();
            return if saved {
                PanelOutcome::Saved
            } else {
                PanelOutcome::KeepOpen
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return PanelOutcome::Close;
        }
        if let Some(outcome) = self.handle_banner_key(key) {
            return outcome;
        }
        if self.editing.is_some() {
            if self.read_only {
                // Defensive: `Enter` is gated so editing should not be set in
                // read-only mode through the normal flow. If a caller forces
                // `editing = Some(...)`, refuse to mutate; only Esc unwinds.
                if matches!(key.code, KeyCode::Esc) {
                    self.editing = None;
                }
                return PanelOutcome::KeepOpen;
            }
            self.handle_edit_key(key);
            return PanelOutcome::KeepOpen;
        }
        match key.code {
            KeyCode::Esc => {
                if self.dirty {
                    self.conflict = Some(ConflictBanner::DiscardPrompt);
                    self.status = "discard unsaved changes? y/n".to_string();
                    PanelOutcome::KeepOpen
                } else {
                    PanelOutcome::Close
                }
            }
            KeyCode::Char('e') if self.read_only => {
                self.read_only = false;
                self.status = "edit mode enabled".to_string();
                PanelOutcome::KeepOpen
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_field(-1);
                PanelOutcome::KeepOpen
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_field(1);
                PanelOutcome::KeepOpen
            }
            // Horizontal arrows are no-ops in navigation mode. Models page now
            // uses a detail drawer instead of per-property h/l cursors.
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l') => {
                PanelOutcome::KeepOpen
            }
            KeyCode::Enter => {
                if !self.read_only {
                    if is_providers_section(self.current_section()) {
                        self.activate_provider_line();
                    } else {
                        self.activate_field();
                    }
                }
                PanelOutcome::KeepOpen
            }
            // Space is a "fast toggle" everywhere it makes sense:
            //  · Bool fields: flip without opening a dropdown.
            //  · Provider rows: open the detail drawer (same as Enter).
            KeyCode::Char(' ') => {
                if !self.read_only {
                    if is_providers_section(self.current_section()) {
                        self.activate_provider_line();
                    } else if let Some(meta) = self.current_meta().copied()
                        && matches!(meta.kind, FieldKind::Bool)
                    {
                        self.flip_bool(&meta);
                    }
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('a') if is_providers_section(self.current_section()) => {
                if !self.read_only {
                    self.open_add_provider_editor();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('x') if is_providers_section(self.current_section()) => {
                if !self.read_only {
                    self.remove_selected_provider();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if is_providers_section(self.current_section()) {
                    self.move_providers_page(1);
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if is_providers_section(self.current_section()) {
                    self.move_providers_page(-1);
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('d') => {
                if !self.read_only {
                    self.reset_field();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('r') if self.current_meta().is_some_and(|m| m.key == "ntfy.topic") => {
                self.reveal_topic = !self.reveal_topic;
                self.status = if self.reveal_topic {
                    "topic revealed".to_string()
                } else {
                    "topic hidden".to_string()
                };
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('q') if self.current_meta().is_some_and(|m| m.key == "ntfy.topic") => {
                if !self.read_only {
                    self.conflict = Some(ConflictBanner::RegenerateTopicPrompt);
                    self.status = "regenerate topic? y/n".to_string();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Tab => {
                self.move_section(1);
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_section(1);
                PanelOutcome::KeepOpen
            }
            KeyCode::BackTab => {
                self.move_section(-1);
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('[') => {
                self.selected_section = 0;
                self.select_first_field_in_current_section();
                PanelOutcome::KeepOpen
            }
            KeyCode::Char(']') => {
                self.selected_section = SECTIONS.len().saturating_sub(1);
                self.select_first_field_in_current_section();
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('g') => {
                if is_providers_section(self.current_section()) {
                    self.providers_cursor = 0;
                    self.providers_scroll.set(0);
                    // Skip the leading group header onto the first provider.
                    self.move_field(1);
                    self.move_field(-1);
                } else {
                    self.select_first_field_in_current_section();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('G') => {
                if is_providers_section(self.current_section()) {
                    let len = providers::get_lines(&self.config).len();
                    self.providers_cursor = len.saturating_sub(1);
                } else if let Some(last) = field_indices_for(self.current_section()).last() {
                    self.selected_field = *last;
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::PageDown => {
                if is_providers_section(self.current_section()) {
                    self.move_providers_page(1);
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::PageUp => {
                if is_providers_section(self.current_section()) {
                    self.move_providers_page(-1);
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('/') => {
                self.open_search();
                PanelOutcome::KeepOpen
            }
            _ => PanelOutcome::KeepOpen,
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

    fn handle_search_key(&mut self, key: KeyEvent) -> PanelOutcome {
        let Some(mut search) = self.searching.take() else {
            return PanelOutcome::KeepOpen;
        };
        match key.code {
            KeyCode::Esc => {
                self.status = "search cancelled".to_string();
                return PanelOutcome::KeepOpen;
            }
            KeyCode::Enter => {
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
                return PanelOutcome::KeepOpen;
            }
            KeyCode::Up if !search.results.is_empty() => {
                search.selected = wrap_index(search.selected, search.results.len(), -1);
            }
            KeyCode::Down if !search.results.is_empty() => {
                search.selected = wrap_index(search.selected, search.results.len(), 1);
            }
            KeyCode::Backspace => {
                search.query.pop();
                Self::recompute_search_results(&mut search);
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                search.query.push(c);
                Self::recompute_search_results(&mut search);
            }
            _ => {}
        }
        self.searching = Some(search);
        PanelOutcome::KeepOpen
    }

    fn handle_banner_key(&mut self, key: KeyEvent) -> Option<PanelOutcome> {
        let banner = self.conflict.clone()?;
        match banner {
            ConflictBanner::MtimeAdvanced => match key.code {
                KeyCode::Char('r') => {
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
                KeyCode::Char('o') => {
                    self.conflict = None;
                    self.save(true);
                    Some(PanelOutcome::KeepOpen)
                }
                KeyCode::Esc => {
                    self.conflict = None;
                    self.status = "kept editing".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                _ => Some(PanelOutcome::KeepOpen),
            },
            ConflictBanner::DiscardPrompt => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.dirty = false;
                    self.conflict = None;
                    Some(PanelOutcome::Close)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.conflict = None;
                    self.status = "kept editing".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                _ => Some(PanelOutcome::KeepOpen),
            },
            ConflictBanner::RegenerateTopicPrompt => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
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
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.conflict = None;
                    self.status = "unchanged".to_string();
                    Some(PanelOutcome::KeepOpen)
                }
                _ => Some(PanelOutcome::KeepOpen),
            },
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent) {
        if let Some(Editing::AddProvider(_)) = self.editing {
            self.handle_add_provider_key(key);
            return;
        }
        if matches!(self.editing, Some(Editing::ProviderDetail { .. })) {
            self.handle_provider_detail_key(key);
            return;
        }
        if matches!(self.editing, Some(Editing::Choice { .. })) {
            self.handle_choice_key(key);
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.status = "edit cancelled".to_string();
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.accept_edit();
            }
            KeyCode::Backspace => {
                self.edit_buffer.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if matches!(self.editing, Some(Editing::Integer)) {
                    if c.is_ascii_digit() {
                        self.edit_buffer.push(c);
                    }
                } else {
                    self.edit_buffer.push(c);
                }
            }
            KeyCode::Up if matches!(self.editing, Some(Editing::Integer)) => {
                let delta = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    10
                } else {
                    1
                };
                self.nudge_integer(delta);
            }
            KeyCode::Down if matches!(self.editing, Some(Editing::Integer)) => {
                let delta = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    -10
                } else {
                    -1
                };
                self.nudge_integer(delta);
            }
            _ => {}
        }
    }

    fn handle_add_provider_key(&mut self, key: KeyEvent) {
        let Some(Editing::AddProvider(ref mut editor)) = self.editing else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.status = "add cancelled".to_string();
            }
            KeyCode::Enter => {
                if editor.commit(&mut self.config) {
                    self.dirty = true;
                    self.status = "provider added".to_string();
                    self.editing = None;
                } else {
                    self.status = "invalid provider data (duplicate or empty fields)".to_string();
                }
            }
            KeyCode::Tab => {
                editor.focus = editor.focus.next();
            }
            KeyCode::BackTab => {
                editor.focus = editor.focus.prev();
            }
            KeyCode::Up => editor.cycle_focused(-1),
            KeyCode::Down => editor.cycle_focused(1),
            KeyCode::Backspace => {
                if matches!(editor.focus, providers::AddProviderField::LaunchName) {
                    editor.launch_name.pop();
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if matches!(editor.focus, providers::AddProviderField::LaunchName) {
                    editor.launch_name.push(c);
                }
            }
            _ => {}
        }
    }

    fn handle_choice_key(&mut self, key: KeyEvent) {
        let Some(Editing::Choice {
            key: field_key,
            options,
            selected,
        }) = self.editing.clone()
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.status = "edit cancelled".to_string();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let next = wrap_index(selected, options.len(), -1);
                self.set_choice_selected(next);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = wrap_index(selected, options.len(), 1);
                self.set_choice_selected(next);
            }
            KeyCode::Enter => {
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
        let lines = providers::get_lines(&self.config);
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

    fn clamp_provider_cursor(&mut self) {
        let len = providers::get_lines(&self.config).len();
        if len == 0 {
            self.providers_cursor = 0;
        } else {
            self.providers_cursor = self.providers_cursor.min(len - 1);
        }
    }

    fn activate_provider_line(&mut self) {
        let lines = providers::get_lines(&self.config);
        let Some(line) = lines.get(self.providers_cursor) else {
            return;
        };

        match line {
            providers::ProvidersLine::GroupHeader { .. } => {}
            providers::ProvidersLine::Provider { entry, is_baked, .. } => {
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

    /// Toggle one property on the provider currently under
    /// `providers_cursor` and refresh status text.
    fn toggle_provider_property(&mut self, idx: usize) {
        let lines = providers::get_lines(&self.config);
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

    fn handle_provider_detail_key(&mut self, key: KeyEvent) {
        let Some(Editing::ProviderDetail { cursor }) = self.editing else {
            return;
        };
        let lines = providers::get_lines(&self.config);
        let is_baked = matches!(
            lines.get(self.providers_cursor),
            Some(providers::ProvidersLine::Provider { is_baked: true, .. })
        );
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.status = "closed details".to_string();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let next = step_toggle(cursor, is_baked, -1);
                self.editing = Some(Editing::ProviderDetail { cursor: next });
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let next = step_toggle(cursor, is_baked, 1);
                self.editing = Some(Editing::ProviderDetail { cursor: next });
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.toggle_provider_property(cursor);
            }
            KeyCode::Char('x') => {
                self.editing = None;
                self.remove_selected_provider();
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
            // Bools have only two states — flipping inline beats popping a
            // two-row dropdown and keeps the keymap consistent with Space.
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
            FieldKind::List | FieldKind::Map => {
                self.status = "read-only".to_string();
            }
            FieldKind::ReadOnly => {
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

    fn current_section(&self) -> &'static str {
        SECTIONS
            .get(self.selected_section)
            .copied()
            .unwrap_or(SECTIONS[0])
    }

    fn current_meta(&self) -> Option<&'static FieldMeta> {
        FIELDS.get(self.selected_field)
    }

    fn move_section(&mut self, delta: isize) {
        self.selected_section = wrap_index(self.selected_section, SECTIONS.len(), delta);
        self.providers_scroll.set(0);
        self.select_first_field_in_current_section();
    }

    fn move_field(&mut self, delta: isize) {
        let section = self.current_section();
        if is_providers_section(section) {
            let lines = providers::get_lines(&self.config);
            if !lines.is_empty() {
                // Skip group headers so j/k always lands on something the
                // user can act on.
                let mut next = wrap_index(self.providers_cursor, lines.len(), delta);
                for _ in 0..lines.len() {
                    if !matches!(lines.get(next), Some(providers::ProvidersLine::GroupHeader { .. }))
                    {
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

    /// Move the providers cursor by roughly half a viewport, snapping past
    /// any group headers so the cursor lands on actionable content. Used by
    /// PgUp/PgDown and Ctrl-U/Ctrl-D.
    fn move_providers_page(&mut self, delta: isize) {
        let lines = providers::get_lines(&self.config);
        if lines.is_empty() {
            return;
        }
        let body_h = self.providers_body_h.get().max(1);
        let step = ((body_h as isize) / 2).max(1) * delta.signum();
        // Step in unit increments so the group-header skip from move_field
        // stays applied. Falls through naturally when total < body.
        for _ in 0..step.unsigned_abs() {
            let before = self.providers_cursor;
            self.move_field(delta.signum());
            if self.providers_cursor == before {
                break;
            }
        }
    }

    fn select_first_field_in_current_section(&mut self) {
        if let Some(idx) = field_indices_for(self.current_section()).first() {
            self.selected_field = *idx;
        }
    }

    fn section_override_count(&self, section: &str) -> usize {
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

    fn value_for(&self, meta: &FieldMeta) -> String {
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
            "ntfy.events.phase_wait" => value_bool(&self.config.ntfy.events.phase_wait),
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

    fn source_for(&self, meta: &FieldMeta) -> &'static str {
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
            "ntfy.events.phase_wait" => source_override(&self.config.ntfy.events.phase_wait),
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

    fn current_validation_error(&self) -> Option<String> {
        if let Some(meta) = self.current_meta()
            && let Some(reason) = self.edit_error_for(*meta, &self.edit_buffer)
        {
            return Some(format!("cannot save: {reason}"));
        }
        None
    }

    fn edit_error_for(&self, meta: FieldMeta, value: &str) -> Option<String> {
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

pub(crate) fn terminal_too_narrow_message() -> &'static str {
    "terminal too narrow (need ≥50 cols)"
}

/// Result of matching a positional `:config <section>` argument against the
/// known section names. Exact match wins so `:config ntfy` doesn't trip
/// on the longer `ntfy.events*` siblings.
pub(crate) enum SectionLookup {
    Exact(&'static str),
    UniquePrefix(&'static str),
    Ambiguous(Vec<&'static str>),
    Unknown,
}

pub(crate) fn lookup_section(arg: &str) -> SectionLookup {
    let needle = arg.trim();
    if needle.is_empty() {
        return SectionLookup::Unknown;
    }
    if let Some(name) = resolve_section_name(needle) {
        return SectionLookup::Exact(name);
    }
    let matches = section_prefix_matches(needle);
    match matches.len() {
        0 => SectionLookup::Unknown,
        1 => SectionLookup::UniquePrefix(matches[0]),
        _ => SectionLookup::Ambiguous(matches),
    }
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
}

pub(crate) fn can_open(width: u16) -> bool {
    width >= MIN_WIDTH
}

pub(crate) fn render(frame: &mut Frame<'_>, area: Rect, state: &ConfigPanelState) {
    frame.render_widget(ConfigPanelWidget { state }, area);
}

struct ConfigPanelWidget<'a> {
    state: &'a ConfigPanelState,
}

impl Widget for ConfigPanelWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        if area.width < MIN_WIDTH {
            Paragraph::new(terminal_too_narrow_message()).render(area, buf);
            return;
        }
        let mut lines = adaptive_lines(self.state, area.width, area.height);
        lines.truncate(area.height as usize);
        while lines.len() < area.height as usize {
            lines.push(Line::from(""));
        }
        Paragraph::new(lines).render(area, buf);
        render_dropdown(self.state, area, buf);
        render_search_overlay(self.state, area, buf);
        render_add_provider_overlay(self.state, area, buf);
        render_provider_detail_overlay(self.state, area, buf);
    }
}

// --- helpers shared by all renderers ----------------------------------------

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(COLOR_DIM))
}

fn focus_span(focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            "▌".to_string(),
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    }
}

fn override_dot(is_override: bool) -> Span<'static> {
    if is_override {
        Span::styled("●".to_string(), Style::default().fg(COLOR_OVERRIDE))
    } else {
        Span::raw(" ")
    }
}

fn pad_right(text: &str, width: usize) -> String {
    let used = text.width();
    if used >= width {
        ellipsize_end(text, width)
    } else {
        format!("{text}{}", " ".repeat(width - used))
    }
}

fn render_add_provider_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(Editing::AddProvider(editor)) = state.editing.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }

    let overlay_w = area.width.saturating_sub(10).max(54).min(area.width);
    let overlay_h: u16 = 12;
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let label_w: usize = 12;
    let value_for = |field: providers::AddProviderField| -> String {
        match field {
            providers::AddProviderField::Model => editor.model.clone(),
            providers::AddProviderField::Subscription => editor.subscription.clone(),
            providers::AddProviderField::Cli => editor.cli.as_str().to_string(),
            providers::AddProviderField::LaunchName => format!("{}_", editor.launch_name),
        }
    };
    let render_row = |field: providers::AddProviderField, label: &str| -> Line<'static> {
        let focused = editor.focus == field;
        let value_style = if focused {
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans: Vec<Span<'static>> = vec![
            Span::raw(" "),
            focus_span(focused),
            Span::raw(" "),
            Span::styled(pad_right(label, label_w), Style::default().fg(COLOR_DIM)),
            Span::styled(value_for(field), value_style),
        ];
        if focused {
            let hint = match field {
                providers::AddProviderField::LaunchName => "  type · ↑↓ presets",
                providers::AddProviderField::Model => "  ↑↓ cycles known models",
                providers::AddProviderField::Subscription => "  ↑↓ cycles subscriptions",
                providers::AddProviderField::Cli => "  ↑↓ cycles CLIs",
            };
            spans.push(Span::styled(
                hint.to_string(),
                Style::default().fg(COLOR_DIM),
            ));
        }
        Line::from(spans)
    };

    let lines: Vec<Line<'static>> = vec![
        Line::from(""),
        render_row(providers::AddProviderField::Model, "Model"),
        render_row(providers::AddProviderField::Subscription, "Subscription"),
        render_row(providers::AddProviderField::Cli, "CLI"),
        render_row(providers::AddProviderField::LaunchName, "Launch name"),
        Line::from(""),
        Line::from(dim("  Tab cycles fields · Enter confirms · Esc cancels")),
    ];

    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS))
                .title(Span::styled(
                    " Add provider ".to_string(),
                    Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);
}

fn render_provider_detail_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(Editing::ProviderDetail { cursor }) = state.editing.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }
    let cursor = *cursor;
    let lines = providers::get_lines(&state.config);
    let Some(providers::ProvidersLine::Provider {
        entry,
        is_baked,
        baked_free,
        baked_official,
    }) = lines.get(state.providers_cursor)
    else {
        return;
    };

    let subscription_label =
        crate::logic::selection::subscription::subscription_kind_to_str(entry.subscription)
            .to_string();
    let title = format!(
        " {} · {} ",
        subscription_label, entry.model,
    );
    let source = if *is_baked { "built-in" } else { "custom" };

    let row_count = PROVIDER_TOGGLES.len();
    let overlay_h = (row_count as u16) + 6; // border + title row + chrome
    let overlay_h = overlay_h.min(area.height);
    let overlay_w = area
        .width
        .saturating_sub(10)
        .max(56)
        .min(area.width);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_h)) / 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let mut body: Vec<Line<'static>> = Vec::new();
    body.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{} · {}", entry.cli.as_str(), entry.launch_name),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("({source})"), Style::default().fg(COLOR_DIM)),
    ]));
    body.push(Line::from(""));

    for (idx, toggle) in PROVIDER_TOGGLES.iter().enumerate() {
        let on = current_toggle_value(entry, *is_baked, *baked_free, *baked_official, toggle);
        let focused = idx == cursor;
        let locked = *is_baked && toggle.baked_locked;
        let check_glyph = if on { "[x]" } else { "[ ]" };
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw(" "));
        spans.push(focus_span(focused));
        spans.push(Span::raw(" "));
        let check_style = if locked {
            Style::default().fg(COLOR_DIM)
        } else if on {
            Style::default().fg(COLOR_OK).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_DIM)
        };
        spans.push(Span::styled(check_glyph.to_string(), check_style));
        spans.push(Span::raw(" "));
        let label_style = if focused {
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
        } else if locked {
            Style::default().fg(COLOR_DIM)
        } else {
            Style::default()
        };
        spans.push(Span::styled(toggle.label.to_string(), label_style));
        if locked {
            spans.push(Span::styled(
                "  built-in".to_string(),
                Style::default().fg(COLOR_DIM),
            ));
        }
        body.push(Line::from(spans));
    }

    body.push(Line::from(""));
    let active_desc = PROVIDER_TOGGLES
        .get(cursor)
        .map(|t| t.description)
        .unwrap_or_default();
    body.push(Line::from(dim(format!(" {active_desc}"))));
    body.push(Line::from(dim(
        " Space toggle · ↑↓ navigate · x delete · Esc close",
    )));

    Paragraph::new(body)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS))
                .title(Span::styled(
                    title,
                    Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);
}

fn current_toggle_value(
    entry: &crate::data::config::schema::ProviderEntry,
    is_baked: bool,
    baked_free: bool,
    baked_official: bool,
    toggle: &ProviderToggle,
) -> bool {
    match toggle.field {
        ToggleField::Enabled => entry.enabled,
        ToggleField::Official => {
            if is_baked {
                baked_official
            } else {
                entry.official
            }
        }
        ToggleField::Free => {
            if is_baked {
                baked_free
            } else {
                entry.free
            }
        }
        ToggleField::QuotaDisabled => entry.quota_disabled,
        ToggleField::Cheap => entry.cheap_eligible,
        ToggleField::Tough => entry.tough_eligible,
        ToggleField::Effort => entry.effort_eligible,
    }
}

fn render_search_overlay(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(search) = state.searching.as_ref() else {
        return;
    };
    if area.width < MIN_WIDTH {
        return;
    }

    let overlay_w = area.width.saturating_sub(4).max(30).min(area.width);
    let max_results: u16 = 8;
    let overlay_h = (search.results.len() as u16).min(max_results) + 4;
    let overlay_h = overlay_h.min(area.height);
    if overlay_h < 5 {
        return;
    }
    let overlay_x = area.x + (area.width.saturating_sub(overlay_w)) / 2;
    let overlay_y = area.y + 2;
    let rect = Rect::new(overlay_x, overlay_y, overlay_w, overlay_h);
    Clear.render(rect, buf);

    let inner_w = overlay_w.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(
            "/ ".to_string(),
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{}_", search.query)),
    ]));
    lines.push(Line::from(Span::styled(
        "─".repeat(inner_w),
        Style::default().fg(COLOR_DIM),
    )));

    if search.results.is_empty() {
        lines.push(Line::from(dim("  no fields match")));
    } else {
        let max_rows = (overlay_h.saturating_sub(4)) as usize;
        let total = search.results.len();
        let win_start = search
            .selected
            .saturating_sub(max_rows.saturating_sub(1))
            .min(total.saturating_sub(max_rows.min(total)));
        for (offset, field_idx) in search
            .results
            .iter()
            .enumerate()
            .skip(win_start)
            .take(max_rows)
        {
            let meta = FIELDS[*field_idx];
            let focused = offset == search.selected;
            lines.push(Line::from(vec![
                focus_span(focused),
                Span::raw(" "),
                Span::styled(
                    meta.label.to_string(),
                    if focused {
                        Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled("  ".to_string(), Style::default().fg(COLOR_DIM)),
                Span::styled(meta.key.to_string(), Style::default().fg(COLOR_DIM)),
            ]));
        }
    }
    lines.push(Line::from(dim("  ↑↓ select · Enter jump · Esc cancel")));

    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS))
                .title(Span::styled(
                    " Search ".to_string(),
                    Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
                )),
        )
        .render(rect, buf);
}

fn render_dropdown(state: &ConfigPanelState, area: Rect, buf: &mut Buffer) {
    let Some(Editing::Choice {
        options, selected, ..
    }) = state.editing.as_ref()
    else {
        return;
    };
    if options.is_empty() {
        return;
    }
    let w = area.width as usize;
    let tab_lines = tab_bar_lines(state, w).len() as u16;
    // Body region: header(1) + tab_lines + separator(1) starts the rows;
    // bottom 3 rows are reserved for separator + help + footer.
    let body_y_start = area.y.saturating_add(2 + tab_lines);
    let body_y_end = area.y.saturating_add(area.height).saturating_sub(3);
    if body_y_end <= body_y_start {
        return;
    }

    let visible = visible_fields(state);
    let pos = visible
        .iter()
        .position(|i| *i == state.selected_field)
        .unwrap_or(0);
    let row_y = body_y_start.saturating_add(pos as u16);
    if row_y >= body_y_end {
        return;
    }

    let max_opt_w = options.iter().map(|o| o.width()).max().unwrap_or(1);
    let popup_w = ((max_opt_w + 4) as u16).max(10).min(area.width);
    let popup_h_wanted = options.len() as u16 + 2; // top/bottom border
    let avail_h = body_y_end.saturating_sub(body_y_start);
    let popup_h = popup_h_wanted.min(avail_h);
    if popup_h < 3 {
        return;
    }

    let name_w = w.min(30) as u16;
    let mut popup_x = area.x.saturating_add(name_w + 3);
    let area_right = area.x.saturating_add(area.width);
    if popup_x + popup_w > area_right {
        popup_x = area_right.saturating_sub(popup_w);
    }
    if popup_x < area.x {
        popup_x = area.x;
    }

    // Anchor below the focused row; if there is not enough room, place it
    // above. Falling back to the bottom of the body keeps the popup
    // fully visible when nothing else fits.
    let mut popup_y = row_y.saturating_add(1);
    if popup_y + popup_h > body_y_end {
        let above_room = row_y.saturating_sub(body_y_start);
        if above_room >= popup_h {
            popup_y = row_y.saturating_sub(popup_h);
        } else {
            popup_y = body_y_end.saturating_sub(popup_h);
        }
    }

    let popup_rect = Rect::new(popup_x, popup_y, popup_w, popup_h);
    Clear.render(popup_rect, buf);

    let lines: Vec<Line<'static>> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let focused = i == *selected;
            Line::from(vec![
                focus_span(focused),
                Span::raw(" "),
                Span::styled(
                    opt.clone(),
                    if focused {
                        Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
            ])
        })
        .collect();

    Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(COLOR_FOCUS)),
        )
        .render(popup_rect, buf);
}

fn adaptive_lines(state: &ConfigPanelState, width: u16, height: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    let mut lines = Vec::new();
    lines.push(header_line(&state.path, width));
    let tab_lines = tab_bar_lines(state, w);
    lines.extend(tab_lines);
    lines.push(bottom_rule(width, None));
    let used = lines.len();
    // Reserve 3 lines for the trailing separator + help + footer so the
    // footer is never sacrificed when the tab bar wraps onto a third line
    // (sub-panels with longer section names hit this case at width 80).
    let body_h = height.saturating_sub(used as u16 + 3) as usize;
    if is_providers_section(state.current_section()) {
        for body_line in providers_body_lines(state, w, body_h).into_iter().take(body_h) {
            lines.push(body_line);
        }
    } else {
        let fields = visible_fields(state);
        for idx in fields.into_iter().take(body_h) {
            lines.push(field_row(state, idx, w));
        }
    }
    lines.push(bottom_rule(width, None));
    lines.push(help_text(state, w));
    lines.push(footer_line(state, w));
    lines
}

fn tab_bar_lines(state: &ConfigPanelState, width: usize) -> Vec<Line<'static>> {
    // Greedy line-wrap of the tab segments (each tab contributing a small
    // group of styled spans). When a row overflows, flush it and start the
    // next one with the current segment; matches the old plain-text wrap.
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_w: usize = 0;
    for (i, section) in SECTIONS.iter().enumerate() {
        let active = i == state.selected_section;
        let dirty = state.section_override_count(section) > 0;
        let title = section_title(section);
        let chevron = if active { "▾" } else { "▸" };
        let plain_w = chevron.width() + 1 + title.width() + if dirty { 2 } else { 0 };
        let sep_w = if current_spans.is_empty() { 0 } else { TAB_SEPARATOR.width() };
        if !current_spans.is_empty() && current_w + sep_w + plain_w > width {
            lines.push(Line::from(std::mem::take(&mut current_spans)));
            current_w = 0;
        }
        if !current_spans.is_empty() {
            current_spans.push(Span::raw(TAB_SEPARATOR));
            current_w += TAB_SEPARATOR.width();
        }
        let chevron_style = if active {
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_DIM)
        };
        current_spans.push(Span::styled(chevron.to_string(), chevron_style));
        current_spans.push(Span::raw(" "));
        let title_style = if active {
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(COLOR_DIM)
        };
        current_spans.push(Span::styled(title.to_string(), title_style));
        if dirty {
            current_spans.push(Span::raw(" "));
            current_spans.push(Span::styled(
                "●".to_string(),
                Style::default().fg(COLOR_OVERRIDE),
            ));
        }
        current_w += plain_w;
    }
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn field_row(state: &ConfigPanelState, idx: usize, width: usize) -> Line<'static> {
    let meta = &FIELDS[idx];
    let focused = idx == state.selected_field;
    let is_override = state.source_for(meta) == "override";
    let read_only = matches!(meta.kind, FieldKind::ReadOnly | FieldKind::List | FieldKind::Map);

    let value_text = if focused && matches!(state.editing, Some(Editing::Integer | Editing::String))
    {
        state.edit_buffer.clone()
    } else {
        render_value_text(state, meta, focused)
    };

    // Width budget: 1 (focus) + 1 (override) + 1 (space) + LABEL_WIDTH +
    // 1 (space) + 1 (separator) + 1 (space) + value (rest) + maybe chip.
    let label_w = LABEL_WIDTH.min(width.saturating_sub(8));
    let prefix_w = 1 + 1 + 1 + label_w + 1 + 1 + 1; // = label_w + 6
    let chip_text = if is_override { " (overridden)" } else { "" };
    let chip_w = chip_text.width();
    let value_w = width.saturating_sub(prefix_w + chip_w).max(1);
    let value_clipped = ellipsize_end(&value_text, value_w);

    let label_style = if focused {
        Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD)
    } else if read_only {
        Style::default().fg(COLOR_DIM)
    } else {
        Style::default()
    };
    let value_style = if focused
        && matches!(state.editing, Some(Editing::Integer | Editing::String))
    {
        Style::default()
            .fg(COLOR_FOCUS)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else if is_override {
        Style::default().fg(COLOR_OVERRIDE)
    } else if read_only {
        Style::default().fg(COLOR_DIM)
    } else {
        Style::default()
    };

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    spans.push(focus_span(focused));
    spans.push(override_dot(is_override));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(pad_right(meta.label, label_w), label_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        "│".to_string(),
        Style::default().fg(COLOR_DIM),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(value_clipped, value_style));

    if focused
        && matches!(meta.kind, FieldKind::Enum(_))
        && !matches!(state.editing, Some(Editing::Choice { .. }))
    {
        spans.push(Span::styled(
            " ▼".to_string(),
            Style::default().fg(COLOR_DIM),
        ));
    }

    if is_override {
        spans.push(Span::styled(
            chip_text.to_string(),
            Style::default().fg(COLOR_OVERRIDE),
        ));
    }

    Line::from(spans)
}

fn render_value_text(state: &ConfigPanelState, meta: &FieldMeta, _focused: bool) -> String {
    let raw = state.value_for(meta);
    if meta.secret && !state.reveal_topic && !raw.is_empty() {
        middle_ellipsis(&raw, 16)
    } else {
        raw
    }
}

fn help_text(state: &ConfigPanelState, width: usize) -> Line<'static> {
    if let Some(err) = state
        .current_meta()
        .and_then(|meta| state.edit_error_for(*meta, &state.edit_buffer))
    {
        return Line::from(Span::styled(
            ellipsize_end(&err, width),
            Style::default().fg(COLOR_DANGER),
        ));
    }
    if let Some(err) = &state.save_error {
        return Line::from(Span::styled(
            ellipsize_end(err, width),
            Style::default().fg(COLOR_DANGER),
        ));
    }
    let banner = match &state.conflict {
        Some(ConflictBanner::MtimeAdvanced) => {
            Some("mtime conflict: r reload · o overwrite · Esc keep editing")
        }
        Some(ConflictBanner::DiscardPrompt) => Some("discard unsaved changes? y discard · n keep"),
        Some(ConflictBanner::RegenerateTopicPrompt) => {
            Some("regenerate ntfy.topic? y accept · n keep")
        }
        None => None,
    };
    if let Some(text) = banner {
        return Line::from(Span::styled(
            ellipsize_end(text, width),
            Style::default().fg(COLOR_OVERRIDE),
        ));
    }
    if is_providers_section(state.current_section()) {
        return Line::from(dim(ellipsize_end(
            "Enter opens the per-provider detail drawer · a adds a custom entry · x removes",
            width,
        )));
    }
    let text = state
        .current_meta()
        .map(|meta| meta.description.to_string())
        .unwrap_or_default();
    Line::from(dim(ellipsize_end(&text, width)))
}

fn footer_line(state: &ConfigPanelState, width: usize) -> Line<'static> {
    let hotkeys: &[(&str, &str)] = if state.searching.is_some() {
        &[("↑↓", "select"), ("Enter", "jump"), ("Esc", "cancel")]
    } else if state.read_only {
        &[
            ("Tab", "page"),
            ("/", "search"),
            ("e", "edit"),
            ("Esc", "close"),
        ]
    } else {
        match &state.editing {
            Some(Editing::Choice { .. }) => &[
                ("↑↓", "select"),
                ("Enter", "commit"),
                ("Esc", "cancel"),
            ],
            Some(Editing::Integer | Editing::String) => {
                &[("Enter", "commit"), ("Esc", "cancel")]
            }
            Some(Editing::AddProvider(_)) => &[
                ("↑↓", "model"),
                ("Enter", "add"),
                ("Ctrl-C", "CLI"),
                ("Esc", "cancel"),
            ],
            Some(Editing::ProviderDetail { .. }) => &[
                ("↑↓", "option"),
                ("Space", "toggle"),
                ("x", "delete"),
                ("Esc", "close"),
            ],
            None if is_providers_section(state.current_section()) => &[
                ("↑↓", "model"),
                ("Enter", "details"),
                ("a", "add"),
                ("x", "remove"),
                ("Ctrl-S", "save"),
                ("Esc", "close"),
            ],
            None => &[
                ("Tab", "page"),
                ("Enter", "edit"),
                ("Space", "toggle"),
                ("d", "default"),
                ("/", "search"),
                ("Ctrl-S", "save"),
                ("Esc", "close"),
            ],
        }
    };

    let invalid = state.current_validation_error();
    let status_text = if let Some(reason) = invalid.clone() {
        reason
    } else if state.dirty {
        format!(
            "unsaved · {} changes · applies after Ctrl-S",
            dirty_count(state)
        )
    } else {
        state.status.clone()
    };
    let status_color = if invalid.is_some() {
        COLOR_DANGER
    } else if state.dirty {
        COLOR_OVERRIDE
    } else {
        COLOR_DIM
    };
    let status_chip_full = format!(" │ {status_text}");

    // Pack hotkeys into the available width, dropping trailing entries that
    // wouldn't fit alongside the status chip. Hotkeys render as `key label`
    // with key colored cyan and label dim.
    let status_w = status_chip_full.width();
    let mut packed: Vec<Span<'static>> = Vec::new();
    let mut packed_w: usize = 0;
    for (idx, (key, label)) in hotkeys.iter().enumerate() {
        let sep = if idx == 0 { "" } else { "  " };
        let segment_w = sep.width() + key.width() + 1 + label.width();
        if packed_w + segment_w + status_w > width {
            break;
        }
        if !sep.is_empty() {
            packed.push(Span::raw(sep));
        }
        packed.push(Span::styled(
            key.to_string(),
            Style::default().fg(COLOR_FOCUS).add_modifier(Modifier::BOLD),
        ));
        packed.push(Span::raw(" "));
        packed.push(Span::styled(
            label.to_string(),
            Style::default().fg(COLOR_DIM),
        ));
        packed_w += segment_w;
    }
    // Status chip on the right; the gap fills with spaces.
    let gap = width.saturating_sub(packed_w + status_w);
    if gap > 0 {
        packed.push(Span::raw(" ".repeat(gap)));
    }
    packed.push(Span::styled(
        " │ ".to_string(),
        Style::default().fg(COLOR_DIM),
    ));
    packed.push(Span::styled(
        ellipsize_end(&status_text, width.saturating_sub(packed_w + 3).max(1)),
        Style::default().fg(status_color),
    ));
    Line::from(packed)
}

fn header_line(path: &Path, width: u16) -> Line<'static> {
    let path = path.display().to_string();
    let right_w = (width as usize).saturating_sub("settings".width() + 4);
    let right = middle_ellipsis(&path, right_w);
    top_rule_with_left_spans(
        vec![Span::styled(
            "settings".to_string(),
            Style::default().fg(Color::DarkGray),
        )],
        Some(&right),
        width,
    )
}

fn visible_fields(state: &ConfigPanelState) -> Vec<usize> {
    field_indices_for(state.current_section())
}

fn field_indices_for(section: &str) -> Vec<usize> {
    FIELDS
        .iter()
        .enumerate()
        .filter_map(|(idx, meta)| (meta.section == section).then_some(idx))
        .collect()
}

fn dirty_count(state: &ConfigPanelState) -> usize {
    let field_count = FIELDS
        .iter()
        .filter(|meta| match meta.key {
            "meta.version" => false,
            _ => state.source_for(meta) == "override",
        })
        .count();
    if state.config.providers.is_explicit() {
        field_count + state.config.providers.value().len().max(1)
    } else {
        field_count
    }
}

fn ellipsize_end(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in value.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw + 1 > width {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

fn middle_ellipsis(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width <= 1 {
        return ellipsize_end(value, width);
    }
    let chars: Vec<char> = value.chars().collect();
    let left = width.saturating_sub(1) / 2;
    let right = width.saturating_sub(1).saturating_sub(left);
    let prefix: String = chars.iter().take(left).collect();
    let suffix: String = chars
        .iter()
        .rev()
        .take(right)
        .copied()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}…{suffix}")
}

fn format_list(values: &[String]) -> String {
    format!("[{}]", values.iter().map(|v| format!("\"{v}\"")).join(", "))
}

fn format_map(map: &std::collections::BTreeMap<String, String>) -> String {
    format!(
        "{{ {} }}",
        map.iter().map(|(k, v)| format!("{k} = \"{v}\"")).join(", ")
    )
}

pub(super) fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as isize;
    (current as isize + delta).rem_euclid(len) as usize
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn value_bool(value: &Override<bool>) -> String {
    value.value().to_string()
}

fn source_override<T>(value: &Override<T>) -> &'static str {
    if value.is_explicit() {
        "override"
    } else {
        "(def)"
    }
}

/// Body lines for the providers section. Provider rows render with a styled
/// chip strip; the per-provider detail drawer is rendered separately as an
/// overlay (see `render_provider_detail_overlay`).
///
/// `body_h` is the number of rows the viewport can hold. The function clamps
/// the persisted scroll offset so the focused row stays visible and emits
/// scroll indicators (`↑ N more above` / `↓ N more below`) when content
/// overflows the viewport.
fn providers_body_lines(
    state: &ConfigPanelState,
    width: usize,
    body_h: usize,
) -> Vec<Line<'static>> {
    let lines = providers::get_lines(&state.config);
    if lines.is_empty() {
        state.providers_scroll.set(0);
        state.providers_body_h.set(body_h);
        return vec![Line::from(dim(
            "  no providers entries · operator-funded providers go here",
        ))];
    }
    state.providers_body_h.set(body_h);

    let total = lines.len();
    let cursor = state.providers_cursor.min(total.saturating_sub(1));
    let mut scroll = state.providers_scroll.get().min(total.saturating_sub(1));

    // Reserve one row for each scroll indicator we'll need to draw.
    let inner_h = body_h.max(1);
    let needs_top_indicator = |scroll: usize| scroll > 0;
    let needs_bottom_indicator =
        |scroll: usize, capacity: usize| scroll + capacity < total;

    // Snap so cursor sits in the visible window. Account for the indicators
    // that will render once we know the final scroll position.
    if cursor < scroll {
        scroll = cursor;
    }
    let mut content_h = inner_h;
    loop {
        let mut budget = content_h;
        if needs_top_indicator(scroll) {
            budget = budget.saturating_sub(1);
        }
        if needs_bottom_indicator(scroll, budget) {
            budget = budget.saturating_sub(1);
        }
        let last_visible = scroll + budget;
        if cursor >= last_visible {
            scroll = (cursor + 1).saturating_sub(budget);
        } else if scroll + budget > total && scroll > 0 {
            scroll = total.saturating_sub(budget);
        } else {
            break;
        }
        if content_h == 0 {
            break;
        }
        content_h = inner_h;
    }
    state.providers_scroll.set(scroll);

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut budget = inner_h;
    if needs_top_indicator(scroll) {
        out.push(Line::from(dim(format!(
            "  ↑ {} more above",
            scroll
        ))));
        budget = budget.saturating_sub(1);
    }
    let bottom_reserved = if scroll + budget < total { 1 } else { 0 };
    let visible_count = budget.saturating_sub(bottom_reserved);
    let end = (scroll + visible_count).min(total);
    for (idx, line) in lines.iter().enumerate().take(end).skip(scroll) {
        out.push(providers::format_line(
            line,
            idx == state.providers_cursor,
            width,
        ));
    }
    if bottom_reserved == 1 {
        let remaining = total.saturating_sub(end);
        out.push(Line::from(dim(format!("  ↓ {remaining} more below"))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render_to_text(state: &ConfigPanelState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), state))
            .unwrap();
        terminal.backend().to_string()
    }

    fn state_with_overrides() -> ConfigPanelState {
        let mut config = Config::baked_defaults();
        mutate::set_value(
            &mut config,
            "ntfy.topic",
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        mutate::set_value(&mut config, "ntfy.detail_mode", "minimal").unwrap();
        mutate::set_value(
            &mut config,
            "paths.sessions_root",
            "$HOME/.codexize/sessions/with/a/very/long/path/that/wraps",
        )
        .unwrap();
        ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            None,
        )
    }

    #[test]
    fn redesigned_panel_opens_on_common_settings_with_friendly_tabs() {
        let state = state_with_overrides();
        assert_eq!(state.current_section_name(), "general");

        let text = render_to_text(&state, 100, 18);
        assert!(text.contains("Common"));
        assert!(text.contains("Models"));
        assert!(text.contains("Notifications"));
        assert!(text.contains("Agents"));
        assert!(text.contains("System"));
        assert!(
            text.contains("Review cadence"),
            "common settings should use friendly labels: {text}"
        );
        assert!(
            !text.contains("full_review_interval"),
            "raw config labels should not be visible in the common page: {text}"
        );
    }

    #[test]
    fn notification_page_uses_friendly_names_not_raw_keys() {
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.select_first_field_in_current_section();

        let text = render_to_text(&state, 100, 18);
        assert!(text.contains("Retry attempts"));
        assert!(text.contains("Topic"));
        assert!(
            !text.contains("retry_attempts"),
            "raw snake_case labels should not render: {text}"
        );
        assert!(
            !text.contains("detail_mode"),
            "raw snake_case labels should not render: {text}"
        );
    }

    #[test]
    fn model_tab_enter_opens_detail_drawer_with_enabled_focused() {
        // Models page Enter (and Space) opens the per-provider detail drawer
        // rather than directly toggling. The drawer's cursor lands on the
        // first user-toggleable property — Enabled, index 0 — so the most
        // common operation (Space, Space) still flips availability.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.providers_cursor = providers::get_lines(&state.config)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .expect("provider row");

        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            state.editing,
            Some(Editing::ProviderDetail { cursor: 0 })
        ));

        // Space inside the drawer flips the focused property and stays open.
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let lines = providers::get_lines(&state.config);
        let providers::ProvidersLine::Provider { entry, .. } = &lines[state.providers_cursor]
        else {
            panic!("cursor should still point at provider");
        };
        assert!(!entry.enabled, "space in drawer should flip availability");
        assert!(state.dirty);
        assert!(matches!(state.editing, Some(Editing::ProviderDetail { .. })));
    }

    #[test]
    fn model_tab_x_deletes_user_added_provider() {
        let mut config = Config::baked_defaults();
        config.providers = crate::data::config::schema::Override::explicit(vec![
            crate::data::config::schema::ProviderEntry {
                cli: crate::selection::CliKind::Opencode,
                launch_name: "custom-opus".to_string(),
                model: "claude-opus-4.7".to_string(),
                subscription: crate::selection::SubscriptionKind::Claude,
                enabled: true,
                free: false,
                official: false,
                quota_disabled: false,
                cheap_eligible: false,
                tough_eligible: true,
                effort_eligible: true,
                effort_mapping: Default::default(),
                quota_lookup_key: None,
                display_order: 0,
            },
        ]);
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.providers_cursor = providers::get_lines(&state.config)
            .iter()
            .position(|l| {
                matches!(
                    l,
                    providers::ProvidersLine::Provider { entry, is_baked: false, .. }
                        if entry.launch_name == "custom-opus"
                )
            })
            .expect("custom provider row");

        state.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert!(
            !state
                .config
                .providers
                .value()
                .iter()
                .any(|entry| entry.launch_name == "custom-opus"),
            "custom provider should be removed from overrides"
        );
        assert!(state.dirty);
    }

    #[test]
    fn adaptive_snapshot_width_120() {
        let state = state_with_overrides();
        insta::assert_snapshot!(render_to_text(&state, 120, 20));
    }

    #[test]
    fn adaptive_snapshot_width_80_keeps_primary_hotkeys() {
        let state = state_with_overrides();
        let text = render_to_text(&state, 80, 18);
        // Footer renders `<key> <label>` pairs. High-frequency keys must
        // survive narrower widths; trailing entries are dropped first when
        // the line gets crowded.
        assert!(text.contains("Tab page"), "missing Tab page: {text}");
        assert!(text.contains("Enter edit"), "missing Enter edit: {text}");
        assert!(text.contains("Space toggle"), "missing Space toggle: {text}");
        assert!(text.contains("d default"), "missing d default: {text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn adaptive_snapshot_width_60_shows_tab_bar() {
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "system").unwrap();
        state.select_first_field_in_current_section();
        let text = render_to_text(&state, 60, 16);
        assert!(text.contains("▸ Common"));
        assert!(text.contains("▾ System"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn ctrl_i_and_tab_both_switch_pages() {
        let mut state = state_with_overrides();
        assert_eq!(state.current_section(), "general");

        state.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL));
        assert_eq!(state.current_section(), "models");

        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(state.current_section(), "notifications");
    }

    #[test]
    fn tab_cycles_sections_with_wrap() {
        let mut state = state_with_overrides();
        state.selected_section = 0;
        state.select_first_field_in_current_section();
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(state.current_section(), SECTIONS[1]);
        for _ in 0..SECTIONS.len() - 1 {
            state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        assert_eq!(state.current_section(), SECTIONS[0]);
    }

    #[test]
    fn shift_tab_cycles_sections_reverse() {
        let mut state = state_with_overrides();
        state.selected_section = 0;
        state.select_first_field_in_current_section();
        state.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(state.current_section(), *SECTIONS.last().unwrap());
    }

    #[test]
    fn bracket_keys_jump_first_last_section() {
        let mut state = state_with_overrides();
        state.selected_section = 5;
        state.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(state.selected_section, 0);
        state.handle_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(state.selected_section, SECTIONS.len() - 1);
    }

    #[test]
    #[allow(non_snake_case)]
    fn g_G_jumps_first_last_field() {
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.selected_field = 5;
        state.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        let fields = field_indices_for(state.current_section());
        assert_eq!(state.selected_field, fields[0]);
        state.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(state.selected_field, *fields.last().unwrap());
    }

    #[test]
    fn narrow_width_refuses_to_render_panel() {
        let state = state_with_overrides();
        let text = render_to_text(&state, 49, 6);
        assert!(text.contains(terminal_too_narrow_message()));
    }

    fn focus_field(state: &mut ConfigPanelState, key: &str) {
        let field_idx = FIELDS.iter().position(|f| f.key == key).unwrap();
        state.selected_section = SECTIONS
            .iter()
            .position(|s| *s == FIELDS[field_idx].section)
            .unwrap();
        state.selected_field = field_idx;
    }

    #[test]
    fn arrow_keys_in_nav_mode_are_no_ops_for_field_and_section() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let section_before = state.selected_section;
        let value_before = state.value_for(state.current_meta().unwrap());
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('h'),
            KeyCode::Char('l'),
        ] {
            state.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            assert_eq!(
                state.selected_section, section_before,
                "{code:?} moved section"
            );
            assert_eq!(
                state.value_for(state.current_meta().unwrap()),
                value_before,
                "{code:?} mutated value"
            );
            assert!(state.editing.is_none(), "{code:?} entered edit mode");
        }
    }

    #[test]
    fn arrow_keys_in_nav_mode_dont_cycle_enum() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        // The override fixture sets detail_mode = "minimal"; cycling under
        // the old keymap would flip to "detailed" — these arrows must now
        // leave the value alone.
        let value_before = state.value_for(state.current_meta().unwrap());
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('h'),
            KeyCode::Char('l'),
        ] {
            state.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            assert_eq!(state.value_for(state.current_meta().unwrap()), value_before);
        }
    }

    #[test]
    fn enter_on_bool_flips_inline_without_dropdown() {
        // Bools toggle directly: a two-row dropdown is dead weight when the
        // only options are true/false, so Enter (and Space) flips the value
        // and stays in nav mode.
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let value_before = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none(), "bool flip must not enter editing");
        let after = state.value_for(state.current_meta().unwrap());
        assert_ne!(after, value_before, "bool value should have flipped");
        assert!(state.dirty);
    }

    #[test]
    fn space_on_bool_flips_inline() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let before = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(state.editing.is_none());
        assert_ne!(state.value_for(state.current_meta().unwrap()), before);
    }

    #[test]
    fn enter_on_enum_opens_dropdown_with_variants() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Editing::Choice { options, .. }) = state.editing.as_ref() else {
            panic!("expected Choice editing state");
        };
        assert!(options.iter().any(|o| o == "detailed"));
        assert!(options.iter().any(|o| o == "minimal"));
    }

    #[test]
    fn enter_on_integer_enters_inline_edit() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        let value = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(state.editing, Some(Editing::Integer)));
        assert_eq!(state.edit_buffer, value);
    }

    #[test]
    fn enter_on_string_enters_inline_edit() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.server");
        let value = state.value_for(state.current_meta().unwrap());
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(state.editing, Some(Editing::String)));
        assert_eq!(state.edit_buffer, value);
    }

    #[test]
    fn inline_edit_enter_commits_buffer() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.edit_buffer = "7".to_string();
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none());
        assert_eq!(state.value_for(state.current_meta().unwrap()), "7");
    }

    #[test]
    fn inline_edit_invalid_integer_blocks_save() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.retry_attempts");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.edit_buffer = "0".to_string();
        assert!(
            state
                .current_validation_error()
                .unwrap()
                .contains("cannot save")
        );
    }

    #[test]
    fn d_resets_overridden_enum_to_default() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        assert_eq!(state.source_for(state.current_meta().unwrap()), "override");
        state.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(state.source_for(state.current_meta().unwrap()), "(def)");
    }

    #[test]
    fn dropdown_snapshot() {
        // Pop the dropdown for an enum field — bools flip inline without a
        // popup, so this exercises the multi-variant overlay.
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.detail_mode");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        insta::assert_snapshot!(render_to_text(&state, 80, 18));
    }

    #[test]
    fn rendered_overrides_show_dirty_marker_source_tag_and_tab_suffix() {
        // The override fixture sets ntfy.topic + ntfy.detail_mode + paths.sessions_root.
        // Verify the three operator-facing override signals: (1) `●` dot in
        // the dirty column, (2) `(overridden)` chip on the value, (3) `●`
        // suffix on tab-bar entries for override-bearing sections.
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.select_first_field_in_current_section();
        let text = render_to_text(&state, 120, 20);
        assert!(
            text.contains(" ● Topic"),
            "missing override dot on topic row: {text}"
        );
        assert!(
            text.contains(" ● Message detail"),
            "missing override dot on detail_mode row: {text}"
        );
        assert!(
            text.contains("(overridden)"),
            "missing override chip on value: {text}"
        );
        assert!(
            text.contains("▾ Notifications ●"),
            "missing override marker on notifications tab: {text}"
        );
        assert!(
            text.contains("▸ System ●"),
            "missing override marker on system tab: {text}"
        );
    }

    #[test]
    fn slash_opens_search_and_typing_filters_results() {
        let mut state = state_with_overrides();
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        let search = state.searching.as_ref().expect("search opened");
        // Empty query lists every field.
        assert_eq!(search.results.len(), FIELDS.len());

        for c in "retry".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let search = state.searching.as_ref().expect("still searching");
        assert!(!search.results.is_empty());
        for &idx in &search.results {
            let key = FIELDS[idx].key;
            assert!(
                key.contains("retry") || FIELDS[idx].label.contains("retry"),
                "result {key} must match query 'retry'"
            );
        }
    }

    #[test]
    fn enter_in_search_jumps_to_field_across_sections() {
        let mut state = state_with_overrides();
        // Start on notifications; jump to a field in system via search.
        state.selected_section = SECTIONS.iter().position(|s| *s == "notifications").unwrap();
        state.select_first_field_in_current_section();
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "max_topics".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.searching.is_none(), "Enter must close search");
        assert_eq!(state.current_section_name(), "system");
        assert_eq!(
            state.current_meta().unwrap().key,
            "memory.max_topics_per_read"
        );
    }

    #[test]
    fn esc_in_search_closes_overlay_without_jumping() {
        let mut state = state_with_overrides();
        let section_before = state.selected_section;
        let field_before = state.selected_field;
        state.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "memory".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.searching.is_none());
        assert_eq!(state.selected_section, section_before);
        assert_eq!(state.selected_field, field_before);
    }

    #[test]
    fn read_only_blocks_dropdown_commit_even_if_editing_is_forced() {
        // The natural flow gates `Enter` so editing cannot be opened while
        // read_only is true; this defensive check verifies a forcibly-set
        // Choice editing state still cannot mutate the underlying value.
        let mut state = state_with_overrides();
        state.read_only = true;
        focus_field(&mut state, "ntfy.enabled");
        let value_before = state.value_for(state.current_meta().unwrap());
        let dirty_before = state.dirty;
        state.editing = Some(Editing::Choice {
            key: "ntfy.enabled",
            options: vec!["true".to_string(), "false".to_string()],
            selected: 1,
        });

        // Enter would normally commit "false" — must be ignored in read-only.
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(state.value_for(state.current_meta().unwrap()), value_before);
        assert_eq!(state.dirty, dirty_before);

        // Down arrow would normally move highlight; mutation keys must be inert.
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.value_for(state.current_meta().unwrap()), value_before);
        assert_eq!(state.dirty, dirty_before);

        // Esc unwinds the forced-edit state defensively.
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.editing.is_none());
    }

    #[test]
    fn providers_section_renders_entries_with_unmatched_warning() {
        // Two entries: one matched against the known universe, one that
        // points at a row no provider serves yet. The matched entry renders
        // bare; the unmatched entry trails the soft-warning suffix.
        let mut config = Config::baked_defaults();
        config.providers = crate::data::config::schema::Override::explicit(vec![
            crate::data::config::schema::ProviderEntry {
                cli: crate::selection::CliKind::Claude,
                launch_name: "claude-opus-4.7".to_string(),
                model: "claude-opus-4.7".to_string(),
                subscription: crate::selection::SubscriptionKind::Claude,
                enabled: true,
                free: false,
                official: true,
                quota_disabled: false,
                cheap_eligible: false,
                tough_eligible: true,
                effort_eligible: true,
                effort_mapping: Default::default(),
                quota_lookup_key: None,
                display_order: 0,
            },
        ]);
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();

        let text = render_to_text(&state, 120, 18);
        assert!(
            text.contains("claude · claude-opus-4.7"),
            "missing group header: {text}"
        );
        assert!(
            text.contains("✓ claude/claude  claude-opus-4.7"),
            "missing enabled provider row: {text}"
        );
        assert!(
            text.contains("built-in · official · paid"),
            "missing provider chip strip: {text}"
        );
    }

    #[test]
    fn providers_section_shows_baked_models_by_default() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();

        // Height tall enough to render every baked row + the Add Provider
        // footer; the section now has 30 baked rows and grows over time.
        let text = render_to_text(&state, 120, 80);
        assert!(
            text.contains("claude · claude-opus-4.7"),
            "should show baked models: {text}"
        );
        assert!(
            text.contains("+ Add model provider"),
            "should show add button: {text}"
        );
    }

    #[test]
    fn providers_section_render_does_not_panic_at_multibyte_boundaries() {
        let config = Config::baked_defaults();
        let state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );

        for width in MIN_WIDTH..=140 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render_to_text(&state, width, 24)
            }));
            assert!(
                result.is_ok(),
                "models page render panicked at width {width}"
            );
        }
    }

    #[test]
    fn add_provider_modal_populates_models_from_baked_universe() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();

        let lines = providers::get_lines(&state.config);
        let add_idx = lines
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::AddAction))
            .expect("AddAction line present");
        state.providers_cursor = add_idx;
        state.activate_provider_line();

        let Some(Editing::AddProvider(editor)) = state.editing.as_ref() else {
            panic!("AddProvider modal should be active");
        };
        assert!(
            !editor.available_models.is_empty(),
            "modal should derive models from the baked + override universe"
        );
        assert!(
            editor
                .available_models
                .iter()
                .any(|(v, m)| v == "claude" && m == "claude-opus-4.7"),
            "expected baked claude/claude-opus-4.7 in available models: {:?}",
            editor.available_models,
        );
        assert_eq!(editor.subscription, editor.available_models[0].0);
        assert_eq!(editor.model, editor.available_models[0].1);
    }

    #[test]
    fn providers_group_by_model_vendor_not_subscription_label() {
        // opencode-go is a subscription (it bills through the opencode pool),
        // not a vendor — the actual model vendor is deepseek/minimax/etc.
        // Group headers must read off the model's vendor so opencode-go
        // entries get filed under their real vendor.
        let config = Config::baked_defaults();
        let lines = providers::get_lines(&config);
        let mut seen_groups: Vec<(String, String)> = Vec::new();
        for line in &lines {
            if let providers::ProvidersLine::GroupHeader { vendor, model } = line {
                seen_groups.push((vendor.clone(), model.clone()));
            }
        }
        assert!(
            seen_groups.iter().any(|(v, m)| v == "deepseek" && m == "deepseek-v4-flash"),
            "expected deepseek-v4-flash filed under vendor 'deepseek', got: {seen_groups:?}"
        );
        assert!(
            !seen_groups
                .iter()
                .any(|(v, _)| v == "opencode-go"),
            "subscription label 'opencode-go' must not appear as a vendor: {seen_groups:?}"
        );
    }

    #[test]
    fn provider_row_renders_subscription_cli_and_launch_name_separately() {
        // Each entry under a (vendor, model) group must display all three
        // facets — subscription, CLI, launch_name — so the user can tell
        // a kimi-via-Kimi pool entry apart from a (hypothetical)
        // kimi-via-Opencode entry at a glance.
        let mut config = Config::baked_defaults();
        // Inject an opencode-routed kimi-k2.6 row alongside the baked
        // kimi-via-Kimi row so the group has two distinct entries.
        let mut overrides = config.providers.value().clone();
        overrides.push(crate::data::config::schema::ProviderEntry {
            cli: crate::selection::CliKind::Opencode,
            launch_name: "opencode-go/kimi-k2.6".to_string(),
            model: "kimi-k2.6".to_string(),
            subscription: crate::selection::SubscriptionKind::OpencodeGo,
            enabled: true,
            free: false,
            official: false,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: Default::default(),
            quota_lookup_key: None,
            display_order: 0,
        });
        config.providers = crate::data::config::schema::Override::explicit(overrides);

        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();

        let text = render_to_text(&state, 120, 80);
        assert!(
            text.contains("kimi · kimi-k2.6"),
            "expected vendor-grouped header: {text}"
        );
        // Subscription label for SubscriptionKind::Kimi is "moonshotai".
        assert!(
            text.contains("moonshotai/kimi  kimi-latest"),
            "expected built-in kimi entry to show subscription/cli + launch_name: {text}"
        );
        assert!(
            text.contains("opencode-go/opencode  opencode-go/kimi-k2.6"),
            "expected opencode-routed kimi entry under the same group: {text}"
        );
    }

    #[test]
    fn providers_pagination_keeps_focused_row_in_viewport() {
        // 30+ baked rows + group headers can't fit a typical viewport. As
        // the cursor moves down, the top of the window must follow so the
        // focused row stays visible and the bottom indicator updates.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();

        // Render once to seed body_h.
        let _ = render_to_text(&state, 100, 18);
        let body_h = state.providers_body_h.get();
        assert!(body_h > 4, "viewport should hold a handful of rows");

        // Push the cursor far past the visible window.
        for _ in 0..(body_h * 3) {
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        let lines_total = providers::get_lines(&state.config).len();
        let cursor = state.providers_cursor;
        assert!(cursor < lines_total);

        // Render again so providers_body_lines snaps the scroll offset.
        let _ = render_to_text(&state, 100, 18);
        let scroll = state.providers_scroll.get();
        assert!(
            cursor >= scroll && cursor < scroll + body_h,
            "cursor {cursor} must be inside [{scroll}, {}]",
            scroll + body_h
        );
    }

    #[test]
    fn providers_pagination_indicator_visible_when_overflowing() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        let text = render_to_text(&state, 100, 14);
        assert!(
            text.contains("more below"),
            "expected bottom scroll indicator: {text}"
        );
    }

    #[test]
    fn providers_navigation_skips_group_headers() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        let lines = providers::get_lines(&state.config);
        // Place the cursor at the second provider (skip the leading header).
        state.providers_cursor = lines
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .unwrap();
        // Walking forward 10 steps must never land on a group header.
        for _ in 0..10 {
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            assert!(
                !matches!(
                    lines.get(state.providers_cursor),
                    Some(providers::ProvidersLine::GroupHeader { .. })
                ),
                "cursor {} landed on a header",
                state.providers_cursor
            );
        }
    }

    #[test]
    fn provider_detail_drawer_skips_baked_locked_rows_during_navigation() {
        // Built-in providers expose `Official` / `Free` as derived flags
        // rather than user-controllable toggles; the cursor must skip those
        // rows so j/k always lands on something Space can flip.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.providers_cursor = providers::get_lines(&state.config)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { is_baked: true, .. }))
            .expect("baked provider row");

        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let initial_cursor = match state.editing {
            Some(Editing::ProviderDetail { cursor }) => cursor,
            _ => panic!("expected detail drawer"),
        };
        assert!(!PROVIDER_TOGGLES[initial_cursor].baked_locked);

        // Walk one full circuit; every visited cursor must be unlocked.
        for _ in 0..PROVIDER_TOGGLES.len() {
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            let cursor = match state.editing {
                Some(Editing::ProviderDetail { cursor }) => cursor,
                _ => panic!("drawer should stay open"),
            };
            assert!(
                !PROVIDER_TOGGLES[cursor].baked_locked,
                "j on baked drawer landed on locked toggle {} (idx {cursor})",
                PROVIDER_TOGGLES[cursor].label
            );
        }
    }

    #[test]
    fn providers_page_render_snapshot() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.providers_cursor = providers::get_lines(&state.config)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .expect("provider row");
        insta::assert_snapshot!(render_to_text(&state, 100, 18));
    }

    #[test]
    fn provider_detail_drawer_render_snapshot() {
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        state.providers_cursor = providers::get_lines(&state.config)
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::Provider { .. }))
            .expect("provider row");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        insta::assert_snapshot!(render_to_text(&state, 90, 22));
    }

    #[test]
    fn add_provider_modal_can_compose_opencode_for_kimi_via_independent_pickers() {
        // The user complaint: kimi has no opencode entry. With the modal's
        // form-style focus walk, the user can pick model=kimi-k2.6 from the
        // baked universe, then Tab to subscription and cycle to OpencodeGo,
        // then Tab to CLI and cycle to Opencode, then type the launch name.
        let config = Config::baked_defaults();
        let mut state = ConfigPanelState::open_at(
            &config,
            PathBuf::from("/tmp/example/config.toml"),
            false,
            Some("models"),
        );
        state.selected_section = SECTIONS.iter().position(|s| *s == "models").unwrap();
        let lines = providers::get_lines(&state.config);
        let add_idx = lines
            .iter()
            .position(|l| matches!(l, providers::ProvidersLine::AddAction))
            .expect("add row");
        state.providers_cursor = add_idx;
        state.activate_provider_line();

        // Cycle the model picker until kimi-k2.6 is selected.
        for _ in 0..50 {
            if let Some(Editing::AddProvider(editor)) = state.editing.as_ref()
                && editor.model == "kimi-k2.6"
            {
                break;
            }
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert!(
            matches!(state.editing.as_ref(),
                Some(Editing::AddProvider(e)) if e.model == "kimi-k2.6"),
            "could not navigate the picker to kimi-k2.6"
        );

        // Tab to Subscription and walk to OpencodeGo.
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for _ in 0..SUBSCRIPTION_OPTIONS_COUNT {
            if let Some(Editing::AddProvider(editor)) = state.editing.as_ref()
                && editor.subscription == "opencode-go"
            {
                break;
            }
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert!(
            matches!(state.editing.as_ref(),
                Some(Editing::AddProvider(e)) if e.subscription == "opencode-go"),
            "subscription picker did not reach opencode-go"
        );

        // Tab to CLI and walk to Opencode.
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for _ in 0..CLI_OPTIONS_COUNT {
            if let Some(Editing::AddProvider(editor)) = state.editing.as_ref()
                && matches!(editor.cli, crate::selection::CliKind::Opencode)
            {
                break;
            }
            state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }

        // Tab to Launch name and type a value.
        state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for c in "opencode-go/kimi-k2.6".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        // Commit and confirm the provider lands under the kimi vendor group.
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none(), "modal should close after commit");

        let added = state
            .config
            .providers
            .value()
            .iter()
            .find(|e| {
                e.model == "kimi-k2.6"
                    && matches!(e.cli, crate::selection::CliKind::Opencode)
                    && matches!(e.subscription, crate::selection::SubscriptionKind::OpencodeGo)
            });
        assert!(added.is_some(), "opencode-routed kimi entry should be persisted");
    }

    const SUBSCRIPTION_OPTIONS_COUNT: usize = 6; // SubscriptionKind variants
    const CLI_OPTIONS_COUNT: usize = 5;

    #[test]
    fn save_detects_mtime_conflict_and_overwrite_writes_sparse_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut state = state_with_overrides();
        state.path = path.clone();
        crate::data::config::save_atomic_to(&path, &state.config).unwrap();
        state.opened_mtime = Some(SystemTime::UNIX_EPOCH);
        fs::write(&path, "[meta]\nversion = 1\n[ntfy]\nretry_attempts = 2\n").unwrap();

        state.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(
            state.conflict,
            Some(ConflictBanner::MtimeAdvanced)
        ));
        state.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        let saved = fs::read_to_string(path).unwrap();
        assert!(saved.contains("topic = "));
        assert!(saved.contains("detail_mode = \"minimal\""));
        assert!(!saved.contains("enabled = true"));
    }
}
