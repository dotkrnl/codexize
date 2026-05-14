//! Dotted-key getters / setters / unsetters and per-section reset.
//!
//! The CLI's `set <dotted.key> <value>`, `get <dotted.key>`,
//! `unset <dotted.key>`, and `reset <section>` commands all funnel
//! through this module. Keeping the schema-walking logic in one place
//! lets the loader, the CLI, and the TUI config panel share a single
//! source of truth for which keys exist and how their values parse.
//!
//! `MutationError` is the structured failure shape; the CLI renders it
//! with `to_string()`, tests match on the variant.

use std::collections::BTreeMap;

use super::defaults::emit_annotated;
use super::fmt::format_string_array as format_array;
use super::schema::{
    AcpAgentSection, AcpAgents, Config, LogLevel, NtfyDetailMode, Override, ShellPolicy,
};

#[derive(Debug)]
pub enum MutationError {
    UnknownKey {
        key: String,
        suggestion: Option<String>,
    },
    ParseValue {
        key: String,
        message: String,
    },
    NotSettable {
        key: String,
        message: String,
    },
    Validation(String),
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey { key, suggestion } => match suggestion {
                Some(s) => write!(f, "config: unknown key '{key}' (did you mean '{s}'?)"),
                None => write!(f, "config: unknown key '{key}'"),
            },
            Self::ParseValue { key, message } => {
                write!(f, "config: invalid value for '{key}': {message}")
            }
            Self::NotSettable { key, message } => {
                write!(f, "config: '{key}' is not directly settable: {message}")
            }
            Self::Validation(msg) => write!(f, "config: validation: {msg}"),
        }
    }
}

impl std::error::Error for MutationError {}

/// Render the effective value at `key` as a single line (scalar) or a
/// TOML sub-table fragment (section). Returns `UnknownKey` if the
/// dotted path is not in the schema.
pub fn get_value(config: &Config, key: &str) -> Result<String, MutationError> {
    // Section / sub-section dumps go through the canonical annotated
    // emitter so `get` and `list <section>` agree on formatting.
    if let Some(s) = section_dump(config, key) {
        return Ok(s);
    }
    let parts: Vec<&str> = key.split('.').collect();
    let scalar = match parts.as_slice() {
        ["meta", "version"] => config.meta.version.to_string(),

        ["ntfy", "enabled"] => config.ntfy.enabled.value().to_string(),
        ["ntfy", "server"] => config.ntfy.server.value().clone(),
        ["ntfy", "topic"] => config.ntfy.topic.value().clone(),
        ["ntfy", "detail_mode"] => config.ntfy.detail_mode.value().as_str().to_string(),
        ["ntfy", "retry_attempts"] => config.ntfy.retry_attempts.value().to_string(),
        ["ntfy", "retry_delay_ms"] => config.ntfy.retry_delay_ms.value().to_string(),
        ["ntfy", "http_timeout_secs"] => config.ntfy.http_timeout_secs.value().to_string(),
        ["ntfy", "body_max_bytes"] => config.ntfy.body_max_bytes.value().to_string(),
        ["ntfy", "excerpt_max_chars"] => config.ntfy.excerpt_max_chars.value().to_string(),
        ["ntfy", "events", "stage_wait"] => config.ntfy.events.stage_wait.value().to_string(),
        ["ntfy", "events", "interactive_wait"] => {
            config.ntfy.events.interactive_wait.value().to_string()
        }
        ["ntfy", "events", "pipeline_done"] => config.ntfy.events.pipeline_done.value().to_string(),

        ["acp", "policy", "shell_policy"] => {
            config.acp.policy.shell_policy.value().as_str().to_string()
        }
        ["acp", "policy", "shell_allowlist"] => {
            format_array(config.acp.policy.shell_allowlist.value())
        }
        ["acp", "policy", "enforce_readonly_workspace"] => config
            .acp
            .policy
            .enforce_readonly_workspace
            .value()
            .to_string(),
        ["acp", "policy", "allowed_write_paths"] => {
            format_array(config.acp.policy.allowed_write_paths.value())
        }

        ["acp", "install", "claude_acp_root"] => config.acp.install.claude_acp_root.value().clone(),
        ["acp", "install", "prefer_local_claude_acp"] => config
            .acp
            .install
            .prefer_local_claude_acp
            .value()
            .to_string(),

        ["acp", "agents", vendor, "enabled"] => agent_for(&config.acp.agents, vendor)?
            .enabled
            .value()
            .to_string(),
        ["acp", "agents", vendor, "program"] => agent_for(&config.acp.agents, vendor)?
            .program
            .value()
            .clone(),
        ["acp", "agents", vendor, "args"] => {
            format_array(agent_for(&config.acp.agents, vendor)?.args.value())
        }
        ["acp", "agents", vendor, "env", env_key] => {
            let env = agent_for(&config.acp.agents, vendor)?.env.value();
            env.get(*env_key)
                .cloned()
                .ok_or_else(|| MutationError::UnknownKey {
                    key: key.to_string(),
                    suggestion: super::util::nearest(
                        env_key,
                        &env.keys().map(|s| s.as_str()).collect::<Vec<_>>(),
                        4,
                    ),
                })?
        }

        ["runner", "full_review_interval"] => {
            config.runner.full_review_interval.value().to_string()
        }

        ["paths", "cache_root"] => config.paths.cache_root.value().clone(),
        ["paths", "sessions_root"] => config.paths.sessions_root.value().clone(),
        ["paths", "runs_root"] => config.paths.runs_root.value().clone(),
        ["paths", "memory_root"] => config.paths.memory_root.value().clone(),

        ["ui", "prefer_split_on_open"] => config.ui.prefer_split_on_open.value().to_string(),
        ["ui", "colon_palette", "show_help"] => {
            config.ui.colon_palette.show_help.value().to_string()
        }
        ["ui", "footer", "show_keys"] => config.ui.footer.show_keys.value().to_string(),

        ["diagnostics", "log_level"] => config.diagnostics.log_level.value().as_str().to_string(),
        ["diagnostics", "json_logs"] => config.diagnostics.json_logs.value().to_string(),

        ["memory", "enabled"] => config.memory.enabled.value().to_string(),
        ["memory", "max_topics_per_read"] => config.memory.max_topics_per_read.value().to_string(),
        ["memory", "journal_retention_months"] => {
            config.memory.journal_retention_months.value().to_string()
        }

        _ => {
            return Err(MutationError::UnknownKey {
                key: key.to_string(),
                suggestion: super::util::nearest(key, &all_scalar_keys(), 4),
            });
        }
    };
    Ok(scalar)
}

