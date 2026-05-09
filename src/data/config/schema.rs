//! Typed v1 config schema.
//!
//! Each section is a struct of [`Override<T>`] fields. The loader marks a
//! field as `explicit` when the on-disk file carried that key; sparse-save
//! and the TUI's source tagging both key off this flag. Equality on
//! `Override<T>` ignores the explicit flag — round-trip tests compare the
//! semantic value, not how the value got there.

use chrono::{DateTime, Utc};
use crate::selection::FreeModelEntry;
use std::collections::BTreeMap;

/// The supported on-disk schema version. The loader rejects any
/// `[meta] version` outside this set.
pub const SUPPORTED_VERSION: u32 = 1;

/// Tracks whether a value came from the baked default or an explicit
/// override on disk. Equality compares the value only.
#[derive(Debug, Clone)]
pub struct Override<T> {
    value: T,
    explicit: bool,
}

impl<T> Override<T> {
    pub const fn baked(value: T) -> Self {
        Self {
            value,
            explicit: false,
        }
    }

    pub const fn explicit(value: T) -> Self {
        Self {
            value,
            explicit: true,
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn into_value(self) -> T {
        self.value
    }

    pub fn is_explicit(&self) -> bool {
        self.explicit
    }

    pub fn set(&mut self, value: T) {
        self.value = value;
        self.explicit = true;
    }

    /// Drop the override, restoring the baked default.
    pub fn reset_to(&mut self, default: T) {
        self.value = default;
        self.explicit = false;
    }
}

impl<T: PartialEq> PartialEq for Override<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl<T: Eq> Eq for Override<T> {}

impl<T: Default> Default for Override<T> {
    fn default() -> Self {
        Self::baked(T::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NtfyDetailMode {
    Detailed,
    Minimal,
}

impl NtfyDetailMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detailed => "detailed",
            Self::Minimal => "minimal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "detailed" => Some(Self::Detailed),
            "minimal" => Some(Self::Minimal),
            _ => None,
        }
    }

    pub const fn variants() -> &'static [&'static str] {
        &["detailed", "minimal"]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPolicy {
    FullAccess,
    Allowlist,
}

impl ShellPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullAccess => "full-access",
            Self::Allowlist => "allowlist",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full-access" => Some(Self::FullAccess),
            "allowlist" => Some(Self::Allowlist),
            _ => None,
        }
    }

