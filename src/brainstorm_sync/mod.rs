//! Brainstorm skill sync subsystem.
//!
//! Detects, plans, and (in later layers) installs vendor-specific copies of
//! the upstream Superpowers `skills/brainstorming` package. This module owns
//! pure planning and persistence: metadata persistence, target discovery,
//! eligibility narrowing, freshness gating, and batch plan construction.
//! Source acquisition, package rendering, installation, and preflight UI live
//! outside this module and consume the plan produced here.

pub mod discovery;
pub mod lock;
pub mod metadata;
pub mod upstream;

pub use metadata::{
    BrainstormMetadata, CachedSource, InstallMode, VendorRecord, default_metadata_dir,
    default_metadata_path, vendor_key,
};