/// Set `key` to the parsed `raw_value`. Validates the resulting
/// `Config` so mutations that would leave the file un-loadable on next
/// launch are rejected here, not at the next launch.
pub fn set_value(config: &mut Config, key: &str, raw_value: &str) -> Result<(), MutationError> {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        // [meta] is managed by the binary and not operator-tunable.
        ["meta", _] => {
            return Err(MutationError::NotSettable {
                key: key.to_string(),
                message: "[meta] is managed by the binary's schema version".to_string(),
            });
        }

        ["ntfy", "enabled"] => set_bool(&mut config.ntfy.enabled, key, raw_value)?,
        ["ntfy", "server"] => config.ntfy.server.set(raw_value.to_string()),
        ["ntfy", "topic"] => config.ntfy.topic.set(raw_value.to_string()),
        ["ntfy", "detail_mode"] => {
            let parsed =
                NtfyDetailMode::parse(raw_value).ok_or_else(|| MutationError::ParseValue {
                    key: key.to_string(),
                    message: format!("expected one of {:?}", NtfyDetailMode::variants()),
                })?;
            config.ntfy.detail_mode.set(parsed);
        }
        ["ntfy", "retry_attempts"] => {
            config.ntfy.retry_attempts.set(parse_u32(key, raw_value)?);
        }
        ["ntfy", "retry_delay_ms"] => {
            config.ntfy.retry_delay_ms.set(parse_u64(key, raw_value)?);
        }
        ["ntfy", "http_timeout_secs"] => {
            config
                .ntfy
                .http_timeout_secs
                .set(parse_u32(key, raw_value)?);
        }
        ["ntfy", "body_max_bytes"] => {
            config.ntfy.body_max_bytes.set(parse_u64(key, raw_value)?);
        }
        ["ntfy", "excerpt_max_chars"] => {
            config
                .ntfy
                .excerpt_max_chars
                .set(parse_u32(key, raw_value)?);
        }
        ["ntfy", "events", "stage_wait"] => {
            set_bool(&mut config.ntfy.events.stage_wait, key, raw_value)?
        }
        ["ntfy", "events", "interactive_wait"] => {
            set_bool(&mut config.ntfy.events.interactive_wait, key, raw_value)?
        }
        ["ntfy", "events", "pipeline_done"] => {
            set_bool(&mut config.ntfy.events.pipeline_done, key, raw_value)?
        }

        ["acp", "policy", "shell_policy"] => {
            let parsed =
                ShellPolicy::parse(raw_value).ok_or_else(|| MutationError::ParseValue {
                    key: key.to_string(),
                    message: format!("expected one of {:?}", ShellPolicy::variants()),
                })?;
            config.acp.policy.shell_policy.set(parsed);
        }
        ["acp", "policy", "shell_allowlist"] => {
            config
                .acp
                .policy
                .shell_allowlist
                .set(parse_string_list(raw_value));
        }
        ["acp", "policy", "enforce_readonly_workspace"] => set_bool(
            &mut config.acp.policy.enforce_readonly_workspace,
            key,
            raw_value,
        )?,
        ["acp", "policy", "allowed_write_paths"] => {
            config
                .acp
                .policy
                .allowed_write_paths
                .set(parse_string_list(raw_value));
        }

        ["acp", "install", "claude_acp_root"] => {
            config
                .acp
                .install
                .claude_acp_root
                .set(raw_value.to_string());
        }
        ["acp", "install", "prefer_local_claude_acp"] => set_bool(
            &mut config.acp.install.prefer_local_claude_acp,
            key,
            raw_value,
        )?,

        ["acp", "agents", vendor, "enabled"] => {
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            set_bool(&mut agent.enabled, key, raw_value)?;
        }
        ["acp", "agents", vendor, "program"] => {
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            agent.program.set(raw_value.to_string());
        }
        ["acp", "agents", vendor, "args"] => {
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            agent.args.set(parse_string_list(raw_value));
        }
        ["acp", "agents", _, "env"] => {
            return Err(MutationError::NotSettable {
                key: key.to_string(),
                message:
                    "use `set acp.agents.<vendor>.env.<KEY> <value>` to add or update one entry"
                        .to_string(),
            });
        }
        ["acp", "agents", vendor, "env", env_key] => {
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            let mut map = agent.env.value().clone();
            map.insert((*env_key).to_string(), raw_value.to_string());
            agent.env.set(map);
        }

        ["runner", "full_review_interval"] => {
            config
                .runner
                .full_review_interval
                .set(parse_u32(key, raw_value)?);
        }

        ["paths", "cache_root"] => config.paths.cache_root.set(raw_value.to_string()),
        ["paths", "sessions_root"] => config.paths.sessions_root.set(raw_value.to_string()),
        ["paths", "runs_root"] => config.paths.runs_root.set(raw_value.to_string()),
        ["paths", "memory_root"] => config.paths.memory_root.set(raw_value.to_string()),

        ["ui", "prefer_split_on_open"] => {
            set_bool(&mut config.ui.prefer_split_on_open, key, raw_value)?
        }
        ["ui", "colon_palette", "show_help"] => {
            set_bool(&mut config.ui.colon_palette.show_help, key, raw_value)?
        }
        ["ui", "footer", "show_keys"] => set_bool(&mut config.ui.footer.show_keys, key, raw_value)?,

        ["diagnostics", "log_level"] => {
            let parsed = LogLevel::parse(raw_value).ok_or_else(|| MutationError::ParseValue {
                key: key.to_string(),
                message: format!("expected one of {:?}", LogLevel::variants()),
            })?;
            config.diagnostics.log_level.set(parsed);
        }
        ["diagnostics", "json_logs"] => {
            set_bool(&mut config.diagnostics.json_logs, key, raw_value)?
        }

        ["memory", "enabled"] => set_bool(&mut config.memory.enabled, key, raw_value)?,
        ["memory", "max_topics_per_read"] => {
            config
                .memory
                .max_topics_per_read
                .set(parse_u32(key, raw_value)?);
        }
        ["memory", "journal_retention_months"] => {
            config
                .memory
                .journal_retention_months
                .set(parse_u32(key, raw_value)?);
        }

        _ => {
            return Err(MutationError::UnknownKey {
                key: key.to_string(),
                suggestion: super::util::nearest(key, &all_settable_keys(), 4),
            });
        }
    }
    config.validate().map_err(MutationError::Validation)?;
    Ok(())
}

