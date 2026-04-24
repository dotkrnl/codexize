use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
};
use crate::state;

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

    writeln!(log_file, "--- Agent Run Started: phase={phase}, role={role} ---")?;
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
