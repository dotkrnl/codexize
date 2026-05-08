use crate::data::{
    config::{
        Config,
        loader::load_from_path,
        mutate,
        schema::{LogLevel, NtfyDetailMode, Override, ShellPolicy},
    },
    notifications,
};
use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MIN_WIDTH: u16 = 50;
const TAB_SEPARATOR: &str = "  ";
const TAG_WIDTH: usize = 9;
const BOOL_OPTIONS: &[&str] = &["true", "false"];

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
        section: "meta",
        key: "meta.version",
        label: "version",
        kind: FieldKind::ReadOnly,
        description: "Config schema version. This is stamped by the binary and cannot be edited.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.enabled",
        label: "enabled",
        kind: FieldKind::Bool,
        description: "Enables notification delivery when a topic is configured.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.server",
        label: "server",
        kind: FieldKind::String,
        description: "Base ntfy server URL. Must start with http:// or https://.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.topic",
        label: "topic",
        kind: FieldKind::String,
        description: "Notification topic. Empty disables delivery; r reveals the full value and q regenerates it.",
        secret: true,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.detail_mode",
        label: "detail_mode",
        kind: FieldKind::Enum(NtfyDetailMode::variants()),
        description: "Controls notification body length. Allowed: detailed, minimal. Default: detailed.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.retry_attempts",
        label: "retry_attempts",
        kind: FieldKind::Integer { min: 1 },
        description: "Number of notification send attempts. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.retry_delay_ms",
        label: "retry_delay_ms",
        kind: FieldKind::Integer { min: 0 },
        description: "Delay between notification retries in milliseconds.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.http_timeout_secs",
        label: "http_timeout_secs",
        kind: FieldKind::Integer { min: 1 },
        description: "HTTP timeout for ntfy requests. Minimum: 1 second.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.body_max_bytes",
        label: "body_max_bytes",
        kind: FieldKind::Integer { min: 256 },
        description: "Maximum notification body size in bytes. Minimum: 256.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.excerpt_max_chars",
        label: "excerpt_max_chars",
        kind: FieldKind::Integer { min: 32 },
        description: "Maximum excerpt characters used in notifications. Minimum: 32.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.created_at",
        label: "created_at",
        kind: FieldKind::ReadOnly,
        description: "Metadata timestamp maintained by config mutation paths.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy",
        key: "ntfy.updated_at",
        label: "updated_at",
        kind: FieldKind::ReadOnly,
        description: "Metadata timestamp maintained by config mutation paths.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy.events",
        key: "ntfy.events.phase_wait",
        label: "events.phase_wait",
        kind: FieldKind::Bool,
        description: "Notify when a phase is waiting.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy.events",
        key: "ntfy.events.interactive_wait",
        label: "events.interactive_wait",
        kind: FieldKind::Bool,
        description: "Notify when an interactive run is waiting for input.",
        secret: false,
    },
    FieldMeta {
        section: "ntfy.events",
        key: "ntfy.events.pipeline_done",
        label: "events.pipeline_done",
        kind: FieldKind::Bool,
        description: "Notify when the pipeline finishes.",
        secret: false,
    },
    FieldMeta {
        section: "acp.policy",
        key: "acp.policy.shell_policy",
        label: "shell_policy",
        kind: FieldKind::Enum(ShellPolicy::variants()),
        description: "Default ACP shell policy. Allowed: full-access, allowlist.",
        secret: false,
    },
    FieldMeta {
        section: "acp.policy",
        key: "acp.policy.shell_allowlist",
        label: "shell_allowlist",
        kind: FieldKind::List,
        description: "Read-only list in this panel; use the CLI for item-level edits.",
        secret: false,
    },
    FieldMeta {
        section: "acp.policy",
        key: "acp.policy.allowed_write_paths",
        label: "allowed_write_paths",
        kind: FieldKind::List,
        description: "Read-only list in this panel; use the CLI for item-level edits.",
        secret: false,
    },
    FieldMeta {
        section: "acp.install",
        key: "acp.install.claude_acp_root",
        label: "claude_acp_root",
        kind: FieldKind::String,
        description: "Claude ACP installation root; $HOME and ~/ are expanded by the loader.",
        secret: false,
    },
    FieldMeta {
        section: "runner",
        key: "runner.full_review_interval",
        label: "full_review_interval",
        kind: FieldKind::Integer { min: 1 },
        description: "Run a full alignment review every N review rounds. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "paths",
        key: "paths.cache_root",
        label: "cache_root",
        kind: FieldKind::String,
        description: "Root for cache files. $HOME and ~/ are expanded at load.",
        secret: false,
    },
    FieldMeta {
        section: "paths",
        key: "paths.sessions_root",
        label: "sessions_root",
        kind: FieldKind::String,
        description: "Root for session artifacts. $HOME and ~/ are expanded at load.",
        secret: false,
    },
    FieldMeta {
        section: "paths",
        key: "paths.runs_root",
        label: "runs_root",
        kind: FieldKind::String,
        description: "Reserved top-level run root; no current subsystem consumes it directly.",
        secret: false,
    },
    FieldMeta {
        section: "paths",
        key: "paths.memory_root",
        label: "memory_root",
        kind: FieldKind::String,
        description: "Root for project memory files. $HOME and ~/ are expanded at load.",
        secret: false,
    },
    FieldMeta {
        section: "ui",
        key: "ui.prefer_split_on_open",
        label: "prefer_split_on_open",
        kind: FieldKind::Bool,
        description: "Prefer the split transcript when opening run output.",
        secret: false,
    },
    FieldMeta {
        section: "ui.colon_palette",
        key: "ui.colon_palette.show_help",
        label: "colon_palette.show_help",
        kind: FieldKind::Bool,
        description: "Show the command palette help row.",
        secret: false,
    },
    FieldMeta {
        section: "ui.footer",
        key: "ui.footer.show_keys",
        label: "footer.show_keys",
        kind: FieldKind::Bool,
        description: "Show footer key hints.",
        secret: false,
    },
    FieldMeta {
        section: "diagnostics",
        key: "diagnostics.log_level",
        label: "log_level",
        kind: FieldKind::Enum(LogLevel::variants()),
        description: "Default log level. RUST_LOG still takes precedence.",
        secret: false,
    },
    FieldMeta {
        section: "diagnostics",
        key: "diagnostics.json_logs",
        label: "json_logs",
        kind: FieldKind::Bool,
        description: "Emit JSON logs unless the environment overrides diagnostics.",
        secret: false,
    },
    FieldMeta {
        section: "memory",
        key: "memory.enabled",
        label: "enabled",
        kind: FieldKind::Bool,
        description: "Enable project memory prompt context.",
        secret: false,
    },
    FieldMeta {
        section: "memory",
        key: "memory.max_topics_per_read",
        label: "max_topics_per_read",
        kind: FieldKind::Integer { min: 1 },
        description: "Maximum memory topics read into one prompt. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "memory",
        key: "memory.journal_retention_months",
        label: "journal_retention_months",
        kind: FieldKind::Integer { min: 1 },
        description: "Monthly memory journals older than this are pruned at launch. Minimum: 1.",
        secret: false,
    },
    FieldMeta {
        section: "acp.agents.claude",
        key: "acp.agents.claude.env",
        label: "claude.env",
        kind: FieldKind::Map,
        description: "Read-only map in this panel; use dotted CLI keys for env entries.",
        secret: false,
    },
];

