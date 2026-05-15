//! Chat surface commands.
//!
//! Chat is a read-only transcript today; scroll behavior lives on tree/split
//! commands. The enum is reserved for future granular chat operations
//! (jump-to-message, copy, …).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatCommand {}
