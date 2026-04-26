use crate::adapters::{AgentAdapter, AgentRun, shell_escape};
use crate::state;
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn run(
    session_id: String,
    phase: String,
    role: String,
    artifacts: Vec<String>,
    command: Vec<String>,
) -> Result<()> {
    if command.is_empty() {
        bail!("no command provided to agent-run");
    }

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

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn: {:?}", command))?;

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
            eprintln!("{line}");
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
/// finish-stamp logic (added in a later phase) is guaranteed to run.
pub fn launch_interactive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    switch: bool,
    status_path: &Path,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.interactive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, switch, status_path)
}

/// Launch an agent non-interactively inside a new tmux window.
/// All agent child-process launches must route through the runner so that
/// finish-stamp logic (added in a later phase) is guaranteed to run.
pub fn launch_noninteractive(
    window_name: &str,
    run: &AgentRun,
    adapter: &dyn AgentAdapter,
    status_path: &Path,
) -> Result<()> {
    let prompt_path = run.prompt_path.to_string_lossy();
    let cmd = adapter.noninteractive_command(&run.model, &prompt_path);
    launch_in_window(window_name, &cmd, adapter, false, status_path)
}

fn launch_in_window(
    window_name: &str,
    agent_cmd: &str,
    adapter: &dyn AgentAdapter,
    switch: bool,
    status_path: &Path,
) -> Result<()> {
    if !adapter.detect() {
        bail!("agent CLI not found — install it first");
    }

    let status_dir = status_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_string_lossy()
        .into_owned();
    let status_path = status_path.to_string_lossy().into_owned();
    let shell_cmd = format!(
        r#"mkdir -p {status_dir}; trap 'status=$?; printf "%s" "$status" > {status_path}' EXIT HUP INT TERM; printf '\033[1;36m>>> starting %s...\033[0m\n\n' {name}; {agent_cmd}"#,
        status_dir = shell_escape(&status_dir),
        status_path = shell_escape(&status_path),
        name = shell_escape(window_name),
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
        fs::write(&md, "# Not TOML at all {{{}}}").unwrap();
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
}
