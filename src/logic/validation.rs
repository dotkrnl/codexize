use crate::tasks::{Ref, Task};
use anyhow::bail;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    GoalMet,
    GoalGap,
    NeedsHuman,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DreamRecommendation {
    Suggest,
    Skip,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gap {
    pub description: String,
    pub checked: Vec<String>,
}
/// Minimal task schema emitted by the validator in a `goal_gap` verdict.
/// Intentionally omits `id`, `spec_refs`, and `plan_refs` — the orchestrator
/// assigns those during ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorGapTask {
    pub title: String,
    pub description: String,
    pub test: String,
    pub estimated_tokens: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationVerdict {
    pub status: ValidationStatus,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub gaps: Vec<Gap>,
    #[serde(default)]
    pub new_tasks: Vec<ValidatorGapTask>,
    #[serde(default)]
    pub dream_recommendation: Option<DreamRecommendation>,
    #[serde(default)]
    pub dream_reason: Option<String>,
}
pub fn parse_verdict_toml(text: &str) -> anyhow::Result<ValidationVerdict> {
    let parsed: ValidationVerdict = toml::from_str(text)?;
    if parsed.summary.trim().is_empty() {
        bail!("summary is empty");
    }
    match parsed.status {
        ValidationStatus::GoalMet => {
            if !parsed.gaps.is_empty() {
                bail!("status=goal_met must not include gaps");
            }
            if !parsed.new_tasks.is_empty() {
                bail!("status=goal_met must not include new_tasks");
            }
            let Some(recommendation) = &parsed.dream_recommendation else {
                bail!("status=goal_met requires dream_recommendation");
            };
            match recommendation {
                DreamRecommendation::Suggest => {
                    if parsed
                        .dream_reason
                        .as_deref()
                        .is_none_or(|reason| reason.trim().is_empty())
                    {
                        bail!(
                            "status=goal_met with dream_recommendation=suggest requires dream_reason"
                        );
                    }
                }
                DreamRecommendation::Skip => {}
            }
        }
        ValidationStatus::GoalGap => {
            if parsed.gaps.is_empty() {
                bail!("status=goal_gap requires at least one gap");
            }
            if parsed.new_tasks.is_empty() {
                bail!("status=goal_gap requires at least one new_task");
            }
            if parsed.dream_recommendation.is_some() || parsed.dream_reason.is_some() {
                bail!("status=goal_gap must not include dream fields");
            }
        }
        ValidationStatus::NeedsHuman => {
            if parsed.gaps.is_empty() {
                bail!("status=needs_human requires at least one gap");
            }
            if !parsed.new_tasks.is_empty() {
                bail!("status=needs_human must not include new_tasks");
            }
            if parsed.dream_recommendation.is_some() || parsed.dream_reason.is_some() {
                bail!("status=needs_human must not include dream fields");
            }
        }
    }
    for (i, gap) in parsed.gaps.iter().enumerate() {
        if gap.description.trim().is_empty() {
            bail!("gaps[{i}]: empty description");
        }
        if gap.checked.is_empty() {
            bail!("gaps[{i}]: checked must not be empty");
        }
        for (j, checked) in gap.checked.iter().enumerate() {
            if checked.trim().is_empty() {
                bail!("gaps[{i}].checked[{j}]: empty citation");
            }
        }
    }
    for (i, task) in parsed.new_tasks.iter().enumerate() {
        if task.title.trim().is_empty() {
            bail!("new_tasks[{i}]: empty title");
        }
        if task.description.trim().is_empty() {
            bail!("new_tasks[{i}]: empty description");
        }
        if task.test.trim().is_empty() {
            bail!("new_tasks[{i}]: empty test");
        }
        if task.estimated_tokens == 0 {
            bail!("new_tasks[{i}]: estimated_tokens must be > 0");
        }
    }
    Ok(parsed)
}
/// Convert validator gap tasks into full [`Task`] entries.
///
/// `max_task_id` is the current highest task ID in the session; new IDs start
/// at `max_task_id + 1`. Each task receives conservative references to
/// `artifacts/spec.md` and the validation verdict artifact so downstream
/// coders have something to anchor on even when the validator did not supply
/// explicit refs.
pub fn normalize_gap_tasks(
    gap_tasks: Vec<ValidatorGapTask>,
    max_task_id: u32,
    verdict_artifact_path: &str,
) -> Vec<Task> {
    let mut next_id = max_task_id + 1;
    gap_tasks
        .into_iter()
        .map(|gap_task| {
            let id = next_id;
            next_id += 1;
            Task {
                id,
                title: gap_task.title,
                description: gap_task.description,
                test: gap_task.test,
                estimated_tokens: gap_task.estimated_tokens,
                tough: false,
                spec_refs: vec![
                    Ref {
                        path: "artifacts/spec.md".to_string(),
                        lines: "1-".to_string(),
                    },
                    Ref {
                        path: verdict_artifact_path.to_string(),
                        lines: "1-".to_string(),
                    },
                ],
                plan_refs: vec![],
            }
        })
        .collect()
}
