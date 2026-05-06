#[rustfmt::skip]
#[allow(clippy::possible_missing_else)]
mod complete;
mod reason;
#[rustfmt::skip]
#[allow(clippy::possible_missing_else)]
mod reasons;
#[rustfmt::skip]
#[allow(clippy::possible_missing_else)]
mod recovery;
#[rustfmt::skip]
#[allow(clippy::possible_missing_else)]
mod retry_policy;
pub(crate) use reason::Reason;
#[rustfmt::skip]
include!("core.rs");
