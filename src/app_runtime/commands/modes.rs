//! Mode-flag toggles.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModesCommand {
    ToggleCheap,
    SetCheap(bool),
    ToggleYolo,
    SetYolo(bool),
    ToggleNoninteractiveTexts,
    ToggleThinkingTexts,
    SkipToImpl,
}
