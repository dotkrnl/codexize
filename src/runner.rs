use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};
use crate::state;

pub fn run(run_id: String, phase: String, role: String, command: Vec<String>) -> Result<()> {
    if command.is_empty() {
        bail!("no command provided to agent-run");
    }

    let dir = state::run_dir(&run_id);
    fs::create_dir_all(&dir)?;

    let log_path = dir.join(format!("{role}.log"));
    let mut log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    writeln!(log_file, "--- Agent Run Started: phase={}, role={} ---", phase, role)?;
    writeln!(log_file, "Command: {:?}", command)?;

    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn agent command: {:?}", command))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let mut log_file_clone = log_file.try_clone()?;
    let stdout_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                println!("{}", line);
                let _ = writeln!(log_file_clone, "[OUT] {}", line);
            }
        }
    });

    let mut log_file_clone = log_file.try_clone()?;
    let stderr_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                eprintln!("{}", line);
                let _ = writeln!(log_file_clone, "[ERR] {}", line);
            }
        }
    });

    let status = child.wait()?;
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    writeln!(log_file, "--- Agent Run Finished: status={} ---", status)?;

    if status.success() {
        Ok(())
    } else {
        bail!("agent command failed with status: {}", status)
    }
}
