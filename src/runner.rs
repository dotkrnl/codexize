use crate::{
    acp::{AcpConfig, AcpConnector, AcpLaunchRequest, ClientUpdate, PromptPayload, SubprocessConnector},
    adapters::AgentRun,
    selection::VendorKind,
};
#[cfg(test)]
use crate::adapters::shell_escape;
use crate::state;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default)]
pub struct ChildLaunch {
    program: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
    stdin_null: bool,
    stdout_null: bool,
    stderr_null: bool,
}

impl ChildLaunch {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    pub fn stdin_null(mut self) -> Self {
        self.stdin_null = true;
        self
    }

    pub fn stdout_null(mut self) -> Self {
        self.stdout_null = true;
        self
    }

    pub fn stderr_null(mut self) -> Self {
        self.stderr_null = true;
        self
    }
}

/// Finish stamp written by the runner-owned wrapper after every agent attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinishStamp {
    pub finished_at: String,
    pub exit_code: i32,
    pub head_before: String,
    pub head_after: String,
    pub head_state: String,
    #[serde(default)]
    pub signal_received: String,
    #[serde(default)]
    pub working_tree_clean: bool,
}

/// Atomic write of a finish stamp: write to a temp file in the same directory,
/// then rename into place.
pub fn write_finish_stamp(path: &Path, stamp: &FinishStamp) -> Result<()> {
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&dir)?;

    let tmp_path = dir.join(format!(".tmp.{}.toml", std::process::id()));
    let text = toml::to_string_pretty(stamp).context("failed to serialize finish stamp")?;
    fs::write(&tmp_path, text)
        .with_context(|| format!("failed to write temp stamp {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename stamp to {}", path.display()))?;
    Ok(())
}

/// Read and parse a finish stamp from disk.
pub fn read_finish_stamp(path: &Path) -> Result<FinishStamp> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read finish stamp {}", path.display()))?;
    let stamp: FinishStamp = toml::from_str(&text)
        .with_context(|| format!("failed to parse finish stamp {}", path.display()))?;
    Ok(stamp)
}

/// Default stabilization budget in milliseconds.
const DEFAULT_STAMP_STABILIZE_BUDGET_MS: u64 = 1500;
/// Default interval between HEAD reads in milliseconds.
const DEFAULT_STAMP_STABILIZE_INTERVAL_MS: u64 = 100;

/// Environment variable overrides for stabilization timing.
const ENV_STAMP_STABILIZE_MS: &str = "CODEXIZE_STAMP_STABILIZE_MS";
const ENV_STAMP_STABILIZE_INTERVAL_MS: &str = "CODEXIZE_STAMP_STABILIZE_INTERVAL_MS";

const ACP_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcpCancelReason {
    Terminate,
}

#[derive(Debug)]
struct ManagedAcpRun {
    cancel_tx: mpsc::Sender<AcpCancelReason>,
    finished: std::sync::Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
struct ManagedAcpLaunch {
    resolved: crate::acp::AcpResolvedLaunch,
    status_path: PathBuf,
    stamp_path: PathBuf,
    required_artifact: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct ManagedAcpOutcome {
    exit_code: i32,
    signal_received: String,
}

fn active_acp_runs() -> &'static Mutex<std::collections::HashMap<String, ManagedAcpRun>> {
    static ACTIVE: OnceLock<Mutex<std::collections::HashMap<String, ManagedAcpRun>>> =
        OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn build_managed_acp_launch(
    vendor: VendorKind,
    run: &AgentRun,
    status_path: &Path,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
    interactive: bool,
) -> Result<ManagedAcpLaunch> {
    let cwd = std::env::current_dir().context("failed to capture launch cwd")?;
    let request = AcpLaunchRequest {
        vendor,
        cwd,
        prompt: PromptPayload::File(run.prompt_path.clone()),
        model: run.model.clone(),
        // The current launch sites already pass the codexize-computed effective
        // effort. Task 2 keeps artifact/finalization ownership in codexize and
        // defers the requested-vs-effective UI split to the later ACP UX work.
        requested_effort: run.effort,
        effective_effort: run.effort,
        interactive,
        modes: run.modes,
        required_artifacts: required_artifact
            .into_iter()
            .map(Path::to_path_buf)
            .collect(),
    };
    let resolved = AcpConfig::default()
        .resolve(&request)
        .map_err(|err| anyhow!("{err}"))?;
    ensure_program_exists(&resolved.spawn.program)?;

    Ok(ManagedAcpLaunch {
        resolved,
        status_path: status_path.to_path_buf(),
        stamp_path: artifacts_dir.join("run-finish").join(format!("{run_key}.toml")),
        required_artifact: required_artifact.map(Path::to_path_buf),
    })
}

fn ensure_program_exists(program: &str) -> Result<()> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        if candidate.exists() {
            return Ok(());
        }
        bail!("ACP agent CLI not found — install it first");
    }

    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|dir| dir.join(program).exists()) {
        Ok(())
    } else {
        bail!("ACP agent CLI not found — install it first");
    }
}

