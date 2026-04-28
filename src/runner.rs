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

    finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    cat > "$tmp_stamp" << STAMPEOF
finished_at = "{finished_at_placeholder}"
exit_code = {exit_code_placeholder}
head_before = "{head_before_placeholder}"
head_after = "{head_after_placeholder}"
head_state = "{head_state_placeholder}"
signal_received = "{signal_received_placeholder}"
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
trap 'on_signal HUP' HUP
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

    // SAFETY: `Command::stdout(Stdio::piped())` at :268 guarantees
    // `child.stdout` is `Some` per std's documented invariant; same for
    // `child.stderr` via :269. Taking each pipe once here is therefore
    // unconditionally safe.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

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
    let status = Command::new("tmux")
        .args(&args)
        .status()
        .context("failed to create tmux window")?;

    if !status.success() {
        bail!("tmux new-window failed");
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
mod tests {
    use super::*;

    #[test]
    fn test_validate_toml_artifacts_all_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let p1 = dir.path().join("a.toml");
        let p2 = dir.path().join("b.toml");
        fs::write(&p1, "status = \"ok\"").unwrap();
        fs::write(&p2, "count = 42").unwrap();
        assert!(validate_toml_artifacts(&[p1.as_path(), p2.as_path()]).is_ok());
    }

    #[test]
    fn test_validate_toml_artifacts_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("nope.toml");
        let result = validate_toml_artifacts(&[missing.as_path()]);
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("missing"));
    }

    #[test]
    fn test_validate_toml_artifacts_malformed() {
        let dir = tempfile::TempDir::new().unwrap();
        let bad = dir.path().join("bad.toml");
        fs::write(&bad, "not { valid } toml [").unwrap();
        let result = validate_toml_artifacts(&[bad.as_path()]);
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("malformed TOML"));
    }

    #[test]
    fn test_validate_toml_artifacts_non_toml_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        let md = dir.path().join("spec.md");
        fs::write(&md, "# Not TOML at all {{{{}}}}}").unwrap();
        assert!(validate_toml_artifacts(&[md.as_path()]).is_ok());
    }

    #[test]
    fn test_validate_toml_artifacts_mix_missing_and_malformed() {
        let dir = tempfile::TempDir::new().unwrap();
        let missing = dir.path().join("gone.toml");
        let bad = dir.path().join("bad.toml");
        fs::write(&bad, "[[[[broken").unwrap();
        let result = validate_toml_artifacts(&[missing.as_path(), bad.as_path()]);
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("missing"));
        assert!(msg.contains("malformed"));
    }

    #[test]
    fn test_extract_model_from_flag() {
        let cmd = vec![
            "claude".to_string(),
            "--model".to_string(),
            "opus-4".to_string(),
        ];
        assert_eq!(extract_model(&cmd), "opus-4");
    }

    #[test]
    fn test_extract_model_from_equals() {
        let cmd = vec!["claude".to_string(), "--model=sonnet-4".to_string()];
        assert_eq!(extract_model(&cmd), "sonnet-4");
    }

    #[test]
    fn test_extract_model_fallback_to_binary() {
        let cmd = vec!["/usr/bin/claude".to_string(), "--fast".to_string()];
        assert_eq!(extract_model(&cmd), "claude");
    }

    #[test]
    fn finish_stamp_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stamp.toml");
        let stamp = FinishStamp {
            finished_at: "2026-04-26T10:00:00Z".to_string(),
            exit_code: 0,
            head_before: "abc123".to_string(),
            head_after: "def456".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
        };
        write_finish_stamp(&path, &stamp).unwrap();
        assert!(path.exists());
        let read = read_finish_stamp(&path).unwrap();
        assert_eq!(read, stamp);
    }

    #[test]
    fn finish_stamp_atomic_write_no_partial_file_on_failure() {
        let dir = tempfile::TempDir::new().unwrap();
        // Use a read-only directory to force the write to fail.
        let ro_dir = dir.path().join("readonly");
        fs::create_dir(&ro_dir).unwrap();
        let mut perms = fs::metadata(&ro_dir).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&ro_dir, perms.clone()).unwrap();

        let path = ro_dir.join("stamp.toml");
        let stamp = FinishStamp {
            finished_at: "2026-04-26T10:00:00Z".to_string(),
            exit_code: 0,
            head_before: "abc123".to_string(),
            head_after: "def456".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
        };
        let result = write_finish_stamp(&path, &stamp);
        assert!(result.is_err());

        // No partial file should remain.
        let entries: Vec<_> = fs::read_dir(&ro_dir).unwrap().flatten().collect();
        assert!(
            entries.is_empty(),
            "expected no partial files, got {:?}",
            entries
        );

        // Restore permissions so the temp dir can be cleaned up.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o700);
            let _ = fs::set_permissions(&ro_dir, perms);
        }
    }

    #[test]
    fn finish_stamp_parses_required_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stamp.toml");
        fs::write(
            &path,
            r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 1
head_before = "000000"
head_after = "111111"
head_state = "unstable"
"#,
        )
        .unwrap();
        let stamp = read_finish_stamp(&path).unwrap();
        assert_eq!(stamp.finished_at, "2026-04-26T10:00:00Z");
        assert_eq!(stamp.exit_code, 1);
        assert_eq!(stamp.head_before, "000000");
        assert_eq!(stamp.head_after, "111111");
        assert_eq!(stamp.head_state, "unstable");
    }

    #[test]
    fn finish_stamp_missing_field_is_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stamp.toml");
        fs::write(
            &path,
            r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 0
head_before = "abc"
head_after = "def"
"#,
        )
        .unwrap();
        assert!(read_finish_stamp(&path).is_err());
    }

    #[test]
    fn finish_stamp_malformed_toml_is_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stamp.toml");
        fs::write(&path, "not { valid toml").unwrap();
        assert!(read_finish_stamp(&path).is_err());
    }

    #[test]
    fn finish_stamp_ignores_unknown_fields() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stamp.toml");
        fs::write(
            &path,
            r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 0
head_before = "abc"
head_after = "def"
head_state = "stable"
extra_field = "ignored"
"#,
        )
        .unwrap();
        let stamp = read_finish_stamp(&path).unwrap();
        assert_eq!(stamp.head_state, "stable");
    }

    #[test]
    fn shell_cmd_contains_stabilization_loop() {
        let cmd = build_shell_cmd(
            "claude -p prompt.md",
            "[Test]",
            "/tmp/status",
            "/tmp/status/run.txt",
            "/tmp/artifacts/run-finish",
            "/tmp/artifacts/run-finish/test-key.toml",
        );
        assert!(cmd.contains("git rev-parse HEAD"), "should capture HEAD");
        assert!(cmd.contains("head_state"), "should write head_state");
        assert!(
            cmd.contains(".git/index.lock"),
            "should wait for index.lock"
        );
        assert!(cmd.contains("stable"), "should mention stable state");
        assert!(cmd.contains("unstable"), "should mention unstable state");
        assert!(cmd.contains("mv "), "should atomically rename stamp");
        assert!(
            cmd.contains("CODEXIZE_STAMP_STABILIZE_MS"),
            "should read env budget"
        );
    }

    #[test]
    fn shell_cmd_escapes_paths() {
        let cmd = build_shell_cmd(
            "echo hello",
            "[Test]",
            "/tmp/weird'path",
            "/tmp/weird'path/status.txt",
            "/tmp/weird'path/finish",
            "/tmp/weird'path/finish/key.toml",
        );
        // Escaped paths should contain the single-quote handling.
        assert!(
            cmd.contains("weird'\\''path"),
            "path should be shell-escaped"
        );
    }

    #[test]
    fn shell_cmd_produces_stable_stamp_in_git_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        let status_dir = dir.path().join("status");
        let status_path = status_dir.join("run.txt");
        let finish_dir = dir.path().join("run-finish");
        let stamp_path = finish_dir.join("test.toml");

        // Initialize a git repo with one commit.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git init");
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "test", "--no-gpg-sign"])
            .current_dir(dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git commit");

        let cmd = build_shell_cmd(
            "true",
            "[Test]",
            &status_dir.to_string_lossy(),
            &status_path.to_string_lossy(),
            &finish_dir.to_string_lossy(),
            &stamp_path.to_string_lossy(),
        );

        let output = std::process::Command::new("bash")
            .args(["-c", &cmd])
            .current_dir(dir.path())
            .output()
            .expect("bash");
        assert!(
            output.status.success(),
            "bash failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(stamp_path.exists(), "stamp should exist");
        let stamp = read_finish_stamp(&stamp_path).unwrap();
        assert_eq!(stamp.exit_code, 0);
        assert_eq!(stamp.head_state, "stable");
        assert!(!stamp.head_before.is_empty());
        assert_eq!(stamp.head_before, stamp.head_after);

        // Status file should also contain the exit code.
        let status_text = fs::read_to_string(&status_path).unwrap();
        assert_eq!(status_text.trim(), "0");
    }

    #[test]
    fn shell_cmd_produces_unstable_stamp_when_head_keeps_changing() {
        let dir = tempfile::TempDir::new().unwrap();
        let status_dir = dir.path().join("status");
        let status_path = status_dir.join("run.txt");
        let finish_dir = dir.path().join("run-finish");
        let stamp_path = finish_dir.join("test.toml");

        // Create a fake git that returns a different SHA each call.
        let bin_dir = dir.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let counter_file = dir.path().join("git_counter");
        let git_script = format!(
            r#"#!/bin/bash
if [ "$1" = "rev-parse" ] && [ "$2" = "HEAD" ]; then
    if [ -f "{counter}" ]; then
        c=$(cat "{counter}")
    else
        c=0
    fi
    c=$((c + 1))
    echo "$c" > "{counter}"
    printf '%040d\n' "$c"
    exit 0
fi
exit 1
"#,
            counter = counter_file.to_string_lossy()
        );
        fs::write(bin_dir.join("git"), git_script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(bin_dir.join("git")).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(bin_dir.join("git"), perms).unwrap();
        }

        // Create a fake .git directory so index.lock check works.
        fs::create_dir(dir.path().join(".git")).unwrap();

        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::from(&bin_dir);
        new_path.push(":");
        new_path.push(&path_env);

        let cmd = build_shell_cmd(
            "true",
            "[Test]",
            &status_dir.to_string_lossy(),
            &status_path.to_string_lossy(),
            &finish_dir.to_string_lossy(),
            &stamp_path.to_string_lossy(),
        );

        let output = std::process::Command::new("bash")
            .args(["-c", &cmd])
            .current_dir(dir.path())
            .env("PATH", &new_path)
            .output()
            .expect("bash");
        assert!(
            output.status.success(),
            "bash failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(stamp_path.exists(), "stamp should exist");
        let stamp = read_finish_stamp(&stamp_path).unwrap();
        assert_eq!(stamp.exit_code, 0);
        assert_eq!(stamp.head_state, "unstable");
        assert!(!stamp.head_after.is_empty());
    }

    #[test]
    fn shell_cmd_writes_stamp_when_path_contains_spaces() {
        let dir = tempfile::Builder::new()
            .prefix("runner with spaces ")
            .tempdir()
            .unwrap();
        let status_dir = dir.path().join("status dir");
        let status_path = status_dir.join("run status.txt");
        let finish_dir = dir.path().join("run finish");
        let stamp_path = finish_dir.join("test key.toml");

        let cmd = build_shell_cmd(
            "true",
            "[Test]",
            &status_dir.to_string_lossy(),
            &status_path.to_string_lossy(),
            &finish_dir.to_string_lossy(),
            &stamp_path.to_string_lossy(),
        );

        let output = std::process::Command::new("bash")
            .args(["-c", &cmd])
            .current_dir(dir.path())
            .output()
            .expect("bash");
        assert!(
            output.status.success(),
            "bash failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(stamp_path.exists(), "stamp should exist at escaped path");
        assert!(read_finish_stamp(&stamp_path).is_ok());
    }

    #[test]
    fn shell_cmd_writes_stamp_when_interrupted() {
        let dir = tempfile::TempDir::new().unwrap();
        let status_dir = dir.path().join("status");
        let status_path = status_dir.join("run.txt");
        let finish_dir = dir.path().join("run-finish");
        let stamp_path = finish_dir.join("interrupted.toml");

        let cmd = build_shell_cmd(
            "sleep 30",
            "[Test]",
            &status_dir.to_string_lossy(),
            &status_path.to_string_lossy(),
            &finish_dir.to_string_lossy(),
            &stamp_path.to_string_lossy(),
        );

        let mut child = std::process::Command::new("bash")
            .args(["-c", &cmd])
            .current_dir(dir.path())
            .spawn()
            .expect("spawn bash");

        std::thread::sleep(Duration::from_millis(200));
        let pid = child.id().to_string();
        let kill_status = std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("kill");
        assert!(kill_status.success(), "failed to signal wrapper process");

        let mut attempts = 0;
        loop {
            if let Some(_status) = child.try_wait().expect("try_wait") {
                break;
            }
            attempts += 1;
            if attempts > 50 {
                let _ = child.kill();
                panic!("wrapper did not exit promptly after TERM");
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        assert!(
            stamp_path.exists(),
            "interrupted run should still produce a finish stamp"
        );
        let stamp = read_finish_stamp(&stamp_path).expect("parse interrupted stamp");
        assert_ne!(
            stamp.exit_code, 0,
            "interrupted run should not report success"
        );
        assert_eq!(
            stamp.signal_received, "TERM",
            "interrupted run should record trapped signal"
        );
    }

    #[test]
    fn finish_stamp_parses_old_stamp_without_signal_received() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stamp.toml");
        fs::write(
            &path,
            r#"finished_at = "2026-04-26T10:00:00Z"
exit_code = 1
head_before = "000000"
head_after = "111111"
head_state = "unstable"
"#,
        )
        .unwrap();
        let stamp = read_finish_stamp(&path).unwrap();
        assert_eq!(stamp.signal_received, "");
    }

    #[test]
    fn run_returns_err_on_empty_command() {
        let result = run(
            "test-empty-cmd-session".to_string(),
            "audit".to_string(),
            "auditor".to_string(),
            vec![],
            vec![],
        );
        assert!(result.is_err(), "empty command must error, not panic");
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("no command provided"),
            "unexpected error message: {msg}"
        );
    }

    fn with_temp_codexize_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("CODEXIZE_ROOT");
        // SAFETY: serialized via test_fs_lock; restored unconditionally.
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
                None => std::env::remove_var("CODEXIZE_ROOT"),
            }
        }
        outcome.unwrap()
    }

    #[test]
    fn run_succeeds_on_zero_exit_with_no_required_artifacts() {
        with_temp_codexize_root(|| {
            // `true` is a POSIX no-op that exits 0 with no output.
            let result = run(
                "test-runner-true".to_string(),
                "audit".to_string(),
                "auditor".to_string(),
                vec![],
                vec!["true".to_string()],
            );
            assert!(result.is_ok(), "true should succeed: {:?}", result.err());
            // The runner writes a per-role log alongside the session dir.
            let log_path = state::session_dir("test-runner-true").join("auditor.log");
            assert!(log_path.exists(), "expected log at {log_path:?}");
        });
    }

    #[test]
    fn run_returns_err_when_required_artifact_missing() {
        with_temp_codexize_root(|| {
            let dir = tempfile::TempDir::new().unwrap();
            let missing = dir.path().join("never-created.toml");
            let result = run(
                "test-runner-missing".to_string(),
                "audit".to_string(),
                "auditor".to_string(),
                vec![missing.to_string_lossy().into_owned()],
                vec!["true".to_string()],
            );
            let err = result.expect_err("missing artifact must error");
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("required artifacts are missing"),
                "missing-artifact error context: {msg}"
            );
        });
    }

    #[test]
    fn run_returns_err_when_command_exits_nonzero() {
        with_temp_codexize_root(|| {
            // `false` exits with status 1.
            let result = run(
                "test-runner-false".to_string(),
                "audit".to_string(),
                "auditor".to_string(),
                vec![],
                vec!["false".to_string()],
            );
            let err = result.expect_err("nonzero exit must error");
            let msg = format!("{:#}", err);
            assert!(
                msg.contains("agent command failed"),
                "exit-status error context: {msg}"
            );
        });
    }

    #[test]
    fn run_child_with_timeout_returns_status_when_child_exits_quickly() {
        let launch = ChildLaunch::new("true")
            .stdin_null()
            .stdout_null()
            .stderr_null();
        let outcome = run_child_with_timeout(&launch, Duration::from_secs(2)).unwrap();
        let status = outcome.expect("child should exit before timeout");
        assert!(status.success(), "expected zero exit");
    }

    #[test]
    fn run_child_with_timeout_returns_none_when_child_outruns_deadline() {
        let launch = ChildLaunch::new("sleep")
            .args(["10"])
            .stdin_null()
            .stdout_null()
            .stderr_null();
        let outcome = run_child_with_timeout(&launch, Duration::from_millis(150)).unwrap();
        assert!(
            outcome.is_none(),
            "expected timeout-killed result, got {outcome:?}"
        );
    }

    #[test]
    fn run_child_with_timeout_propagates_spawn_failure() {
        let launch = ChildLaunch::new("/this/program/definitely/does/not/exist-xyz");
        let err = run_child_with_timeout(&launch, Duration::from_millis(100))
            .expect_err("spawning a missing binary must error before any timeout work");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("failed to spawn"),
            "spawn error context: {msg}"
        );
    }

    struct DetectFailsAdapter;
    impl AgentAdapter for DetectFailsAdapter {
        fn detect(&self) -> bool {
            false
        }
        fn interactive_command(
            &self,
            _model: &str,
            _prompt_path: &str,
            _effort: crate::adapters::EffortLevel,
        ) -> String {
            String::new()
        }
        fn noninteractive_command(
            &self,
            _model: &str,
            _prompt_path: &str,
            _effort: crate::adapters::EffortLevel,
        ) -> String {
            String::new()
        }
    }

    fn launch_test_run() -> AgentRun {
        AgentRun {
            model: "model-x".to_string(),
            prompt_path: PathBuf::from("/tmp/prompt.txt"),
            effort: crate::adapters::EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
        }
    }

    #[test]
    fn launch_interactive_bails_when_adapter_detect_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let run = launch_test_run();
        let result = launch_interactive(
            "[Coder]",
            &run,
            &DetectFailsAdapter,
            false,
            &dir.path().join("status.toml"),
            "run-1",
            dir.path(),
        );
        let err = result.expect_err("missing CLI must bail before tmux");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("agent CLI not found"),
            "expected adapter-detect bail: {msg}"
        );
    }

    #[test]
    fn launch_noninteractive_bails_when_adapter_detect_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let run = launch_test_run();
        let result = launch_noninteractive(
            "[Coder]",
            &run,
            &DetectFailsAdapter,
            &dir.path().join("status.toml"),
            "run-2",
            dir.path(),
        );
        let err = result.expect_err("missing CLI must bail before tmux");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("agent CLI not found"),
            "expected adapter-detect bail: {msg}"
        );
    }
}
