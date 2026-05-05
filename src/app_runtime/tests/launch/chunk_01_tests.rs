use super::*;

#[test]
fn brainstorm_finalization_overlength_nothing_to_do_enters_skip_pending() {
    with_temp_root(|| {
        let session_id = "brainstorm-skip-overlength";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;

        let run = RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        };
        state.agent_runs.push(run.clone());

        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("mk artifacts dir");

        let rationale = "x".repeat(520);
        let proposal_toml = format!(
            "proposed = true\nstatus = \"nothing_to_do\"\nrationale = \"{}\"\n",
            rationale
        );
        std::fs::write(artifacts.join("skip_proposal.toml"), proposal_toml)
            .expect("write skip proposal");

        let mut app = idle_app(state);
        app.complete_run_finalization(&run, None)
            .expect("finalization should succeed");

        assert_eq!(app.state.current_phase, Phase::SkipToImplPending);
        assert_eq!(
            app.state.skip_to_impl_kind,
            Some(crate::artifacts::SkipProposalStatus::NothingToDo)
        );
        let stored_rationale = app
            .state
            .skip_to_impl_rationale
            .expect("rationale should be set");
        assert_eq!(stored_rationale.chars().count(), 500);
    });
}

#[test]
fn launch_recovery_uses_interactive_prompt_for_human_blocked() {
    use crate::state::PipelineItemStatus;
    with_temp_root(|| {
        let session_id = "recovery-interactive-launch";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(1);
        state.builder.recovery_trigger_task_id = Some(1);
        state.builder.recovery_trigger_summary = Some("needs human judgment".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: Some("Human-blocked recovery".to_string()),
            mode: None,
            trigger: Some("human_blocked".to_string()),
            interactive: Some(true),
            iteration: 1,
        });

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            1,
            10,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        let ok = app.launch_recovery_with_model(None);
        assert!(ok, "launch_recovery_with_model must succeed");

        let prompt_path = session_dir.join("prompts").join("recovery-r1.md");
        let prompt = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            prompt.contains("INTERACTIVE"),
            "human_blocked recovery prompt file must be INTERACTIVE"
        );
        assert!(
            !prompt.contains("NON-INTERACTIVE"),
            "human_blocked recovery prompt file must not be NON-INTERACTIVE"
        );
    });
}

#[test]
fn launch_recovery_uses_noninteractive_prompt_for_agent_pivot() {
    use crate::state::PipelineItemStatus;
    with_temp_root(|| {
        let session_id = "recovery-noninteractive-launch";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        state.builder.recovery_trigger_task_id = Some(1);
        state.builder.recovery_trigger_summary = Some("plan is wrong".to_string());
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(2),
            status: PipelineItemStatus::Running,
            title: Some("Agent pivot recovery".to_string()),
            mode: None,
            trigger: Some("agent_pivot".to_string()),
            interactive: Some(false),
            iteration: 1,
        });

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            1,
            10,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        let ok = app.launch_recovery_with_model(None);
        assert!(ok, "launch_recovery_with_model must succeed");

        let prompt_path = session_dir.join("prompts").join("recovery-r2.md");
        let prompt = std::fs::read_to_string(&prompt_path).unwrap();
        assert!(
            prompt.contains("NON-INTERACTIVE"),
            "agent_pivot recovery prompt file must be NON-INTERACTIVE"
        );
    });
}

#[test]
fn coder_gate_accepts_done_summary_without_head_advance() {
    with_temp_root(|| {
        let session_id = "coder-summary-done";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "base123",
            "stable",
        );
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            r#"status = "done"
summary = "Already complete"
"#,
        )
        .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        assert_eq!(app.coder_gate_reason(&run, &round_dir), None);
    });
}