/// Drop the override at `key`, restoring the baked default at that path.
/// For `acp.agents.<vendor>.env.<KEY>` this removes that one entry; for
/// `acp.agents.<vendor>.env` the whole map reverts to baked-empty.
pub fn unset_value(config: &mut Config, key: &str) -> Result<(), MutationError> {
    let baked = Config::baked_defaults();
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["meta", _] => {
            return Err(MutationError::NotSettable {
                key: key.to_string(),
                message: "[meta] is managed by the binary's schema version".to_string(),
            });
        }

        ["ntfy", "enabled"] => config.ntfy.enabled.reset_to(*baked.ntfy.enabled.value()),
        ["ntfy", "server"] => config.ntfy.server.reset_to(baked.ntfy.server.into_value()),
        ["ntfy", "topic"] => config.ntfy.topic.reset_to(baked.ntfy.topic.into_value()),
        ["ntfy", "detail_mode"] => config
            .ntfy
            .detail_mode
            .reset_to(*baked.ntfy.detail_mode.value()),
        ["ntfy", "retry_attempts"] => config
            .ntfy
            .retry_attempts
            .reset_to(*baked.ntfy.retry_attempts.value()),
        ["ntfy", "retry_delay_ms"] => config
            .ntfy
            .retry_delay_ms
            .reset_to(*baked.ntfy.retry_delay_ms.value()),
        ["ntfy", "http_timeout_secs"] => config
            .ntfy
            .http_timeout_secs
            .reset_to(*baked.ntfy.http_timeout_secs.value()),
        ["ntfy", "body_max_bytes"] => config
            .ntfy
            .body_max_bytes
            .reset_to(*baked.ntfy.body_max_bytes.value()),
        ["ntfy", "excerpt_max_chars"] => config
            .ntfy
            .excerpt_max_chars
            .reset_to(*baked.ntfy.excerpt_max_chars.value()),
        ["ntfy", "events", "stage_wait"] => config
            .ntfy
            .events
            .stage_wait
            .reset_to(*baked.ntfy.events.stage_wait.value()),
        ["ntfy", "events", "interactive_wait"] => config
            .ntfy
            .events
            .interactive_wait
            .reset_to(*baked.ntfy.events.interactive_wait.value()),
        ["ntfy", "events", "pipeline_done"] => config
            .ntfy
            .events
            .pipeline_done
            .reset_to(*baked.ntfy.events.pipeline_done.value()),

        ["acp", "policy", "shell_policy"] => config
            .acp
            .policy
            .shell_policy
            .reset_to(*baked.acp.policy.shell_policy.value()),
        ["acp", "policy", "shell_allowlist"] => config
            .acp
            .policy
            .shell_allowlist
            .reset_to(baked.acp.policy.shell_allowlist.into_value()),
        ["acp", "policy", "enforce_readonly_workspace"] => config
            .acp
            .policy
            .enforce_readonly_workspace
            .reset_to(*baked.acp.policy.enforce_readonly_workspace.value()),
        ["acp", "policy", "allowed_write_paths"] => config
            .acp
            .policy
            .allowed_write_paths
            .reset_to(baked.acp.policy.allowed_write_paths.into_value()),

        ["acp", "install", "claude_acp_root"] => config
            .acp
            .install
            .claude_acp_root
            .reset_to(baked.acp.install.claude_acp_root.into_value()),
        ["acp", "install", "prefer_local_claude_acp"] => config
            .acp
            .install
            .prefer_local_claude_acp
            .reset_to(*baked.acp.install.prefer_local_claude_acp.value()),

        ["acp", "agents", vendor, "enabled"] => {
            let baked_agent = agent_for(&baked.acp.agents, vendor)?.clone();
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            agent.enabled.reset_to(*baked_agent.enabled.value());
        }
        ["acp", "agents", vendor, "program"] => {
            let baked_agent = agent_for(&baked.acp.agents, vendor)?.clone();
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            agent.program.reset_to(baked_agent.program.into_value());
        }
        ["acp", "agents", vendor, "args"] => {
            let baked_agent = agent_for(&baked.acp.agents, vendor)?.clone();
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            agent.args.reset_to(baked_agent.args.into_value());
        }
        ["acp", "agents", vendor, "env"] => {
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            agent.env.reset_to(BTreeMap::new());
        }
        ["acp", "agents", vendor, "env", env_key] => {
            let agent = agent_for_mut(&mut config.acp.agents, vendor)?;
            let mut map = agent.env.value().clone();
            if map.remove(*env_key).is_none() {
                return Err(MutationError::UnknownKey {
                    key: key.to_string(),
                    suggestion: None,
                });
            }
            // If the operator removes the last entry, treat the override
            // as cleared (matches `unset acp.agents.<vendor>.env`).
            if map.is_empty() {
                agent.env.reset_to(BTreeMap::new());
            } else {
                agent.env.set(map);
            }
        }

        ["runner", "full_review_interval"] => config
            .runner
            .full_review_interval
            .reset_to(*baked.runner.full_review_interval.value()),

        ["paths", "cache_root"] => config
            .paths
            .cache_root
            .reset_to(baked.paths.cache_root.into_value()),
        ["paths", "sessions_root"] => config
            .paths
            .sessions_root
            .reset_to(baked.paths.sessions_root.into_value()),
        ["paths", "runs_root"] => config
            .paths
            .runs_root
            .reset_to(baked.paths.runs_root.into_value()),
        ["paths", "memory_root"] => config
            .paths
            .memory_root
            .reset_to(baked.paths.memory_root.into_value()),

        ["ui", "prefer_split_on_open"] => config
            .ui
            .prefer_split_on_open
            .reset_to(*baked.ui.prefer_split_on_open.value()),
        ["ui", "colon_palette", "show_help"] => config
            .ui
            .colon_palette
            .show_help
            .reset_to(*baked.ui.colon_palette.show_help.value()),
        ["ui", "footer", "show_keys"] => config
            .ui
            .footer
            .show_keys
            .reset_to(*baked.ui.footer.show_keys.value()),

        ["diagnostics", "log_level"] => config
            .diagnostics
            .log_level
            .reset_to(*baked.diagnostics.log_level.value()),
        ["diagnostics", "json_logs"] => config
            .diagnostics
            .json_logs
            .reset_to(*baked.diagnostics.json_logs.value()),

        ["memory", "enabled"] => config
            .memory
            .enabled
            .reset_to(*baked.memory.enabled.value()),
        ["memory", "max_topics_per_read"] => config
            .memory
            .max_topics_per_read
            .reset_to(*baked.memory.max_topics_per_read.value()),
        ["memory", "journal_retention_months"] => config
            .memory
            .journal_retention_months
            .reset_to(*baked.memory.journal_retention_months.value()),

        _ => {
            return Err(MutationError::UnknownKey {
                key: key.to_string(),
                suggestion: super::util::nearest(key, &all_settable_keys(), 4),
            });
        }
    }
    Ok(())
}

