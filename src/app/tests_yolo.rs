// tests_yolo.rs
use super::*;
use super::{guard, prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    state::{self as session_state, Phase, RunRecord, RunStatus, SessionState},
};

#[test]
fn yolo_path_violation_is_audited_and_allows_reviewer_progression() {
    with_temp_root(|| {
        let session_id = "yolo-path-violation";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).unwrap();
        std::fs::write(round_dir.join("task.toml"), "id = 1\n").unwrap();
        write_review_scope(&round_dir, "base123");
        let guard_dir = session_dir.join(".guards").join("coder-task-1-r1-a1");
        guard::capture_coder(&guard_dir, &session_dir, 1).expect("capture coder guard");
        std::fs::write(round_dir.join("task.toml"), "id = 1\n# changed by coder\n").unwrap();
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "stable",
        );

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.modes.yolo = true;
        let mut run = make_coder_run(1, 1, 1);
        run.modes.yolo = true;
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        app.finalize_current_run(&run).expect("finalize coder");

        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Done);
        assert_eq!(finalized.error, None);
        assert_eq!(app.state.current_phase, Phase::ReviewRound(1));
        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("yolo_auto_approved: gate=path_violation"));
    });
}

#[test]
fn yolo_toggle_resolves_paused_gate_on_next_tick() {
    with_temp_root(|| {
        let session_id = "yolo-next-tick";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SpecReviewPaused;
        state.save().expect("save session");
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "yolo on".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(
            app.state.current_phase,
            Phase::SpecReviewPaused,
            "palette toggle arms resolution, but the gate advances on the next loop tick"
        );

        app.maybe_yolo_auto_resolve();

        assert_eq!(app.state.current_phase, Phase::PlanningRunning);
        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("yolo_auto_approved: gate=spec_approval"));
        assert!(events.contains("yolo_toggled_resolved_gate=spec_approval"));
    });
}

#[test]
fn yolo_planning_finalization_skips_plan_review_after_plan_artifact_exists() {
    with_temp_root(|| {
        let session_id = "yolo-planning-skip";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("plan artifact");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut run = make_planning_run(1, 1);
        run.modes = crate::state::LaunchModes {
            yolo: true,
            cheap: false,
            interactive: false,
        };
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        app.finalize_current_run(&run)
            .expect("finalize yolo planning");

        assert_eq!(app.state.current_phase, Phase::ShardingRunning);
        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert_eq!(
            events
                .matches("yolo_auto_approved: gate=plan_review_skipped")
                .count(),
            1
        );
    });
}

#[test]
fn yolo_dirty_worktree_gate_is_audited_from_launch_snapshot() {
    with_temp_root(|| {
        let session_id = "yolo-dirty-worktree";
        let state = SessionState::new(session_id.to_string());
        state.save().expect("save session");
        let mut app = idle_app(state);

        app.record_dirty_worktree_yolo_gate(
            true,
            crate::state::LaunchModes {
                yolo: true,
                cheap: false,
                interactive: false,
            },
        );

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert!(events.contains("yolo_auto_approved: gate=dirty_worktree"));
    });
}

#[test]
fn yolo_exit_artifact_readiness_covers_all_supported_stages() {
    with_temp_root(|| {
        let session_id = "yolo-exit-ready-stages";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("003");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");

        std::fs::write(artifacts.join("spec.md"), "# Spec\n").expect("spec");
        std::fs::write(
            artifacts.join("session_summary.toml"),
            "title = \"Session\"\nsummary = \"Ready\"\n",
        )
        .expect("session summary");
        std::fs::write(artifacts.join("spec-review-3.md"), "# Review\n").expect("review");
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("plan");
        std::fs::write(artifacts.join("tasks.toml"), "[[tasks]]\nid = 1\n").expect("tasks");
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            "status = \"done\"\nsummary = \"ok\"\n",
        )
        .expect("coder summary");
        std::fs::write(
            round_dir.join("review.toml"),
            "status = \"approved\"\nsummary = \"ok\"\nfeedback = []\n",
        )
        .expect("review verdict");
        std::fs::write(
            round_dir.join("recovery.toml"),
            "status = \"agent_pivot\"\nsummary = \"ok\"\nfeedback = []\n",
        )
        .expect("recovery summary");

        let state = SessionState::new(session_id.to_string());
        let app = idle_app(state);
        for stage in [
            "brainstorm",
            "spec-review",
            "planning",
            "sharding",
            "coder",
            "reviewer",
            "recovery",
        ] {
            let mut run = make_stage_run(7, stage, 3, 1);
            if stage == "coder" {
                run.task_id = Some(1);
            }
            assert!(
                app.yolo_exit_artifact_ready(&run),
                "{stage} should be ready"
            );
        }

        let _ = std::fs::remove_file(artifacts.join("session_summary.toml"));
        let brainstorm = make_stage_run(8, "brainstorm", 3, 1);
        assert!(
            !app.yolo_exit_artifact_ready(&brainstorm),
            "brainstorm needs both spec.md and session_summary.toml"
        );
    });
}

