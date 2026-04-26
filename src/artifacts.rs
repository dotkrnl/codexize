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
    /// Returns `Ok(None)` if the file is absent. Returns `Err` if the file is
    /// present but malformed or invalid; callers log a warning and fall through
    /// to the normal flow on error.
    pub fn read_from_path(path: &std::path::Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let proposal: Self = toml::from_str(&content).map_err(|err| {
            anyhow::anyhow!("unsupported old JSON/JSONL artifact or malformed TOML: {err}")
        })?;
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
pub struct ReviewScopeArtifact {
    pub base_sha: String,
    #[serde(default)]
    pub dirty_after: bool,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_artifact_round_trips_as_toml() {
        let artifact = SessionArtifact {
            id: "20260424-233547".to_string(),
            created_at: "2026-04-24T23:35:47Z".to_string(),
            operator: "dotkrnl".to_string(),
            status: "running".to_string(),
        };

        let encoded = toml::to_string(&artifact).expect("encode session");
        assert!(encoded.contains("id = \"20260424-233547\""));
        let decoded: SessionArtifact = toml::from_str(&encoded).expect("decode session");
        assert_eq!(decoded.id, artifact.id);
    }

    #[test]
    fn round_artifacts_round_trip_as_toml() {
        let review_scope = ReviewScopeArtifact {
            base_sha: "abc123".to_string(),
            dirty_after: true,
        };
        let encoded = toml::to_string(&review_scope).expect("encode review scope");
        let decoded: ReviewScopeArtifact = toml::from_str(&encoded).expect("decode review scope");
        assert_eq!(decoded.base_sha, "abc123");
        assert!(decoded.dirty_after);

        let review = ReviewArtifact {
            status: ReviewStatus::Revise,
            summary: "needs split".to_string(),
            feedback: vec!["split the work".to_string()],
            new_tasks: vec![TaskArtifact {
                id: 2,
                title: "Split work".to_string(),
                description: "Do less at once.".to_string(),
                test: "cargo test".to_string(),
                estimated_tokens: 1000,
                spec_refs: vec![],
                plan_refs: vec![],
            }],
        };
        let encoded = toml::to_string(&review).expect("encode review");
        let decoded: ReviewArtifact = toml::from_str(&encoded).expect("decode review");
        assert_eq!(decoded.status, ReviewStatus::Revise);
        assert_eq!(decoded.new_tasks.len(), 1);
    }

    #[test]
    fn json_artifacts_are_rejected_by_toml_parsers() {
        let json = r#"{ "status": "approved", "summary": "old json" }"#;
        let error = toml::from_str::<ReviewArtifact>(json).expect_err("json must fail");
        assert!(error.to_string().contains("TOML"));
    }

    #[test]
    fn review_scope_defaults_dirty_after_for_legacy_files() {
        let decoded: ReviewScopeArtifact =
            toml::from_str("base_sha = \"abc123\"\n").expect("decode legacy review scope");
        assert_eq!(decoded.base_sha, "abc123");
        assert!(!decoded.dirty_after);
    }
}
