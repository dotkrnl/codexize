//! Truly global operator-intent commands. These apply across every session.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlobalCommand {
    /// Quit the application. Subject to the runtime's pending-agent
    /// gating (which may open a confirmation modal instead of exiting).
    Quit,
    /// Stop whichever agent is currently running, if any. Bound to Ctrl-C
    /// in the TUI; surfaced as a discrete command so non-terminal frontends
    /// can request the same effect.
    StopRunningAgent,
}