#[test]
fn yolo_exit_issues_once_per_invocation_after_new_observable_update() {
    with_temp_root(|| {
        let session_id = "yolo-exit-idempotent";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");

        let state = SessionState::new(session_id.to_string());
        state.save().expect("save session");
        let mut app = idle_app(state);
        let mut run = make_stage_run(42, "planning", 1, 1);
        run.modes = crate::state::LaunchModes {
            yolo: true,
            cheap: false,
            interactive: false,
        };

        app.maybe_issue_yolo_exit(&run);
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("plan artifact");
        app.maybe_issue_yolo_exit(&run);
        app.maybe_issue_yolo_exit(&run);

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert_eq!(
            events
                .matches("yolo_auto_approved: gate=planning_exit")
                .count(),
            1
        );
    });
}

#[test]
fn yolo_exit_resume_guard_waits_for_new_observable_update() {
    with_temp_root(|| {
        let session_id = "yolo-exit-resume-guard";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("stale plan artifact");

        let state = SessionState::new(session_id.to_string());
        state.save().expect("save session");
        let mut app = idle_app(state);
        let mut run = make_stage_run(43, "planning", 1, 1);
        run.modes = crate::state::LaunchModes {
            yolo: true,
            cheap: false,
            interactive: false,
        };

        app.maybe_issue_yolo_exit(&run);
        assert!(
            !app.yolo_exit_issued.contains(&run.id),
            "stale artifacts alone must not exit a resumed invocation"
        );

        write_finish_stamp_for_run(&app, &run, 0, "");

        app.maybe_issue_yolo_exit(&run);
        assert!(app.yolo_exit_issued.contains(&run.id));

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert_eq!(
            events
                .matches("yolo_auto_approved: gate=planning_exit")
                .count(),
            1
        );
    });
}

#[test]
fn yolo_recovery_finalization_skips_recovery_plan_review_after_recovery_artifact_exists() {
    with_temp_root(|| {
        let session_id = "yolo-recovery-skip";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("002");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            artifacts.join("spec.md"),
            "Spec\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
        )
        .expect("spec");
        std::fs::write(
            artifacts.join("plan.md"),
            "Plan\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
        )
        .expect("plan");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 2
title = "Finish task 2"
description = "do it"
test = "cargo test"
estimated_tokens = 10

[[tasks]]
id = 5
title = "New follow-up"
description = "new work"
test = "cargo test"
estimated_tokens = 10
"#,
        )
        .expect("tasks");
        std::fs::write(
            round_dir.join("recovery.toml"),
            r#"status = "approved"
trigger = "agent_pivot"
interactive = false
summary = "recovered queue"
feedback = ["split task 2"]
changed_files = ["artifacts/spec.md", "artifacts/plan.md", "artifacts/tasks.toml"]
"#,
        )
        .expect("recovery");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        state.builder.done = vec![1, 4];
        state.builder.pending = vec![2, 3];
        state.builder.current_task = Some(2);
        state.builder.recovery_prev_max_task_id = Some(4);
        state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4];
        state.agent_runs.push(RunRecord {
            id: 7,
            stage: "coder".to_string(),
            task_id: Some(2),
            round: 2,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let mut run = make_stage_run(8, "recovery", 2, 1);
        run.modes = crate::state::LaunchModes {
            yolo: true,
            cheap: false,
            interactive: false,
        };
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);

        app.finalize_current_run(&run)
            .expect("finalize yolo recovery");

        assert_eq!(app.state.current_phase, Phase::BuilderRecoverySharding(2));
        assert!(
            app.state
                .builder
                .pipeline_items
                .iter()
                .all(|item| item.stage != "plan-review"),
            "yolo recovery should not queue a recovery plan-review item"
        );
        assert!(
            app.state
                .builder
                .pipeline_items
                .iter()
                .any(|item| item.stage == "sharding" && item.mode.as_deref() == Some("recovery"))
        );

        let events =
            std::fs::read_to_string(session_state::session_dir(session_id).join("events.toml"))
                .expect("events");
        assert_eq!(
            events
                .matches("yolo_auto_approved: gate=recovery_plan_review_skipped")
                .count(),
            1
        );
    });
}

#[test]
fn yolo_prompts_insert_trust_preamble_and_drop_interactive_exit_cues() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("yolo-prompts");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        let trust_preamble = "You have the operator's full trust. Make very good decisions — be bold and\ndecisive. Do not hedge or ask for confirmation. Resolve every ambiguity using\nyour best judgement and move forward.";

        let brainstorm = brainstorm_prompt(
            "add a feature",
            &spec_path.display().to_string(),
            &summary_path.display().to_string(),
            &live_summary.display().to_string(),
            None,
            true,
        );
        assert_eq!(brainstorm.matches(trust_preamble).count(), 1);
        assert!(!brainstorm.contains("Operator IS available for design questions"));
        assert!(
            !brainstorm
                .contains("Stage completion — ONLY once all pending design questions are resolved")
        );
        assert!(!brainstorm.contains("`/exit`"));
        assert!(brainstorm.contains("and on each sub-goal change"));
        assert!(!brainstorm.contains("so the operator can follow along"));

        let planning = planning_prompt(&spec_path, &[], &plan_path, &live_summary, true);
        assert_eq!(planning.matches(trust_preamble).count(), 1);
        assert!(!planning.contains("ASK the operator (this is interactive)."));
        assert!(
            !planning.contains(
                "Stage completion — ONLY once all pending trade-off decisions are resolved"
            )
        );
        assert!(!planning.contains("`/exit`"));
        assert!(planning.contains("and on each sub-goal change"));
        assert!(!planning.contains("so the operator can follow along"));
    });
}
