// artifacts.rs — artifact path helpers and typed wrappers.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkipToImplKind {
    #[default]
    SkipToImpl,
    NothingToDo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipToImplProposal {
    pub proposed: bool,
    #[serde(default)]
    pub kind: SkipToImplKind,
    pub rationale: String,
}

impl SkipToImplProposal {
    pub fn new(proposed: bool, rationale: String) -> Self {
        Self { proposed, kind: SkipToImplKind::SkipToImpl, rationale }
    }

    /// Read the skip-to-implementation proposal artifact from `path`.
    ///
    /// Returns `Ok(None)` if the file is absent. Returns `Err` if the file is
    /// present but malformed or invalid; callers log a warning and fall through
    /// to the normal flow on error.
    pub fn read_from_path(path: &std::path::Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let proposal: Self = serde_json::from_str(&content)?;
        proposal.validate()?;
        Ok(Some(proposal))
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.proposed && self.rationale.trim().is_empty() {
            anyhow::bail!("rationale cannot be empty if proposed is true");
        }
        if self.rationale.len() > 500 {
            anyhow::bail!("rationale cannot exceed 500 characters");
        }
        Ok(())
    }
}

/// Minimal Spec representation used by synthetic-artifact generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Spec {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub spec_refs: Vec<String>,
}

/// Minimal implementation-pointer artifact written alongside synthetic plan/tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Implementation {
    pub current_task_id: u32,
    pub current_round: u32,
    pub remaining_tasks: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Spec,
    SpecReview,
    Plan,
    PlanReview,
    CodeReview,
    Implementation,
    Tasks,
    SkipToImpl,
}

impl ArtifactKind {
    pub fn filename(&self) -> &'static str {
        match self {
            ArtifactKind::Spec => "spec.md",
            ArtifactKind::SpecReview => "spec_review.md",
            ArtifactKind::Plan => "plan.md",
            ArtifactKind::PlanReview => "plan_review.md",
            ArtifactKind::CodeReview => "code_review.md",
            ArtifactKind::Implementation => "implementation.json",
            ArtifactKind::Tasks => "tasks.toml",
            ArtifactKind::SkipToImpl => "skip_to_impl.json",
        }
    }
}