#[test]
fn coder_gate_retries_partial_summary_even_after_head_advances() {
    with_temp_root(|| {
        let session_id = "coder-summary-partial";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "stable",
        );
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            r#"status = "partial"
summary = "Still working"
"#,
        )
        .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        assert_eq!(
            app.coder_gate_reason(&run, &round_dir).as_deref(),
            Some("coder_partial")
        );
    });
}

#[test]
fn coder_gate_rejects_invalid_summary_even_after_head_advances() {
    with_temp_root(|| {
        let session_id = "coder-summary-invalid";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        write_finish_stamp(
            &session_dir,
            &App::run_key_for("coder", Some(1), 1, 1),
            "head456",
            "stable",
        );
        std::fs::write(
            round_dir.join("coder_summary.toml"),
            r#"status = "done"
summary = "   "
"#,
        )
        .unwrap();

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        assert_eq!(
            app.coder_gate_reason(&run, &round_dir).as_deref(),
            Some("invalid_coder_summary")
        );
    });
}

#[test]
fn coder_gate_rejects_dirty_working_tree_finish_stamp() {
    with_temp_root(|| {
        let session_id = "coder-dirty-finish-stamp";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");
        let stamp = crate::runner::FinishStamp {
            finished_at: chrono::Utc::now().to_rfc3339(),
            exit_code: 0,
            head_before: "base123".to_string(),
            head_after: "head456".to_string(),
            head_state: "stable".to_string(),
            signal_received: String::new(),
            working_tree_clean: false,
        };
        let stamp_path = session_dir
            .join("artifacts")
            .join("run-finish")
            .join(format!("{}.toml", App::run_key_for("coder", Some(1), 1, 1)));
        crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write finish stamp");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        let run = make_coder_run(1, 1, 1);
        state.agent_runs.push(run.clone());
        let app = idle_app(state);

        let reason = app.coder_gate_reason(&run, &round_dir).unwrap();
        assert!(reason.contains("working tree not clean on exit"));
    });
}

#[test]
fn reviewer_prompt_always_scopes_base_to_head() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("reviewer-prompt-base-head");
        let task_file = session_dir.join("rounds/001/task.toml");
        let scope_file = session_dir.join("rounds/001/review_scope.toml");
        let summary_file = session_dir.join("rounds/001/coder_summary.toml");
        let review_file = session_dir.join("rounds/001/review.toml");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(task_file.parent().unwrap()).unwrap();

        let prompt = reviewer_prompt(ReviewerPromptInputs {
            session_dir: &session_dir,
            task_id: 1,
            round: 2,
            task_file: &task_file,
            review_scope_file: &scope_file,
            coder_summary_file: Some(&summary_file),
            review_file: &review_file,
            live_summary_path: &live_summary,
        });

        assert!(!prompt.contains("DIRTY WORKING TREE"));
        assert!(!prompt.contains("git diff HEAD"));
        assert!(!prompt.contains("git ls-files --others --exclude-standard"));
        assert!(prompt.contains("review only `base..HEAD`"));
        assert!(prompt.contains("Coder summary:"));
        assert!(prompt.contains("Coder rebuttal (round 2):"));
    });
}

#[test]
fn brainstorm_prompts_require_authoritative_user_requirements() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("brainstorm-authoritative-section");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        for yolo in [false, true] {
            let prompt = brainstorm_prompt(
                "add retries unless disabled",
                &spec_path.display().to_string(),
                &summary_path.display().to_string(),
                &live_summary.display().to_string(),
                yolo,
            );

            assert!(prompt.contains("## User-stated requirements (authoritative)"));
            assert!(
                prompt.contains("Quote each user-stated decision from the Idea above verbatim")
            );
            assert!(prompt.contains("Use the user's own wording, not a paraphrase."));
            assert!(prompt.contains("Never silently reinterpret."));
            assert!(prompt.contains("must not silently invent exclusions"));
            assert!(
                prompt
                    .contains("If you are uncertain whether something is in or out of scope, ask")
            );
            assert!(prompt.contains("## Out of scope"));
            assert!(
                prompt.contains("Each bullet here must either quote a user statement verbatim")
            );
            if yolo {
                assert!(prompt.contains("pick the narrowest reasonable reading"));
                assert!(prompt.contains("recording the choice under\n`## Assumptions`"));
            } else {
                assert!(prompt.contains("statement is ambiguous, ask the operator."));
                assert!(prompt.contains(
                    "If two user statements conflict with\neach other, ask the operator."
                ));
            }
        }
    });
}

