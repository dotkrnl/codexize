// artifacts.rs — artifact path helpers (currently minimal; expanded as needed)
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipToImplProposal {
    pub proposed: bool,
    pub rationale: String,
}

impl SkipToImplProposal {
    pub fn new(proposed: bool, rationale: String) -> Self {
        Self { proposed, rationale }
    }

    pub async fn read_from_path(path: &std::path::Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = tokio::fs::read_to_string(path).await?;
        let proposal: Self = serde_json::from_str(&content)?;
        proposal.validate()?;
        Ok(Some(proposal))
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.proposed && self.rationale.is_empty() {
            anyhow::bail!("rationale cannot be empty if proposed is true");
        }
        if self.rationale.len() > 500 {
            anyhow::bail!("rationale cannot exceed 500 characters");
        }
        Ok(())
    }
}

// Minimal Spec struct for synthetic artifact generation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Spec {
    pub spec_refs: Vec<String>,
    // Add other fields as they become relevant, or keep it minimal
}

// Minimal Implementation struct for synthetic artifact generation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Implementation {
    pub current_task_id: u32,
    pub current_round: u32,
    pub remaining_tasks: Vec<u32>,
    // Add other fields as they become relevant
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Spec,
    SpecReview,
    Plan,
    PlanReview,
    CodeReview,
    Implementation,
    Tasks, // Added for tasks.toml
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
        }
    }
}

