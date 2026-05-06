//! TUI-safe diagnostics setup.
//!
//! Routine tracing must never use stdout/stderr once the terminal UI is active.
//! The subscriber here writes JSON lines into the session directory so debug
//! logs stay adjacent to events, messages, and per-run ACP traces.

use anyhow::{Context, Result};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt, prelude::*};

const DIAGNOSTICS_LOG: &str = "diagnostics.jsonl";

struct SharedFileWriter {
    file: Arc<Mutex<File>>,
}

impl Write for SharedFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file
            .lock()
            .map_err(|_| io::Error::other("diagnostics log lock poisoned"))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .lock()
            .map_err(|_| io::Error::other("diagnostics log lock poisoned"))?
            .flush()
    }
}

pub fn session_diagnostics_path(session_id: &str) -> PathBuf {
    crate::state::session_dir(session_id).join(DIAGNOSTICS_LOG)
}

pub fn init_session_tracing(session_id: &str) -> Result<()> {
    let path = session_diagnostics_path(session_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create diagnostics directory {}",
                parent.display()
            )
        })?;
    }
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open diagnostics log {}", path.display()))?;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let file = Arc::new(Mutex::new(file));
    let writer = (move || SharedFileWriter {
        file: Arc::clone(&file),
    })
    .with_max_level(tracing::Level::TRACE);
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(writer);

    tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init()
        .context("failed to initialize tracing subscriber")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(process_cwd)]
    fn diagnostics_path_is_session_local_jsonl() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let root = tempfile::TempDir::new().unwrap();
        let previous = std::env::var_os("CODEXIZE_ROOT");
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", root.path());
        }

        let path = session_diagnostics_path("session-1");

        assert_eq!(
            path,
            root.path()
                .join("sessions")
                .join("session-1")
                .join("diagnostics.jsonl")
        );

        unsafe {
            match previous {
                Some(value) => std::env::set_var("CODEXIZE_ROOT", value),
                None => std::env::remove_var("CODEXIZE_ROOT"),
            }
        }
    }
}
