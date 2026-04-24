// artifacts.rs — artifact path helpers (currently minimal; expanded as needed)
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipToImplProposal {
    pub proposed: bool,
    pub rationale: String,
}

impl SkipToImplProposal {
    pub fn new(proposed: bool, rationale: String) -> Self {
        Self { proposed, rationale }
    }
}

