use std::{
    future::Future,
    sync::{Condvar, Mutex},
    time::Duration,
};
/// Run an async IO primitive from a synchronous caller.
///
/// Runtime-owned async paths should prefer the async function directly; this
/// bridge is for synchronous UI and test callers above tokio-native IO.
///
/// When called from a multi-thread Tokio worker this uses `block_in_place`.
/// Current-thread runtimes cannot be re-entered from a sync bridge, so they
/// fail with an explicit message instead of Tokio's lower-level panic.
pub(crate) fn block_on_io<F>(future: F) -> F::Output
where
    F: Future,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ) {
            return tokio::task::block_in_place(|| handle.block_on(future));
        }
        panic!("block_on_io cannot be called from a current-thread Tokio runtime");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build temporary tokio runtime for sync IO bridge")
        .block_on(future)
}
/// Block the current thread for `duration` without re-entering Tokio.
///
/// This helper is used from synchronous callers and from `spawn_blocking`
/// closures. A plain condvar wait avoids depending on `block_in_place` from
/// non-runtime blocking-pool threads.
pub(crate) fn sleep_blocking(duration: Duration) {
    let mutex = Mutex::new(());
    let condvar = Condvar::new();
    let guard = mutex.lock().expect("sleep mutex poisoned");
    let _ = condvar
        .wait_timeout(guard, duration)
        .expect("sleep condvar poisoned");
}
