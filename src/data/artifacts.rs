// artifacts.rs — artifact path helpers and typed wrappers.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SkipProposalStatus {
    #[default]
    SkipToImpl,
    NothingToDo,
}
pub type SkipToImplKind = SkipProposalStatus;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipToImplProposal {
    pub proposed: bool,
    #[serde(default)]
    pub status: SkipProposalStatus,
    pub rationale: String,
}
impl SkipToImplProposal {
    pub fn new(proposed: bool, rationale: String) -> Self {
        Self {
            proposed,
            status: SkipProposalStatus::SkipToImpl,
            rationale,
        }
    }
    /// Read the skip-to-implementation proposal artifact from `path`.
    ///
    /// Returns `Ok((None, []))` if the file is absent. Returns `Err` for
    /// genuine parse failures or missing required fields. Soft issues (e.g.
    /// over-length rationale) are returned as warnings in the second tuple
    /// element — callers route these through their own logger.
    pub fn read_from_path(path: &std::path::Path) -> anyhow::Result<(Option<Self>, Vec<String>)> {
        if !path.exists() {
            return Ok((None, vec![]));
        }
        let content = std::fs::read_to_string(path)?;
        let mut proposal: Self =
            toml::from_str(&content).map_err(|err| anyhow::anyhow!("malformed TOML: {err}"))?;
        let warnings = proposal.validate_and_fixup()?;
        Ok((Some(proposal), warnings))
    }
    fn validate_and_fixup(&mut self) -> anyhow::Result<Vec<String>> {
        if self.proposed && self.rationale.trim().is_empty() {
            anyhow::bail!("rationale cannot be empty if proposed is true");
        }
        let mut warnings = Vec::new();
        let char_count = self.rationale.chars().count();
        if char_count > 500 {
            let truncated: String = self.rationale.chars().take(500).collect();
            self.rationale = truncated;
            warnings.push(format!(
                "rationale truncated to 500 chars (was {char_count})"
            ));
        }
        Ok(warnings)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummaryArtifact {
    pub title: String,
}
impl SessionSummaryArtifact {
    /// Read the session summary artifact from `path`. Returns `Ok(None)` if
    /// the file is absent. Returns `Err` on malformed TOML or invalid
    /// content; callers log and fall through.
    pub fn read_from_path(path: &std::path::Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let artifact: Self = toml::from_str(&content)
            .map_err(|err| anyhow::anyhow!("malformed session_summary.toml: {err}"))?;
        artifact.validate()?;
        Ok(Some(artifact))
    }
    fn validate(&self) -> anyhow::Result<()> {
        let trimmed = self.title.trim();
        if trimmed.is_empty() {
            anyhow::bail!("title cannot be empty");
        }
        if trimmed.chars().count() > 80 {
            anyhow::bail!("title cannot exceed 80 characters");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionArtifact {
    pub id: String,
    pub created_at: String,
    pub operator: String,
    pub status: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MessagesArtifact {
    #[serde(default)]
    pub messages: Vec<MessageArtifact>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageArtifact {
    pub role: String,
    pub content: String,
    pub timestamp: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct EventsArtifact {
    #[serde(default)]
    pub events: Vec<EventArtifact>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventArtifact {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub payload: toml::map::Map<String, toml::Value>,
    pub timestamp: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub path: String,
    pub lines: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskArtifact {
    pub id: u32,
    pub title: String,
    pub description: String,
    pub test: String,
    pub estimated_tokens: u32,
    #[serde(default)]
    pub spec_refs: Vec<ArtifactRef>,
    #[serde(default)]
    pub plan_refs: Vec<ArtifactRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TasksArtifact {
    pub tasks: Vec<TaskArtifact>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewScopeArtifact {
    pub base_sha: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Approved,
    Revise,
    HumanBlocked,
    AgentPivot,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewArtifact {
    pub status: ReviewStatus,
    pub summary: String,
    #[serde(default)]
    pub feedback: Vec<String>,
    #[serde(default)]
    pub new_tasks: Vec<TaskArtifact>,
}
pub type SpecReviewArtifact = ReviewArtifact;
pub type PlanReviewArtifact = ReviewArtifact;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryArtifact {
    pub status: ReviewStatus,
    pub trigger: ReviewStatus,
    pub interactive: bool,
    pub summary: String,
    #[serde(default)]
    pub feedback: Vec<String>,
    #[serde(default)]
    pub changed_files: Vec<String>,
}
/// Minimal Spec representation used by synthetic-artifact generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Spec {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub spec_refs: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Spec,
    SpecReview,
    Plan,
    PlanReview,
    CodeReview,
    Tasks,
    SkipToImpl,
    SessionSummary,
    RepoStateUpdate,
}
impl ArtifactKind {
    pub fn filename(&self) -> &'static str {
        match self {
            ArtifactKind::Spec => "spec.md",
            ArtifactKind::SpecReview => "spec_review.toml",
            ArtifactKind::Plan => "plan.md",
            ArtifactKind::PlanReview => "plan_review.toml",
            ArtifactKind::CodeReview => "review.toml",
            ArtifactKind::Tasks => "tasks.toml",
            ArtifactKind::SkipToImpl => "skip_proposal.toml",
            ArtifactKind::SessionSummary => "session_summary.toml",
            ArtifactKind::RepoStateUpdate => "repo-state-update.toml",
        }
    }
}
#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
