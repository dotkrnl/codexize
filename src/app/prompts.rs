// Public prompt-builder facade. The heavy template helpers live in
// `prompt_builders` so this module remains a thin orchestration surface.
#[cfg(test)]
pub(crate) use super::prompt_builders::dreaming_prompt;
pub(crate) use super::prompt_builders::{
    ReviewerPromptInputs, brainstorm_prompt, coder_prompt, final_validation_prompt,
    plan_review_prompt, planning_prompt, recovery_plan_review_prompt, recovery_prompt,
    recovery_sharding_prompt, reviewer_prompt, sharding_prompt, simplifier_prompt,
    spec_review_prompt,
};
#[cfg(test)]
pub(crate) use super::prompt_ctx::{
    live_summary_instruction, live_summary_instruction_interactive,
};
#[allow(unused_imports)]
pub(crate) use super::stage_support::{
    assigned_revise_task_ids, git_rev_parse_head, read_review_scope, read_review_scope_base_sha,
    restore_artifacts, rewrite_tasks_for_revise, task_effort_for, task_toml_for,
    validate_stage_toml_writes, write_review_scope_artifact,
};
// formatdoc! remains the canonical renderer for thin inline wrappers; see
// `prompt_builders::PromptCtx::live_summary_instruction`.
#[allow(unused_imports)]
pub(super) use super::review_banner::{REVIEW_BANNER, prepend_review_banner, strip_review_banner};
#[allow(unused_imports)]
use indoc::formatdoc;
