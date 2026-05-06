#[path = "../finalization_complete.rs"]
mod complete;
mod reason;
#[path = "../finalization_reasons.rs"]
mod reasons;
#[path = "../finalization_recovery.rs"]
mod recovery;
#[path = "../finalization_retry_policy.rs"]
mod retry_policy;

pub(crate) use reason::Reason;

// Keep heavy orchestration logic outside `app/finalization/` while preserving
// the same module paths for call sites and tests.
include!("../finalization_core.rs");
