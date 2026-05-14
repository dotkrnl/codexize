//! End-to-end integration: drive the real app-runtime headless entrypoint
//! through the stubbed-UI seam and verify the published `AppView` snapshots
//! reflect logic decisions plus data-dispatch outcomes. This is the same
//! surface a future server/web binary will reuse — no `ratatui`/`crossterm`,
//! only the public seam.
//!
//! Cargo treats `tests/integration/` as a single test binary because of
//! `main.rs`; per-feature integration tests can be added as siblings and
//! `mod`'d in below.

mod config;
mod support;
