use crate::state::{Phase, RunState};

use super::state::{PipelineSection, SectionStatus};

pub(super) fn phase_done_summary(state: &RunState, phase: &str, label: &str) -> String {
    match state.phase_models.get(phase) {
        Some(pm) => format!("{label} · {} ({})", pm.model, pm.vendor),
        None => label.to_string(),
    }
}

pub(super) fn build_sections(state: &RunState, window_launched: bool) -> Vec<PipelineSection> {
    let phase = state.current_phase;
    vec![
        match phase {
            Phase::IdeaInput => PipelineSection::waiting_user(
                "Idea",
                "waiting for idea",
                Vec::<String>::new(),
                Vec::<String>::new(),
                "describe what you want to build",
            ),
            _ => PipelineSection::done(
                "Idea",
                state.idea_text.as_deref().unwrap_or("idea captured"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput => PipelineSection::pending("Brainstorm", "waiting for idea"),
            Phase::BrainstormRunning => {
                if let Some(err) = &state.agent_error {
                    PipelineSection::action(
                        "Brainstorm",
                        "failed — press Enter to retry",
                        vec![
                            format!("error: {err}"),
                            format!("model: {}", state.selected_model.as_deref().unwrap_or("unknown")),
                        ],
                    )
                } else if window_launched {
                    PipelineSection::running(
                        "Brainstorm",
                        "agent running in [Brainstorm] window",
                        vec![
                            format!("model: {}", state.selected_model.as_deref().unwrap_or("unknown")),
                            "waiting for spec.md artifact".to_string(),
                        ],
                    )
                } else {
                    PipelineSection::action(
                        "Brainstorm",
                        "press Enter to run",
                        vec![
                            format!("model: {}", state.selected_model.as_deref().unwrap_or("unknown")),
                        ],
                    )
                }
            }
            _ => PipelineSection::done(
                "Brainstorm",
                phase_done_summary(state, "brainstorm", "spec written"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput | Phase::BrainstormRunning => {
                PipelineSection::pending("Spec Review", "blocked on brainstorm")
            }
            Phase::SpecReviewRunning if window_launched => PipelineSection::running(
                "Spec Review",
                "agent running in [Spec Review] window",
                vec!["waiting for spec-review.md artifact".to_string()],
            ),
            Phase::SpecReviewRunning => {
                if let Some(err) = &state.agent_error {
                    let n_done = state.spec_reviewers.len();
                    let mut events = Vec::new();
                    for (i, r) in state.spec_reviewers.iter().enumerate() {
                        events.push(format!("  ✓ round {}  {} ({})", i + 1, r.model, r.vendor));
                    }
                    if n_done > 0 {
                        events.push(String::new());
                    }
                    events.push(format!("  ✗ round {} failed: {err}", n_done + 1));
                    events.push(String::new());
                    events.push(if n_done > 0 {
                        format!("[Enter] retry  ·  [n] proceed with {n_done} review{}",
                            if n_done == 1 { "" } else { "s" })
                    } else {
                        "[Enter] retry  ·  [n] skip review, proceed to planning".to_string()
                    });
                    PipelineSection::action("Spec Review", "failed", events)
                } else {
                    PipelineSection::action(
                        "Spec Review",
                        "press Enter to run",
                        Vec::<String>::new(),
                    )
                }
            }
            Phase::SpecReviewPaused => {
                let n = state.spec_reviewers.len();
                let mut events = Vec::new();
                for (i, r) in state.spec_reviewers.iter().enumerate() {
                    events.push(format!("  ✓ round {}  {} ({})", i + 1, r.model, r.vendor));
                }
                events.push(String::new());
                events.push(format!("[Enter] add another review · [n] proceed to planning"));
                PipelineSection::action(
                    "Spec Review",
                    format!("{n} review{} done", if n == 1 { "" } else { "s" }),
                    events,
                )
            }
            _ => PipelineSection::done(
                "Spec Review",
                phase_done_summary(state, "spec-review", "review complete"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput | Phase::BrainstormRunning
            | Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                PipelineSection::pending("Planning", "blocked on spec review")
            }
            Phase::PlanningRunning if window_launched => PipelineSection::running(
                "Planning",
                "agent running in [Planning] window",
                vec!["waiting for plan.md artifact".to_string()],
            ),
            Phase::PlanningRunning => {
                if let Some(err) = &state.agent_error {
                    PipelineSection::action(
                        "Planning",
                        "failed — press Enter to retry",
                        vec![format!("error: {err}")],
                    )
                } else {
                    let n = state.spec_reviewers.len();
                    PipelineSection::action(
                        "Planning",
                        "press Enter to run",
                        vec![format!("inputs: spec + {n} review{}", if n == 1 { "" } else { "s" })],
                    )
                }
            }
            _ => PipelineSection::done(
                "Planning",
                phase_done_summary(state, "planning", "plan drafted"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput | Phase::BrainstormRunning
            | Phase::SpecReviewRunning | Phase::SpecReviewPaused
            | Phase::PlanningRunning => {
                PipelineSection::pending("Sharding", "blocked on planning")
            }
            Phase::ShardingRunning if window_launched => PipelineSection::running(
                "Sharding",
                "agent running in [Sharding] window",
                vec!["waiting for tasks.toml artifact".to_string()],
            ),
            Phase::ShardingRunning => {
                if let Some(err) = &state.agent_error {
                    PipelineSection::action(
                        "Sharding",
                        "failed — press Enter to retry",
                        vec![format!("error: {err}")],
                    )
                } else {
                    PipelineSection::action(
                        "Sharding",
                        "press Enter to run",
                        vec!["splits plan into ~200k-token tasks → tasks.toml".to_string()],
                    )
                }
            }
            _ => PipelineSection::done(
                "Sharding",
                phase_done_summary(state, "sharding", "tasks.toml written"),
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
        },
        match phase {
            Phase::IdeaInput
            | Phase::BrainstormRunning
            | Phase::SpecReviewRunning
            | Phase::SpecReviewPaused
            | Phase::PlanningRunning
            | Phase::ShardingRunning => {
                PipelineSection::pending("Builder Loop", "blocked on sharding")
            }
            Phase::Done => PipelineSection::done(
                "Builder Loop",
                "complete",
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
            Phase::BlockedNeedsUser => PipelineSection::action(
                "Builder Loop",
                "blocked — needs user",
                builder_queue_lines(state),
            ),
            Phase::ImplementationRound(r) | Phase::ReviewRound(r) => {
                let (role, window) = match phase {
                    Phase::ImplementationRound(_) => ("coder", format!("[Coder r{r}]")),
                    _ => ("reviewer", format!("[Review r{r}]")),
                };
                let mut events = builder_queue_lines(state);
                events.push(String::new());
                events.push(format!("current round: {r}  ({role})"));
                if window_launched {
                    PipelineSection::running(
                        "Builder Loop",
                        format!("round {r} · {role} running in {window}"),
                        events,
                    )
                } else if let Some(err) = &state.agent_error {
                    events.insert(0, format!("error: {err}"));
                    PipelineSection::action(
                        "Builder Loop",
                        format!("round {r} · {role} failed — Enter to retry"),
                        events,
                    )
                } else {
                    let verdict_hint = state.builder.last_verdict.as_deref()
                        .map(|v| format!(" (last verdict: {v})"))
                        .unwrap_or_default();
                    PipelineSection::action(
                        "Builder Loop",
                        format!("round {r} · Enter to start {role}{verdict_hint}"),
                        events,
                    )
                }
            }
        },
    ]
}

fn builder_queue_lines(state: &RunState) -> Vec<String> {
    let b = &state.builder;
    let mut lines = Vec::new();
    for id in &b.done {
        lines.push(format!("  ✓ task {id}"));
    }
    if let Some(id) = b.current_task {
        lines.push(format!("  → task {id}  (current)"));
    }
    for id in &b.pending {
        lines.push(format!("  ⋯ task {id}"));
    }
    if lines.is_empty() {
        lines.push("  (no tasks loaded yet)".to_string());
    }
    lines
}

pub(super) fn current_section_index(sections: &[PipelineSection]) -> usize {
    sections
        .iter()
        .position(|s| s.status == SectionStatus::WaitingUser || s.status == SectionStatus::Running)
        .or_else(|| {
            sections
                .iter()
                .position(|s| s.status == SectionStatus::Done)
                .map(|i| i.min(sections.len().saturating_sub(1)))
        })
        .unwrap_or(0)
}
