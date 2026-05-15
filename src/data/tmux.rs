//! tmux integration helpers.
//!
//! When codexize is launched inside a tmux session, we relabel the active
//! window so the operator can tell at a glance which directory each
//! codexize instance is working on. The `tmux rename-window` subprocess
//! is used (rather than the in-band `\ek…\e\\` screen-style escape)
//! because it works regardless of the user's `allow-rename` setting,
//! which is off by default in modern tmux.
use std::path::Path;
/// If running inside tmux, set the active window's name to the
/// basename of the current working directory. Best-effort: silently
/// no-ops outside tmux, when `tmux` is not on `PATH`, when the cwd
/// has no usable basename, or when the rename subprocess otherwise
/// fails.
pub fn maybe_set_window_title() {
    if std::env::var_os("TMUX").is_none() {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Some(name) = directory_basename(&cwd) else {
        return;
    };
    let _ = std::process::Command::new("tmux")
        .arg("rename-window")
        .arg(&name)
        .status();
}
fn directory_basename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    let trimmed = name.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
