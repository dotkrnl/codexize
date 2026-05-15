use anyhow::{Context, Result};
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
/// Current-thread runtimes cannot be re-entered from a sync bridge, so this
/// returns an error instead of panicking.
pub(crate) fn block_on_io<F>(future: F) -> Result<F::Output>
where
    F: Future,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ) {
            return Ok(tokio::task::block_in_place(|| handle.block_on(future)));
        }
        anyhow::bail!("block_on_io cannot be called from a current-thread Tokio runtime");
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build temporary tokio runtime for sync IO bridge")?;
    Ok(rt.block_on(future))
}
/// Block the current thread for `duration` without re-entering Tokio.
///
/// This helper is used from synchronous callers and from `spawn_blocking`
/// closures. A plain condvar wait avoids depending on `block_in_place` from
/// non-runtime blocking-pool threads.
pub(crate) fn sleep_blocking(duration: Duration) {
    let mutex = Mutex::new(());
    let condvar = Condvar::new();
    let guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
    let _ = condvar.wait_timeout(guard, duration).unwrap_or_else(|e| e.into_inner());
}
