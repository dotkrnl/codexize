use crate::adapters::{AgentAdapter, AgentRun, shell_escape};
use crate::state;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
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

/// Build the shell command that runs the agent and then writes a finish stamp.
fn build_shell_cmd(
    agent_cmd: &str,
    window_name: &str,
    status_dir: &str,
    status_path: &str,
    finish_dir: &str,
    stamp_path: &str,
) -> String {
    // Compute interval as a decimal string for `sleep` (e.g. "0.1").
    let interval_ms = DEFAULT_STAMP_STABILIZE_INTERVAL_MS;
    let interval_sec = format!("{:.3}", interval_ms as f64 / 1000.0)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string();
    // Reviewer note: interrupted attempts are stamped with 128+signal to keep
    // a deterministic non-zero exit code even when the child status is not recoverable.

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
{agent_cmd} &
child_pid=$!
wait "$child_pid"
exit_code=$?
finalize
trap - EXIT HUP INT TERM
exit $exit_code"#,
        status_dir = shell_escape(status_dir),
        finish_dir = shell_escape(finish_dir),
        status_path = shell_escape(status_path),
        name = shell_escape(window_name),
        agent_cmd = agent_cmd,
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
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    switch: bool,
    status_path: &Path,
    run_key: &str,
    artifacts_dir: &Path,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.interactive_command(&run.model, &prompt_path, run.effort);
    launch_in_window(
        window_name,
        &cmd,
        adapter,
        switch,
        status_path,
        run_key,
        artifacts_dir,
    )
}

/// Launch an agent non-interactively inside a new tmux window.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic is guaranteed to run.
pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    status_path: &Path,
    run_key: &str,
    artifacts_dir: &Path,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.noninteractive_command(&run.model, &prompt_path, run.effort);
    launch_in_window(
        window_name,
        &cmd,
        adapter,
        false,
        status_path,
        run_key,
        artifacts_dir,
    )
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

fn launch_in_window(
    window_name: &str,
    agent_cmd: &str,
    adapter: &dyn AgentAdapter,
    switch: bool,
    status_path: &Path,
    run_key: &str,
    artifacts_dir: &Path,
) -> Result<()> {
    if !adapter.detect() {
        bail!("agent CLI not found — install it first");
    }

    let status_dir = status_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy()
        .into_owned();
    let status_path_str = status_path.to_string_lossy().into_owned();

    let finish_dir = artifacts_dir
        .join("run-finish")
        .to_string_lossy()
        .into_owned();
    let stamp_path = artifacts_dir
        .join("run-finish")
        .join(format!("{run_key}.toml"))
        .to_string_lossy()
        .into_owned();

    let shell_cmd = build_shell_cmd(
        agent_cmd,
        window_name,
        &status_dir,
        &status_path_str,
        &finish_dir,
        &stamp_path,
    );

    let args: Vec<&str> = if switch {
        vec!["new-window", "-n", window_name, &shell_cmd]
    } else {
        vec!["new-window", "-d", "-n", window_name, &shell_cmd]
    };
    let output = Command::new("tmux")
        .args(&args)
        .output()
        .context("failed to create tmux window")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tmux new-window failed: {}", stderr.trim());
    }

    Ok(())
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
