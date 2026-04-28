use anyhow::{Context, Result, bail};
#[cfg(unix)]
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    thread,
    time::{Duration, Instant},
};

pub struct WarmupSpec<'a> {
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub script: &'a str,
    pub env: &'a [(&'a str, &'a str)],
    pub settle_timeout: Duration,
}

pub fn run(spec: WarmupSpec<'_>) -> Result<()> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .with_context(|| format!("failed to allocate PTY for {} warm-up", spec.program))?;

    let mut command = CommandBuilder::new(spec.program);
    for arg in spec.args {
        command.arg(arg);
    }
    for (key, value) in spec.env {
        command.env(key, value);
    }

    let master = pair.master;
    let slave = pair.slave;

    let mut child = slave
        .spawn_command(command)
        .with_context(|| format!("failed to start {} warm-up", spec.program))?;
    drop(slave);

    let mut stdin = master
        .take_writer()
        .context("failed to open warm-up PTY writer")?;
    stdin
        .write_all(spec.script.as_bytes())
        .with_context(|| format!("failed to write {} warm-up script", spec.program))?;
    drop(stdin);

    // Drain PTY output in a background thread so the child never blocks on a
    // full PTY buffer. Without this, CLIs that produce large startup output
    // (e.g. Kimi's ~2.5 KB banner vs the ~4 KB kernel buffer) stall before
    // they can finish initialising (token refresh, etc.).
    let mut reader = master
        .try_clone_reader()
        .context("failed to open warm-up PTY reader")?;
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while reader.read(&mut buf).unwrap_or(0) > 0 {}
    });

    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("failed to poll warm-up process")? {
            if status.success() {
                return Ok(());
            }

            if started.elapsed() < Duration::from_millis(300) {
                bail!(
                    "{} warm-up exited immediately with {}",
                    spec.program,
                    status
                );
            }

            return Ok(());
        }

        if started.elapsed() >= spec.settle_timeout {
            #[cfg(unix)]
            if let Some(pgid) = master.process_group_leader() {
                let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
            }
            let _ = child.kill();
            return Ok(());
        }

        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_ok_when_program_exits_zero() {
        // `true` exits 0 immediately; the success branch returns Ok(()).
        let result = run(WarmupSpec {
            program: "true",
            args: &[],
            script: "",
            env: &[],
            settle_timeout: Duration::from_secs(2),
        });
        assert!(result.is_ok(), "warmup with `true` should succeed: {:?}", result.err());
    }

    #[test]
    fn run_returns_err_when_program_exits_immediately_nonzero() {
        // `false` exits 1 within the 300ms grace window, hitting the
        // "warm-up exited immediately" bail.
        let result = run(WarmupSpec {
            program: "false",
            args: &[],
            script: "",
            env: &[],
            settle_timeout: Duration::from_secs(2),
        });
        let err = result.expect_err("warmup with `false` must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("warm-up exited immediately"),
            "expected immediate-exit context: {msg}"
        );
    }

    #[test]
    fn run_returns_ok_after_settle_timeout_kills_child() {
        // `sleep 5` outruns the 200ms settle timeout; the timeout branch
        // SIGKILLs the child and returns Ok(()).
        let result = run(WarmupSpec {
            program: "sleep",
            args: &["5"],
            script: "",
            env: &[],
            settle_timeout: Duration::from_millis(200),
        });
        assert!(
            result.is_ok(),
            "warmup must return Ok after killing on settle timeout: {:?}",
            result.err()
        );
    }

    #[test]
    fn run_returns_err_when_spawn_fails() {
        let result = run(WarmupSpec {
            program: "/this/program/definitely/does/not/exist-xyz",
            args: &[],
            script: "",
            env: &[],
            settle_timeout: Duration::from_secs(1),
        });
        let err = result.expect_err("missing binary should fail spawn");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("failed to start") || msg.contains("warm-up"),
            "expected spawn-failure context: {msg}"
        );
    }
}
