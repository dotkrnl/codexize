//! Session/idea picker commands.
//!
//! The startup picker is owned outside the running app loop today; no
//! production key paths route through here yet. Reserved so the enum tree
//! mirrors the views tree and headless frontends can address the surface
//! when it ships.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PickerCommand {}