/// Drop every override under the named section, restoring its baked
/// defaults wholesale. Returns `UnknownKey` for an unrecognized
/// section name.
pub fn reset_section(config: &mut Config, section: &str) -> Result<(), MutationError> {
    let baked = Config::baked_defaults();
    match section {
        "meta" => {
            return Err(MutationError::NotSettable {
                key: section.to_string(),
                message: "[meta] is managed by the binary's schema version".to_string(),
            });
        }
        "ntfy" => config.ntfy = baked.ntfy,
        "ntfy.events" => config.ntfy.events = baked.ntfy.events,
        "acp" => config.acp = baked.acp,
        "acp.policy" => config.acp.policy = baked.acp.policy,
        "acp.install" => config.acp.install = baked.acp.install,
        "acp.agents" => config.acp.agents = baked.acp.agents,
        "acp.agents.claude" => config.acp.agents.claude = baked.acp.agents.claude,
        "acp.agents.codex" => config.acp.agents.codex = baked.acp.agents.codex,
        "acp.agents.gemini" => config.acp.agents.gemini = baked.acp.agents.gemini,
        "acp.agents.kimi" => config.acp.agents.kimi = baked.acp.agents.kimi,
        "acp.agents.opencode" => config.acp.agents.opencode = baked.acp.agents.opencode,
        "runner" => config.runner = baked.runner,
        "paths" => config.paths = baked.paths,
        "ui" => config.ui = baked.ui,
        "ui.colon_palette" => config.ui.colon_palette = baked.ui.colon_palette,
        "ui.footer" => config.ui.footer = baked.ui.footer,
        "diagnostics" => config.diagnostics = baked.diagnostics,
        "memory" => config.memory = baked.memory,
        _ => {
            return Err(MutationError::UnknownKey {
                key: section.to_string(),
                suggestion: super::util::nearest(section, all_section_names(), 4),
            });
        }
    }
    Ok(())
}

