use dashmap::DashMap;
use parking_lot::Mutex as PlMutex;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

use super::{CancelSignal, RunHandle, RunId, Supervisor};
use crate::data::runner::transport::{AcpCancelReason, AcpInput};

pub(in crate::data::runner) fn test_supervisor() -> &'static Supervisor {
    static SUPERVISOR: std::sync::OnceLock<Supervisor> = std::sync::OnceLock::new();
    SUPERVISOR.get_or_init(|| {
        Supervisor::new(std::sync::Arc::new(
            crate::data::config::Config::baked_defaults(),
        ))
    })
}

fn test_run_ids() -> &'static DashMap<String, RunId> {
    static RUN_IDS: std::sync::OnceLock<DashMap<String, RunId>> = std::sync::OnceLock::new();
    RUN_IDS.get_or_init(DashMap::new)
}

fn test_input_observers() -> &'static DashMap<String, PlMutex<mpsc::UnboundedReceiver<AcpInput>>> {
    static OBSERVERS: std::sync::OnceLock<
        DashMap<String, PlMutex<mpsc::UnboundedReceiver<AcpInput>>>,
    > = std::sync::OnceLock::new();
    OBSERVERS.get_or_init(DashMap::new)
}

/// Test-only side observer for the cancel signal of a fixture run. The
/// observer holds the same `Arc<CancelSignal>` as the registered RunHandle,
/// so signals issued through `request_run_label_exit` /
/// `cancel_run_labels_matching` remain visible to drain helpers even after
/// the supervisor has removed the run from its registry.
fn test_cancel_observers() -> &'static DashMap<String, Arc<CancelSignal>> {
    static OBSERVERS: std::sync::OnceLock<DashMap<String, Arc<CancelSignal>>> =
        std::sync::OnceLock::new();
    OBSERVERS.get_or_init(DashMap::new)
}

pub fn request_run_label_interactive_input_for_test(window_name: &str) {
    register_test_run_label(window_name, true);
}

pub fn request_run_label_active_for_test(window_name: &str) {
    register_test_run_label(window_name, false);
}

fn register_test_run_label(window_name: &str, waiting: bool) {
    let supervisor = test_supervisor();
    let run_id = assign_test_run_id(window_name);
    let cancel = supervisor.child_cancel_signal();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<AcpInput>();
    let (_waiting_tx, waiting_rx) = watch::channel(waiting);
    let (_finished_tx, finished_rx) = watch::channel(false);

    test_cancel_observers().insert(window_name.to_string(), Arc::clone(&cancel));
    test_input_observers().insert(window_name.to_string(), PlMutex::new(input_rx));

    supervisor.inner.runs.insert(
        run_id,
        RunHandle {
            #[cfg(test)]
            window_name: window_name.to_string(),
            cancel,
            input_tx,
            waiting_for_input: waiting_rx,
            finished: finished_rx,
            join: None,
        },
    );
}

fn test_run_id_for_label(window_name: &str) -> Option<RunId> {
    test_run_ids().get(window_name).map(|entry| *entry.value())
}

pub(in crate::data::runner) fn assign_test_run_id(window_name: &str) -> RunId {
    test_run_ids()
        .get(window_name)
        .map(|entry| *entry.value())
        .unwrap_or_else(|| {
            let next = test_run_ids().len() as RunId + 1;
            test_run_ids().insert(window_name.to_string(), next);
            next
        })
}

pub fn register_test_run_id(window_name: &str, run_id: RunId) {
    let previous = test_run_ids().insert(window_name.to_string(), run_id);
    if let Some(previous) = previous
        && previous != run_id
        && let Some((_, run)) = test_supervisor().inner.runs.remove(&previous)
    {
        // Re-keying onto an already-occupied slot would silently drop the
        // existing handle. The process-global fixture supervisor still gets
        // re-used across tests today, so collisions surface when a leaked
        // fixture from an earlier test still has a handle at this id; warn
        // so the leak is visible. Once each `App` owns its own supervisor,
        // this can graduate to a `debug_assert!`.
        if test_supervisor().inner.runs.contains_key(&run_id) {
            eprintln!(
                "register_test_run_id: replacing existing RunHandle at run_id {run_id} \
                 (window_name={window_name}); shared process-global fixture coupling - \
                 chunked harness isolation pending"
            );
        }
        test_supervisor().inner.runs.insert(run_id, run);
    }
}

pub fn run_label_is_active(window_name: &str) -> bool {
    let Some(run_id) = test_run_id_for_label(window_name) else {
        return false;
    };
    test_supervisor().run_is_active(run_id)
}

pub fn run_label_is_waiting_for_input(window_name: &str) -> bool {
    let Some(run_id) = test_run_id_for_label(window_name) else {
        return false;
    };
    test_supervisor().run_is_waiting_for_input(run_id)
}

pub fn cancel_run_labels_matching(base: &str) {
    test_supervisor().cancel_runs_matching_label(base);
}

pub fn request_run_label_exit(window_name: &str) {
    if let Some(run_id) = test_run_id_for_label(window_name) {
        test_supervisor().request_run_exit(run_id);
    }
}

pub fn send_run_label_input(window_name: &str, text: String) -> bool {
    test_run_id_for_label(window_name)
        .is_some_and(|run_id| test_supervisor().send_run_input(run_id, text))
}

pub fn interrupt_run_label_input(window_name: &str, text: String) -> bool {
    test_run_id_for_label(window_name)
        .is_some_and(|run_id| test_supervisor().interrupt_run_input(run_id, text))
}

pub fn force_interrupt_run_label(window_name: &str, text: String) -> bool {
    test_run_id_for_label(window_name)
        .is_some_and(|run_id| test_supervisor().force_interrupt_run(run_id, text))
}

pub fn terminate_run_label(window_name: &str) -> bool {
    test_run_id_for_label(window_name).is_some_and(|run_id| test_supervisor().terminate_run(run_id))
}

pub fn shutdown_all_runs() {
    test_supervisor().shutdown_all_runs();
}

pub(super) fn clear_fixture_state() {
    test_run_ids().clear();
    test_input_observers().clear();
    test_cancel_observers().clear();
}

/// Test-only: drain queued `AcpInput` messages on the per-window observer
/// registered by `request_run_label_*_for_test`. Returns each queued input as
/// a stable `(kind, text)` pair so callers do not need access to the private
/// `AcpInput` enum.
pub fn drain_test_input_receiver_for(window_name: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(entry) = test_input_observers().get(window_name) {
        let mut rx = entry.value().lock();
        while let Ok(input) = rx.try_recv() {
            match input {
                AcpInput::Prompt(text) => out.push(("prompt", text)),
                AcpInput::Interrupt(text) => out.push(("interrupt", text)),
            }
        }
    }
    out
}

/// Test-only: observe whether the per-window cancel signal has been raised.
/// Returns the recorded reason once and clears it from the observer slot so
/// repeat calls without a fresh signal return empty.
pub fn drain_test_cancel_receiver_for(window_name: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if let Some(entry) = test_cancel_observers().get(window_name) {
        let signal = entry.value();
        if signal.token.is_cancelled()
            && let Some(reason) = signal.reason.lock().take()
        {
            out.push(match reason {
                AcpCancelReason::Terminate => "terminate",
                AcpCancelReason::Complete => "complete",
            });
        }
    }
    out
}

#[cfg(test)]
#[path = "tests_support_tests.rs"]
mod tests;
