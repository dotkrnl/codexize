use crate::app_runtime::commands::AppCommand;
use crate::app_runtime::frontend::{Frontend, FrontendConnector, SnapshotHandle};
use crate::app_runtime::root_view::{RootEvent, RootEventPayload};
use anyhow::{Context, Result};
use serde::Serialize;
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

/// Line-delimited JSON frontend driven over stdin/stdout.
///
/// Selected via `codexize headless`. On startup it emits exactly one
/// `Snapshot` JSON line, then loops multiplexing the runtime event stream
/// (one JSON line per [`RootEvent`]) with stdin command parsing (one JSON
/// line per [`AppCommand`]).
pub struct HeadlessFrontend;

impl HeadlessFrontend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HeadlessFrontend {
    fn default() -> Self {
        Self::new()
    }
}

impl Frontend for HeadlessFrontend {
    fn run(self, connector: FrontendConnector) -> Result<()> {
        let _ = ctrlc::set_handler({
            let shutdown = connector.shutdown.clone();
            move || shutdown.set()
        });

        emit_json_line(&RootEvent {
            seq: connector.snapshot.read().seq,
            payload: RootEventPayload::Snapshot(connector.snapshot.read()),
        })
        .context("headless: initial snapshot")?;

        let (stdin_tx, stdin_rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                match line {
                    Ok(l) => {
                        if stdin_tx.send(l).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("headless: stdin read error: {e}");
                        return;
                    }
                }
            }
        });

        let poll_interval = Duration::from_millis(100);
        loop {
            if connector.shutdown.is_set() {
                return Ok(());
            }

            match connector.events.recv_timeout(poll_interval) {
                Ok(event) => {
                    emit_json_line(&event)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Ok(());
                }
            }

            loop {
                match stdin_rx.try_recv() {
                    Ok(line) => {
                        if let Err(e) =
                            handle_stdin_line(&line, &connector.commands, &connector.snapshot)
                        {
                            eprintln!("headless: command error: {e:#}");
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        eprintln!("headless: stdin closed");
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn emit_json_line<T: Serialize>(value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, value)
        .map_err(|e| annotate_io_err("stdout write", e.into()))?;
    writeln!(out).map_err(|e| annotate_io_err("stdout write", e.into()))?;
    out.flush()
        .map_err(|e| annotate_io_err("stdout flush", e.into()))?;
    Ok(())
}

fn annotate_io_err(ctx: &'static str, e: anyhow::Error) -> anyhow::Error {
    if let Some(io_err) = e.downcast_ref::<io::Error>()
        && io_err.kind() == io::ErrorKind::BrokenPipe
    {
        eprintln!("headless: stdout closed (broken pipe)");
    }
    e.context(ctx)
}

fn handle_stdin_line(
    line: &str,
    commands: &mpsc::Sender<AppCommand>,
    snapshot: &SnapshotHandle,
) -> Result<()> {
    match serde_json::from_str::<AppCommand>(line) {
        Ok(cmd) => commands.send(cmd)?,
        Err(e) => {
            let seq = snapshot.read().seq;
            emit_json_line(&RootEvent {
                seq,
                payload: RootEventPayload::Error(format!("{e}")),
            })
            .context("headless: error event emit")?;
            eprintln!("headless: stdin parse error: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_runtime::commands::*;
    use crate::app_runtime::root_view::*;
    use crate::app_runtime::views::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn empty_root_view() -> RootView {
        RootView {
            seq: 0,
            shell: ShellView::default(),
            sessions: BTreeMap::new(),
            focus: Arc::<str>::from(""),
        }
    }

    #[test]
    fn round_trip_root_event_snapshot() {
        let view = empty_root_view();
        let event = RootEvent {
            seq: 0,
            payload: RootEventPayload::Snapshot(view),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""Snapshot""#));
        assert!(json.contains(r#""seq":0"#));
    }

    #[test]
    fn round_trip_granular_deltas() {
        let session_id: Arc<str> = Arc::<str>::from("test-123");
        let delta = SessionViewDelta::Palette(PaletteView::default());
        let event = RootEvent {
            seq: 42,
            payload: RootEventPayload::SessionChanged(session_id.clone(), delta),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""SessionChanged""#));
        assert!(json.contains(r#""test-123""#));
        assert!(json.contains(r#""Palette""#));
        assert_eq!(event.seq, 42);

        let shell_event = RootEvent {
            seq: 7,
            payload: RootEventPayload::ShellChanged(ShellViewDelta::Full(ShellView::default())),
        };
        let json = serde_json::to_string(&shell_event).unwrap();
        assert!(json.contains(r#""ShellChanged""#));
        assert!(json.contains(r#""Full""#));
    }

    #[test]
    fn deserialize_app_command() {
        let json = r#"{"Global":"Quit"}"#;
        let cmd: AppCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd, AppCommand::Global(GlobalCommand::Quit));

        let json = r#"{"Shell":"ToggleSidebar"}"#;
        let cmd: AppCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd, AppCommand::Shell(ShellCommand::ToggleSidebar));

        let sid: Arc<str> = Arc::<str>::from("abc");
        let json = r#"{"Session":["abc",{"Input":{"InsertText":"hello"}}]}"#;
        let cmd: AppCommand = serde_json::from_str(json).unwrap();
        assert_eq!(
            cmd,
            AppCommand::Session(
                sid,
                SessionCommand::Input(InputCommand::InsertText("hello".into()))
            )
        );
    }

    #[test]
    fn parse_error_produces_error_payload() {
        let bad = r#"{"not a valid command": 42}"#;
        let result: Result<AppCommand, _> = serde_json::from_str(bad);
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        let error_payload = RootEventPayload::Error(err_msg);
        let event = RootEvent {
            seq: 0,
            payload: error_payload,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""Error""#));
    }
}
