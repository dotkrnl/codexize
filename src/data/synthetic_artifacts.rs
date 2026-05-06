use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::data::artifacts::{ArtifactKind, Spec};
use crate::tasks::{Ref, Task, TasksFile};

/// Write synthetic plan/tasks artifacts into `<session_dir>/artifacts/`
/// so the downstream builder flow can proceed as if sharding produced a single task.
pub fn generate_synthetic_artifacts(session_dir: &Path, spec: &Spec) -> Result<()> {
    let artifacts_dir = session_dir.join("artifacts");
    fs::create_dir_all(&artifacts_dir)
        .with_context(|| format!("creating {}", artifacts_dir.display()))?;

    let plan_path = artifacts_dir.join(ArtifactKind::Plan.filename());
    let plan_content = format!(
        "# Synthetic Plan for Direct Implementation\n\n\
This plan was generated automatically because the brainstorm agent judged the task \
simple enough to skip the usual planning and sharding phases.\n\n\
## Task 1: Implement according to Spec\n\n\
- Refer to the spec: {spec_filename}\n",
        spec_filename = ArtifactKind::Spec.filename()
    );
    fs::write(&plan_path, plan_content)
        .with_context(|| format!("writing {}", plan_path.display()))?;

    let tasks_path = artifacts_dir.join(ArtifactKind::Tasks.filename());
    let spec_refs = if spec.spec_refs.is_empty() {
        vec![Ref {
            path: ArtifactKind::Spec.filename().to_string(),
            lines: "all".to_string(),
        }]
    } else {
        spec.spec_refs
            .iter()
            .map(|s| Ref {
                path: s.clone(),
                lines: "all".to_string(),
            })
            .collect()
    };
    let task = Task {
        id: 1,
        title: "Implement according to Spec".to_string(),
        description:
            "Implement the feature described in the spec directly; no sharding was performed."
                .to_string(),
        test: "Run the tests described in the spec.".to_string(),
        estimated_tokens: 1000,
        tough: false,
        spec_refs,
        plan_refs: vec![Ref {
            path: ArtifactKind::Plan.filename().to_string(),
            lines: "all".to_string(),
        }],
    };
    let tasks_file = TasksFile { tasks: vec![task] };
    let tasks_content = toml::to_string(&tasks_file)?;
    fs::write(&tasks_path, tasks_content)
        .with_context(|| format!("writing {}", tasks_path.display()))?;

    Ok(())
}

#[cfg(test)]
#[path = "synthetic_artifacts_tests.rs"]
mod tests;
