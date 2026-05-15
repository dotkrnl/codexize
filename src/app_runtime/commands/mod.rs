//! Per-surface typed operator-intent commands.
//!
//! Today's `AppCommand` remains the unified enum used across the seam;
//! later tasks split it into `Global` / `Shell` / `Session(SessionId,
//! SessionCommand)` groupings with one file per surface here.
