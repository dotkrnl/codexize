use std::{fs, path::Path};

fn production_source_files(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            production_source_files(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tests_") || name == "test_harness.rs")
            && !path.starts_with("src/state")
        {
            files.push(path);
        }
    }
}

fn production_prefix(contents: &str) -> &str {
    contents
        .split_once("\n#[cfg(test)]")
        .map(|(prefix, _)| prefix)
        .unwrap_or(contents)
}

#[test]
fn runtime_state_mutations_go_through_transitions_module() {
    let forbidden_mutator_patterns = [
        ".state.create_run_record(",
        ".state.transition_to(",
        ".state.pending_guard_decision.take(",
        ".state.builder.ensure_task_for_round(",
        ".state.builder.push_pipeline_item(",
        ".state.builder.set_task_status(",
        ".state.builder.apply_revise_with_new_tasks(",
        ".state.builder.reset_task_pipeline(",
        ".state.builder.pipeline_items.iter_mut(",
        ".state.builder.pending_refine_feedback",
        ".state.builder.recovery_prev_task_ids.clear(",
        ".state.builder.task_titles.insert(",
        ".state.agent_runs.iter_mut(",
        ".state.resume_running_runs(",
    ];
    let forbidden_assignment_patterns = [
        ".state.current_phase =",
        ".state.agent_error =",
        ".state.archived =",
        ".state.idea_text =",
        ".state.selected_model =",
        ".state.title =",
        ".state.skip_to_impl_rationale =",
        ".state.skip_to_impl_kind =",
        ".state.pending_guard_decision =",
        ".state.modes.yolo =",
        ".state.modes.cheap =",
        ".state.builder =",
        ".state.builder.pipeline_items =",
        ".state.builder.last_verdict =",
        ".state.builder.recovery_cycle_count +=",
        ".state.builder.recovery_cycle_count =",
        ".state.builder.recovery_trigger_task_id =",
        ".state.builder.recovery_prev_max_task_id =",
        ".state.builder.recovery_prev_task_ids =",
        ".state.builder.recovery_trigger_summary =",
        ".state.builder.retry_reset_run_id_cutoff =",
        ".state.builder.task_titles =",
    ];

    let mut files = Vec::new();
    production_source_files(Path::new("src"), &mut files);

    let mut violations = Vec::new();
    for path in files {
        let contents = fs::read_to_string(&path).expect("read source file");
        for (line_idx, line) in production_prefix(&contents).lines().enumerate() {
            if forbidden_mutator_patterns
                .iter()
                .any(|pattern| line.contains(pattern))
                || forbidden_assignment_patterns
                    .iter()
                    .any(|pattern| line.contains(pattern) && !line.contains("=="))
            {
                violations.push(format!("{}:{}", path.display(), line_idx + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "runtime SessionState mutations outside src/state must call state::transitions:\n{}",
        violations.join("\n")
    );
}