const SECTIONS: &[&str] = &[
    "meta",
    "ntfy",
    "ntfy.events",
    "acp.policy",
    "acp.install",
    "acp.agents.claude",
    "runner",
    "paths",
    "ui",
    "ui.colon_palette",
    "ui.footer",
    "diagnostics",
    "memory",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Editing {
    Integer,
    String,
    Choice {
        key: &'static str,
        options: Vec<String>,
        selected: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConflictBanner {
    MtimeAdvanced,
    DiscardPrompt,
    RegenerateTopicPrompt,
    ResetSectionPrompt { section: String, count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelOutcome {
    KeepOpen,
    Close,
    Saved,
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigPanelState {
    config: Config,
    path: PathBuf,
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
}

impl ConfigPanelState {
    pub(crate) fn open(config: &Config, path: PathBuf, read_only: bool) -> Self {
        let opened_mtime = mtime(&path);
        Self {
            config: config.clone(),
            path,
            opened_mtime,
            selected_section: 1,
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
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        if !self.read_only
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('s')
        {
            self.save(false);
            return if self.conflict.is_none() && self.save_error.is_none() {
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
            KeyCode::Up => {
                self.move_field(-1);
                PanelOutcome::KeepOpen
            }
            KeyCode::Down => {
                self.move_field(1);
                PanelOutcome::KeepOpen
            }
            // Horizontal arrows are no-ops in navigation mode: mutation is
            // gated behind Enter, and section switching is bound to Tab/[/].
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                PanelOutcome::KeepOpen
            }
            KeyCode::Enter => {
                if !self.read_only {
                    self.activate_field();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('j') => {
                self.move_field(1);
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('k') => {
                self.move_field(-1);
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('d') => {
                if !self.read_only {
                    self.reset_field();
                }
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('D') => {
                if !self.read_only {
                    let section = self.current_section().to_string();
                    let count = self.section_override_count(&section);
                    self.conflict = Some(ConflictBanner::ResetSectionPrompt { section, count });
                    self.status = format!("reset section? {count} overrides");
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
                self.select_first_field_in_current_section();
                PanelOutcome::KeepOpen
            }
            KeyCode::Char('G') => {
                if let Some(last) = field_indices_for(self.current_section()).last() {
                    self.selected_field = *last;
                }
                PanelOutcome::KeepOpen
            }
            _ => PanelOutcome::KeepOpen,
        }
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
            ConflictBanner::ResetSectionPrompt { section, count } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    match mutate::reset_section(&mut self.config, &section) {
                        Ok(()) => {
                            self.dirty = count > 0 || self.dirty;
                            self.status = format!("reset {section} to defaults");
                            self.select_first_field_in_current_section();
                        }
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

    fn activate_field(&mut self) {
        let Some(meta) = self.current_meta().copied() else {
            return;
        };
        match meta.kind {
            FieldKind::Bool => {
                let options: Vec<String> = BOOL_OPTIONS.iter().map(|s| (*s).to_string()).collect();
                let current = self.value_for(&meta);
                let selected = options.iter().position(|v| v == &current).unwrap_or(0);
                self.editing = Some(Editing::Choice {
                    key: meta.key,
                    options,
                    selected,
                });
                self.status = format!("choose {}", meta.key);
            }
            FieldKind::Enum(variants) => {
                let options: Vec<String> = variants.iter().map(|v| (*v).to_string()).collect();
                let current = self.value_for(&meta);
                let selected = options.iter().position(|v| v == &current).unwrap_or(0);
                self.editing = Some(Editing::Choice {
                    key: meta.key,
                    options,
                    selected,
                });
                self.status = format!("choose {}", meta.key);
            }
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
                self.status = "saved · applies on next launch".to_string();
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
        self.select_first_field_in_current_section();
    }

    fn move_field(&mut self, delta: isize) {
        let section = self.current_section();
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

    fn select_first_field_in_current_section(&mut self) {
        if let Some(idx) = field_indices_for(self.current_section()).first() {
            self.selected_field = *idx;
        }
    }

    fn section_override_count(&self, section: &str) -> usize {
        FIELDS
            .iter()
            .filter(|meta| meta.section == section && self.source_for(meta) == "override")
            .count()
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
            "acp.policy.allowed_write_paths" => {
                format_list(self.config.acp.policy.allowed_write_paths.value())
            }
            "acp.install.claude_acp_root" => {
                self.config.acp.install.claude_acp_root.value().clone()
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
            "ntfy.enabled" => source_bool(&self.config.ntfy.enabled),
            "ntfy.server" => source_string(&self.config.ntfy.server),
            "ntfy.topic" => source_string(&self.config.ntfy.topic),
            "ntfy.detail_mode" => source_copy(&self.config.ntfy.detail_mode),
            "ntfy.retry_attempts" => source_copy(&self.config.ntfy.retry_attempts),
            "ntfy.retry_delay_ms" => source_copy(&self.config.ntfy.retry_delay_ms),
            "ntfy.http_timeout_secs" => source_copy(&self.config.ntfy.http_timeout_secs),
            "ntfy.body_max_bytes" => source_copy(&self.config.ntfy.body_max_bytes),
            "ntfy.excerpt_max_chars" => source_copy(&self.config.ntfy.excerpt_max_chars),
            "ntfy.created_at" => source_copy(&self.config.ntfy.created_at),
            "ntfy.updated_at" => source_copy(&self.config.ntfy.updated_at),
            "ntfy.events.phase_wait" => source_bool(&self.config.ntfy.events.phase_wait),
            "ntfy.events.interactive_wait" => {
                source_bool(&self.config.ntfy.events.interactive_wait)
            }
            "ntfy.events.pipeline_done" => source_bool(&self.config.ntfy.events.pipeline_done),
            "acp.policy.shell_policy" => source_copy(&self.config.acp.policy.shell_policy),
            "acp.policy.shell_allowlist" => source_vec(&self.config.acp.policy.shell_allowlist),
            "acp.policy.allowed_write_paths" => {
                source_vec(&self.config.acp.policy.allowed_write_paths)
            }
            "acp.install.claude_acp_root" => {
                source_string(&self.config.acp.install.claude_acp_root)
            }
            "runner.full_review_interval" => source_copy(&self.config.runner.full_review_interval),
            "paths.cache_root" => source_string(&self.config.paths.cache_root),
            "paths.sessions_root" => source_string(&self.config.paths.sessions_root),
            "paths.runs_root" => source_string(&self.config.paths.runs_root),
            "paths.memory_root" => source_string(&self.config.paths.memory_root),
            "ui.prefer_split_on_open" => source_bool(&self.config.ui.prefer_split_on_open),
            "ui.colon_palette.show_help" => source_bool(&self.config.ui.colon_palette.show_help),
            "ui.footer.show_keys" => source_bool(&self.config.ui.footer.show_keys),
            "diagnostics.log_level" => source_copy(&self.config.diagnostics.log_level),
            "diagnostics.json_logs" => source_bool(&self.config.diagnostics.json_logs),
            "memory.enabled" => source_bool(&self.config.memory.enabled),
            "memory.max_topics_per_read" => source_copy(&self.config.memory.max_topics_per_read),
            "memory.journal_retention_months" => {
                source_copy(&self.config.memory.journal_retention_months)
            }
            "acp.agents.claude.env" => source_copy(&self.config.acp.agents.claude.env),
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
    }
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

    let name_w = w.min(22) as u16;
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

    let inner_w = popup_rect.width.saturating_sub(2) as usize;
    let lines: Vec<Line<'static>> = options
        .iter()
        .enumerate()
        .map(|(i, opt)| {
            let marker = if i == *selected { '>' } else { ' ' };
            let text = fit_cell(&format!("{marker} {opt}"), inner_w);
            Line::from(text)
        })
        .collect();

    Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL))
        .render(popup_rect, buf);
}

fn adaptive_lines(state: &ConfigPanelState, width: u16, height: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    let mut lines = Vec::new();
    lines.push(Line::from(header_text(&state.path, w)));
    let tab_lines = tab_bar_lines(state, w);
    for line in tab_lines {
        lines.push(Line::from(line));
    }
    lines.push(Line::from("─".repeat(w)));
    let used = lines.len();
    let body_h = height.saturating_sub(used as u16 + 2) as usize;
    let fields = visible_fields(state);
    for (pos, idx) in fields.into_iter().take(body_h).enumerate() {
        let row = field_row(state, idx, w);
        let row = if pos > 0 && state.value_for(&FIELDS[idx]).width() > value_width(w) {
            format!("  {}", field_row(state, idx, w))
        } else {
            row
        };
        lines.push(Line::from(row));
    }
    lines.push(Line::from("─".repeat(w)));
    lines.push(Line::from(help_text(state, w)));
    lines.push(Line::from(footer_line(state, w)));
    lines
}

fn tab_bar_lines(state: &ConfigPanelState, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for (i, section) in SECTIONS.iter().enumerate() {
        let active = i == state.selected_section;
        let dirty = state.section_override_count(section) > 0;
        let label = match (active, dirty) {
            (true, true) => format!("▾{section}*"),
            (true, false) => format!("▾{section}"),
            (false, true) => format!("▸{section}*"),
            (false, false) => format!("▸{section}"),
        };
        let sep = if current.is_empty() {
            String::new()
        } else {
            TAB_SEPARATOR.to_string()
        };
        let candidate = format!("{current}{sep}{label}");
        if candidate.width() <= width {
            current = candidate;
        } else {
            if !current.is_empty() {
                lines.push(fit_cell(&current, width));
            }
            current = label;
        }
        if i == SECTIONS.len() - 1 && !current.is_empty() {
            lines.push(fit_cell(&current, width));
        }
    }
    if lines.is_empty() {
        lines.push(fit_cell(&current, width));
    }
    lines
}

fn field_row(state: &ConfigPanelState, idx: usize, width: usize) -> String {
    let meta = &FIELDS[idx];
    let focused = idx == state.selected_field;
    let name_w = width.min(22);
    let tag = state.source_for(meta);
    let mut value = if focused && matches!(state.editing, Some(Editing::Integer | Editing::String))
    {
        state.edit_buffer.clone()
    } else {
        render_value(state, meta, focused)
    };
    let fixed = name_w + 3 + 1 + TAG_WIDTH;
    let val_w = width.saturating_sub(fixed).max(1);
    value = ellipsize_end(&value, val_w);
    let focus = if focused { ">" } else { " " };
    let name = fit_cell(&format!("{focus} {}", meta.label), name_w);
    format!("{name} │ {value:<val_w$} {tag:>TAG_WIDTH$}")
}

fn render_value(state: &ConfigPanelState, meta: &FieldMeta, focused: bool) -> String {
    let raw = state.value_for(meta);
    let mut value = if meta.secret && !state.reveal_topic && !raw.is_empty() {
        middle_ellipsis(&raw, 16)
    } else {
        raw
    };
    if focused && matches!(meta.kind, FieldKind::Enum(_)) {
        value.push_str(" ▼");
    }
    value
}

fn help_text(state: &ConfigPanelState, width: usize) -> String {
    if let Some(err) = state
        .current_meta()
        .and_then(|meta| state.edit_error_for(*meta, &state.edit_buffer))
    {
        return fit_cell(&err, width);
    }
    if let Some(err) = &state.save_error {
        return fit_cell(err, width);
    }
    let banner = match &state.conflict {
        Some(ConflictBanner::MtimeAdvanced) => {
            Some("mtime conflict: r reload · o overwrite · Esc keep editing")
        }
        Some(ConflictBanner::DiscardPrompt) => Some("discard unsaved changes? y discard · n keep"),
        Some(ConflictBanner::RegenerateTopicPrompt) => {
            Some("regenerate ntfy.topic? y accept · n keep")
        }
        Some(ConflictBanner::ResetSectionPrompt { .. }) => {
            Some("reset section overrides? y accept · n keep")
        }
        None => None,
    };
    if let Some(text) = banner {
        return fit_cell(text, width);
    }
    state
        .current_meta()
        .map(|meta| fit_cell(meta.description, width))
        .unwrap_or_default()
}

fn footer_line(state: &ConfigPanelState, width: usize) -> String {
    let hotkeys: &[&str] = if state.read_only {
        &["Tab section", "e edit", "Esc close"]
    } else {
        match &state.editing {
            Some(Editing::Choice { .. }) => &["↑↓ select", "Enter commit", "Esc cancel"],
            Some(Editing::Integer | Editing::String) => &["Enter commit", "Esc cancel"],
            None => &[
                "Tab section",
                "Enter edit",
                "d default",
                "Esc close",
                "Ctrl-S save",
            ],
        }
    };
    let mut first = String::new();
    for item in hotkeys {
        let next = if first.is_empty() {
            item.to_string()
        } else {
            format!("{first} · {item}")
        };
        if next.width() <= width {
            first = next;
        }
    }
    let invalid = state.current_validation_error();
    let second = if let Some(reason) = invalid {
        reason
    } else if state.dirty {
        format!(
            "unsaved · {} changes · applies on next launch",
            dirty_count(state)
        )
    } else {
        state.status.clone()
    };
    fit_cell(&format!("{first}  |  {second}"), width)
}

fn header_text(path: &Path, width: usize) -> String {
    let title = "codexize · config";
    let path = path.display().to_string();
    let reserve = title.width() + 3;
    let path_w = width.saturating_sub(reserve);
    let shown = middle_ellipsis(&path, path_w);
    fit_cell(&format!("{title} · {shown}"), width)
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
    FIELDS
        .iter()
        .filter(|meta| match meta.key {
            "meta.version" => false,
            _ => state.source_for(meta) == "override",
        })
        .count()
}

fn value_width(width: usize) -> usize {
    width.saturating_sub(22 + TAG_WIDTH + 3)
}

fn fit_cell(value: &str, width: usize) -> String {
    let clipped = ellipsize_end(value, width);
    let pad = width.saturating_sub(clipped.width());
    format!("{clipped}{}", " ".repeat(pad))
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
    let parts = values
        .iter()
        .map(|v| format!("\"{v}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{parts}]")
}

fn format_map(map: &std::collections::BTreeMap<String, String>) -> String {
    let parts: Vec<String> = map.iter().map(|(k, v)| format!("{k} = \"{v}\"")).collect();
    format!("{{ {} }}", parts.join(", "))
}

fn wrap_index(current: usize, len: usize, delta: isize) -> usize {
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

fn source_bool(value: &Override<bool>) -> &'static str {
    if value.is_explicit() {
        "override"
    } else {
        "(def)"
    }
}

fn source_string(value: &Override<String>) -> &'static str {
    if value.is_explicit() {
        "override"
    } else {
        "(def)"
    }
}

fn source_vec(value: &Override<Vec<String>>) -> &'static str {
    if value.is_explicit() {
        "override"
    } else {
        "(def)"
    }
}

fn source_copy<T>(value: &Override<T>) -> &'static str {
    if value.is_explicit() {
        "override"
    } else {
        "(def)"
    }
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
        ConfigPanelState::open(&config, PathBuf::from("/tmp/example/config.toml"), false)
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
        assert!(text.contains("Ctrl-S save"));
        assert!(text.contains("Esc close"));
        assert!(text.contains("d default"));
        assert!(text.contains("Enter edit"));
        assert!(text.contains("Tab section"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn adaptive_snapshot_width_60_shows_tab_bar() {
        let mut state = state_with_overrides();
        state.selected_section = SECTIONS.iter().position(|s| *s == "paths").unwrap();
        state.select_first_field_in_current_section();
        let text = render_to_text(&state, 60, 16);
        assert!(text.contains("▸meta"));
        assert!(text.contains("▾paths"));
        insta::assert_snapshot!(text);
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
        state.selected_section = SECTIONS.iter().position(|s| *s == "ntfy").unwrap();
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
    fn enter_on_bool_opens_dropdown_with_current_value_selected() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        // Default is true; expect "true" preselected in the dropdown.
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Editing::Choice {
            key,
            options,
            selected,
        }) = state.editing.as_ref()
        else {
            panic!("expected Choice editing state, got {:?}", state.editing);
        };
        assert_eq!(*key, "ntfy.enabled");
        assert_eq!(options, &vec!["true".to_string(), "false".to_string()]);
        assert_eq!(options[*selected], "true");
        assert_eq!(state.value_for(state.current_meta().unwrap()), "true");
    }

    #[test]
    fn enter_on_dropdown_commits_and_returns_to_nav() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Move highlight to "false" then commit.
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(state.editing.is_none());
        assert_eq!(state.value_for(state.current_meta().unwrap()), "false");
        assert!(state.dirty);
    }

    #[test]
    fn esc_in_dropdown_returns_to_nav_without_mutation() {
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        let value_before = state.value_for(state.current_meta().unwrap());
        let dirty_before = state.dirty;
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // Move the highlight then cancel — must not commit.
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.editing.is_none());
        assert_eq!(state.value_for(state.current_meta().unwrap()), value_before);
        assert_eq!(state.dirty, dirty_before);
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
        let mut state = state_with_overrides();
        focus_field(&mut state, "ntfy.enabled");
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        insta::assert_snapshot!(render_to_text(&state, 80, 18));
    }

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
