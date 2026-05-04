//! Thin application runtime seam.
//!
//! The event pump still lives behind `app::App` today; this module gives later
//! moves a stable canonical path without changing public CLI behavior.

pub use crate::app::App;
pub use crate::app::AppStartupOrigin;
