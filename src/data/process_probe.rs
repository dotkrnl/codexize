//! Cross-process liveness and identity probing shared by `app_lock` and
//! `cache_lock`.
//!
//! Both locks need to answer the same question after a crash: is the pid in
//! the lock file the same long-lived codexize process that wrote it, or a
//! recycled identifier from an unrelated process? The probe surfaces three
//! observations — `is_alive`, `start_time`, `executable` — combined into the
//! [`same_host_holder_alive`] decision used by the lock acquirers.
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use std::path::PathBuf;
use std::process::Command;

/// Maximum drift between the recorded `started_at` and the live process's
/// reported start time before we consider the holder a recycled (i.e.,
/// unrelated) pid. The clamp absorbs the gap between our `Utc::now()` at lock
/// write and the OS's coarser per-process start-time accounting.
pub const STALE_TOLERANCE_SECS: i64 = 30;

/// Object the lock modules consult about another pid on the same host. Real
/// callers use [`RealProbe`]; tests inject a stub so they can drive the
/// stale/live edges without spawning real processes.
pub trait ProcessProbe: Send + Sync {
    fn is_alive(&self, pid: i32) -> bool;
    fn start_time(&self, pid: i32) -> Option<DateTime<Utc>>;
    fn executable(&self, pid: i32) -> Option<PathBuf>;
}

/// Production probe. Liveness uses `kill(pid, 0)`; start-time and executable
/// shell out to `ps` because both macOS and Linux ship the `lstart`/`comm`
/// columns and the cost is paid once at lock-acquire, not in any hot path.
pub struct RealProbe;

impl ProcessProbe for RealProbe {
    fn is_alive(&self, pid: i32) -> bool {
        use nix::sys::signal;
        use nix::unistd::Pid;
        process_alive_from_kill_result(signal::kill(Pid::from_raw(pid), None))
    }

    fn start_time(&self, pid: i32) -> Option<DateTime<Utc>> {
        let output = Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_lstart(&String::from_utf8_lossy(&output.stdout))
    }

    fn executable(&self, pid: i32) -> Option<PathBuf> {
        let output = Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw.is_empty() {
            None
        } else {
            Some(PathBuf::from(raw))
        }
    }
}

/// Returns `true` when the recorded lock owner is plausibly still the live
/// process (alive AND its OS-reported start time is within tolerance OR its
/// executable basename is `codexize`). False means the lock can be replaced.
pub fn same_host_holder_alive(
    pid: i32,
    recorded_started_at: DateTime<Utc>,
    probe: &dyn ProcessProbe,
) -> bool {
    if !probe.is_alive(pid) {
        return false;
    }
    let by_time = match probe.start_time(pid) {
        Some(actual) => (actual - recorded_started_at).num_seconds().abs() <= STALE_TOLERANCE_SECS,
        None => false,
    };
    if by_time {
        return true;
    }
    matches!(
        probe.executable(pid).as_deref().and_then(|p| p.file_name()),
        Some(name) if name == std::ffi::OsStr::new("codexize")
    )
}

/// Best-effort hostname lookup. Falls back to the literal `"unknown"` when
/// the system call refuses or returns non-UTF-8 bytes; the lock writer uses
/// the result as-is, so two startups on a "broken hostname" host still match
/// each other and refuse rather than crashing.
pub fn hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) fn process_alive_from_kill_result(result: Result<(), nix::errno::Errno>) -> bool {
    use nix::errno::Errno;
    matches!(result, Ok(()) | Err(Errno::EPERM))
}

fn parse_lstart(raw: &str) -> Option<DateTime<Utc>> {
    // `ps -o lstart=` outputs `Mon May  5 14:30:05 2026` (single-digit days
    // get a leading space for column alignment). Splitting on whitespace
    // first sidesteps chrono's strict-whitespace matching for the `%e`
    // padding so day-padding variations across `ps` implementations all
    // parse cleanly.
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let canonical = format!("{} {} {} {}", parts[1], parts[2], parts[3], parts[4]);
    let naive = NaiveDateTime::parse_from_str(&canonical, "%b %d %H:%M:%S %Y").ok()?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
#[path = "process_probe_tests.rs"]
pub(crate) mod tests;
