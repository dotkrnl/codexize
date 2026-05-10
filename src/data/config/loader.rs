//! `config.toml` loader and saver built on `toml_edit`.
//!
//! Loader contract (spec §3):
//! - Missing file → baked defaults; nothing is written.
//! - Strict unknown-key rejection with nearest-key suggestion.
//! - Type mismatches and unsupported versions return structured errors.
//!
//! Saver writes the **sparse** form: only fields whose `Override<T>`
//! is `is_explicit() == true` and whose value differs from the baked
//! default are emitted. Section tables that contain no overrides drop
//! out entirely.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use toml_edit::{DocumentMut, Item, Value};

use super::fmt::{format_inline_env as inline_env, format_string_array, toml_quote};
use super::paths::config_path;
use super::schema::{
    AcpAgentSection, AcpAgents, AcpInstallSection, AcpPolicySection, Config, DiagnosticsSection,
    EffortMapping, LogLevel, MemorySection, MetaSection, NtfyDetailMode, NtfyEvents, NtfySection,
    Override, PathsSection, ProviderEntry, RunnerSection, SUPPORTED_VERSION, ShellPolicy,
    UiColonPalette, UiFooter, UiSection,
};
use crate::logic::selection::baked;
use crate::selection::{CliKind, SubscriptionKind};

/// Structured loader error. The CLI/TUI render `to_string()`; tests match
/// on the variant tag.
#[derive(Debug)]
pub enum LoadError {
    /// Underlying I/O failure on a path we expected to read.
    Io(std::io::Error),
    /// `toml_edit` parse failure — the carried message already includes
    /// line/column from the parser.
    Parse(String),
    /// Unsupported `[meta] version` value.
    UnsupportedVersion { found: i64 },
    /// Unknown key, optionally with the nearest valid sibling.
    UnknownKey {
        path: String,
        line: usize,
        column: usize,
        suggestion: Option<String>,
    },
    /// Type mismatch (e.g. expected bool, found string).
    TypeMismatch {
        path: String,
        expected: &'static str,
        line: usize,
        column: usize,
    },
    /// Validation rule violation from `Config::validate()` — the loader
    /// runs validation after decoding so the binary refuses to launch
    /// with a clear message instead of silently degrading.
    Validation(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "config: io: {e}"),
            Self::Parse(msg) => write!(f, "config: parse: {msg}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "config: unsupported [meta] version = {found}; this binary supports version {SUPPORTED_VERSION}"
            ),
            Self::UnknownKey {
                path,
                line,
                column,
                suggestion,
            } => match suggestion {
                Some(s) => write!(
                    f,
                    "config: unknown key '{path}' at line {line}, column {column} (did you mean '{s}'?)"
                ),
                None => write!(
                    f,
                    "config: unknown key '{path}' at line {line}, column {column}"
                ),
            },
            Self::TypeMismatch {
                path,
                expected,
                line,
                column,
            } => write!(
                f,
                "config: '{path}' expected {expected} at line {line}, column {column}"
            ),
            Self::Validation(msg) => write!(f, "config: validation: {msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Load the resolved [`config_path`] or fall back to baked defaults if
/// the file is absent. Other I/O errors surface as
/// [`LoadError::Io`] — silent degradation is the wrong default per
/// spec §3.
pub fn load_or_default() -> Result<Config, LoadError> {
    let path = config_path();
    load_from_path(&path)
}

/// Same as [`load_or_default`] but lets the caller pin the path; used
/// by `validate <path>` and the loader tests.
pub fn load_from_path(path: &Path) -> Result<Config, LoadError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::baked_defaults());
        }
        Err(err) => return Err(LoadError::Io(err)),
    };
    load_str(&text)
}

/// Parse a TOML string into a [`Config`], applying validation and
/// resolving `$HOME` / `~` in the `[paths]` and `[acp.install]` values.
pub fn load_str(text: &str) -> Result<Config, LoadError> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| LoadError::Parse(format!("{e}")))?;

    let mut config = Config::baked_defaults();
    let known_top: &[&str] = &[
        "meta",
        "ntfy",
        "acp",
        "runner",
        "paths",
        "ui",
        "diagnostics",
        "memory",
        "providers",
    ];

    for (key, item) in doc.iter() {
        match key {
            "meta" => decode_meta(item, &mut config.meta, key)?,
            "ntfy" => decode_ntfy(item, &mut config.ntfy, key)?,
            "acp" => decode_acp(item, &mut config.acp, key)?,
            "runner" => decode_runner(item, &mut config.runner, key)?,
            "paths" => decode_paths(item, &mut config.paths, key)?,
            "ui" => decode_ui(item, &mut config.ui, key)?,
            "diagnostics" => decode_diagnostics(item, &mut config.diagnostics, key)?,
            "memory" => decode_memory(item, &mut config.memory, key)?,
            "providers" => decode_providers(item, &mut config.providers, key)?,
            unknown => {
                let (line, column) = item_position(item);
                return Err(LoadError::UnknownKey {
                    path: unknown.to_string(),
                    line,
                    column,
                    suggestion: super::util::nearest(unknown, known_top, 3),
                });
            }
        }
    }

    config.validate().map_err(LoadError::Validation)?;
    Ok(config)
}

fn decode_meta(item: &Item, out: &mut MetaSection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    for (k, v) in table.iter() {
        match k {
            "version" => {
                let n = require_integer(v, &dotted(parent, "version"))?;
                if n as u32 != SUPPORTED_VERSION {
                    return Err(LoadError::UnsupportedVersion { found: n });
                }
                out.version = n as u32;
            }
            other => {
                return unknown(parent, other, v, &["version"]);
            }
        }
    }
    Ok(())
}

