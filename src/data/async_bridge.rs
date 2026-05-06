use std::{future::Future, time::Duration};

/// Run an async IO primitive from a sync compatibility surface.
///
/// New runtime-owned paths should prefer the async function directly. This
/// bridge exists for legacy sync call sites that still sit above the data
/// layer while the IO implementation underneath is tokio-native.
pub(crate) fn block_on_io<F>(future: F) -> F::Output
where
    F: Future,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return tokio::task::block_in_place(|| handle.block_on(future));
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build temporary tokio runtime for sync IO bridge")
        .block_on(future)
}

pub(crate) fn sleep_blocking(duration: Duration) {
    block_on_io(tokio::time::sleep(duration));
}