// ---- helpers -------------------------------------------------------------

fn agent_for<'a>(
    agents: &'a AcpAgents,
    vendor: &str,
) -> Result<&'a AcpAgentSection, MutationError> {
    match vendor {
        "claude" => Ok(&agents.claude),
        "codex" => Ok(&agents.codex),
        "gemini" => Ok(&agents.gemini),
        "kimi" => Ok(&agents.kimi),
        "opencode" => Ok(&agents.opencode),
        _ => Err(MutationError::UnknownKey {
            key: format!("acp.agents.{vendor}"),
            suggestion: super::util::nearest(
                vendor,
                &["claude", "codex", "gemini", "kimi", "opencode"],
                4,
            ),
        }),
    }
}

fn agent_for_mut<'a>(
    agents: &'a mut AcpAgents,
    vendor: &str,
) -> Result<&'a mut AcpAgentSection, MutationError> {
    match vendor {
        "claude" => Ok(&mut agents.claude),
        "codex" => Ok(&mut agents.codex),
        "gemini" => Ok(&mut agents.gemini),
        "kimi" => Ok(&mut agents.kimi),
        "opencode" => Ok(&mut agents.opencode),
        _ => Err(MutationError::UnknownKey {
            key: format!("acp.agents.{vendor}"),
            suggestion: super::util::nearest(
                vendor,
                &["claude", "codex", "gemini", "kimi", "opencode"],
                4,
            ),
        }),
    }
}

