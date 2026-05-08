//! Resolution of `~/.codexize/config.toml` with `CODEXIZE_CONFIG`
//! env-override for tests and operators with non-standard `$HOME`.

use std::path::PathBuf;

pub const CONFIG_ENV: &str = "CODEXIZE_CONFIG";
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Resolved path of the unified config file. The env override is the
/// canonical test seam (see `tests::load_or_default::missing_file`).
pub fn config_path() -> PathBuf {
    if let Some(value) = std::env::var_os(CONFIG_ENV) {
        return PathBuf::from(value);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codexize").join(CONFIG_FILE_NAME)
}

/// Expand a leading `$HOME` or `~/` against the current `HOME` env var.
/// Used by the loader to resolve `[paths]` and `[acp.install]` values
/// at load time per spec §3.
pub fn expand_home(value: &str) -> String {
    let home = std::env::var_os("HOME")
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if home.is_empty() {
        return value.to_string();
    }
    if let Some(rest) = value.strip_prefix("$HOME") {
        return format!("{home}{rest}");
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    if value == "~" {
        return home;
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_home_handles_both_prefixes() {
        let prev = std::env::var_os("HOME");
        // SAFETY: tests in this crate already mutate env via the
        // documented `unsafe` pattern; expand_home is pure-string and
        // we restore on exit.
        unsafe {
            std::env::set_var("HOME", "/tmp/fakehome");
        }
        assert_eq!(expand_home("$HOME/x"), "/tmp/fakehome/x");
        assert_eq!(expand_home("~/y"), "/tmp/fakehome/y");
        assert_eq!(expand_home("~"), "/tmp/fakehome");
        assert_eq!(expand_home("/abs"), "/abs");
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
