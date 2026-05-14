//! Typed read-only projections the loaded [`Config`] hands to subsystems.
//! Each view unwraps the schema's `Override<T>` fields so consumers stay
//! decoupled from `Override`'s mutability story.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::paths::expand_home;
use super::schema::{Config, LogLevel, NtfyDetailMode, ShellPolicy};

/// Read-only projection of `[ntfy]` for the notification publisher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfyView {
    pub enabled: bool,
    pub server: String,
    pub topic: String,
    pub detail_mode: NtfyDetailMode,
    pub retry_attempts: u32,
    pub retry_delay_ms: u64,
    pub http_timeout_secs: u32,
    pub body_max_bytes: u64,
    pub excerpt_max_chars: u32,
    pub events: NtfyEventsView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtfyEventsView {
    pub stage_wait: bool,
    pub interactive_wait: bool,
    pub pipeline_done: bool,
}

/// Read-only projection of `[acp.policy]` for the per-call
/// `AcpLaunchPolicy` constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpPolicyDefaultsView {
    pub shell_policy: ShellPolicy,
    pub shell_allowlist: Vec<String>,
    pub enforce_readonly_workspace: bool,
    pub allowed_write_paths: Vec<String>,
}

/// Read-only projection of one `[acp.agents.<vendor>]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentView {
    pub enabled: bool,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// Read-only projection of `[acp.install]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpInstallView {
    pub claude_acp_root: PathBuf,
    pub prefer_local_claude_acp: bool,
}

/// Read-only projection of `[runner]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerView {
    pub full_review_interval: u32,
}

/// Read-only projection of `[paths]` with `$HOME` already expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsView {
    pub cache_root: PathBuf,
    pub sessions_root: PathBuf,
    pub runs_root: PathBuf,
    pub memory_root: PathBuf,
}

/// Read-only projection of `[diagnostics]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsView {
    pub log_level: LogLevel,
    pub json_logs: bool,
}

/// Read-only projection of `[memory]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryView {
    pub enabled: bool,
    pub max_topics_per_read: u32,
    pub journal_retention_months: u32,
}

/// Read-only projection of `[ui]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiView {
    pub prefer_split_on_open: bool,
    pub colon_palette_show_help: bool,
    pub footer_show_keys: bool,
}

impl Config {
    pub fn ntfy_view(&self) -> NtfyView {
        let n = &self.ntfy;
        NtfyView {
            enabled: *n.enabled.value(),
            server: n.server.value().clone(),
            topic: n.topic.value().clone(),
            detail_mode: *n.detail_mode.value(),
            retry_attempts: *n.retry_attempts.value(),
            retry_delay_ms: *n.retry_delay_ms.value(),
            http_timeout_secs: *n.http_timeout_secs.value(),
            body_max_bytes: *n.body_max_bytes.value(),
            excerpt_max_chars: *n.excerpt_max_chars.value(),
            events: NtfyEventsView {
                stage_wait: *n.events.stage_wait.value(),
                interactive_wait: *n.events.interactive_wait.value(),
                pipeline_done: *n.events.pipeline_done.value(),
            },
        }
    }

    pub fn acp_policy_view(&self) -> AcpPolicyDefaultsView {
        let p = &self.acp.policy;
        AcpPolicyDefaultsView {
            shell_policy: *p.shell_policy.value(),
            shell_allowlist: p.shell_allowlist.value().clone(),
            enforce_readonly_workspace: *p.enforce_readonly_workspace.value(),
            allowed_write_paths: p.allowed_write_paths.value().clone(),
        }
    }

    pub fn acp_install_view(&self) -> AcpInstallView {
        let i = &self.acp.install;
        AcpInstallView {
            claude_acp_root: PathBuf::from(expand_home(i.claude_acp_root.value())),
            prefer_local_claude_acp: *i.prefer_local_claude_acp.value(),
        }
    }

    pub fn acp_agent_view(&self, agent: &super::schema::AcpAgentSection) -> AcpAgentView {
        AcpAgentView {
            enabled: *agent.enabled.value(),
            program: agent.program.value().clone(),
            args: agent.args.value().clone(),
            env: agent.env.value().clone(),
        }
    }

    pub fn runner_view(&self) -> RunnerView {
        RunnerView {
            full_review_interval: *self.runner.full_review_interval.value(),
        }
    }

    pub fn paths_view(&self) -> PathsView {
        let p = &self.paths;
        PathsView {
            cache_root: PathBuf::from(expand_home(p.cache_root.value())),
            sessions_root: PathBuf::from(expand_home(p.sessions_root.value())),
            runs_root: PathBuf::from(expand_home(p.runs_root.value())),
            memory_root: PathBuf::from(expand_home(p.memory_root.value())),
        }
    }

    pub fn diagnostics_view(&self) -> DiagnosticsView {
        DiagnosticsView {
            log_level: *self.diagnostics.log_level.value(),
            json_logs: *self.diagnostics.json_logs.value(),
        }
    }

    pub fn memory_view(&self) -> MemoryView {
        MemoryView {
            enabled: *self.memory.enabled.value(),
            max_topics_per_read: *self.memory.max_topics_per_read.value(),
            journal_retention_months: *self.memory.journal_retention_months.value(),
        }
    }

    pub fn ui_view(&self) -> UiView {
        let u = &self.ui;
        UiView {
            prefer_split_on_open: *u.prefer_split_on_open.value(),
            colon_palette_show_help: *u.colon_palette.show_help.value(),
            footer_show_keys: *u.footer.show_keys.value(),
        }
    }
}
