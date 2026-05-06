use crate::{
    adapters::EffortLevel,
    artifacts::ReviewScopeArtifact,
    state::{self as session_state},
    tasks,
};
use anyhow::Context;

pub(crate) fn restore_artifacts(pairs: &[(&std::path::Path, &std::path::Path)]) {
    for (backup, target) in pairs {
        if backup.exists() {
            let _ = std::fs::copy(backup, target);
        }
    }
}

pub(crate) fn task_toml_for(session_dir: &std::path::Path, task_id: u32) -> anyhow::Result<String> {
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml")?;
    let task = parsed
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| anyhow::anyhow!("task id {task_id} not found"))?;
    toml::to_string_pretty(task).context("serialize task.toml")
}

pub(crate) fn task_effort_for(session_dir: &std::path::Path, task_id: u32) -> EffortLevel {
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let Ok(parsed) = tasks::validate(&tasks_path) else {
        // Preserve the launch fallback when task metadata is unavailable.
        return EffortLevel::Normal;
    };
    parsed
        .tasks
        .iter()
        .find(|task| task.id == task_id && task.tough)
        .map(|_| EffortLevel::Tough)
        .unwrap_or_default()
}

pub(crate) fn assigned_revise_task_ids(
    builder: &session_state::BuilderState,
    count: usize,
) -> Vec<u32> {
    (builder.max_task_id() + 1..builder.max_task_id() + 1 + count as u32).collect()
}

pub(crate) fn rewrite_tasks_for_revise(
    session_dir: &std::path::Path,
    current_task_id: u32,
    new_tasks: &[tasks::Task],
    assigned_ids: &[u32],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        new_tasks.len() == assigned_ids.len(),
        "new task count does not match assigned id count"
    );
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml before revise")?;
    let Some(current_idx) = parsed
        .tasks
        .iter()
        .position(|task| task.id == current_task_id)
    else {
        anyhow::bail!("task id {current_task_id} not found in tasks.toml");
    };

    let mut rewritten = Vec::with_capacity(parsed.tasks.len() + new_tasks.len());
    rewritten.extend(parsed.tasks[..current_idx].iter().cloned());

    for (task, id) in new_tasks.iter().zip(assigned_ids.iter().copied()) {
        let mut inserted = task.clone();
        inserted.id = id;
        rewritten.push(inserted);
    }

    let next_pending_id = assigned_ids
        .iter()
        .copied()
        .max()
        .unwrap_or_else(|| parsed.tasks.iter().map(|task| task.id).max().unwrap_or(0));
    for (next_pending_id, task) in
        ((next_pending_id + 1)..).zip(parsed.tasks[current_idx + 1..].iter().cloned())
    {
        let mut renumbered = task;
        renumbered.id = next_pending_id;
        rewritten.push(renumbered);
    }

    let file = tasks::TasksFile { tasks: rewritten };
    let text = toml::to_string_pretty(&file).context("serialize revised tasks.toml")?;
    std::fs::write(&tasks_path, text)
        .with_context(|| format!("write revised {}", tasks_path.display()))?;
    Ok(())
}

pub(crate) fn validate_stage_toml_writes(
    session_dir: &std::path::Path,
    stage: &str,
    round: u32,
) -> anyhow::Result<()> {
    let Some(io) = session_state::transitions::stage_io(stage) else {
        return Ok(());
    };
    let round_token = format!("{round:03}");
    let paths = io
        .writes
        .iter()
        .filter(|template| template.ends_with(".toml"))
        .map(|template| session_dir.join(template.replace("{round}", &round_token)))
        .collect::<Vec<_>>();
    let refs = paths.iter().map(|path| path.as_path()).collect::<Vec<_>>();
    crate::runner::validate_toml_artifacts(&refs)
}

pub(crate) fn read_review_scope(path: &std::path::Path) -> anyhow::Result<ReviewScopeArtifact> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let scope: ReviewScopeArtifact =
        toml::from_str(&text).with_context(|| format!("malformed TOML in {}", path.display()))?;
    if scope.base_sha.trim().is_empty() {
        anyhow::bail!("base_sha is empty in {}", path.display());
    }
    Ok(scope)
}

pub(crate) fn read_review_scope_base_sha(path: &std::path::Path) -> anyhow::Result<String> {
    Ok(read_review_scope(path)?.base_sha.trim().to_string())
}

pub(crate) fn write_review_scope_artifact(
    round_dir: &std::path::Path,
    base_sha: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(round_dir)?;
    std::fs::write(
        round_dir.join("review_scope.toml"),
        format!("base_sha = \"{base_sha}\"\n"),
    )
}

// `capture_round_base` writes a deterministic placeholder in `cfg(test)`
// builds so transitions never shell out to git from the test process; this
// helper is only reachable on the production path.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn git_rev_parse_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}