fn write_status_code(path: &Path, exit_code: i32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create status dir {}", parent.display()))?;
    }
    fs::write(path, exit_code.to_string())
        .with_context(|| format!("failed to write run status {}", path.display()))
}

fn git_rev_parse_head() -> Option<String> {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
}

fn working_tree_clean() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout.is_empty())
        .unwrap_or(false)
}

fn stamp_stabilize_budget() -> Duration {
    std::env::var(ENV_STAMP_STABILIZE_MS)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_STABILIZE_BUDGET_MS))
}

fn stamp_stabilize_interval() -> Duration {
    std::env::var(ENV_STAMP_STABILIZE_INTERVAL_MS)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_STABILIZE_INTERVAL_MS))
}

fn wait_for_stable_head() -> (String, String) {
    let budget = stamp_stabilize_budget();
    let interval = stamp_stabilize_interval();
    let deadline = Instant::now() + budget;

    loop {
        let lock_path = Path::new(".git").join("index.lock");
        while lock_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }

        let first = git_rev_parse_head().unwrap_or_default();
        thread::sleep(interval);
        let second = git_rev_parse_head().unwrap_or_default();
        if first == second {
            return (second, "stable".to_string());
        }
        if Instant::now() >= deadline {
            return (second, "unstable".to_string());
        }
    }
}

fn write_finish_stamp_for_outcome(
    stamp_path: &Path,
    head_before: String,
    outcome: &ManagedAcpOutcome,
) -> Result<()> {
    let (head_after, head_state) = wait_for_stable_head();
    let stamp = FinishStamp {
        finished_at: chrono::Utc::now().to_rfc3339(),
        exit_code: outcome.exit_code,
        head_before,
        head_after,
        head_state,
        signal_received: outcome.signal_received.clone(),
        working_tree_clean: working_tree_clean(),
    };
    write_finish_stamp(stamp_path, &stamp)
}

fn run_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel_rx: mpsc::Receiver<AcpCancelReason>,
) -> Result<ManagedAcpOutcome> {
    let head_before = git_rev_parse_head().unwrap_or_default();
    let connector = SubprocessConnector;
    let mut session = connector
        .connect(&launch.resolved)
        .map_err(|err| anyhow!("{err}"))?;

    let outcome = loop {
        if cancel_rx.try_recv().is_ok() {
            session.close().map_err(|err| anyhow!("{err}"))?;
            break ManagedAcpOutcome {
                exit_code: 143,
                signal_received: "TERM".to_string(),
            };
        }

        match session.try_next_update().map_err(|err| anyhow!("{err}"))? {
            Some(ClientUpdate::PromptTurnFinished) => {
                session.close().map_err(|err| anyhow!("{err}"))?;
                if let Some(path) = launch.required_artifact.as_deref() {
                    validate_toml_artifacts(&[path])?;
                }
                break ManagedAcpOutcome {
                    exit_code: 0,
                    signal_received: String::new(),
                };
            }
            Some(ClientUpdate::PromptTurnFailed { .. }) => {
                session.close().map_err(|err| anyhow!("{err}"))?;
                break ManagedAcpOutcome {
                    exit_code: 1,
                    signal_received: String::new(),
                };
            }
            Some(
                ClientUpdate::AgentMessageText(_)
                | ClientUpdate::AgentThoughtText(_)
                | ClientUpdate::SessionInfoUpdate { .. }
                | ClientUpdate::Unknown { .. },
            )
            | None => thread::sleep(ACP_POLL_INTERVAL),
        }
    };

    write_status_code(&launch.status_path, outcome.exit_code)?;
    write_finish_stamp_for_outcome(&launch.stamp_path, head_before, &outcome)?;
    Ok(outcome)
}

fn finalize_managed_acp_launch(
    launch: ManagedAcpLaunch,
    cancel_rx: mpsc::Receiver<AcpCancelReason>,
) {
    if run_managed_acp_launch(launch.clone(), cancel_rx).is_ok() {
        return;
    }

    let fallback_head_before = git_rev_parse_head().unwrap_or_default();
    let outcome = ManagedAcpOutcome {
        exit_code: 1,
        signal_received: String::new(),
    };
    let _ = write_status_code(&launch.status_path, outcome.exit_code);
    let _ = write_finish_stamp_for_outcome(&launch.stamp_path, fallback_head_before, &outcome);
}