#[test]
fn brainstorm_prompt_embeds_no_skill_clause() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("brainstorm-inline-workflow");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        for yolo in [false, true] {
            let prompt = brainstorm_prompt(
                "add retries unless disabled",
                &spec_path.display().to_string(),
                &summary_path.display().to_string(),
                &live_summary.display().to_string(),
                yolo,
            );

            assert!(prompt.contains("Do not invoke any skill"));
            assert!(prompt.contains("## User-stated requirements (authoritative)"));
            assert!(
                prompt
                    .contains("Outputs (all under artifacts/, SPEC-ONLY phase — no code, no VCS):")
            );
            assert!(
                prompt.contains("No `git add`/`commit`/`stash` or any version-control mutation")
            );
            if yolo {
                assert!(prompt.contains("and on each sub-goal change"));
                assert!(!prompt.contains("`/exit`"));
            } else {
                assert!(prompt.contains("so the operator can follow along"));
                assert!(prompt.contains("operator to enter `/exit`"));
            }
        }
    });
}

#[test]
fn brainstorm_launch_renders_inline_workflow_prompt() {
    with_temp_root(|| {
        let session_id = "brainstorm-launch-inline-workflow";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5.5",
            1,
            1,
            1,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_brainstorm_with_model(
            "add retries".to_string(),
            Some(ranked_model(
                selection::VendorKind::Codex,
                "gpt-5.5",
                1,
                1,
                1,
            )),
        ));

        let prompt_path = session_state::session_dir(session_id)
            .join("prompts")
            .join("brainstorm.md");
        let prompt = std::fs::read_to_string(prompt_path).unwrap();
        assert!(prompt.contains("Run this workflow\nend-to-end inside this prompt"));
        assert!(prompt.contains("Do not invoke any skill"));
    });
}

#[test]
fn coder_prompt_requires_clean_exit_and_new_summary_schema() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("coder-clean-exit-prompt");
        let round_dir = session_dir.join("rounds/001");
        let task_file = round_dir.join("task.toml");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(&round_dir).unwrap();

        let prompt = coder_prompt(&session_dir, 1, 1, &task_file, &live_summary, false, &[]);

        assert!(prompt.contains("Working tree must be clean on exit."));
        assert!(prompt.contains("git status --porcelain` MUST be empty when you stop"));
        assert!(prompt.contains("tree dirty is a hard failure"));
        assert!(!prompt.contains("dirty_before"));
        assert!(!prompt.contains("dirty_after"));
        assert!(prompt.contains("independently verifies the working tree is clean"));
    });
}

#[test]
fn planning_prompt_flags_ai_written_reviews_for_triage() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("planning-prompt-ai-reviews");
        let spec_path = session_dir.join("artifacts/spec.md");
        let plan_path = session_dir.join("artifacts/plan.md");
        let review_path = session_dir.join("artifacts/spec-review-1.md");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
        std::fs::write(&review_path, "review").unwrap();

        let prompt = planning_prompt(&spec_path, &[review_path], &plan_path, &live_summary, false);

        assert!(prompt.contains("written by AI"));
        assert!(prompt.contains("be skeptical"));
        assert!(prompt.contains("genuinely improves the spec or plan"));
        assert!(prompt.contains("reject the rest with a brief reason"));
    });
}

