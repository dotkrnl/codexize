//! Sheet surface commands (reserved; sheet is read-only today).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SheetCommand {}