fn cleanup_finished_acp_runs() {
    let mut finished = Vec::new();
    {
        let guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (window_name, run) in guard.iter() {
            if run.finished.load(Ordering::SeqCst) {
                finished.push(window_name.clone());
            }
        }
    }
    for window_name in finished {
        if let Some(mut run) = take_managed_acp_run(&window_name)
            && let Some(handle) = run.join.take()
        {
            let _ = handle.join();
        }
    }
}

fn take_managed_acp_run(window_name: &str) -> Option<ManagedAcpRun> {
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(window_name)
}

fn launch_managed_acp_window(window_name: &str, launch: ManagedAcpLaunch) -> Result<()> {
    cleanup_finished_acp_runs();

    let mut guard = active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.values().any(|run| !run.finished.load(Ordering::SeqCst)) {
        bail!("codexize only supports one active ACP run at a time");
    }

    let (cancel_tx, cancel_rx) = mpsc::channel();
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let finished_flag = std::sync::Arc::clone(&finished);
    let launch_window = window_name.to_string();
    let handle = thread::spawn(move || {
        finalize_managed_acp_launch(launch, cancel_rx);
        finished_flag.store(true, Ordering::SeqCst);
    });
    guard.insert(
        launch_window,
        ManagedAcpRun {
            cancel_tx,
            finished,
            join: Some(handle),
        },
    );
    Ok(())
}

pub fn window_is_active(window_name: &str) -> bool {
    cleanup_finished_acp_runs();
    active_acp_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(window_name)
        .is_some_and(|run| !run.finished.load(Ordering::SeqCst))
}

pub fn cancel_windows_matching(base: &str) {
    let prefix = format!("{base} ");
    let matching = {
        let guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard
            .keys()
            .filter(|name| *name == base || name.starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>()
    };

    for window_name in matching {
        if let Some(mut run) = take_managed_acp_run(&window_name) {
            let _ = run.cancel_tx.send(AcpCancelReason::Terminate);
            if let Some(handle) = run.join.take() {
                let _ = handle.join();
            }
        }
    }
}

pub fn request_window_exit(window_name: &str) {
    // Until the interactive ACP command surface lands, local `/exit` requests
    // resolve by closing the managed ACP session directly so task-finish and
    // yolo cleanup still terminate the child process deterministically.
    cancel_windows_matching(window_name);
}

pub fn shutdown_all_runs() {
    let runs = {
        let mut guard = active_acp_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard).into_values().collect::<Vec<_>>()
    };

    for mut run in runs {
        let _ = run.cancel_tx.send(AcpCancelReason::Terminate);
        if let Some(handle) = run.join.take() {
            let _ = handle.join();
        }
    }
}

