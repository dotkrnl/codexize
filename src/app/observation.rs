// observation.rs
use super::*;
use crate::state::{Message, MessageKind, MessageSender};
use anyhow::Result;

use notify::Watcher;
use std::sync::mpsc;
impl App {
    pub(super) fn setup_watcher(&mut self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let watcher_result = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        );
        match watcher_result {
            Ok(mut watcher) => {
                let Some(path) = self.live_summary_path.clone() else {
                    self.live_summary_watcher = None;
                    self.live_summary_change_rx = None;
                    return Ok(());
                };
                if let Err(e) = watcher.watch(&path, notify::RecursiveMode::NonRecursive) {
                    self.surface_boundary_error(
                        format!("watcher setup failed: {e}, falling back to poll"),
                        false,
                    );
                    self.live_summary_watcher = None;
                    self.live_summary_change_rx = None;
                    return Ok(());
                }
                self.live_summary_watcher = Some(watcher);
                self.live_summary_change_rx = Some(rx);
                Ok(())
            }
            Err(e) => {
                self.surface_boundary_error(
                    format!("watcher init failed: {e}, falling back to poll"),
                    false,
                );
                self.live_summary_watcher = None;
                self.live_summary_change_rx = None;
                Ok(())
            }
        }
    }

    pub(super) fn process_live_summary_changes(&mut self) {
        if let Some(ref rx) = self.live_summary_change_rx {
            let mut saw_change = false;
            while rx.try_recv().is_ok() {
                saw_change = true;
            }
            if saw_change {
                self.read_live_summary_pipeline();
            }
        } else {
            self.poll_live_summary_fallback();
        }
    }

    pub(super) fn poll_live_summary_fallback(&mut self) {
        if !self.window_launched {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary_cached_text.clear();
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            self.live_summary_cached_text.clear();
            return;
        }
        let should_read = match self.live_summary_cached_mtime {
            None => true,
            Some(cached) => mtime > cached,
        };
        if should_read {
            self.read_live_summary_pipeline();
        }
    }

    pub(super) fn read_live_summary_pipeline(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some(run) = self.running_run() else {
            return;
        };
        if !self.window_exists(&run.window_name) {
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        if let Some(cached_mtime) = self.live_summary_cached_mtime
            && mtime <= cached_mtime
        {
            return;
        }
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let sanitized = render::sanitize_live_summary(&content);
        if sanitized.is_empty() {
            return;
        }
        if sanitized == self.live_summary_cached_text {
            return;
        }
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: run.model.clone(),
                vendor: run.vendor.clone(),
            },
            text: sanitized.clone(),
        };
        if let Err(err) = self.state.append_message(&msg) {
            let _ = self.state.log_event(format!(
                "failed to append brief message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(msg);
        }
        self.live_summary_cached_text = sanitized;
        self.live_summary_cached_mtime = Some(mtime);
        // Live-summary deltas are an automatic refocus event, but they are
        // not a re-enable boundary: if the operator has manually navigated
        // away the arrow stays put.
        self.maybe_refocus_to_progress();
    }

    /// Final read + cleanup of the live-summary file when a run finishes.
    /// Emits any last summary as a Brief message, then deletes the file so
    /// the next run starts with a clean slate.
    pub(super) fn drain_live_summary(&mut self, run: &crate::state::RunRecord) {
        let path = self.live_summary_path_for(run);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let sanitized = render::sanitize_live_summary(&content);
            if !sanitized.is_empty() && sanitized != self.live_summary_cached_text {
                let msg = Message {
                    ts: chrono::Utc::now(),
                    run_id: run.id,
                    kind: MessageKind::Brief,
                    sender: MessageSender::Agent {
                        model: run.model.clone(),
                        vendor: run.vendor.clone(),
                    },
                    text: sanitized,
                };
                if let Err(err) = self.state.append_message(&msg) {
                    let _ = self.state.log_event(format!(
                        "failed to append final brief message for run {}: {err}",
                        run.id
                    ));
                } else {
                    self.messages.push(msg);
                }
            }
        }
        let _ = std::fs::remove_file(&path);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.live_summary_watcher = None;
        self.live_summary_change_rx = None;
    }
}
