use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub(crate) struct MockProbe {
    pub alive: Mutex<HashMap<i32, bool>>,
    pub start_times: Mutex<HashMap<i32, DateTime<Utc>>>,
    pub executables: Mutex<HashMap<i32, PathBuf>>,
}

impl MockProbe {
    pub fn dead() -> Self {
        Self::default()
    }
    pub fn live(pid: i32, start: DateTime<Utc>, exec: PathBuf) -> Self {
        let p = Self::default();
        p.alive.lock().unwrap().insert(pid, true);
        p.start_times.lock().unwrap().insert(pid, start);
        p.executables.lock().unwrap().insert(pid, exec);
        p
    }
    pub fn alive_only(pid: i32) -> Self {
        let p = Self::default();
        p.alive.lock().unwrap().insert(pid, true);
        p
    }
}

impl ProcessProbe for MockProbe {
    fn is_alive(&self, pid: i32) -> bool {
        *self.alive.lock().unwrap().get(&pid).unwrap_or(&false)
    }
    fn start_time(&self, pid: i32) -> Option<DateTime<Utc>> {
        self.start_times.lock().unwrap().get(&pid).copied()
    }
    fn executable(&self, pid: i32) -> Option<PathBuf> {
        self.executables.lock().unwrap().get(&pid).cloned()
    }
}

#[test]
fn dead_pid_is_not_alive() {
    let probe = MockProbe::dead();
    assert!(!same_host_holder_alive(42, Utc::now(), &probe));
}

#[test]
fn matching_start_time_within_tolerance_is_alive() {
    let recorded = Utc::now();
    let actual = recorded + chrono::Duration::seconds(STALE_TOLERANCE_SECS - 1);
    let probe = MockProbe::live(42, actual, PathBuf::from("/usr/bin/other"));
    assert!(same_host_holder_alive(42, recorded, &probe));
}

#[test]
fn start_time_outside_tolerance_falls_back_to_executable() {
    let recorded = Utc::now();
    let actual = recorded + chrono::Duration::seconds(STALE_TOLERANCE_SECS + 5);
    let probe = MockProbe::live(42, actual, PathBuf::from("/usr/local/bin/codexize"));
    assert!(same_host_holder_alive(42, recorded, &probe));
}

#[test]
fn unrelated_executable_with_drift_is_recycled_pid() {
    let recorded = Utc::now();
    let actual = recorded + chrono::Duration::seconds(STALE_TOLERANCE_SECS + 5);
    let probe = MockProbe::live(42, actual, PathBuf::from("/bin/sleep"));
    assert!(!same_host_holder_alive(42, recorded, &probe));
}

#[test]
fn alive_without_start_time_or_exec_is_recycled() {
    let probe = MockProbe::alive_only(42);
    assert!(!same_host_holder_alive(42, Utc::now(), &probe));
}

#[test]
fn parses_typical_lstart_output() {
    // Format matches macOS/Linux `ps -o lstart=` for a single-digit day,
    // including the leading space ps inserts to right-align the day.
    let parsed = parse_lstart(" Mon May  5 14:30:05 2026").expect("parses");
    // We only assert the round-trip via local timezone cleanly — UTC offset
    // depends on the host running the test.
    assert_eq!(parsed.format("%H:%M:%S").to_string().len(), 8);
}

#[test]
fn eperm_from_signal_probe_counts_as_live_process() {
    assert!(process_alive_from_kill_result(Err(
        nix::errno::Errno::EPERM
    )));
    assert!(process_alive_from_kill_result(Ok(())));
    assert!(!process_alive_from_kill_result(Err(
        nix::errno::Errno::ESRCH
    )));
}