fn set_bool(ov: &mut Override<bool>, key: &str, raw: &str) -> Result<(), MutationError> {
    match raw.to_ascii_lowercase().as_str() {
        "true" => {
            ov.set(true);
            Ok(())
        }
        "false" => {
            ov.set(false);
            Ok(())
        }
        _ => Err(MutationError::ParseValue {
            key: key.to_string(),
            message: "expected `true` or `false`".to_string(),
        }),
    }
}

fn parse_u32(key: &str, raw: &str) -> Result<u32, MutationError> {
    raw.parse::<u32>().map_err(|e| MutationError::ParseValue {
        key: key.to_string(),
        message: format!("expected non-negative integer ≤ u32::MAX ({e})"),
    })
}

fn parse_u64(key: &str, raw: &str) -> Result<u64, MutationError> {
    raw.parse::<u64>().map_err(|e| MutationError::ParseValue {
        key: key.to_string(),
        message: format!("expected non-negative integer ≤ u64::MAX ({e})"),
    })
}

/// Parse a CLI list value: comma-separated, whitespace trimmed, with
/// `\,` as the literal-comma escape. An empty (or all-whitespace) input
/// clears the list, matching spec §4.
fn parse_string_list(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek()
                && next == ','
            {
                chars.next();
                cur.push(',');
                continue;
            }
            cur.push(ch);
        } else if ch == ',' {
            out.push(std::mem::take(&mut cur).trim().to_string());
        } else {
            cur.push(ch);
        }
    }
    out.push(cur.trim().to_string());
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// Pull the requested section out of the canonical annotated dump. Used
/// by `get` (for sub-table queries) and `list <section>`.
pub fn section_dump(config: &Config, section: &str) -> Option<String> {
    if !is_known_section(section) {
        return None;
    }
    let dump = emit_annotated(config);
    let header = format!("[{section}]");
    let mut out = String::new();
    let mut in_section = false;
    for line in dump.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            // `[acp.policy]` should NOT be emitted under `acp` because
            // `list <section>` prints exactly that subtree per spec; we
            // include nested sub-headers only when they are descendants
            // of `section`.
            in_section = trimmed == header || trimmed.starts_with(&format!("[{section}."));
            if in_section {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn is_known_section(name: &str) -> bool {
    all_section_names().contains(&name)
}

fn all_section_names() -> &'static [&'static str] {
    &[
        "meta",
        "ntfy",
        "ntfy.events",
        "acp",
        "acp.policy",
        "acp.install",
        "acp.agents",
        "acp.agents.claude",
        "acp.agents.codex",
        "acp.agents.gemini",
        "acp.agents.kimi",
        "acp.agents.opencode",
        "runner",
        "paths",
        "ui",
        "ui.colon_palette",
        "ui.footer",
        "diagnostics",
        "memory",
    ]
}

fn all_scalar_keys() -> Vec<&'static str> {
    let mut v = all_settable_keys();
    v.push("meta.version");
    v
}

