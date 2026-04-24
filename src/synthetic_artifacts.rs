use anyhow::Result;
use std::path::Path;

use crate::artifacts::{ArtifactKind, Implementation, Spec};
use crate::tasks::{Ref, Task, TasksFile};

pub async fn generate_synthetic_artifacts(session_dir: &Path, spec: &Spec) -> Result<()> {
    // Generate synthetic plan.md
    let plan_path = session_dir.join(ArtifactKind::Plan.filename());
    let plan_content = format!(
        "# Synthetic Plan for Direct Implementation

This is a synthetic plan generated because the task was deemed simple enough for direct implementation.

## Task 1: Implement according to Spec

- Refer to the spec: {spec_filename}
",
        spec_filename = ArtifactKind::Spec.filename()
    );
    tokio::fs::write(&plan_path, plan_content.as_bytes()).await?;

    // Generate synthetic tasks.toml
    let tasks_path = session_dir.join(ArtifactKind::Tasks.filename());
    let task = Task {
        id: 1,
        title: "Implement according to Spec".to_string(),
        description: "Implement the feature described in the spec directly.".to_string(),
        test: "Run the specified tests.".to_string(), // Placeholder, actual test needs to be determined later
        estimated_tokens: 1000, // Placeholder
        spec_refs: spec.spec_refs.iter().map(|s| Ref { path: s.clone(), lines: "all".to_string() }).collect(), // Convert String to Ref
        plan_refs: vec![Ref { path: ArtifactKind::Plan.filename().to_string(), lines: "1-10".to_string() }], // Reference synthetic plan
    };
    let tasks_file = TasksFile {
        tasks: vec![task],
        remaining_tokens_estimate: 1000, // Placeholder
        shards_remaining: 1,
    };
    let tasks_content = toml::to_string(&tasks_file)?;
    tokio::fs::write(&tasks_path, tasks_content.as_bytes()).await?; // write_all requires bytes

    // Generate minimal Implementation artifact stub
    let impl_path = session_dir.join(ArtifactKind::Implementation.filename());
    let implementation_stub = Implementation {
        // Default or empty implementation artifact details
        current_task_id: 1,
        current_round: 1,
        // ... other fields as needed, can be minimal defaults
        remaining_tasks: vec![1], // Only task 1 is remaining initially
    };
    let impl_content = serde_json::to_string_pretty(&implementation_stub)?;
    tokio::fs::write(&impl_path, impl_content.as_bytes()).await?; // write_all requires bytes

    Ok(())
}