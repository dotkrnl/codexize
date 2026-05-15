//! End-to-end integration: drive the real app-runtime headless entrypoint
//! through the headless runtime seam and verify the published `AppView` snapshots
//! reflect logic decisions plus data-dispatch outcomes. The seam stays free
//! of `ratatui`/`crossterm` types.
//!
//! Cargo treats `tests/integration/` as a single test binary because of
//! `main.rs`; per-feature integration tests can be added as siblings and
//! `mod`'d in below.