/// Build the shell command that runs the agent and then writes a finish stamp.
#[cfg(test)]
fn build_shell_cmd(
    agent_cmd: &str,
    window_name: &str,
    status_dir: &str,
    status_path: &str,
    finish_dir: &str,
    stamp_path: &str,
) -> String {
    build_shell_cmd_with_mode(
        agent_cmd,
        window_name,
        status_dir,
        status_path,
        finish_dir,
        stamp_path,
        ShellAgentMode::Background,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum ShellAgentMode {
    Foreground,
    Background,
}

#[cfg(test)]
fn build_shell_cmd_with_mode(
    agent_cmd: &str,
    window_name: &str,
    status_dir: &str,
    status_path: &str,
    finish_dir: &str,
    stamp_path: &str,
    mode: ShellAgentMode,
) -> String {
    // Compute interval as a decimal string for `sleep` (e.g. "0.1").
    let interval_ms = DEFAULT_STAMP_STABILIZE_INTERVAL_MS;
    let interval_sec = format!("{:.3}", interval_ms as f64 / 1000.0)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string();
    // Reviewer note: interrupted attempts are stamped with 128+signal to keep
    // a deterministic non-zero exit code even when the child status is not recoverable.

    let agent_invocation = match mode {
        ShellAgentMode::Foreground => format!(
            r#"{agent_cmd}
exit_code=$?"#,
            agent_cmd = agent_cmd,
        ),
        ShellAgentMode::Background => format!(
            r#"{agent_cmd} &
child_pid=$!
wait "$child_pid"
exit_code=$?"#,
            agent_cmd = agent_cmd,
        ),
    };

    format!(
        r#"mkdir -p {status_dir} {finish_dir}
head_before=$(git rev-parse HEAD 2>/dev/null || echo "")
status_file={status_path}
tmp_stamp={tmp_stamp}
stamp_file={stamp_path}
exit_code=0
child_pid=""
finalized=0
trapped_signal=""

finalize() {{
    if [ "$finalized" -eq 1 ]; then
        return
    fi
    finalized=1
    printf "%s" "$exit_code" > "$status_file"

    budget_ms=${{{env_budget}:-{budget}}}
    interval_ms=${{{env_interval}:-{interval}}}
    if [ "$interval_ms" -le 0 ]; then
        interval_ms={interval}
    fi
    iterations=$((budget_ms / interval_ms))
    if [ "$iterations" -lt 1 ]; then
        iterations=1
    fi
    last_head=""
    head_state="unstable"
    working_tree_clean=false
    i=0
    while [ "$i" -lt "$iterations" ]; do
        while [ -f .git/index.lock ]; do
            sleep 0.05
            i=$((i + 1))
            if [ "$i" -ge "$iterations" ]; then break 2; fi
        done
        h1=$(git rev-parse HEAD 2>/dev/null || echo "")
        sleep {interval_sec}
        i=$((i + 1))
        h2=$(git rev-parse HEAD 2>/dev/null || echo "")
        if [ "$h1" = "$h2" ]; then
            last_head="$h1"
            head_state="stable"
            break
        fi
        last_head="$h2"
    done

    if git_status=$(git status --porcelain 2>/dev/null) && [ -z "$git_status" ]; then
        working_tree_clean=true
    fi

    finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    cat > "$tmp_stamp" << STAMPEOF
finished_at = "{finished_at_placeholder}"
exit_code = {exit_code_placeholder}
head_before = "{head_before_placeholder}"
head_after = "{head_after_placeholder}"
head_state = "{head_state_placeholder}"
signal_received = "{signal_received_placeholder}"
working_tree_clean = {working_tree_clean_placeholder}
STAMPEOF
    mv "$tmp_stamp" "$stamp_file"
}}

on_signal() {{
    signal_name="$1"
    case "$signal_name" in
        HUP) signal_code=1 ;;
        INT) signal_code=2 ;;
        TERM) signal_code=15 ;;
        *) signal_code=0 ;;
    esac
    trapped_signal="$signal_name"
    exit_code=$((128 + signal_code))
    if [ -n "$child_pid" ] && kill -0 "$child_pid" 2>/dev/null; then
        kill -"$signal_name" "$child_pid" 2>/dev/null || kill -TERM "$child_pid" 2>/dev/null || true
        wait "$child_pid" 2>/dev/null || true
    fi
    finalize
    trap - EXIT HUP INT TERM
    exit "$exit_code"
}}

trap 'status=$?; if [ "$finalized" -eq 0 ]; then exit_code=$status; finalize; fi' EXIT
trap '' HUP
trap 'on_signal INT' INT
trap 'on_signal TERM' TERM
printf '\033[1;36m>>> starting %s...\033[0m\n\n' {name}
{agent_invocation}
finalize
trap - EXIT HUP INT TERM
exit $exit_code"#,
        status_dir = shell_escape(status_dir),
        finish_dir = shell_escape(finish_dir),
        status_path = shell_escape(status_path),
        name = shell_escape(window_name),
        agent_invocation = agent_invocation,
        budget = DEFAULT_STAMP_STABILIZE_BUDGET_MS,
        interval = interval_ms,
        env_budget = ENV_STAMP_STABILIZE_MS,
        env_interval = ENV_STAMP_STABILIZE_INTERVAL_MS,
        interval_sec = interval_sec,
        tmp_stamp = shell_escape(&format!("{}.tmp", stamp_path)),
        finished_at_placeholder = "$finished_at",
        exit_code_placeholder = "$exit_code",
        head_before_placeholder = "$head_before",
        head_after_placeholder = "$last_head",
        head_state_placeholder = "$head_state",
        signal_received_placeholder = "$trapped_signal",
        working_tree_clean_placeholder = "$working_tree_clean",
        stamp_path = shell_escape(stamp_path),
    )
}

