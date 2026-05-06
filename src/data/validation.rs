//! Filesystem entry point for the final-validation verdict artifact.
//!
//! Pure parsing, schema rules, and gap-task normalization live in
//! [`crate::logic::validation`]. This module is the thin IO shell that reads
//! a verdict file off disk and delegates checking to the pure parser.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub use crate::logic::validation::{
    Gap, ValidationStatus, ValidationVerdict, ValidatorGapTask, normalize_gap_tasks,
    parse_verdict_toml,
};

/// Read the final-validation verdict at `path` and return the parsed,
/// schema-checked value. The on-disk read is the only IO this function
/// performs; the rule checks come from [`crate::logic::validation`].
pub fn validate(path: &Path) -> Result<ValidationVerdict> {
    let text =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse_verdict_toml(&text)
        .with_context(|| format!("malformed validation TOML in {}", path.display()))
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
