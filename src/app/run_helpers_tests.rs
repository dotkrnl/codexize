use crate::adapters::EffortLevel;
use crate::app::test_support::{mk_app, with_temp_root};
use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};
fn run(stage: &str, task_id: Option<u32>, round: u32, attempt: u32) -> RunRecord {
    RunRecord {
        id: round as u64 * 100 + attempt as u64,
        stage: stage.to_string(),
        task_id,
        round,
        attempt,
        model: "codex-latest".to_string(),
        vendor: "openai".to_string(),
        window_name: format!("[{stage} r{round}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Done,
        error: None,
        effort: EffortLevel::Normal,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}
fn fresh_state() -> SessionState {
    SessionState::new(format!(
        "task-rounds-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ))
}
#[test]
fn task_round_index_is_one_for_a_brand_new_task_at_any_global_round() {
    // Replicates the user's exact scenario: an earlier task chewed
    // through global rounds 1-3, so this task starts at global round 4.
    // The auto-tough rule must measure rounds spent on THIS task only,
    // so global round 4 is its first task-round (index = 1).
    let mut state = fresh_state();
    for global_round in 1..=3 {
        state
            .agent_runs
            .push(run("coder", Some(1), global_round, 1));
    }
    let app = mk_app(state);
    assert_eq!(app.task_round_index(2, 4), 1);
}
#[test]
fn task_round_index_counts_only_this_tasks_distinct_rounds() {
    // Task 2 has been launched at global rounds 4, 5, 6 (three task-rounds).
    // Asking for "what task-round is global round 7?" must return 4 — even
    // though task 1's rounds 1-3 sit alongside in agent_runs.
    let mut state = fresh_state();
    for r in 1..=3 {
        state.agent_runs.push(run("coder", Some(1), r, 1));
    }
    for r in 4..=6 {
        state.agent_runs.push(run("coder", Some(2), r, 1));
    }
    let app = mk_app(state);
    assert_eq!(app.task_round_index(2, 7), 4);
}
#[test]
fn task_round_index_dedupes_attempts_within_the_same_round() {
    // Two attempts at the same round count once toward the task-round
    // index — attempts are already excluded by spec ("after three rounds,
    // not attempts").
    let mut state = fresh_state();
    state.agent_runs.push(run("coder", Some(1), 1, 1));
    state.agent_runs.push(run("coder", Some(1), 1, 2));
    state.agent_runs.push(run("coder", Some(1), 2, 1));
    let app = mk_app(state);
    assert_eq!(app.task_round_index(1, 2), 2);
}
#[test]
fn task_round_index_ignores_reviewer_runs() {
    // Round-counting must look at coder runs only — reviewer runs
    // mirror the same rounds, and counting both would double the
    // ordinal.
    let mut state = fresh_state();
    state.agent_runs.push(run("coder", Some(1), 1, 1));
    state.agent_runs.push(run("reviewer", Some(1), 1, 1));
    state.agent_runs.push(run("coder", Some(1), 2, 1));
    state.agent_runs.push(run("reviewer", Some(1), 2, 1));
    let app = mk_app(state);
    assert_eq!(app.task_round_index(1, 2), 2);
}
#[test]
fn task_round_index_is_one_when_no_prior_runs_exist() {
    // First-ever launch for this task: agent_runs has no entries for
    // it, so the only round in the dedup set is the supplied one.
    let app = mk_app(fresh_state());
    assert_eq!(app.task_round_index(99, 1), 1);
    assert_eq!(app.task_round_index(99, 42), 1);
}
#[test]
fn task_effort_for_round_promotes_when_task_round_index_passes_threshold() {
    // Mirrors the spec: a Normal task that has already had three
    // rounds gets bumped to Tough on the fourth.
    with_temp_root(|| {
        let mut state = fresh_state();
        let session_dir = crate::state::session_dir(&state.session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).unwrap();
        std::fs::write(
            session_dir.join("artifacts").join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"x\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
        )
        .unwrap();
        for r in 1..=3 {
            state.agent_runs.push(run("coder", Some(1), r, 1));
        }
        let app = mk_app(state);
        assert_eq!(
            app.task_effort_for_round(&session_dir, 1, 4),
            EffortLevel::Tough,
            "task's 4th round (global round 4) on a Normal task auto-promotes"
        );
    });
}
#[test]
fn task_effort_for_round_does_not_promote_when_global_round_is_high_but_task_is_new() {
    // The user's correction case: this task starts late (global round
    // 4 because earlier tasks consumed 1-3) but it's only the task's
    // 1st round, so it must NOT auto-promote yet.
    with_temp_root(|| {
        let mut state = fresh_state();
        let session_dir = crate::state::session_dir(&state.session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).unwrap();
        std::fs::write(
            session_dir.join("artifacts").join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"a\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n[[tasks]]\nid = 2\ntitle = \"b\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
        )
        .unwrap();
        for r in 1..=3 {
            state.agent_runs.push(run("coder", Some(1), r, 1));
        }
        let app = mk_app(state);
        assert_eq!(
            app.task_effort_for_round(&session_dir, 2, 4),
            EffortLevel::Normal,
            "task 2's first round (global round 4) must stay Normal"
        );
        assert_eq!(
            app.task_effort_for_round(&session_dir, 2, 6),
            EffortLevel::Normal,
            "task 2's third task-round (global round 6) must stay Normal"
        );
    });
}
#[test]
fn session_dir_matches_runner_path_when_paths_are_default() {
    // Regression: brainstorm.rs and the runner write artifacts under
    // `state::session_dir(...)` (cwd-relative `.codexize/sessions/<id>`),
    // but `App::session_dir()` was reading the baked `$HOME/.codexize/sessions`
    // default whenever the operator hadn't set `paths.sessions_root` explicitly.
    // The mismatch made every brainstorm finalize as "missing finish stamp"
    // because finish_stamp_path_for_run looked under `$HOME/...` while the
    // wrapper had written to the project-local `.codexize/sessions/...`.
    with_temp_root(|| {
        let state = fresh_state();
        let expected = crate::state::session_dir(&state.session_id);
        let app = mk_app(state);
        assert_eq!(app.session_dir(), expected);
    });
}