pub fn run(
    session_id: String,
    phase: String,
    role: String,
    artifacts: Vec<String>,
    command: Vec<String>,
) -> Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow!("no command provided to agent-run"))?;

    let dir = state::session_dir(&session_id);
    fs::create_dir_all(&dir)?;

    let log_path = dir.join(format!("{role}.log"));
    let mut log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    writeln!(
        log_file,
        "--- Agent Run Started: phase={phase}, role={role} ---"
    )?;
    writeln!(log_file, "Command: {command:?}")?;
    if !artifacts.is_empty() {
        writeln!(log_file, "Required artifacts: {artifacts:?}")?;
    }

    print_title_box(&phase, &role, &command);

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn: {:?}", command))?;

    // The piped stdio setup above is a documented invariant, but keep this as
    // a recoverable error so process-boundary failures never become panics.
    let stdout = child
        .stdout
        .take()
        .context("runner stdout pipe missing after piped spawn")?;
    let stderr = child
        .stderr
        .take()
        .context("runner stderr pipe missing after piped spawn")?;

    let mut log_out = log_file.try_clone()?;
    let stdout_handle = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("{line}");
            let _ = writeln!(log_out, "[OUT] {line}");
        }
    });

    let mut log_err = log_file.try_clone()?;
    let stderr_handle = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = writeln!(log_err, "[ERR] {line}");
        }
    });

    let status = child.wait()?;
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    writeln!(log_file, "--- Agent Run Finished: status={status} ---")?;

    // Validate required artifacts regardless of exit status
    let mut missing: Vec<&str> = Vec::new();
    for path in &artifacts {
        if !PathBuf::from(path).exists() {
            missing.push(path);
            writeln!(log_file, "[MISSING ARTIFACT] {path}")?;
        }
    }

    if !missing.is_empty() {
        bail!(
            "agent exited but required artifacts are missing:\n{}",
            missing.join("\n")
        );
    }

    if !status.success() {
        bail!("agent command failed with status: {status}");
    }

    Ok(())
}

/// Validate that all required TOML artifacts exist and are parseable.
/// Missing or malformed artifacts signal an incomplete agent turn; the
/// orchestrator should retry the agent execution phase.
pub fn validate_toml_artifacts(paths: &[&Path]) -> Result<()> {
    let mut errors = Vec::new();
    for path in paths {
        if !path.exists() {
            errors.push(format!("missing: {}", path.display()));
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            let text = fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if let Err(e) = toml::from_str::<toml::Value>(&text) {
                errors.push(format!("malformed TOML in {}: {e}", path.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!(
            "incomplete agent turn — artifact validation failed:\n{}",
            errors.join("\n")
        )
    }
}

/// Launch an agent interactively inside a new tmux window.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic is guaranteed to run.
#[allow(clippy::too_many_arguments)]
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    switch: bool,
    status_path: &Path,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
) -> Result<()> {
    let _ = switch;
    let launch = build_managed_acp_launch(
        vendor,
        run,
        status_path,
        run_key,
        artifacts_dir,
        required_artifact,
        true,
    )?;
    launch_managed_acp_window(window_name, launch)
}

/// Launch an agent non-interactively inside a new tmux window.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic is guaranteed to run.
pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    vendor: VendorKind,
    status_path: &Path,
    run_key: &str,
    artifacts_dir: &Path,
    required_artifact: Option<&Path>,
) -> Result<()> {
    let launch = build_managed_acp_launch(
        vendor,
        run,
        status_path,
        run_key,
        artifacts_dir,
        required_artifact,
        false,
    )?;
    launch_managed_acp_window(window_name, launch)
}

pub fn run_child_with_timeout(
    launch: &ChildLaunch,
    timeout: Duration,
) -> Result<Option<ExitStatus>> {
    let mut command = Command::new(&launch.program);
    command.args(&launch.args);
    for (key, value) in &launch.envs {
        command.env(key, value);
    }
    if launch.stdin_null {
        command.stdin(Stdio::null());
    }
    if launch.stdout_null {
        command.stdout(Stdio::null());
    }
    if launch.stderr_null {
        command.stderr(Stdio::null());
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn: {:?}", launch))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn extract_model(command: &[String]) -> String {
    let mut it = command.iter();
    while let Some(arg) = it.next() {
        if arg == "--model" || arg == "-m" {
            if let Some(val) = it.next() {
                return val.clone();
            }
        } else if let Some(val) = arg.strip_prefix("--model=") {
            return val.to_string();
        }
    }
    command
        .first()
        .and_then(|bin| std::path::Path::new(bin).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

fn print_title_box(phase: &str, role: &str, command: &[String]) {
    let model = extract_model(command);
    let line = format!(" {role} · {phase} · model: {model} ");
    let width = line.chars().count().max(40);
    let pad = width - line.chars().count();
    let top = format!("╭{}╮", "─".repeat(width));
    let mid = format!("│{line}{}│", " ".repeat(pad));
    let bot = format!("╰{}╯", "─".repeat(width));
    println!("{top}");
    println!("{mid}");
    println!("{bot}");
}

#[cfg(test)]
mod tests_mod;
