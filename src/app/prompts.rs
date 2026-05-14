// Public prompt-builder facade. The heavy template helpers live in
// `prompt_builders` so this module remains a thin orchestration surface.
pub(crate) use super::prompt_builders::dreaming_prompt;
pub(crate) use super::prompt_builders::{
    CoderPromptInputs, RepoStateUpdateCompletedSession, RepoStateUpdatePromptInputs,
    ReviewerPromptInputs, brainstorm_prompt, coder_prompt, final_validation_prompt,
    plan_review_prompt, planning_prompt, recovery_plan_review_prompt, recovery_prompt,
    recovery_sharding_prompt, repo_state_update_prompt, reviewer_full_alignment_prompt,
    reviewer_prompt, sharding_prompt, simplifier_prompt, spec_review_prompt,
};
pub(crate) use super::prompt_ctx::PromptMeta;
#[cfg(test)]
pub(crate) use super::prompt_ctx::{
    live_summary_instruction, live_summary_instruction_interactive,
};
pub(super) use super::review_banner::{prepend_review_banner, strip_review_banner};
#[cfg(not(test))]
pub(crate) use super::stage_support::git_rev_parse_head;
pub(crate) use super::stage_support::{
    assigned_revise_task_ids, auto_tough_effort, read_review_scope, read_review_scope_base_sha,
    rewrite_tasks_for_revise, task_effort_for, task_toml_for, validate_stage_toml_writes,
    write_review_scope_artifact,
};