    pub const fn variants() -> &'static [&'static str] {
        &["full-access", "allowlist"]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    pub const fn variants() -> &'static [&'static str] {
        &["trace", "debug", "info", "warn", "error"]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaSection {
    pub version: u32,
}

impl Default for MetaSection {
    fn default() -> Self {
        Self {
            version: SUPPORTED_VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfyEvents {
    pub phase_wait: Override<bool>,
    pub interactive_wait: Override<bool>,
    pub pipeline_done: Override<bool>,
}

impl Default for NtfyEvents {
    fn default() -> Self {
        Self {
            phase_wait: Override::baked(true),
            interactive_wait: Override::baked(true),
            pipeline_done: Override::baked(true),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtfySection {
    pub enabled: Override<bool>,
    pub server: Override<String>,
    pub topic: Override<String>,
    pub detail_mode: Override<NtfyDetailMode>,
    pub retry_attempts: Override<u32>,
    pub retry_delay_ms: Override<u64>,
    pub http_timeout_secs: Override<u32>,
    pub body_max_bytes: Override<u64>,
    pub excerpt_max_chars: Override<u32>,
    pub created_at: Override<Option<DateTime<Utc>>>,
    pub updated_at: Override<Option<DateTime<Utc>>>,
    pub events: NtfyEvents,
}

impl Default for NtfySection {
    fn default() -> Self {
        Self {
            enabled: Override::baked(true),
            server: Override::baked("https://ntfy.sh".to_string()),
            topic: Override::baked(String::new()),
            detail_mode: Override::baked(NtfyDetailMode::Detailed),
            retry_attempts: Override::baked(3),
            retry_delay_ms: Override::baked(250),
            http_timeout_secs: Override::baked(10),
            body_max_bytes: Override::baked(4096),
            excerpt_max_chars: Override::baked(600),
            created_at: Override::baked(None),
            updated_at: Override::baked(None),
            events: NtfyEvents::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpPolicySection {
    pub shell_policy: Override<ShellPolicy>,
    pub shell_allowlist: Override<Vec<String>>,
    pub enforce_readonly_workspace: Override<bool>,
    pub allowed_write_paths: Override<Vec<String>>,
}

impl Default for AcpPolicySection {
    fn default() -> Self {
        Self {
            shell_policy: Override::baked(ShellPolicy::FullAccess),
            shell_allowlist: Override::baked(Vec::new()),
            enforce_readonly_workspace: Override::baked(false),
            allowed_write_paths: Override::baked(Vec::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpInstallSection {
    pub claude_acp_root: Override<String>,
    pub prefer_local_claude_acp: Override<bool>,
}

impl Default for AcpInstallSection {
    fn default() -> Self {
        Self {
            claude_acp_root: Override::baked("$HOME/.codexize/acp".to_string()),
            prefer_local_claude_acp: Override::baked(true),
        }
    }
}

/// One ACP vendor's launch knobs. Defaults differ per vendor; see
/// [`AcpAgents::default`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentSection {
    pub enabled: Override<bool>,
    pub program: Override<String>,
    pub args: Override<Vec<String>>,
    pub env: Override<BTreeMap<String, String>>,
}

impl AcpAgentSection {
    fn baked(program: &str, args: &[&str]) -> Self {
        Self {
            enabled: Override::baked(true),
            program: Override::baked(program.to_string()),
            args: Override::baked(args.iter().map(|a| (*a).to_string()).collect()),
            env: Override::baked(BTreeMap::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgents {
    pub claude: AcpAgentSection,
    pub codex: AcpAgentSection,
    pub gemini: AcpAgentSection,
    pub kimi: AcpAgentSection,
    pub opencode: AcpAgentSection,
}

impl Default for AcpAgents {
    fn default() -> Self {
        Self {
            claude: AcpAgentSection::baked("claude-agent-acp", &[]),
            codex: AcpAgentSection::baked(
                "codex-acp",
                &[
                    "-c",
                    "sandbox_mode=\"danger-full-access\"",
                    "-c",
                    "approval_policy=\"never\"",
                ],
            ),
            gemini: AcpAgentSection::baked("gemini", &["--yolo", "--acp"]),
            kimi: AcpAgentSection::baked("kimi", &["--yolo", "--thinking", "acp"]),
            opencode: AcpAgentSection::baked("opencode", &["acp"]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AcpSection {
    pub policy: AcpPolicySection,
    pub install: AcpInstallSection,
    pub agents: AcpAgents,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerSection {
    pub full_review_interval: Override<u32>,
}

impl Default for RunnerSection {
    fn default() -> Self {
        Self {
            full_review_interval: Override::baked(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsSection {
    pub cache_root: Override<String>,
    pub sessions_root: Override<String>,
    pub runs_root: Override<String>,
    pub memory_root: Override<String>,
}

impl Default for PathsSection {
    fn default() -> Self {
        Self {
            cache_root: Override::baked(".codexize/cache".to_string()),
            sessions_root: Override::baked(".codexize/sessions".to_string()),
            runs_root: Override::baked(".codexize/runs".to_string()),
            memory_root: Override::baked(".codexize/memory".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiColonPalette {
    pub show_help: Override<bool>,
}

impl Default for UiColonPalette {
    fn default() -> Self {
        Self {
            show_help: Override::baked(true),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiFooter {
    pub show_keys: Override<bool>,
}

impl Default for UiFooter {
    fn default() -> Self {
        Self {
            show_keys: Override::baked(true),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSection {
    pub prefer_split_on_open: Override<bool>,
    pub colon_palette: UiColonPalette,
    pub footer: UiFooter,
}

impl Default for UiSection {
    fn default() -> Self {
        Self {
            prefer_split_on_open: Override::baked(false),
            colon_palette: UiColonPalette::default(),
            footer: UiFooter::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsSection {
    pub log_level: Override<LogLevel>,
    pub json_logs: Override<bool>,
}

impl Default for DiagnosticsSection {
    fn default() -> Self {
        Self {
            log_level: Override::baked(LogLevel::Info),
            json_logs: Override::baked(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySection {
    pub enabled: Override<bool>,
    pub max_topics_per_read: Override<u32>,
    pub journal_retention_months: Override<u32>,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            enabled: Override::baked(true),
            max_topics_per_read: Override::baked(6),
            journal_retention_months: Override::baked(12),
        }
    }
}

/// The full v1 config tree — defaults match today's runtime, overrides
/// come from `~/.codexize/config.toml` per the spec's load-on-launch
/// contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    pub meta: MetaSection,
    pub ntfy: NtfySection,
    pub acp: AcpSection,
    pub runner: RunnerSection,
    pub paths: PathsSection,
    pub ui: UiSection,
    pub diagnostics: DiagnosticsSection,
    pub memory: MemorySection,
    pub free_models: Override<Vec<FreeModelEntry>>,
}

impl Config {
    /// Construct a `Config` populated from the baked defaults (no
    /// overrides). Equivalent to `Config::default()` but named to match
    /// the spec's vocabulary.
    pub fn baked_defaults() -> Self {
        Self::default()
    }

    /// Apply every validation rule from spec §3 and return the first
    /// failure as a single human-readable string. Subsequent CLI/TUI
    /// surfaces will surface multiple errors at once; for now the loader
    /// only needs to reject bad files at launch.
    pub fn validate(&self) -> Result<(), String> {
        let n = &self.ntfy;
        if *n.enabled.value() && n.server.value().trim().is_empty() {
            return Err("ntfy.server must be non-empty when ntfy.enabled = true".into());
        }
        if !n.server.value().trim().is_empty() {
            let s = n.server.value().trim();
            if !(s.starts_with("http://") || s.starts_with("https://")) {
                return Err("ntfy.server must start with http:// or https://".into());
            }
        }
        if !n.topic.value().is_empty() && !valid_ntfy_topic(n.topic.value()) {
            return Err("ntfy.topic has invalid characters or is too short".into());
        }
        if *n.retry_attempts.value() < 1 {
            return Err("ntfy.retry_attempts must be >= 1".into());
        }
        if *n.http_timeout_secs.value() < 1 {
            return Err("ntfy.http_timeout_secs must be >= 1".into());
        }
        if *n.body_max_bytes.value() < 256 {
            return Err("ntfy.body_max_bytes must be >= 256".into());
        }
        if *n.excerpt_max_chars.value() < 32 {
            return Err("ntfy.excerpt_max_chars must be >= 32".into());
        }

        for (vendor, agent) in [
            ("claude", &self.acp.agents.claude),
            ("codex", &self.acp.agents.codex),
            ("gemini", &self.acp.agents.gemini),
            ("kimi", &self.acp.agents.kimi),
            ("opencode", &self.acp.agents.opencode),
        ] {
            if *agent.enabled.value() && agent.program.value().trim().is_empty() {
                return Err(format!(
                    "acp.agents.{vendor}.program must be non-empty when enabled = true"
                ));
            }
            for key in agent.env.value().keys() {
                if key.is_empty() {
                    return Err(format!("acp.agents.{vendor}.env contains an empty key"));
                }
                if key.contains('=') || key.contains('\0') {
                    return Err(format!(
                        "acp.agents.{vendor}.env key {key:?} contains '=' or NUL"
                    ));
                }
                if key.starts_with("CODEXIZE_ACP_") {
                    return Err(format!(
                        "acp.agents.{vendor}.env key {key:?} uses reserved CODEXIZE_ACP_ prefix"
                    ));
                }
            }
        }

        for entry in self.acp.policy.shell_allowlist.value() {
            if entry.trim().is_empty() {
                return Err("acp.policy.shell_allowlist contains an empty entry".into());
            }
        }
        for entry in self.acp.policy.allowed_write_paths.value() {
            if entry.trim().is_empty() {
                return Err("acp.policy.allowed_write_paths contains an empty entry".into());
            }
        }

        if *self.runner.full_review_interval.value() < 1 {
            return Err("runner.full_review_interval must be >= 1".into());
        }

        for (name, value) in [
            ("paths.cache_root", self.paths.cache_root.value()),
            ("paths.sessions_root", self.paths.sessions_root.value()),
            ("paths.runs_root", self.paths.runs_root.value()),
            ("paths.memory_root", self.paths.memory_root.value()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{name} must be non-empty"));
            }
        }

        if *self.memory.max_topics_per_read.value() < 1 {
            return Err("memory.max_topics_per_read must be >= 1".into());
        }
        if *self.memory.journal_retention_months.value() < 1 {
            return Err("memory.journal_retention_months must be >= 1".into());
        }

        for (i, entry) in self.free_models.value().iter().enumerate() {
            if entry.mapped_into.trim().is_empty() {
                return Err(format!(
                    "free_models[{i}].mapped_into must be non-empty"
                ));
            }
            if entry.model_name.trim().is_empty() {
                return Err(format!(
                    "free_models[{i}].model_name must be non-empty"
                ));
            }
        }

        Ok(())
    }
}

/// Topic format used by `notifications::generate_topic`: 16 random bytes
/// rendered as 32 lowercase hex chars. Operator-pasted topics may use
/// the broader ntfy.sh charset (alnum + `-` + `_`) and length ≥22; mirror
/// that here so a hand-picked topic still validates.
fn valid_ntfy_topic(topic: &str) -> bool {
    topic.len() >= 22
        && topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_defaults_validate() {
        Config::baked_defaults().validate().unwrap();
    }

    #[test]
    fn override_equality_ignores_explicit_flag() {
        assert_eq!(Override::baked(3u32), Override::explicit(3u32));
        assert_ne!(Override::baked(3u32), Override::baked(4u32));
    }

    #[test]
    fn validate_rejects_reserved_env_prefix() {
        let mut c = Config::baked_defaults();
        let mut env = BTreeMap::new();
        env.insert("CODEXIZE_ACP_FOO".to_string(), "x".to_string());
        c.acp.agents.claude.env = Override::explicit(env);
        let err = c.validate().unwrap_err();
        assert!(err.contains("CODEXIZE_ACP_"), "{err}");
    }

    #[test]
    fn validate_rejects_low_retry_attempts() {
        let mut c = Config::baked_defaults();
        c.ntfy.retry_attempts = Override::explicit(0);
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_low_runner_interval() {
        let mut c = Config::baked_defaults();
        c.runner.full_review_interval = Override::explicit(0);
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_bad_ntfy_server_scheme() {
        let mut c = Config::baked_defaults();
        c.ntfy.server = Override::explicit("ftp://nope".into());
        assert!(c.validate().is_err());
    }
}