fn decode_ntfy(item: &Item, out: &mut NtfySection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &[
        "enabled",
        "server",
        "topic",
        "detail_mode",
        "retry_attempts",
        "retry_delay_ms",
        "http_timeout_secs",
        "body_max_bytes",
        "excerpt_max_chars",
        "created_at",
        "updated_at",
        "events",
    ];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "enabled" => out.enabled = Override::explicit(require_bool(v, &path)?),
            "server" => out.server = Override::explicit(require_string(v, &path)?),
            "topic" => out.topic = Override::explicit(require_string(v, &path)?),
            "detail_mode" => {
                let raw = require_string(v, &path)?;
                let parsed = NtfyDetailMode::parse(&raw).ok_or_else(|| {
                    LoadError::Validation(format!(
                        "{path} = {raw:?} is not one of {:?}",
                        NtfyDetailMode::variants()
                    ))
                })?;
                out.detail_mode = Override::explicit(parsed);
            }
            "retry_attempts" => {
                out.retry_attempts = Override::explicit(require_u32(v, &path)?);
            }
            "retry_delay_ms" => {
                out.retry_delay_ms = Override::explicit(require_u64(v, &path)?);
            }
            "http_timeout_secs" => {
                out.http_timeout_secs = Override::explicit(require_u32(v, &path)?);
            }
            "body_max_bytes" => {
                out.body_max_bytes = Override::explicit(require_u64(v, &path)?);
            }
            "excerpt_max_chars" => {
                out.excerpt_max_chars = Override::explicit(require_u32(v, &path)?);
            }
            "created_at" => out.created_at = Override::explicit(parse_timestamp(v, &path)?),
            "updated_at" => out.updated_at = Override::explicit(parse_timestamp(v, &path)?),
            "events" => decode_ntfy_events(v, &mut out.events, &path)?,
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_ntfy_events(item: &Item, out: &mut NtfyEvents, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["phase_wait", "interactive_wait", "pipeline_done"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "phase_wait" => out.phase_wait = Override::explicit(require_bool(v, &path)?),
            "interactive_wait" => {
                out.interactive_wait = Override::explicit(require_bool(v, &path)?)
            }
            "pipeline_done" => out.pipeline_done = Override::explicit(require_bool(v, &path)?),
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_acp(
    item: &Item,
    out: &mut super::schema::AcpSection,
    parent: &str,
) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["policy", "install", "agents"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "policy" => decode_acp_policy(v, &mut out.policy, &path)?,
            "install" => decode_acp_install(v, &mut out.install, &path)?,
            "agents" => decode_acp_agents(v, &mut out.agents, &path)?,
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_acp_policy(
    item: &Item,
    out: &mut AcpPolicySection,
    parent: &str,
) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &[
        "shell_policy",
        "shell_allowlist",
        "enforce_readonly_workspace",
        "allowed_write_paths",
    ];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "shell_policy" => {
                let raw = require_string(v, &path)?;
                let parsed = ShellPolicy::parse(&raw).ok_or_else(|| {
                    LoadError::Validation(format!(
                        "{path} = {raw:?} is not one of {:?}",
                        ShellPolicy::variants()
                    ))
                })?;
                out.shell_policy = Override::explicit(parsed);
            }
            "shell_allowlist" => {
                out.shell_allowlist = Override::explicit(require_string_array(v, &path)?);
            }
            "enforce_readonly_workspace" => {
                out.enforce_readonly_workspace = Override::explicit(require_bool(v, &path)?);
            }
            "allowed_write_paths" => {
                out.allowed_write_paths = Override::explicit(require_string_array(v, &path)?);
            }
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_acp_install(
    item: &Item,
    out: &mut AcpInstallSection,
    parent: &str,
) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["claude_acp_root", "prefer_local_claude_acp"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "claude_acp_root" => {
                // Store the raw string verbatim; `acp_install_view()`
                // expands `$HOME`/`~` at the read site so the on-disk
                // file remains stable across machines with different
                // `$HOME` values.
                out.claude_acp_root = Override::explicit(require_string(v, &path)?);
            }
            "prefer_local_claude_acp" => {
                out.prefer_local_claude_acp = Override::explicit(require_bool(v, &path)?);
            }
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_acp_agents(item: &Item, out: &mut AcpAgents, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["claude", "codex", "gemini", "kimi", "opencode"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "claude" => decode_acp_agent(v, &mut out.claude, &path)?,
            "codex" => decode_acp_agent(v, &mut out.codex, &path)?,
            "gemini" => decode_acp_agent(v, &mut out.gemini, &path)?,
            "kimi" => decode_acp_agent(v, &mut out.kimi, &path)?,
            "opencode" => decode_acp_agent(v, &mut out.opencode, &path)?,
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_acp_agent(item: &Item, out: &mut AcpAgentSection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["enabled", "program", "args", "env"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "enabled" => out.enabled = Override::explicit(require_bool(v, &path)?),
            "program" => out.program = Override::explicit(require_string(v, &path)?),
            "args" => out.args = Override::explicit(require_string_array(v, &path)?),
            "env" => out.env = Override::explicit(require_string_map(v, &path)?),
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_runner(item: &Item, out: &mut RunnerSection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["full_review_interval"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "full_review_interval" => {
                out.full_review_interval = Override::explicit(require_u32(v, &path)?);
            }
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_paths(item: &Item, out: &mut PathsSection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["cache_root", "sessions_root", "runs_root", "memory_root"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        let target: &mut Override<String> = match k {
            "cache_root" => &mut out.cache_root,
            "sessions_root" => &mut out.sessions_root,
            "runs_root" => &mut out.runs_root,
            "memory_root" => &mut out.memory_root,
            other => return unknown(parent, other, v, known),
        };
        // Path strings are stored verbatim; `paths_view()` expands
        // `$HOME`/`~` at the read site (see comment in
        // `decode_acp_install`).
        *target = Override::explicit(require_string(v, &path)?);
    }
    Ok(())
}

fn decode_ui(item: &Item, out: &mut UiSection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["prefer_split_on_open", "colon_palette", "footer"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "prefer_split_on_open" => {
                out.prefer_split_on_open = Override::explicit(require_bool(v, &path)?);
            }
            "colon_palette" => decode_ui_palette(v, &mut out.colon_palette, &path)?,
            "footer" => decode_ui_footer(v, &mut out.footer, &path)?,
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_ui_palette(item: &Item, out: &mut UiColonPalette, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["show_help"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "show_help" => out.show_help = Override::explicit(require_bool(v, &path)?),
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_ui_footer(item: &Item, out: &mut UiFooter, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["show_keys"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "show_keys" => out.show_keys = Override::explicit(require_bool(v, &path)?),
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_diagnostics(
    item: &Item,
    out: &mut DiagnosticsSection,
    parent: &str,
) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["log_level", "json_logs"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "log_level" => {
                let raw = require_string(v, &path)?;
                let parsed = LogLevel::parse(&raw).ok_or_else(|| {
                    LoadError::Validation(format!(
                        "{path} = {raw:?} is not one of {:?}",
                        LogLevel::variants()
                    ))
                })?;
                out.log_level = Override::explicit(parsed);
            }
            "json_logs" => out.json_logs = Override::explicit(require_bool(v, &path)?),
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_memory(item: &Item, out: &mut MemorySection, parent: &str) -> Result<(), LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["enabled", "max_topics_per_read", "journal_retention_months"];
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "enabled" => out.enabled = Override::explicit(require_bool(v, &path)?),
            "max_topics_per_read" => {
                out.max_topics_per_read = Override::explicit(require_u32(v, &path)?);
            }
            "journal_retention_months" => {
                out.journal_retention_months = Override::explicit(require_u32(v, &path)?);
            }
            other => return unknown(parent, other, v, known),
        }
    }
    Ok(())
}

fn decode_providers(
    item: &Item,
    out: &mut Override<Vec<ProviderEntry>>,
    parent: &str,
) -> Result<(), LoadError> {
    let aot = item
        .as_array_of_tables()
        .ok_or_else(|| LoadError::TypeMismatch {
            path: parent.to_string(),
            expected: "array of tables",
            line: 1,
            column: 1,
        })?;
    let known: &[&str] = &[
        "launch",
        "model",
        "subscription",
        "enabled",
        "free",
        "official",
        "quota_disabled",
        "cheap_eligible",
        "tough_eligible",
        "effort_eligible",
        "effort_mapping",
        "quota_lookup_key",
        "display_order",
    ];
    let mut entries: Vec<ProviderEntry> = Vec::with_capacity(aot.len());
    for (i, table) in aot.iter().enumerate() {
        let path = |key: &str| format!("{parent}[{i}].{key}");
        let mut cli: Option<CliKind> = None;
        let mut launch_name: Option<String> = None;
        let mut model: Option<String> = None;
        let mut subscription: Option<SubscriptionKind> = None;
        let mut quota_lookup_key: Option<String> = None;
        let mut enabled = true;
        let mut free = false;
        let mut official = false;
        let mut quota_disabled = false;
        let mut cheap_eligible = false;
        let mut tough_eligible = false;
        let mut effort_eligible = false;
        let mut effort_mapping = EffortMapping::default();
        let mut display_order: u16 = 0;
        for (k, v) in table.iter() {
            match k {
                "launch" => {
                    let raw = require_string(v, &path(k))?;
                    let (cli_str, launch_str) = raw.split_once('/').ok_or_else(|| {
                        LoadError::Validation(format!("{} = {:?} must contain '/'", path(k), raw))
                    })?;
                    if cli_str.is_empty() {
                        return Err(LoadError::Validation(format!(
                            "{} prefix (cli) is empty",
                            path(k)
                        )));
                    }
                    if launch_str.is_empty() {
                        return Err(LoadError::Validation(format!(
                            "{} suffix (launch_name) is empty",
                            path(k)
                        )));
                    }
                    cli = Some(CliKind::parse(cli_str).ok_or_else(|| {
                        LoadError::Validation(format!(
                            "{} = {:?}: cli prefix {:?} not in {:?}",
                            path(k),
                            raw,
                            cli_str,
                            CliKind::variants()
                        ))
                    })?);
                    launch_name = Some(launch_str.to_string());
                }
                "model" => model = Some(require_string(v, &path(k))?),
                "subscription" => {
                    let raw = require_string(v, &path(k))?;
                    subscription = Some(
                        crate::logic::selection::assemble::parse_subscription_str(&raw)
                            .ok_or_else(|| {
                                LoadError::Validation(format!(
                                    "{} = {:?} not a recognized subscription",
                                    path(k),
                                    raw
                                ))
                            })?,
                    );
                }
                "quota_lookup_key" => {
                    quota_lookup_key = Some(require_string(v, &path(k))?);
                }
                "enabled" => enabled = require_bool(v, &path(k))?,
                "free" => free = require_bool(v, &path(k))?,
                "official" => official = require_bool(v, &path(k))?,
                "quota_disabled" => quota_disabled = require_bool(v, &path(k))?,
                "cheap_eligible" => cheap_eligible = require_bool(v, &path(k))?,
                "tough_eligible" => tough_eligible = require_bool(v, &path(k))?,
                "effort_eligible" => effort_eligible = require_bool(v, &path(k))?,
                "effort_mapping" => effort_mapping = decode_effort_mapping(v, &path(k))?,
                "display_order" => {
                    let n = require_integer(v, &path(k))?;
                    if !(0..=u16::MAX as i64).contains(&n) {
                        return Err(LoadError::Validation(format!(
                            "{} = {n} is out of range for u16",
                            path(k),
                        )));
                    }
                    display_order = n as u16;
                }
                other => {
                    let (line, column) = item_position(item);
                    return Err(LoadError::UnknownKey {
                        path: format!("{parent}[{i}].{other}"),
                        line,
                        column,
                        suggestion: super::util::nearest(other, known, 3),
                    });
                }
            }
        }
        let cli = cli.ok_or_else(|| {
            LoadError::Validation(format!("{parent}[{i}]: missing required \"launch\""))
        })?;
        let launch_name = launch_name.expect("set together with cli");
        let model = model.ok_or_else(|| {
            LoadError::Validation(format!("{parent}[{i}]: missing required \"model\""))
        })?;
        let subscription = subscription.ok_or_else(|| {
            LoadError::Validation(format!("{parent}[{i}]: missing required \"subscription\""))
        })?;
        entries.push(ProviderEntry {
            cli,
            launch_name,
            model,
            subscription,
            enabled,
            free,
            official,
            quota_disabled,
            cheap_eligible,
            tough_eligible,
            effort_eligible,
            effort_mapping,
            quota_lookup_key,
            display_order,
        });
    }
    *out = Override::explicit(entries);
    Ok(())
}

fn decode_effort_mapping(item: &Item, parent: &str) -> Result<EffortMapping, LoadError> {
    let table = require_table(item, parent)?;
    let known: &[&str] = &["cheap", "normal", "tough"];
    let mut mapping = EffortMapping::default();
    for (k, v) in table.iter() {
        let path = dotted(parent, k);
        match k {
            "cheap" => mapping.cheap = require_string(v, &path)?,
            "normal" => mapping.normal = require_string(v, &path)?,
            "tough" => mapping.tough = require_string(v, &path)?,
            other => {
                unknown(parent, other, v, known)?;
                unreachable!("unknown returns Err");
            }
        }
    }
    Ok(mapping)
}

// --- helpers --------------------------------------------------------------

fn dotted(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}.{child}")
    }
}

fn unknown(parent: &str, key: &str, item: &Item, known: &[&str]) -> Result<(), LoadError> {
    let (line, column) = item_position(item);
    Err(LoadError::UnknownKey {
        path: dotted(parent, key),
        line,
        column,
        suggestion: super::util::nearest(key, known, 3),
    })
}

fn item_position(item: &Item) -> (usize, usize) {
    // toml_edit exposes spans on the prefix decor; for v0.25 we report
    // the best-effort 1/1 fallback when no span is present (e.g. items
    // synthesised programmatically). Real parse positions go through
    // the dedicated Parse error variant.
    let _ = item;
    (1, 1)
}

fn require_table<'a>(item: &'a Item, path: &str) -> Result<&'a toml_edit::Table, LoadError> {
    item.as_table().ok_or_else(|| LoadError::TypeMismatch {
        path: path.to_string(),
        expected: "table",
        line: 1,
        column: 1,
    })
}

fn require_bool(item: &Item, path: &str) -> Result<bool, LoadError> {
    item.as_bool().ok_or_else(|| LoadError::TypeMismatch {
        path: path.to_string(),
        expected: "bool",
        line: 1,
        column: 1,
    })
}

fn require_string(item: &Item, path: &str) -> Result<String, LoadError> {
    item.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| LoadError::TypeMismatch {
            path: path.to_string(),
            expected: "string",
            line: 1,
            column: 1,
        })
}

fn require_integer(item: &Item, path: &str) -> Result<i64, LoadError> {
    item.as_integer().ok_or_else(|| LoadError::TypeMismatch {
        path: path.to_string(),
        expected: "integer",
        line: 1,
        column: 1,
    })
}

fn require_u32(item: &Item, path: &str) -> Result<u32, LoadError> {
    let n = require_integer(item, path)?;
    if !(0..=u32::MAX as i64).contains(&n) {
        return Err(LoadError::Validation(format!(
            "{path} = {n} is out of range for a 32-bit unsigned integer"
        )));
    }
    Ok(n as u32)
}

fn require_u64(item: &Item, path: &str) -> Result<u64, LoadError> {
    let n = require_integer(item, path)?;
    if n < 0 {
        return Err(LoadError::Validation(format!(
            "{path} = {n} must be non-negative"
        )));
    }
    Ok(n as u64)
}

fn require_string_array(item: &Item, path: &str) -> Result<Vec<String>, LoadError> {
    let arr = item.as_array().ok_or_else(|| LoadError::TypeMismatch {
        path: path.to_string(),
        expected: "array of strings",
        line: 1,
        column: 1,
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let s = v.as_str().ok_or_else(|| LoadError::TypeMismatch {
            path: format!("{path}[{i}]"),
            expected: "string",
            line: 1,
            column: 1,
        })?;
        out.push(s.to_string());
    }
    Ok(out)
}

fn require_string_map(item: &Item, path: &str) -> Result<BTreeMap<String, String>, LoadError> {
    // env may be either an inline table value or a regular [section] table.
    let mut out = BTreeMap::new();
    if let Some(table) = item.as_table() {
        for (k, v) in table.iter() {
            let s = v.as_str().ok_or_else(|| LoadError::TypeMismatch {
                path: format!("{path}.{k}"),
                expected: "string",
                line: 1,
                column: 1,
            })?;
            out.insert(k.to_string(), s.to_string());
        }
        return Ok(out);
    }
    if let Some(Value::InlineTable(inline)) = item.as_value() {
        for (k, v) in inline.iter() {
            let s = v.as_str().ok_or_else(|| LoadError::TypeMismatch {
                path: format!("{path}.{k}"),
                expected: "string",
                line: 1,
                column: 1,
            })?;
            out.insert(k.to_string(), s.to_string());
        }
        return Ok(out);
    }
    Err(LoadError::TypeMismatch {
        path: path.to_string(),
        expected: "table of strings",
        line: 1,
        column: 1,
    })
}

fn parse_timestamp(item: &Item, path: &str) -> Result<Option<DateTime<Utc>>, LoadError> {
    let raw = require_string(item, path)?;
    if raw.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| Some(dt.with_timezone(&Utc)))
        .map_err(|e| {
            LoadError::Validation(format!(
                "{path} = {raw:?} is not a valid RFC-3339 datetime: {e}"
            ))
        })
}

// --- save -----------------------------------------------------------------

/// Atomically write the sparse normalized form of `config` to the
/// resolved path. Only fields whose `Override<T>` is `is_explicit()` and
/// differs from the baked default get emitted; sections containing zero
/// such fields disappear entirely.
pub fn save_atomic(config: &Config) -> Result<(), LoadError> {
    save_atomic_to(&config_path(), config)
}

/// Variant of [`save_atomic`] that targets `path` directly. The CLI
/// `validate <path>` and the integration tests exercise non-default
/// paths through `CODEXIZE_CONFIG`, but anything that already holds a
/// resolved path (e.g. the ntfy-alias rewrite) calls this directly so
/// it doesn't have to go back through the env.
pub fn save_atomic_to(path: &Path, config: &Config) -> Result<(), LoadError> {
    let bytes = render_sparse(config).into_bytes();
    crate::data::atomic::atomic_write(path, &bytes)
        .map_err(|e| LoadError::Io(std::io::Error::other(format!("{e:#}"))))
}

/// Render the sparse on-disk form of `config`. Public for tests and the
/// pending CLI `set`/`unset`/`reset <section>` paths.
pub fn render_sparse(config: &Config) -> String {
    let baked = Config::baked_defaults();
    let mut out = String::new();

    // Always stamp the current schema generation so sparse files identify
    // the format that wrote them.
    out.push_str("[meta]\n");
    let _ = writeln!(out, "version = {}", config.meta.version);

    let mut ntfy_block = String::new();
    push_explicit_bool(
        &mut ntfy_block,
        "enabled",
        &config.ntfy.enabled,
        &baked.ntfy.enabled,
    );
    push_explicit_string(
        &mut ntfy_block,
        "server",
        &config.ntfy.server,
        &baked.ntfy.server,
    );
    push_explicit_string(
        &mut ntfy_block,
        "topic",
        &config.ntfy.topic,
        &baked.ntfy.topic,
    );
    if config.ntfy.detail_mode.is_explicit()
        && config.ntfy.detail_mode.value() != baked.ntfy.detail_mode.value()
    {
        let _ = writeln!(
            ntfy_block,
            "detail_mode = \"{}\"",
            config.ntfy.detail_mode.value().as_str()
        );
    }
    push_explicit_int(
        &mut ntfy_block,
        "retry_attempts",
        config.ntfy.retry_attempts.is_explicit(),
        *config.ntfy.retry_attempts.value() as i128,
        *baked.ntfy.retry_attempts.value() as i128,
    );
    push_explicit_int(
        &mut ntfy_block,
        "retry_delay_ms",
        config.ntfy.retry_delay_ms.is_explicit(),
        *config.ntfy.retry_delay_ms.value() as i128,
        *baked.ntfy.retry_delay_ms.value() as i128,
    );
    push_explicit_int(
        &mut ntfy_block,
        "http_timeout_secs",
        config.ntfy.http_timeout_secs.is_explicit(),
        *config.ntfy.http_timeout_secs.value() as i128,
        *baked.ntfy.http_timeout_secs.value() as i128,
    );
    push_explicit_int(
        &mut ntfy_block,
        "body_max_bytes",
        config.ntfy.body_max_bytes.is_explicit(),
        *config.ntfy.body_max_bytes.value() as i128,
        *baked.ntfy.body_max_bytes.value() as i128,
    );
    push_explicit_int(
        &mut ntfy_block,
        "excerpt_max_chars",
        config.ntfy.excerpt_max_chars.is_explicit(),
        *config.ntfy.excerpt_max_chars.value() as i128,
        *baked.ntfy.excerpt_max_chars.value() as i128,
    );
    if let Some(ts) = config.ntfy.created_at.value() {
        let _ = writeln!(
            ntfy_block,
            "created_at = \"{}\"",
            ts.to_rfc3339_opts(SecondsFormat::Secs, true)
        );
    }
    if let Some(ts) = config.ntfy.updated_at.value() {
        let _ = writeln!(
            ntfy_block,
            "updated_at = \"{}\"",
            ts.to_rfc3339_opts(SecondsFormat::Secs, true)
        );
    }
    if !ntfy_block.is_empty() {
        out.push_str("\n[ntfy]\n");
        out.push_str(&ntfy_block);
    }

    let mut events_block = String::new();
    push_explicit_bool(
        &mut events_block,
        "phase_wait",
        &config.ntfy.events.phase_wait,
        &baked.ntfy.events.phase_wait,
    );
    push_explicit_bool(
        &mut events_block,
        "interactive_wait",
        &config.ntfy.events.interactive_wait,
        &baked.ntfy.events.interactive_wait,
    );
    push_explicit_bool(
        &mut events_block,
        "pipeline_done",
        &config.ntfy.events.pipeline_done,
        &baked.ntfy.events.pipeline_done,
    );
    if !events_block.is_empty() {
        out.push_str("\n[ntfy.events]\n");
        out.push_str(&events_block);
    }

    let mut policy_block = String::new();
    if config.acp.policy.shell_policy.is_explicit()
        && config.acp.policy.shell_policy.value() != baked.acp.policy.shell_policy.value()
    {
        let _ = writeln!(
            policy_block,
            "shell_policy = \"{}\"",
            config.acp.policy.shell_policy.value().as_str()
        );
    }
    push_explicit_string_array(
        &mut policy_block,
        "shell_allowlist",
        &config.acp.policy.shell_allowlist,
        &baked.acp.policy.shell_allowlist,
    );
    push_explicit_bool(
        &mut policy_block,
        "enforce_readonly_workspace",
        &config.acp.policy.enforce_readonly_workspace,
        &baked.acp.policy.enforce_readonly_workspace,
    );
    push_explicit_string_array(
        &mut policy_block,
        "allowed_write_paths",
        &config.acp.policy.allowed_write_paths,
        &baked.acp.policy.allowed_write_paths,
    );
    if !policy_block.is_empty() {
        out.push_str("\n[acp.policy]\n");
        out.push_str(&policy_block);
    }

    let mut install_block = String::new();
    push_explicit_string(
        &mut install_block,
        "claude_acp_root",
        &config.acp.install.claude_acp_root,
        &baked.acp.install.claude_acp_root,
    );
    push_explicit_bool(
        &mut install_block,
        "prefer_local_claude_acp",
        &config.acp.install.prefer_local_claude_acp,
        &baked.acp.install.prefer_local_claude_acp,
    );
    if !install_block.is_empty() {
        out.push_str("\n[acp.install]\n");
        out.push_str(&install_block);
    }

    for (vendor, agent, baked_agent) in [
        (
            "claude",
            &config.acp.agents.claude,
            &baked.acp.agents.claude,
        ),
        ("codex", &config.acp.agents.codex, &baked.acp.agents.codex),
        (
            "gemini",
            &config.acp.agents.gemini,
            &baked.acp.agents.gemini,
        ),
        ("kimi", &config.acp.agents.kimi, &baked.acp.agents.kimi),
        (
            "opencode",
            &config.acp.agents.opencode,
            &baked.acp.agents.opencode,
        ),
    ] {
        let mut block = String::new();
        push_explicit_bool(&mut block, "enabled", &agent.enabled, &baked_agent.enabled);
        push_explicit_string(&mut block, "program", &agent.program, &baked_agent.program);
        push_explicit_string_array(&mut block, "args", &agent.args, &baked_agent.args);
        if agent.env.is_explicit() && agent.env.value() != baked_agent.env.value() {
            let _ = writeln!(block, "env = {}", inline_env(agent.env.value()));
        }
        if !block.is_empty() {
            let _ = write!(out, "\n[acp.agents.{vendor}]\n{block}");
        }
    }

    let mut runner_block = String::new();
    push_explicit_int(
        &mut runner_block,
        "full_review_interval",
        config.runner.full_review_interval.is_explicit(),
        *config.runner.full_review_interval.value() as i128,
        *baked.runner.full_review_interval.value() as i128,
    );
    if !runner_block.is_empty() {
        out.push_str("\n[runner]\n");
        out.push_str(&runner_block);
    }

    let mut paths_block = String::new();
    push_explicit_string(
        &mut paths_block,
        "cache_root",
        &config.paths.cache_root,
        &baked.paths.cache_root,
    );
    push_explicit_string(
        &mut paths_block,
        "sessions_root",
        &config.paths.sessions_root,
        &baked.paths.sessions_root,
    );
    push_explicit_string(
        &mut paths_block,
        "runs_root",
        &config.paths.runs_root,
        &baked.paths.runs_root,
    );
    push_explicit_string(
        &mut paths_block,
        "memory_root",
        &config.paths.memory_root,
        &baked.paths.memory_root,
    );
    if !paths_block.is_empty() {
        out.push_str("\n[paths]\n");
        out.push_str(&paths_block);
    }

    let mut ui_block = String::new();
    push_explicit_bool(
        &mut ui_block,
        "prefer_split_on_open",
        &config.ui.prefer_split_on_open,
        &baked.ui.prefer_split_on_open,
    );
    if !ui_block.is_empty() {
        out.push_str("\n[ui]\n");
        out.push_str(&ui_block);
    }
    let mut palette_block = String::new();
    push_explicit_bool(
        &mut palette_block,
        "show_help",
        &config.ui.colon_palette.show_help,
        &baked.ui.colon_palette.show_help,
    );
    if !palette_block.is_empty() {
        out.push_str("\n[ui.colon_palette]\n");
        out.push_str(&palette_block);
    }
    let mut footer_block = String::new();
    push_explicit_bool(
        &mut footer_block,
        "show_keys",
        &config.ui.footer.show_keys,
        &baked.ui.footer.show_keys,
    );
    if !footer_block.is_empty() {
        out.push_str("\n[ui.footer]\n");
        out.push_str(&footer_block);
    }

    let mut diag_block = String::new();
    if config.diagnostics.log_level.is_explicit()
        && config.diagnostics.log_level.value() != baked.diagnostics.log_level.value()
    {
        let _ = writeln!(
            diag_block,
            "log_level = \"{}\"",
            config.diagnostics.log_level.value().as_str()
        );
    }
    push_explicit_bool(
        &mut diag_block,
        "json_logs",
        &config.diagnostics.json_logs,
        &baked.diagnostics.json_logs,
    );
    if !diag_block.is_empty() {
        out.push_str("\n[diagnostics]\n");
        out.push_str(&diag_block);
    }

    let mut mem_block = String::new();
    push_explicit_bool(
        &mut mem_block,
        "enabled",
        &config.memory.enabled,
        &baked.memory.enabled,
    );
    push_explicit_int(
        &mut mem_block,
        "max_topics_per_read",
        config.memory.max_topics_per_read.is_explicit(),
        *config.memory.max_topics_per_read.value() as i128,
        *baked.memory.max_topics_per_read.value() as i128,
    );
    push_explicit_int(
        &mut mem_block,
        "journal_retention_months",
        config.memory.journal_retention_months.is_explicit(),
        *config.memory.journal_retention_months.value() as i128,
        *baked.memory.journal_retention_months.value() as i128,
    );
    if !mem_block.is_empty() {
        out.push_str("\n[memory]\n");
        out.push_str(&mem_block);
    }

    // [[providers]] — sparse-save: omit when empty/default.
    if config.providers.is_explicit() && !config.providers.value().is_empty() {
        let baked_default = EffortMapping::default();
        for entry in config.providers.value() {
            out.push_str("\n[[providers]]\n");
            let _ = writeln!(
                out,
                "launch = {}",
                toml_quote(&format!("{}/{}", entry.cli.as_str(), entry.launch_name))
            );
            let _ = writeln!(out, "model = {}", toml_quote(&entry.model));
            let _ = writeln!(
                out,
                "subscription = {}",
                toml_quote(crate::selection::subscription::subscription_kind_to_str(
                    entry.subscription
                ))
            );
            let _ = writeln!(out, "enabled = {}", entry.enabled);
            let _ = writeln!(out, "free = {}", entry.free);
            let _ = writeln!(out, "official = {}", entry.official);
            let _ = writeln!(out, "quota_disabled = {}", entry.quota_disabled);
            let _ = writeln!(out, "cheap_eligible = {}", entry.cheap_eligible);
            let _ = writeln!(out, "tough_eligible = {}", entry.tough_eligible);
            let _ = writeln!(out, "effort_eligible = {}", entry.effort_eligible);
            if let Some(key) = entry.quota_lookup_key.as_deref() {
                let _ = writeln!(out, "quota_lookup_key = {}", toml_quote(key));
            }
            if entry.display_order != 0 {
                let _ = writeln!(out, "display_order = {}", entry.display_order);
            }
            // Per spec: `effort_mapping` is saved as an atomic block
            // when any sub-field diverges from the baked default.
            let resolved_baked = baked::baked_for(&entry.model, entry.cli, &entry.launch_name)
                .map_or_else(|| baked_default.clone(), |p| p.effort_mapping);
            if entry.effort_mapping != resolved_baked {
                out.push_str("\n[providers.effort_mapping]\n");
                let _ = writeln!(out, "cheap = {}", toml_quote(&entry.effort_mapping.cheap));
                let _ = writeln!(out, "normal = {}", toml_quote(&entry.effort_mapping.normal));
                let _ = writeln!(out, "tough = {}", toml_quote(&entry.effort_mapping.tough));
            }
        }
    }

    out
}

fn push_explicit_bool(buf: &mut String, key: &str, ov: &Override<bool>, baked: &Override<bool>) {
    if ov.is_explicit() && ov.value() != baked.value() {
        let _ = writeln!(buf, "{key} = {}", ov.value());
    }
}

fn push_explicit_string(
    buf: &mut String,
    key: &str,
    ov: &Override<String>,
    baked: &Override<String>,
) {
    if ov.is_explicit() && ov.value() != baked.value() {
        let _ = writeln!(buf, "{key} = {}", toml_quote(ov.value()));
    }
}

fn push_explicit_int(buf: &mut String, key: &str, explicit: bool, v: i128, baked: i128) {
    if explicit && v != baked {
        let _ = writeln!(buf, "{key} = {v}");
    }
}

fn push_explicit_string_array(
    buf: &mut String,
    key: &str,
    ov: &Override<Vec<String>>,
    baked: &Override<Vec<String>>,
) {
    if ov.is_explicit() && ov.value() != baked.value() {
        let _ = writeln!(buf, "{key} = {}", format_string_array(ov.value()));
    }
}

#[cfg(test)]
mod tests {
    use super::super::defaults::emit_annotated;
    use super::*;

    #[test]
    fn missing_meta_section_treated_as_v1() {
        let cfg = load_str("[ntfy]\nenabled = false\n").unwrap();
        assert_eq!(cfg.meta.version, SUPPORTED_VERSION);
        assert!(!*cfg.ntfy.enabled.value());
    }

    #[test]
    fn unsupported_version_rejected() {
        let err = load_str("[meta]\nversion = 2\n").unwrap_err();
        match err {
            LoadError::UnsupportedVersion { found } => assert_eq!(found, 2),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_with_suggestion() {
        let err = load_str("[ntfu]\n").unwrap_err();
        match err {
            LoadError::UnknownKey {
                path, suggestion, ..
            } => {
                assert_eq!(path, "ntfu");
                assert_eq!(suggestion.as_deref(), Some("ntfy"));
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn unknown_nested_key_with_suggestion() {
        let err = load_str("[ntfy]\nenable = true\n").unwrap_err();
        match err {
            LoadError::UnknownKey {
                path, suggestion, ..
            } => {
                assert_eq!(path, "ntfy.enable");
                assert_eq!(suggestion.as_deref(), Some("enabled"));
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn round_trip_defaults_dump_is_semantically_equal() {
        let baked = Config::baked_defaults();
        let dump = emit_annotated(&baked);
        let parsed = load_str(&dump).expect("annotated dump must parse");
        assert_eq!(parsed, baked);
    }

    #[test]
    fn override_then_round_trip_preserves_value() {
        let mut cfg = Config::baked_defaults();
        cfg.ntfy.detail_mode = Override::explicit(NtfyDetailMode::Minimal);
        cfg.runner.full_review_interval = Override::explicit(7);
        let dump = emit_annotated(&cfg);
        let parsed = load_str(&dump).expect("dump parses");
        assert_eq!(parsed.ntfy.detail_mode.value(), &NtfyDetailMode::Minimal);
        assert_eq!(*parsed.runner.full_review_interval.value(), 7);
    }

    #[test]
    fn sparse_render_drops_default_values() {
        let cfg = Config::baked_defaults();
        let out = render_sparse(&cfg);
        // Only meta should be present; everything else equals baked.
        assert!(out.contains("[meta]"));
        assert!(!out.contains("[ntfy]"));
        assert!(!out.contains("[runner]"));
        assert!(!out.contains("[acp."));
    }

    #[test]
    fn sparse_render_keeps_explicit_overrides() {
        let mut cfg = Config::baked_defaults();
        cfg.ntfy.detail_mode = Override::explicit(NtfyDetailMode::Minimal);
        cfg.runner.full_review_interval = Override::explicit(8);
        let out = render_sparse(&cfg);
        assert!(out.contains("[ntfy]\ndetail_mode = \"minimal\""));
        assert!(out.contains("[runner]\nfull_review_interval = 8"));
    }

    #[test]
    fn sparse_render_drops_keys_explicit_but_equal_to_default() {
        let mut cfg = Config::baked_defaults();
        // Explicit, same value as default → must NOT emit.
        cfg.ntfy.retry_attempts = Override::explicit(3);
        let out = render_sparse(&cfg);
        assert!(!out.contains("retry_attempts"));
    }

    #[test]
    fn type_mismatch_returns_structured_error() {
        let err = load_str("[ntfy]\nenabled = \"yes\"\n").unwrap_err();
        match err {
            LoadError::TypeMismatch { path, expected, .. } => {
                assert_eq!(path, "ntfy.enabled");
                assert_eq!(expected, "bool");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn enum_value_rejected_with_allowed_list() {
        let err = load_str("[ntfy]\ndetail_mode = \"verbose\"\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("detailed"), "{msg}");
        assert!(msg.contains("minimal"), "{msg}");
    }

    #[test]
    fn validation_runs_after_decode() {
        // retry_attempts = 0 must trip the schema validator.
        let err = load_str("[ntfy]\nretry_attempts = 0\n").unwrap_err();
        match err {
            LoadError::Validation(msg) => assert!(msg.contains("retry_attempts"), "{msg}"),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn reserved_env_prefix_rejected() {
        let toml = "[acp.agents.claude.env]\nCODEXIZE_ACP_FOO = \"x\"\n";
        let err = load_str(toml).unwrap_err();
        assert!(err.to_string().contains("CODEXIZE_ACP_"), "{err}");
    }

    #[test]
    fn load_from_path_missing_file_returns_baked_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let absent = dir.path().join("nope.toml");
        assert!(!absent.exists());
        let cfg = load_from_path(&absent).expect("missing file is the baked-defaults path");
        assert_eq!(cfg, Config::baked_defaults());
        assert!(!absent.exists(), "loader must not write on missing-file");
    }

    #[test]
    fn env_inline_table_decoded() {
        let toml = "[acp.agents.claude]\nenv = { FOO = \"bar\" }\n";
        let cfg = load_str(toml).unwrap();
        assert_eq!(
            cfg.acp.agents.claude.env.value().get("FOO"),
            Some(&"bar".to_string())
        );
    }

    #[test]
    fn removed_free_models_block_is_rejected() {
        let toml = "[[free_models]]\nmapped_into = \"deepseek-v4-flash\"\ncli = \"opencode\"\nmodel_name = \"dsk-4-flash\"\n";
        let err = load_str(toml).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("free_models"),
            "expected free_models in error, got: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("unknown"),
            "expected UnknownKey, got: {msg}"
        );
    }

    #[test]
    fn providers_removed_keys_rejected() {
        for removed_key in [
            "vendor = \"claude\"",
            "cli = \"claude\"",
            "launch_name = \"x\"",
        ] {
            let toml = format!(
                "[[providers]]\nlaunch = \"claude/claude-opus-4.7\"\nmodel = \"claude-opus-4.7\"\nsubscription = \"claude\"\n{removed_key}\n"
            );
            let err = load_str(&toml).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.to_lowercase().contains("unknown"),
                "expected UnknownKey for {removed_key}, got: {msg}"
            );
        }
    }

    #[test]
    fn providers_round_trip_minimal() {
        let toml = "[[providers]]\nlaunch = \"opencode/opencode-go/deepseek-v4-flash\"\nmodel = \"deepseek-v4-flash\"\nsubscription = \"opencode-go\"\n";
        let cfg = load_str(toml).unwrap();
        assert_eq!(cfg.providers.value().len(), 1);
        let p = &cfg.providers.value()[0];
        assert_eq!(p.cli, crate::selection::CliKind::Opencode);
        assert_eq!(p.launch_name, "opencode-go/deepseek-v4-flash");
        assert_eq!(p.model, "deepseek-v4-flash");
        assert_eq!(
            p.subscription,
            crate::selection::SubscriptionKind::OpencodeGo
        );
    }

    #[test]
    fn providers_cli_subscription_independence() {
        let toml = "[[providers]]\nlaunch = \"opencode/opencode-go/deepseek-v4-flash\"\nmodel = \"deepseek-v4-flash\"\nsubscription = \"codex\"\nquota_disabled = true\n";
        let cfg = load_str(toml).unwrap();
        let p = &cfg.providers.value()[0];
        assert_eq!(p.cli, crate::selection::CliKind::Opencode);
        assert_eq!(p.subscription, crate::selection::SubscriptionKind::Codex);
        assert!(p.quota_disabled);
    }

    #[test]
    fn cli_kind_parse_rejects_slash() {
        assert!(crate::selection::CliKind::parse("claude/foo").is_none());
        assert!(crate::selection::CliKind::parse("opencode/").is_none());
    }
}
