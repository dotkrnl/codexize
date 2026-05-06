//! Per-frame render cache.
//!
//! `App::draw` rebuilds a transcript that mixes thousands of wrapped chat
//! lines with viewport math; without memoization the same lines get walked 5–7
//! times per frame (once each for `header_y_offsets`, `running_depth_0_header`,
//! `pipeline_render_lines`, the live-summary spinner probe, the unread badge,
//! and the actual body draw). On large sessions that's the dominant TUI cost.
//!
//! This module owns a thread-local cache scoped to the lifetime of a single
//! [`FrameGuard`]. The renderer wraps `App::draw` in a guard so the cache is
//! populated lazily on the first call and invalidated on guard drop. Outside a
//! guard, callers (event handlers, tests) hit the bypass branch and recompute
//! against live state — so stale frames cannot leak into mutators.
//!
//! `PipelineLine` / `PipelineLineKind` live here rather than `pipeline.rs` so
//! the layout helpers under `app/lifecycle/` can also see them without
//! depending on `view.rs`'s private submodules.

use ratatui::text::Line;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PipelineLineKind {
    Other,
    RunningLeafTail { run_id: u64 },
    RunningContainerPlaceholder { run_id: u64 },
}

#[derive(Clone)]
pub(crate) struct PipelineLine {
    pub(crate) line: Line<'static>,
    pub(crate) kind: PipelineLineKind,
}

#[derive(Default)]
struct FrameCache {
    pipeline_lines: Option<Vec<PipelineLine>>,
    header_y_offsets: Option<(Vec<usize>, usize)>,
    running_depth_0_header: Option<Option<(usize, usize)>>,
    /// Per-row body lines, keyed by `visible_rows` index. Both
    /// `header_y_offsets` (which needs only the body length) and
    /// `compute_pipeline_render_lines` (which extends the line list) read
    /// from the same cache, so each row's messages are wrapped at most once
    /// per frame.
    row_bodies: HashMap<usize, Rc<Vec<PipelineLine>>>,
}

thread_local! {
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
    static CACHE: RefCell<FrameCache> = RefCell::new(FrameCache::default());
}

/// RAII guard that opens a render frame: callers (here, `App::draw`) hold
/// one for the duration of a frame so cached helpers reuse a single
/// computation and the cache is cleared on drop, even on panic.
pub(crate) struct FrameGuard {
    _private: (),
}

impl FrameGuard {
    pub(crate) fn enter() -> Self {
        IN_FRAME.with(|f| f.set(true));
        CACHE.with(|c| *c.borrow_mut() = FrameCache::default());
        FrameGuard { _private: () }
    }
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        CACHE.with(|c| *c.borrow_mut() = FrameCache::default());
        IN_FRAME.with(|f| f.set(false));
    }
}

fn in_frame() -> bool {
    IN_FRAME.with(|f| f.get())
}

/// Return the cached full-transcript pipeline line list (computed with an
/// empty `suppressed_container_runs` set), populating it via `populate` on
/// first miss. Outside a frame guard the helper bypasses the cache.
pub(crate) fn cached_pipeline_lines<F>(populate: F) -> Vec<PipelineLine>
where
    F: FnOnce() -> Vec<PipelineLine>,
{
    if !in_frame() {
        return populate();
    }
    let already = CACHE.with(|c| c.borrow().pipeline_lines.is_some());
    if !already {
        let lines = populate();
        CACHE.with(|c| c.borrow_mut().pipeline_lines = Some(lines));
    }
    CACHE.with(|c| {
        c.borrow()
            .pipeline_lines
            .as_ref()
            .expect("just populated")
            .clone()
    })
}

/// Variant of [`cached_pipeline_lines`] that returns a filtered clone with
/// `RunningContainerPlaceholder` entries dropped for any `run_id` in
/// `suppressed_container_runs`. Equivalent to recomputing
/// `pipeline_render_lines` with a non-empty suppressed set: suppression only
/// affects whether the trailing tail line is appended for a container row,
/// so dropping that one tagged line per run reproduces the original output.
pub(crate) fn cached_pipeline_lines_filtered<F>(
    suppressed_container_runs: &BTreeSet<u64>,
    populate: F,
) -> Vec<PipelineLine>
where
    F: FnOnce() -> Vec<PipelineLine>,
{
    let base = cached_pipeline_lines(populate);
    if suppressed_container_runs.is_empty() {
        return base;
    }
    base.into_iter()
        .filter(|line| match line.kind {
            PipelineLineKind::RunningContainerPlaceholder { run_id } => {
                !suppressed_container_runs.contains(&run_id)
            }
            _ => true,
        })
        .collect()
}

/// Return the cached `(ys, total)` header offset table, populating via
/// `populate` on first miss. Outside a frame guard the helper bypasses the
/// cache.
pub(crate) fn cached_header_y_offsets<F>(populate: F) -> (Vec<usize>, usize)
where
    F: FnOnce() -> (Vec<usize>, usize),
{
    if !in_frame() {
        return populate();
    }
    let already = CACHE.with(|c| c.borrow().header_y_offsets.is_some());
    if !already {
        let result = populate();
        CACHE.with(|c| c.borrow_mut().header_y_offsets = Some(result));
    }
    CACHE.with(|c| {
        c.borrow()
            .header_y_offsets
            .as_ref()
            .expect("just populated")
            .clone()
    })
}

/// Return the cached `running_depth_0_header` lookup, populating via
/// `populate` on first miss. Outside a frame guard the helper bypasses the
/// cache.
pub(crate) fn cached_running_depth_0_header<F>(populate: F) -> Option<(usize, usize)>
where
    F: FnOnce() -> Option<(usize, usize)>,
{
    if !in_frame() {
        return populate();
    }
    let already = CACHE.with(|c| c.borrow().running_depth_0_header.is_some());
    if !already {
        let result = populate();
        CACHE.with(|c| c.borrow_mut().running_depth_0_header = Some(result));
    }
    CACHE.with(|c| {
        *c.borrow()
            .running_depth_0_header
            .as_ref()
            .expect("just populated")
    })
}

/// Return the cached body lines for `index` (computed with an empty
/// `suppressed_container_runs`), populating via `populate` on first miss.
/// Returns an `Rc` so callers can share the cached body without cloning the
/// inner vec; copying out individual `Line`s remains the caller's choice.
pub(crate) fn cached_row_body<F>(index: usize, populate: F) -> Rc<Vec<PipelineLine>>
where
    F: FnOnce() -> Vec<PipelineLine>,
{
    if !in_frame() {
        return Rc::new(populate());
    }
    if let Some(body) = CACHE.with(|c| c.borrow().row_bodies.get(&index).cloned()) {
        return body;
    }
    let body = Rc::new(populate());
    CACHE.with(|c| {
        c.borrow_mut().row_bodies.insert(index, Rc::clone(&body));
    });
    body
}

#[cfg(test)]
#[path = "frame_cache_tests.rs"]
mod tests;
