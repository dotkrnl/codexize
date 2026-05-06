use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_NTFY_SERVER: &str = "https://ntfy.sh";
const NTFY_CONFIG_ENV: &str = "CODEXIZE_NTFY_CONFIG";
const NTFY_CONFIG_VERSION: u32 = 1;
const TOPIC_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NtfyDetailMode {
    Detailed,
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NtfyConfig {
    pub version: u32,
    pub server: String,
    pub topic: String,
    pub enabled: bool,
    pub detail_mode: NtfyDetailMode,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl NtfyConfig {
    pub fn subscribe_url(&self) -> String {
        format!("{}/{}", self.server.trim_end_matches('/'), self.topic)
    }
}

pub fn load_ntfy_config() -> Option<NtfyConfig> {
    load_ntfy_config_at(&ntfy_config_path())
}

pub fn ensure_ntfy_config(reset: bool) -> Result<NtfyConfig> {
    ensure_ntfy_config_at(&ntfy_config_path(), reset)
}

pub(crate) fn load_ntfy_config_at(path: &Path) -> Option<NtfyConfig> {
    let text = fs::read_to_string(path).ok()?;
    let config: NtfyConfig = toml::from_str(&text).ok()?;
    validate_enabled_config(config).ok()
}

pub(crate) fn ensure_ntfy_config_at(path: &Path, reset: bool) -> Result<NtfyConfig> {
    if !reset && let Some(config) = load_ntfy_config_at(path) {
        return Ok(config);
    }

    let now = Utc::now();
    let created_at = if reset {
        load_ntfy_config_at(path)
            .map(|config| config.created_at)
            .unwrap_or(now)
    } else {
        now
    };
    let config = NtfyConfig {
        version: NTFY_CONFIG_VERSION,
        server: DEFAULT_NTFY_SERVER.to_string(),
        topic: generate_topic()?,
        enabled: true,
        detail_mode: NtfyDetailMode::Detailed,
        created_at,
        updated_at: now,
    };
    atomic_write_config(path, &config)?;
    Ok(config)
}

pub(crate) fn generate_topic() -> Result<String> {
    let mut bytes = [0_u8; TOPIC_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|err| anyhow::anyhow!("failed to generate ntfy topic entropy: {err}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ntfy_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os(NTFY_CONFIG_ENV) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".codexize").join("ntfy.toml")
}

fn validate_enabled_config(config: NtfyConfig) -> Result<NtfyConfig> {
    if config.version != NTFY_CONFIG_VERSION {
        bail!("unsupported ntfy config version");
    }
    if !config.enabled {
        bail!("ntfy config is disabled");
    }
    if config.server.trim().is_empty() {
        bail!("ntfy server is empty");
    }
    if !valid_topic(&config.topic) {
        bail!("ntfy topic is invalid");
    }
    Ok(config)
}

fn valid_topic(topic: &str) -> bool {
    topic.len() >= 22
        && topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn atomic_write_config(path: &Path, config: &NtfyConfig) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).context("failed to create ntfy config directory")?;
    let tmp_path = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("ntfy.toml"),
        std::process::id()
    ));
    let text = toml::to_string_pretty(config).context("failed to serialise ntfy config")?;
    {
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut tmp = options
            .open(&tmp_path)
            .context("failed to create temp ntfy config file")?;
        tmp.write_all(text.as_bytes())
            .context("failed to write temp ntfy config file")?;
        tmp.sync_all()
            .context("failed to sync temp ntfy config file")?;
    }
    fs::rename(&tmp_path, path).context("failed to rename temp ntfy config file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .context("failed to set ntfy config permissions")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