fn all_settable_keys() -> Vec<&'static str> {
    vec![
        "ntfy.enabled",
        "ntfy.server",
        "ntfy.topic",
        "ntfy.detail_mode",
        "ntfy.retry_attempts",
        "ntfy.retry_delay_ms",
        "ntfy.http_timeout_secs",
        "ntfy.body_max_bytes",
        "ntfy.excerpt_max_chars",
        "ntfy.events.stage_wait",
        "ntfy.events.interactive_wait",
        "ntfy.events.pipeline_done",
        "acp.policy.shell_policy",
        "acp.policy.shell_allowlist",
        "acp.policy.enforce_readonly_workspace",
        "acp.policy.allowed_write_paths",
        "acp.install.claude_acp_root",
        "acp.install.prefer_local_claude_acp",
        "acp.agents.claude.enabled",
        "acp.agents.claude.program",
        "acp.agents.claude.args",
        "acp.agents.codex.enabled",
        "acp.agents.codex.program",
        "acp.agents.codex.args",
        "acp.agents.gemini.enabled",
        "acp.agents.gemini.program",
        "acp.agents.gemini.args",
        "acp.agents.kimi.enabled",
        "acp.agents.kimi.program",
        "acp.agents.kimi.args",
        "acp.agents.opencode.enabled",
        "acp.agents.opencode.program",
        "acp.agents.opencode.args",
        "runner.full_review_interval",
        "paths.cache_root",
        "paths.sessions_root",
        "paths.runs_root",
        "paths.memory_root",
        "ui.prefer_split_on_open",
        "ui.colon_palette.show_help",
        "ui.footer.show_keys",
        "diagnostics.log_level",
        "diagnostics.json_logs",
        "memory.enabled",
        "memory.max_topics_per_read",
        "memory.journal_retention_months",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_scalar_returns_baked_default_string() {
        let cfg = Config::baked_defaults();
        assert_eq!(get_value(&cfg, "ntfy.detail_mode").unwrap(), "detailed");
        assert_eq!(get_value(&cfg, "runner.full_review_interval").unwrap(), "5");
        assert_eq!(get_value(&cfg, "ntfy.enabled").unwrap(), "true");
    }

    #[test]
    fn get_unknown_key_suggests_nearest() {
        let cfg = Config::baked_defaults();
        let err = get_value(&cfg, "ntfy.detial_mode").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("did you mean 'ntfy.detail_mode'"), "{msg}");
    }

    #[test]
    fn get_section_dumps_subtree() {
        let cfg = Config::baked_defaults();
        let out = get_value(&cfg, "ntfy").unwrap();
        assert!(out.contains("[ntfy]"));
        assert!(out.contains("detail_mode = \"detailed\""));
        assert!(out.contains("[ntfy.events]"));
    }

    #[test]
    fn set_then_get_round_trip() {
        let mut cfg = Config::baked_defaults();
        set_value(&mut cfg, "ntfy.detail_mode", "minimal").unwrap();
        assert_eq!(get_value(&cfg, "ntfy.detail_mode").unwrap(), "minimal");
    }

    #[test]
    fn set_unknown_key_rejected() {
        let mut cfg = Config::baked_defaults();
        let err = set_value(&mut cfg, "ntfy.frop", "1").unwrap_err();
        assert!(matches!(err, MutationError::UnknownKey { .. }));
    }

    #[test]
    fn set_validates_after_mutation() {
        let mut cfg = Config::baked_defaults();
        let err = set_value(&mut cfg, "ntfy.retry_attempts", "0").unwrap_err();
        assert!(matches!(err, MutationError::Validation(_)));
    }

    #[test]
    fn set_rejects_meta_section() {
        let mut cfg = Config::baked_defaults();
        let err = set_value(&mut cfg, "meta.version", "2").unwrap_err();
        assert!(matches!(err, MutationError::NotSettable { .. }));
    }

    #[test]
    fn set_env_pair_inserts_and_unset_removes() {
        let mut cfg = Config::baked_defaults();
        set_value(&mut cfg, "acp.agents.claude.env.FOO", "bar").unwrap();
        assert_eq!(
            cfg.acp.agents.claude.env.value().get("FOO"),
            Some(&"bar".to_string())
        );
        assert!(cfg.acp.agents.claude.env.is_explicit());

        unset_value(&mut cfg, "acp.agents.claude.env.FOO").unwrap();
        // Emptying the last entry collapses back to the baked-empty map.
        assert!(cfg.acp.agents.claude.env.value().is_empty());
        assert!(!cfg.acp.agents.claude.env.is_explicit());
    }

    #[test]
    fn set_reserved_env_prefix_rejected_by_validate() {
        let mut cfg = Config::baked_defaults();
        let err = set_value(&mut cfg, "acp.agents.claude.env.CODEXIZE_ACP_X", "y").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("CODEXIZE_ACP_"), "{msg}");
    }

    #[test]
    fn unset_drops_explicit_flag() {
        let mut cfg = Config::baked_defaults();
        set_value(&mut cfg, "ntfy.detail_mode", "minimal").unwrap();
        assert!(cfg.ntfy.detail_mode.is_explicit());
        unset_value(&mut cfg, "ntfy.detail_mode").unwrap();
        assert!(!cfg.ntfy.detail_mode.is_explicit());
        assert_eq!(cfg.ntfy.detail_mode.value(), &NtfyDetailMode::Detailed);
    }

    #[test]
    fn parse_string_list_handles_escapes_and_empty() {
        assert!(parse_string_list("").is_empty());
        assert!(parse_string_list("   ").is_empty());
        assert_eq!(
            parse_string_list("a, b, c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            parse_string_list("a\\,b , c"),
            vec!["a,b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn reset_section_clears_all_overrides_under_section() {
        let mut cfg = Config::baked_defaults();
        set_value(&mut cfg, "ntfy.detail_mode", "minimal").unwrap();
        set_value(&mut cfg, "ntfy.events.stage_wait", "false").unwrap();
        reset_section(&mut cfg, "ntfy").unwrap();
        assert!(!cfg.ntfy.detail_mode.is_explicit());
        assert!(!cfg.ntfy.events.stage_wait.is_explicit());
    }

    #[test]
    fn reset_unknown_section_rejected() {
        let mut cfg = Config::baked_defaults();
        let err = reset_section(&mut cfg, "ntfu").unwrap_err();
        assert!(matches!(err, MutationError::UnknownKey { .. }));
    }

    #[test]
    fn list_string_parser_accepts_empty_to_clear() {
        let mut cfg = Config::baked_defaults();
        set_value(&mut cfg, "acp.policy.shell_allowlist", "git, ls").unwrap();
        assert_eq!(cfg.acp.policy.shell_allowlist.value().len(), 2);
        set_value(&mut cfg, "acp.policy.shell_allowlist", "").unwrap();
        assert!(cfg.acp.policy.shell_allowlist.value().is_empty());
    }
}