#[test]
fn coder_prompt_tells_resume_rounds_to_rebut_unhelpful_ai_feedback() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("coder-prompt-ai-feedback");
        let round_dir = session_dir.join("rounds/001");
        let task_file = round_dir.join("task.toml");
        let live_summary = session_dir.join("artifacts/live_summary.txt");
        std::fs::create_dir_all(&round_dir).unwrap();
        std::fs::write(round_dir.join("review.toml"), "feedback").unwrap();

        let prompt = coder_prompt(&session_dir, 1, 2, &task_file, &live_summary, true, &[]);

        assert!(prompt.contains("Previous reviewer feedback (round 1):"));
        assert!(prompt.contains("Reviewer feedback comes from an AI agent."));
        assert!(prompt.contains(
                "Evaluate each item critically — address what improves the code, rebut the rest in coder_summary.toml."
            ));
    });
}

#[test]
fn final_validation_launch_uses_session_model_review_effort_and_window_label() {
    with_temp_root(|| {
        let session_id = "final-validation-launch";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(
            artifacts.join("spec.md"),
            "# Spec\n\n## User-stated requirements (authoritative)\n- run\n",
        )
        .expect("write spec");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(2);
        state.idea_text = Some("Make the validator agent run end-to-end".to_string());
        state.selected_model = Some("claude-sonnet-4-6".to_string());

        let mut app = idle_app(state);
        // The session-selected model should be used; other models in the list
        // exist only to confirm the picker doesn't replace the selection.
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 1, 10),
            ranked_model(
                selection::VendorKind::Claude,
                "claude-sonnet-4-6",
                10,
                10,
                10,
            ),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(
                        "status = \"goal_met\"\nsummary = \"ok\"\nfindings = [\"workspace clean\"]\n"
                            .to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_final_validation_with_model(None));

        let run = app
            .state
            .agent_runs
            .last()
            .expect("final validation must record a run");
        assert_eq!(run.stage, "final-validation");
        assert_eq!(run.task_id, None);
        assert_eq!(run.round, 2);
        assert_eq!(run.model, "claude-sonnet-4-6");
        assert_eq!(run.vendor, "claude");
        assert_eq!(run.effort, EffortLevel::Normal);
        assert!(
            !run.modes.interactive,
            "final validation must launch non-interactively"
        );
        assert!(
            run.window_name.starts_with("[FinalValidation] "),
            "expected `[FinalValidation] {{model_short}}` window label, got {}",
            run.window_name
        );
        assert!(
            run.window_name.contains("sonnet-4-6"),
            "window label must include short model name, got {}",
            run.window_name
        );

        let verdict_path = artifacts.join("final_validation_2.toml");
        assert!(verdict_path.exists(), "harness must write the verdict path");
        let live_summary = artifacts.join(format!(
            "live_summary.{}.txt",
            App::run_key_for("final-validation", None, 2, 1)
        ));
        let prompt_path = session_dir.join("prompts").join("final-validation-r2.md");
        let prompt = std::fs::read_to_string(&prompt_path).expect("prompt file");
        assert!(prompt.contains(&verdict_path.display().to_string()));
        assert!(prompt.contains(&live_summary.display().to_string()));
    });
}

#[test]
fn final_validation_launch_falls_back_when_selected_model_missing() {
    with_temp_root(|| {
        let session_id = "final-validation-fallback";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::write(artifacts.join("spec.md"), "# Spec\n").expect("spec");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::FinalValidation(1);
        state.idea_text = Some("idea".to_string());
        // No `selected_model` — the launcher must still pick a model rather
        // than refuse to start.

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-opus-4-7",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(
                        "status = \"goal_met\"\nsummary = \"ok\"\nfindings = []\n".to_string(),
                    ),
                    launch_error: None,
                }]),
            },
        )));

        assert!(app.launch_final_validation_with_model(None));
        let run = app.state.agent_runs.last().expect("run record");
        assert_eq!(run.model, "claude-opus-4-7");
        assert_eq!(run.stage, "final-validation");
    });
}
