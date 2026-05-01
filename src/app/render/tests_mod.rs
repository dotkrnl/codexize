use super::*;
use crate::{
    app::tree::{flatten_visible_rows, node_key_at_path},
    state::{
        Message, MessageKind, MessageSender, Node, NodeKind, NodeStatus, RunRecord, RunStatus,
        SessionState,
    },
};
use ratatui::layout::Rect;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

fn test_app(nodes: Vec<Node>, runs: Vec<RunRecord>, messages: Vec<Message>) -> App {
    let mut state = SessionState::new("render-test".to_string());
    state.agent_runs = runs;
    let selected_key = node_key_at_path(&nodes, &[0]);
    let visible_rows = flatten_visible_rows(&nodes, |row| row.is_expandable());
    let collapsed_overrides = visible_rows
        .iter()
        .filter(|row| row.is_expandable())
        .map(|row| (row.key.clone(), super::super::ExpansionOverride::Expanded))
        .collect();
    App {
        state,
        nodes,
        visible_rows,
        models: Vec::new(),
        versions: crate::selection::ranking::build_version_index(&[]),
        model_refresh: ModelRefreshState::Idle(Instant::now()),
        selected: 0,
        selected_key,
        collapsed_overrides,
        viewport_top: 0,
        follow_tail: true,
        explicit_viewport_scroll: false,
        progress_follow_active: true,
        tail_detach_baseline: None,
        body_inner_height: 20,
        body_inner_width: 80,
        split_target: None,
        split_follow_tail: true,
        split_scroll_offset: 0,
        split_fullscreen: false,
        input_mode: false,
        input_buffer: String::new(),
        input_cursor: 0,
        pending_view_path: None,
        confirm_back: false,
        run_launched: false,
        quota_errors: Vec::new(),
        quota_retry_delay: Duration::from_secs(60),
        agent_line_count: 0,
        agent_content_hash: 0,
        agent_last_change: None,
        spinner_tick: 0,
        live_summary_spinner_visible: false,
        live_summary_watcher: None,
        live_summary_change_rx: None,
        live_summary_path: None,
        live_summary_cached_text: String::new(),
        live_summary_cached_mtime: None,
        pending_drain_deadline: None,
        current_run_id: None,
        failed_models: HashMap::new(),
        pending_yolo_toggle_gate: None,
        yolo_exit_issued: std::collections::HashSet::new(),
        yolo_exit_observations: HashMap::new(),
        test_launch_harness: None,
        messages,
        status_line: std::rc::Rc::new(std::cell::RefCell::new(
            super::super::status_line::StatusLine::new(),
        )),
        prev_models_mode: super::super::models_area::ModelsAreaMode::default(),
        palette: super::super::palette::PaletteState::default(),
        command_return_target: None,
    }
}

fn run_record(id: u64, status: RunStatus) -> RunRecord {
    RunRecord {
        id,
        stage: format!("run-{id}"),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "model".to_string(),
        vendor: "vendor".to_string(),
        window_name: format!("[Run {id}]"),
        started_at: chrono::Utc::now(),
        ended_at: if status == RunStatus::Running {
            None
        } else {
            Some(chrono::Utc::now())
        },
        status,
        error: None,
        effort: crate::adapters::EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    }
}

fn message(run_id: u64, text: &str) -> Message {
    Message {
        ts: chrono::Utc::now(),
        run_id,
        kind: MessageKind::Summary,
        sender: MessageSender::Agent {
            model: "model".to_string(),
            vendor: "vendor".to_string(),
        },
        text: text.to_string(),
    }
}

fn agent_text(run_id: u64, text: &str) -> Message {
    Message {
        ts: chrono::Utc::now(),
        run_id,
        kind: MessageKind::AgentText,
        sender: MessageSender::Agent {
            model: "model".to_string(),
            vendor: "vendor".to_string(),
        },
        text: text.to_string(),
    }
}

fn agent_thought(run_id: u64, text: &str) -> Message {
    Message {
        ts: chrono::Utc::now(),
        run_id,
        kind: MessageKind::AgentThought,
        sender: MessageSender::Agent {
            model: "model".to_string(),
            vendor: "vendor".to_string(),
        },
        text: text.to_string(),
    }
}

fn user_input(run_id: u64, text: &str) -> Message {
    Message {
        ts: chrono::Utc::now(),
        run_id,
        kind: MessageKind::UserInput,
        sender: MessageSender::System,
        text: text.to_string(),
    }
}

fn kind_message(run_id: u64, kind: MessageKind, text: &str) -> Message {
    Message {
        ts: chrono::Utc::now(),
        run_id,
        kind,
        sender: MessageSender::System,
        text: text.to_string(),
    }
}

// model_strip_* full-table rendering tests have moved to
// src/app/models_area.rs and target the new responsive_models_area
// entry point. The underlying model_strip / model_strip_height /
// format_model_name_spans helpers stay alive here only until the
// chrome cutover wires the new renderer into App::draw.

fn node(
    label: &str,
    kind: NodeKind,
    status: NodeStatus,
    children: Vec<Node>,
    run_id: Option<u64>,
    leaf_run_id: Option<u64>,
) -> Node {
    Node {
        label: label.to_string(),
        kind,
        status,
        summary: format!("{label} summary"),
        children,
        run_id,
        leaf_run_id,
    }
}

fn nested_transcript_tree() -> Vec<Node> {
    vec![node(
        "Root",
        NodeKind::Stage,
        NodeStatus::Running,
        vec![node(
            "Task A",
            NodeKind::Task,
            NodeStatus::Running,
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            )],
            None,
            None,
        )],
        None,
        None,
    )]
}

fn line_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| buf[(x, y)].symbol())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn line_to_string(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.to_string())
        .collect::<String>()
}

fn render_lines(app: &App, height: u16) -> Vec<String> {
    let width = 80;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    PipelineWidget { app }.render(area, &mut buf);
    (0..height).map(|y| line_text(&buf, y, width)).collect()
}

#[test]
fn renders_depth_indented_visible_rows_with_main_panel_transcript() {
    let app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Running)],
        vec![message(1, "coder transcript body")],
    );

    let lines = render_lines(&app, 10);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("▾ Root") && line.contains("running"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("└─ ▾ Task A") && line.contains("running"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("└─ ▾ Builder") && line.contains("running"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Coder transcript body")),
        "main panel must show system summary messages: {lines:#?}"
    );
}

#[test]
fn expanded_structural_parents_do_not_render_duplicate_child_list_body() {
    let app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Running)],
        vec![message(1, "only the transcript body")],
    );

    let lines = render_lines(&app, 20);

    assert!(!lines.iter().any(|line| line.contains("── Task A")));
    assert!(!lines.iter().any(|line| line.contains("── Builder")));
}

#[test]
fn noninteractive_main_panel_shows_summaries_and_hides_agent_text() {
    let app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Running)],
        vec![
            message(1, "summary stays visible"),
            agent_text(1, "raw noninteractive text"),
        ],
    );

    let lines = render_lines(&app, 20);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Summary stays visible")),
        "system summaries belong in the main panel: {lines:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("raw noninteractive text")),
        "ACP output must stay out of the main panel"
    );

    let mut app = app;
    app.state.show_noninteractive_texts = true;
    let lines = render_lines(&app, 12);

    assert!(
        !lines
            .iter()
            .any(|line| line.contains("raw noninteractive text")),
        "legacy `:texts` toggle must not leak ACP output into the main panel"
    );
}

#[test]
fn interactive_agent_text_is_never_visible_in_tree_body() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_text(1, "live interactive text")],
    );

    let lines = render_lines(&app, 8);

    assert!(
        !lines
            .iter()
            .any(|line| line.contains("live interactive text")),
        "interactive agent text must not render inside the tree body"
    );
}

#[test]
fn user_input_messages_render_in_main_panel_for_both_modes() {
    for interactive in [false, true] {
        let mut run = run_record(1, RunStatus::Running);
        run.modes.interactive = interactive;
        let app = test_app(
            nested_transcript_tree(),
            vec![run],
            vec![user_input(1, "please continue")],
        );

        let lines = render_lines(&app, 8);

        assert!(
            lines.iter().any(|line| line.contains("› please continue")),
            "user input echo belongs in the main panel (interactive={interactive}): {lines:#?}"
        );
    }
}

#[test]
fn thinking_text_is_never_visible_in_tree_body() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_thought(1, "internal chain")],
    );

    let lines = render_lines(&app, 8);
    assert!(
        !lines.iter().any(|line| line.contains("Internal chain")),
        "thinking text must not render inside the tree body"
    );

    app.state.show_thinking_texts = true;
    let lines = render_lines(&app, 8);
    assert!(
        !lines.iter().any(|line| line.contains("Internal chain")),
        "thinking text must not render inside the tree body even in verbose mode"
    );
}

#[test]
fn expanded_absorbed_simple_stage_renders_transcript_in_main_panel() {
    let app = test_app(
        vec![node(
            "Brainstorm",
            NodeKind::Stage,
            NodeStatus::Done,
            Vec::new(),
            None,
            Some(7),
        )],
        vec![run_record(7, RunStatus::Done)],
        vec![message(7, "absorbed transcript body")],
    );

    let lines = render_lines(&app, 8);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Brainstorm") && line.contains("done"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Absorbed transcript body")),
        "absorbed simple stage transcript belongs in main panel: {lines:#?}"
    );
}

#[test]
fn multiple_open_agent_rows_render_each_runs_main_panel_transcript() {
    let nodes = vec![node(
        "Root",
        NodeKind::Stage,
        NodeStatus::Running,
        vec![
            node(
                "First",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            ),
            node(
                "Second",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(2),
                None,
            ),
        ],
        None,
        None,
    )];
    let app = test_app(
        nodes,
        vec![
            run_record(1, RunStatus::Running),
            run_record(2, RunStatus::Running),
        ],
        vec![
            message(1, "first transcript"),
            message(2, "second transcript"),
        ],
    );

    let lines = render_lines(&app, 12);
    let first_header = lines
        .iter()
        .position(|line| line.contains("First") && line.contains("running"))
        .expect("first header rendered");
    let second_header = lines
        .iter()
        .position(|line| line.contains("Second") && line.contains("running"))
        .expect("second header rendered");

    assert!(first_header < second_header);
    assert!(
        lines.iter().any(|line| line.contains("First transcript")),
        "first transcript should render in main panel: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Second transcript")),
        "second transcript should render in main panel: {lines:#?}"
    );
}

#[test]
fn failed_unverified_render_shows_distinct_status_without_transcript_in_tree() {
    let app = test_app(
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::FailedUnverified,
                Vec::new(),
                Some(1),
                None,
            )],
            vec![run_record(1, RunStatus::FailedUnverified)],
            vec![Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: MessageSender::System,
                text: "attempt 1 unverified: missing finish stamp at artifacts/run-finish/coder-t1-r1-a1.toml".to_string(),
            }],
        );

    let lines = render_lines(&app, 8);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Builder") && line.contains("failed-unverified"))
    );
    assert!(
        lines.iter().any(|line| line.contains("run-finish")),
        "End message belongs in the main panel transcript: {lines:#?}"
    );
}

#[test]
fn header_only_viewports_render_headers_without_body() {
    let app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Running)],
        vec![message(1, "hidden transcript body")],
    );

    let lines = render_lines(&app, 3);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("Root") && line.contains("running"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Task A") && line.contains("running"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("Builder") && line.contains("running"))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("hidden transcript body"))
    );
}

fn tall_app() -> App {
    // Use a large structural tree (many children under one root) so the body
    // is tall even though transcript messages no longer render in the tree.
    let mut children = Vec::new();
    for i in 0..25 {
        children.push(node(
            &format!("Filler {i}"),
            NodeKind::Task,
            NodeStatus::Done,
            Vec::new(),
            None,
            None,
        ));
    }
    // One expandable child with a run so unread-tracking tests still have a
    // rendered-line anchor in `first_unread_rendered_line`.
    children.push(node(
        "Builder",
        NodeKind::Mode,
        NodeStatus::Running,
        Vec::new(),
        Some(1),
        None,
    ));
    for i in 25..50 {
        children.push(node(
            &format!("Filler {i}"),
            NodeKind::Task,
            NodeStatus::Done,
            Vec::new(),
            None,
            None,
        ));
    }
    let nodes = vec![node(
        "Root",
        NodeKind::Stage,
        NodeStatus::Running,
        children,
        None,
        None,
    )];
    let mut messages = Vec::new();
    for i in 0..50 {
        messages.push(message(1, &format!("message {i}")));
    }
    let runs = vec![run_record(1, RunStatus::Running)];
    let mut app = test_app(nodes, runs, messages);
    app.body_inner_height = 5;
    app.body_inner_width = 80;
    app
}

fn transcript_then_stage_tree() -> Vec<Node> {
    vec![node(
        "Root",
        NodeKind::Stage,
        NodeStatus::Running,
        vec![
            node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            ),
            node(
                "Review",
                NodeKind::Stage,
                NodeStatus::Pending,
                Vec::new(),
                None,
                None,
            ),
        ],
        None,
        None,
    )]
}

#[test]
fn explicit_page_scroll_moves_viewport_without_focus_clamping() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.selected = 0;
    let step = app.body_inner_height.saturating_sub(1).max(1) as isize;
    app.scroll_viewport(step, true);
    assert_eq!(app.selected, 0);
    assert!(app.explicit_viewport_scroll);
    app.clamp_viewport();
    assert_eq!(app.selected, 0);
    assert!(app.viewport_top > 0);
}

#[test]
fn page_scroll_to_bottom_reattaches_tail_and_hides_badge() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.messages.push(message(1, "new unread"));
    let max_top = app.max_viewport_top();
    app.scroll_viewport(max_top as isize, true);
    app.clamp_viewport();
    assert!(app.follow_tail);
    assert_eq!(app.tail_detach_baseline, None);
    assert!(
        app.unread_badge().is_none(),
        "badge should be hidden at bottom"
    );
}

#[test]
fn unread_badge_shows_when_new_content_below_viewport() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.messages.push(message(1, "new unread"));
    app.viewport_top = 0;
    app.clamp_viewport();
    let badge = app.unread_badge();
    assert!(badge.is_some(), "should report unread badge");
    assert_eq!(badge.unwrap().count, 1);
}

#[test]
fn unread_badge_hides_once_first_unread_line_is_visible() {
    let mut app = test_app(
        transcript_then_stage_tree(),
        vec![run_record(1, RunStatus::Running)],
        vec![
            message(1, "old message 1"),
            message(1, "old message 2"),
            message(1, "old message 3"),
        ],
    );
    app.body_inner_height = 5;
    app.body_inner_width = 80;
    app.set_follow_tail(false);
    app.messages.push(message(1, "new unread"));
    app.scroll_viewport(2, true);

    let lines = render_lines(&app, app.body_inner_height as u16 + 2);
    assert!(
        lines.iter().any(|line| line.contains("New unread")),
        "main-panel transcript should now show the unread message: {lines:#?}"
    );
    assert!(
        app.unread_badge().is_none(),
        "badge should be hidden when unread is visible"
    );
}

#[test]
fn page_up_scrolls_viewport_without_moving_focus() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.viewport_top = app.max_viewport_top();
    app.selected = 2;
    let initial_selected = app.selected;
    let step = app.body_inner_height.saturating_sub(1).max(1) as isize;
    app.scroll_viewport(-step, true);
    assert_eq!(app.selected, initial_selected);
    assert!(app.viewport_top < app.max_viewport_top());
    app.clamp_viewport();
    assert_eq!(app.selected, initial_selected);
}

#[test]
fn page_down_key_pages_without_moving_focus() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    let initial_key = app.selected_key.clone();
    let step = app.body_inner_height.saturating_sub(1).max(1);

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::PageDown,
        crossterm::event::KeyModifiers::NONE,
    ));

    assert_eq!(app.selected, 0);
    assert_eq!(app.selected_key, initial_key);
    assert_eq!(app.viewport_top, step);
    assert!(app.explicit_viewport_scroll);
}

#[test]
fn page_up_key_pages_without_moving_focus() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.viewport_top = app.max_viewport_top();
    app.explicit_viewport_scroll = true;
    app.selected = 2;
    app.selected_key = Some(app.visible_rows[2].key.clone());
    let initial_key = app.selected_key.clone();
    let initial_top = app.viewport_top;
    let step = app.body_inner_height.saturating_sub(1).max(1);

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::PageUp,
        crossterm::event::KeyModifiers::NONE,
    ));

    assert_eq!(app.selected, 2);
    assert_eq!(app.selected_key, initial_key);
    assert_eq!(app.viewport_top, initial_top.saturating_sub(step));
    assert!(app.explicit_viewport_scroll);
}

#[test]
fn focus_driven_scroll_clears_explicit_flag() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.scroll_viewport(5, true);
    assert!(app.explicit_viewport_scroll);
    app.scroll_viewport(1, false);
    assert!(!app.explicit_viewport_scroll);
}

#[test]
fn clamp_viewport_restores_focus_visibility_after_focus_movement() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.viewport_top = 10;
    app.selected = 0;
    app.explicit_viewport_scroll = false;
    app.clamp_viewport();
    let (ys, _) = app.header_y_offsets();
    let section_bottom = ys.get(1).copied().unwrap_or(ys.len());
    assert!(app.viewport_top < section_bottom);
}

#[test]
fn clamp_viewport_reattaches_tail_when_bottom_shrinks_under_viewport() {
    let mut app = tall_app();
    app.set_follow_tail(false);
    app.messages.push(message(1, "new unread"));
    app.viewport_top = 10;
    app.body_inner_height = 200;

    app.clamp_viewport();

    assert!(app.follow_tail);
    assert_eq!(app.tail_detach_baseline, None);
    assert_eq!(app.viewport_top, app.max_viewport_top());
}

#[test]
fn render_spec_review_paused_modal_without_panic() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SpecReviewPaused;
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

#[test]
fn render_plan_review_paused_modal_without_panic() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::PlanReviewPaused;
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

#[test]
fn render_stage_error_modal_without_panic() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SpecReviewRunning;
    app.state.agent_error = Some("model timeout".to_string());
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
}

fn leaf_only_tree() -> Vec<Node> {
    vec![node(
        "Brainstorm",
        NodeKind::Stage,
        NodeStatus::Running,
        Vec::new(),
        None,
        Some(7),
    )]
}

#[test]
fn running_leaf_row_renders_live_agent_message_and_history_in_main_panel() {
    let mut app = test_app(
        leaf_only_tree(),
        vec![run_record(7, RunStatus::Running)],
        vec![message(7, "earlier transcript line")],
    );
    app.live_summary_cached_text = "drafting plan | full body of work".to_string();
    app.state.current_phase = Phase::PlanningRunning;

    let lines = render_lines(&app, 12);

    assert!(
        !lines.iter().any(|l| l.contains("working...")),
        "running leaf must not emit the legacy 'working...' line"
    );
    assert!(
        lines.iter().any(|l| l.contains("Drafting plan")),
        "live-summary tail belongs in the main panel: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("Earlier transcript line")),
        "system summary history belongs in the main panel: {lines:#?}"
    );
}

#[test]
fn expanded_child_transcript_does_not_render_tail_or_placeholder_in_tree() {
    let mut app = test_app(
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            )],
            None,
            Some(1),
        )],
        vec![run_record(1, RunStatus::Running)],
        Vec::new(),
    );
    app.spinner_tick = 0;
    app.live_summary_cached_text = "drafting patch | details".to_string();

    let lines = render_lines(&app, 8);
    let spinner_rows = lines.iter().filter(|line| line.contains("⠋")).count();

    // The leaf row carries the live-agent-message tail. The container row's
    // tree-shape spinner is suppressed by `visible_live_summary_tail_runs`
    // because the same run already has a visible leaf tail.
    assert_eq!(
        spinner_rows, 1,
        "exactly one spinner row (the leaf live-agent-message): {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Drafting patch")),
        "live-summary tail belongs in the main panel: {lines:#?}"
    );
}

#[test]
fn container_placeholder_renders_in_main_panel_when_no_leaf_tail_visible() {
    let mut app = test_app(
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            )],
            None,
            Some(1),
        )],
        vec![run_record(1, RunStatus::Running)],
        Vec::new(),
    );
    app.spinner_tick = 0;

    // With a viewport too short to expose the child leaf's live-agent-message
    // tail, the container placeholder takes responsibility for representing
    // run progress.
    let lines = render_lines(&app, 2);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("⠋") && line.contains("running")),
        "container placeholder should render in main panel: {lines:#?}"
    );
}

#[test]
fn running_leaf_renders_phase_tail_in_main_panel() {
    let mut app = test_app(
        leaf_only_tree(),
        vec![run_record(7, RunStatus::Running)],
        Vec::new(),
    );
    app.state.current_phase = Phase::BrainstormRunning;

    let lines = render_lines(&app, 8);

    assert!(
        lines.iter().any(|l| l.contains("Brainstorming")),
        "leaf tail should render the active phase label: {lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("working...")));
}

#[test]
fn running_tail_renders_active_spinner_in_main_panel() {
    let mut run = run_record(7, RunStatus::Running);
    run.started_at = chrono::Utc::now() - chrono::Duration::seconds(11);
    let mut app = test_app(leaf_only_tree(), vec![run], Vec::new());
    app.agent_last_change = Some(Instant::now() - Duration::from_secs(11));
    app.spinner_tick = 1;

    let lines = render_lines(&app, 8);

    assert!(
        lines.iter().any(|line| line.contains("⠙")),
        "running spinner should render in main panel during active tool calls: {lines:#?}"
    );
}

#[test]
fn running_tail_renders_stalled_label_in_main_panel() {
    let mut run = run_record(7, RunStatus::Running);
    run.started_at = chrono::Utc::now() - chrono::Duration::seconds(601);
    let mut app = test_app(leaf_only_tree(), vec![run], Vec::new());
    app.agent_last_change = Some(Instant::now() - Duration::from_secs(601));
    app.spinner_tick = 1;

    let lines = render_lines(&app, 8);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("⠋") && line.contains("stalled")),
        "stalled tail should render in main panel: {lines:#?}"
    );
    // The stall freezes the spinner at frame 0, so the tick-1 glyph is absent.
    assert!(
        !lines.iter().any(|line| line.contains("⠙")),
        "stalled spinner must freeze at frame 0: {lines:#?}"
    );
    assert!(app.live_summary_spinner_visible_for_height(8));
}

#[test]
fn running_tail_renders_in_main_panel_when_active() {
    let mut app = test_app(
        leaf_only_tree(),
        vec![run_record(7, RunStatus::Running)],
        Vec::new(),
    );
    app.agent_last_change = Some(Instant::now() - Duration::from_secs(9));

    let lines = render_lines(&app, 8);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("⠋") || line.contains("⠙")),
        "running tail should render in main panel: {lines:#?}"
    );
}

#[test]
fn running_tail_renders_in_main_panel_for_recent_run() {
    let mut run = run_record(7, RunStatus::Running);
    run.started_at = chrono::Utc::now();
    let mut app = test_app(leaf_only_tree(), vec![run], Vec::new());
    app.current_run_id = Some(7);
    app.agent_last_change = Some(Instant::now() - Duration::from_secs(11));

    let lines = render_lines(&app, 8);

    assert!(
        lines
            .iter()
            .any(|line| line.contains("⠋") || line.contains("⠙")),
        "running tail should render in main panel: {lines:#?}"
    );
}

#[test]
fn completed_run_renders_history_without_running_tail_in_main_panel() {
    let app = test_app(
        leaf_only_tree(),
        vec![run_record(7, RunStatus::Done)],
        vec![message(7, "final summary")],
    );

    let lines = render_lines(&app, 8);

    assert!(
        lines.iter().any(|l| l.contains("Final summary")),
        "system summary belongs in the main panel transcript: {lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("working...")));
}

#[test]
fn container_row_running_tail_keeps_tree_shape_spinner() {
    // Container with visible children: the root row's body (if any) keeps
    // the legacy tree-shape spinner while children render their own
    // live-agent-message tails.
    let nodes = vec![node(
        "Root",
        NodeKind::Stage,
        NodeStatus::Running,
        vec![node(
            "Builder",
            NodeKind::Mode,
            NodeStatus::Running,
            Vec::new(),
            Some(1),
            None,
        )],
        None,
        // Root absorbs Builder's run via leaf_run_id so its body renders
        // the transcript inline.
        Some(1),
    )];
    let app = test_app(
        nodes,
        vec![run_record(1, RunStatus::Running)],
        vec![message(1, "shared transcript")],
    );

    let row = &app.visible_rows[0];
    let run = &app.state.agent_runs[0];
    let clock = super::super::clock::WallClock::new();
    let tail = app
        .running_tail_for_row(0, run, &clock, &BTreeSet::new())
        .expect("running container should produce a tail line");

    let text: String = tail
        .line
        .spans
        .iter()
        .map(|s| s.content.to_string())
        .collect();
    assert!(
        row.has_children,
        "container precondition: has visible children"
    );
    assert!(
        text.contains("running"),
        "container tail should keep the 'running' state label"
    );
    assert!(
        !text.contains("working..."),
        "container tail must not regress to the legacy cyan 'working...' line"
    );
}

fn render_full_frame(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h).map(|y| line_text(&buf, y, w)).collect()
}

fn full_frame_text(app: &mut App, w: u16, h: u16) -> String {
    render_full_frame(app, w, h).join("\n")
}

/// Returns just the split-panel rows of a full-frame render. The frame
/// layout is `[models]─[main]─[split]─[footer]`, so the split lives
/// between the second and third horizontal-rule rows.
fn split_panel_text(app: &mut App, w: u16, h: u16) -> String {
    let lines = render_full_frame(app, w, h);
    let rule = "─".repeat(w as usize);
    let rule_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| (line == &rule).then_some(idx))
        .collect();
    if rule_indices.len() < 2 {
        return String::new();
    }
    let start = rule_indices[rule_indices.len() - 2] + 1;
    let end = rule_indices[rule_indices.len() - 1];
    lines[start..end].join("\n")
}

fn render_full_frame_buf(app: &mut App, w: u16, h: u16) -> Buffer {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    terminal.backend().buffer().clone()
}

fn raw_line_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>()
}

fn expected_dialog_rect(terminal_width: u16, terminal_height: u16, content_h: usize) -> Rect {
    let max_w = terminal_width.saturating_sub(4).max(1);
    let dialog_w = max_w.min(80).max(max_w.min(40));
    let dialog_h = ((content_h + 5) as u16).min(terminal_height.saturating_sub(4));
    Rect::new(
        (terminal_width - dialog_w) / 2,
        (terminal_height - dialog_h) / 2,
        dialog_w,
        dialog_h,
    )
}

#[test]
fn palette_overlay_renders_buffer_and_ghost_completion() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    app.palette.buffer = "qu".to_string();
    app.palette.cursor = 2;

    let lines = render_full_frame(&mut app, 80, 24);
    let text = lines.join("\n");

    assert!(text.contains(":qu"));
    assert!(
        text.contains("quit"),
        "ghost completion should make the target command visible"
    );
}

#[test]
fn palette_overlay_empty_buffer_lists_available_commands() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    let lines = render_full_frame(&mut app, 80, 24);
    let text = lines.join("\n");
    // Every always-available command is listed in the empty browser, with
    // its help text. `quit` carries an `Esc` shortcut hint; palette-only
    // commands `cheap` and `yolo` advertise no shortcut.
    assert!(text.contains("quit"), "empty browser shows quit");
    assert!(text.contains("Exit the TUI"), "shows quit help");
    assert!(text.contains("Esc"), "shows quit shortcut");
    assert!(text.contains("cheap"), "empty browser shows cheap");
    assert!(text.contains("Toggle cheap mode"));
    assert!(text.contains("yolo"), "empty browser shows yolo");
    assert!(text.contains("Toggle YOLO mode"));
}

#[test]
fn palette_overlay_filters_by_typed_input() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    app.palette.buffer = "yol".to_string();
    app.palette.cursor = 3;
    let lines = render_full_frame(&mut app, 80, 24);
    let text = lines.join("\n");
    assert!(text.contains("yolo"), "filtered list keeps the match");
    assert!(
        !text.contains("Toggle cheap mode"),
        "non-matching commands drop out: {text}"
    );
}

#[test]
fn palette_overlay_palette_only_commands_show_no_shortcut_text() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    app.palette.buffer = "che".to_string();
    app.palette.cursor = 3;
    let lines = render_full_frame(&mut app, 80, 24);
    // The cheap row is the suggestion line; it must not append a shortcut
    // glyph because cheap has no real direct keybinding in the running app.
    let cheap_row = lines
        .iter()
        .find(|l| l.contains("Toggle cheap mode"))
        .expect("cheap suggestion present");
    // No leading colon or `:cheap` annotation; the shortcut column is empty.
    // We assert there is no trailing single-letter shortcut by scanning for
    // common direct-key glyphs the app uses elsewhere.
    for hint in ["Esc", "Tab", "Enter"] {
        let trimmed = cheap_row.trim_end();
        assert!(
            !trimmed.ends_with(hint),
            "cheap suggestion must not end with shortcut hint {hint}: {trimmed:?}"
        );
    }
}

#[test]
fn palette_overlay_clamp_preserves_input_row_on_short_terminal() {
    // Very short terminal: the overlay must still render the input row even
    // if the suggestion list and help row have to drop out entirely.
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    app.palette.buffer = "qu".to_string();
    app.palette.cursor = 2;
    // 4 rows total — top chrome and body floor consume most rows; overlay
    // collapses to the bottom 2 rows or fewer.
    let lines = render_full_frame(&mut app, 80, 4);
    let text = lines.join("\n");
    assert!(
        text.contains(":qu"),
        "input row must remain visible: {text}"
    );
}

#[test]
fn palette_overlay_does_not_exceed_width() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    let width: u16 = 32;
    let lines = render_full_frame(&mut app, width, 24);
    for (idx, l) in lines.iter().enumerate() {
        assert!(
            l.chars().count() as u16 <= width,
            "line {idx} exceeded width {width}: len={} got={l:?}",
            l.chars().count()
        );
    }
}

#[test]
fn palette_overlay_grows_beyond_two_rows_when_room() {
    // With a roomy terminal and an empty buffer, the overlay should grow
    // past the legacy 2-row shape and surface multiple suggestions.
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.palette.open();
    let lines = render_full_frame(&mut app, 80, 30);
    let text = lines.join("\n");
    // At least quit, cheap, and yolo all visible at once — that's 3+
    // suggestion rows in addition to the input row.
    assert!(text.contains("quit"));
    assert!(text.contains("cheap"));
    assert!(text.contains("yolo"));
}

#[test]
fn interactive_run_does_not_show_input_sheet_until_waiting() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![message(1, "waiting")],
    );
    app.current_run_id = Some(1);
    app.state.current_phase = Phase::BrainstormRunning;

    let lines = render_full_frame(&mut app, 80, 24);
    let text = lines.join("\n");

    assert!(
        !text.contains("type to agents..."),
        "interactive run should not show the input sheet before waiting for input: {text}"
    );
    assert!(
        !text.contains("Esc close  Tab complete  Enter run"),
        "palette instructions should stay hidden until ':' opens the palette: {text}"
    );
}

#[test]
fn interactive_run_input_sheet_content_uses_agent_placeholder() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![message(1, "waiting")],
    );
    app.current_run_id = Some(1);
    app.state.current_phase = Phase::BrainstormRunning;

    let lines = app
        .input_sheet_content(80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let text = lines.join("\n");

    assert!(
        text.contains("type to agents..."),
        "input sheet should use the agent-directed placeholder: {text}"
    );
    assert!(
        !text.contains("describe what you want to build"),
        "old placeholder should not render: {text}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("> ▌type to agents...")),
        "prompt and placeholder should render on the same line: {lines:#?}"
    );
}

#[test]
fn interactive_run_input_sheet_does_not_render_duplicate_separator_rule() {
    let mut run = run_record(7, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![message(7, "waiting")],
    );
    app.current_run_id = Some(7);
    app.state.current_phase = Phase::BrainstormRunning;
    app.input_mode = true;

    let lines = render_full_frame(&mut app, 80, 24);
    let full_rule = "─".repeat(80);
    let rule_rows = lines.iter().filter(|line| **line == full_rule).count();

    assert_eq!(
        rule_rows, 1,
        "only the app chrome bottom rule should separate body from input sheet: {lines:#?}"
    );
}

#[test]
fn split_run_renders_tree_above_transcript_on_tall_terminals() {
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Done)],
        vec![agent_text(1, "split transcript body")],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));

    let lines = render_full_frame(&mut app, 80, 90);
    let tree_y = lines
        .iter()
        .position(|line| line.contains("Root"))
        .expect("tree row should remain visible above the split");
    let split_y = lines
        .iter()
        .position(|line| line.contains("split transcript body"))
        .expect("run transcript should render in the split");

    assert!(
        tree_y < split_y,
        "tall split layout should render tree before split transcript: {lines:#?}"
    );
}

#[test]
fn split_run_uses_full_body_and_hides_tree_at_small_terminal_height() {
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Done)],
        vec![agent_text(1, "full body split transcript")],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));

    let text = full_frame_text(&mut app, 80, super::super::RESPONSIVE_HEIGHT_THRESHOLD);

    assert!(text.contains("full body split transcript"), "{text}");
    assert!(
        !text.contains("Root") && !text.contains("Task A") && !text.contains("Builder"),
        "tree rows should be hidden in full-body split mode: {text}"
    );
}

#[test]
fn split_run_uses_split_panel_visibility_for_transcripts() {
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Done)],
        vec![
            message(1, "hidden split summary"),
            agent_text(1, "visible noninteractive text"),
            agent_thought(1, "hidden thought text"),
        ],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));

    let text = full_frame_text(&mut app, 80, 90);

    assert!(!text.contains("hidden split summary"), "{text}");
    assert!(text.contains("visible noninteractive text"), "{text}");
    assert!(!text.contains("hidden thought text"), "{text}");
}

#[test]
fn interactive_run_split_renders_model_output_and_user_input() {
    let mut run = run_record(1, RunStatus::Done);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![
            kind_message(1, MessageKind::Started, "lifecycle started"),
            kind_message(1, MessageKind::Brief, "lifecycle brief"),
            user_input(1, "visible operator input"),
            agent_text(1, "visible model answer"),
            agent_thought(1, "hidden model thought"),
            message(1, "lifecycle summary"),
            kind_message(1, MessageKind::SummaryWarn, "lifecycle warning"),
            kind_message(1, MessageKind::End, "lifecycle end"),
        ],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));

    let split_text = split_panel_text(&mut app, 80, 90);

    assert!(split_text.contains("visible model answer"), "{split_text}");
    assert!(
        split_text.contains("visible operator input"),
        "{split_text}"
    );
    // Started + End messages now appear in both panels (split picks up the
    // same start/finish lifecycle markers as the main panel).
    assert!(split_text.contains("Lifecycle started"), "{split_text}");
    assert!(split_text.contains("Lifecycle end"), "{split_text}");
    // Brief / Summary / SummaryWarn remain main-panel-only and AgentThought
    // gates on the verbose-thinking toggle.
    assert!(!split_text.contains("lifecycle brief"), "{split_text}");
    assert!(!split_text.contains("hidden model thought"), "{split_text}");
    assert!(!split_text.contains("Lifecycle summary"), "{split_text}");
    assert!(!split_text.contains("lifecycle warning"), "{split_text}");

    app.state.show_thinking_texts = true;
    let split_text = split_panel_text(&mut app, 80, 90);
    assert!(split_text.contains("Hidden model thought"), "{split_text}");
}

#[test]
fn split_renders_one_message_per_finalized_acp_block_with_main_panel_clean() {
    // Acceptance: a multi-boundary ACP stream persists N+1 distinct
    // AgentText messages (see runner::tests_mod for the persistence proof);
    // each must render in the split as a separate message and stay out of
    // the main panel even with a final live remainder present.
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![
            agent_text(1, "alpha paragraph"),
            agent_text(1, "beta paragraph"),
            agent_text(1, "gamma overflow extra"),
            agent_text(1, "live remainder"),
        ],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));

    let split_text = split_panel_text(&mut app, 80, 90);
    for block in [
        "alpha paragraph",
        "beta paragraph",
        "gamma overflow extra",
        "live remainder",
    ] {
        assert!(
            split_text.contains(block),
            "split must render finalized block {block:?}: {split_text}"
        );
    }

    let main_panel_lines = render_lines(&app, 30);
    for block in [
        "alpha paragraph",
        "beta paragraph",
        "gamma overflow extra",
        "live remainder",
    ] {
        assert!(
            !main_panel_lines.iter().any(|line| line.contains(block)),
            "main panel must not render ACP block {block:?}: {main_panel_lines:#?}"
        );
    }
}

#[test]
fn interactive_run_split_height_uses_same_filter_as_rendering() {
    let mut run = run_record(1, RunStatus::Done);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![
            kind_message(1, MessageKind::Started, "started"),
            user_input(1, "operator input"),
            agent_text(1, "model answer"),
            agent_thought(1, "hidden model thought"),
            message(1, "hidden summary"),
            kind_message(1, MessageKind::End, "end"),
        ],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));
    app.body_inner_width = 80;

    let run = app
        .state
        .agent_runs
        .iter()
        .find(|run| run.id == 1)
        .expect("run");
    let local_offset = chrono::FixedOffset::east_opt(0).expect("zero offset");
    let expected = crate::app::chat_widget::message_lines(
        &[
            kind_message(1, MessageKind::Started, "started"),
            user_input(1, "operator input"),
            agent_text(1, "model answer"),
            kind_message(1, MessageKind::End, "end"),
        ],
        run,
        &local_offset,
        None,
        app.body_inner_width,
        0,
        true,
    )
    .len();

    assert_eq!(app.current_split_content_height(), expected);
}

#[test]
fn split_transcript_tail_line_renders_transcript_leaf_shape() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = false;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_text(1, "visible model answer")],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));
    app.body_inner_height = 30;
    app.body_inner_width = 80;
    app.selected = 0; // a container row; ensures tail shape is independent of selection
    app.live_summary_cached_text =
        "unique streaming tail | should appear as transcript-leaf body".to_string();

    let run = app
        .state
        .agent_runs
        .iter()
        .find(|run| run.id == 1)
        .expect("run");
    let tail = app
        .split_transcript_tail_line(run)
        .expect("split transcript tail should be available for an active run");

    let text: String = tail
        .spans
        .iter()
        .map(|span| span.content.to_string())
        .collect();
    // Container placeholders look like `  ⠋  running` (two leading spaces + spinner + label).
    // Transcript-leaf shape is `HH:MM:SS ⠋ <title>` — starts with a digit timestamp.
    assert!(
        text.chars().next().is_some_and(|c| c.is_ascii_digit()),
        "split tail must start with a transcript timestamp, got: {text:?}"
    );
    assert!(
        !text.contains("running") || text.contains("Unique streaming tail"),
        "split tail must not be the container placeholder `  ⠋  running`: {text:?}"
    );
}

#[test]
fn split_transcript_tail_line_includes_tail_for_interactive_runs() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_text(1, "visible model answer")],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));
    app.body_inner_height = 30;
    app.body_inner_width = 80;
    app.live_summary_cached_text = "interactive streaming tail".to_string();

    let run_ref = app
        .state
        .agent_runs
        .iter()
        .find(|run| run.id == 1)
        .expect("run");
    // Without `current_run_id` set, `interactive_run_waiting_for_input` is false,
    // so the new split tail should mirror the main panel and show a tail.
    assert!(
        app.split_transcript_tail_line(run_ref).is_some(),
        "interactive runs that are not waiting for input should show a transcript tail in split"
    );

    // Bookkeeping must include the tail; height is strictly greater than a
    // tail-less render of the same messages. Avoid timestamp races by comparing
    // counts (transcript-leaf rows are 1 line at width 80 with a short title).
    let local_offset = chrono::FixedOffset::east_opt(0).expect("zero offset");
    let no_tail = crate::app::chat_widget::message_lines(
        &[agent_text(1, "visible model answer")],
        run_ref,
        &local_offset,
        None,
        app.body_inner_width,
        app.spinner_tick,
        true,
    )
    .len();

    assert!(
        app.current_split_content_height() > no_tail,
        "split height should include the running tail line"
    );
}

#[test]
fn split_run_renders_separator_between_tree_and_transcript() {
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Done)],
        vec![agent_text(1, "split transcript body")],
    );
    app.split_target = Some(super::super::split::SplitTarget::Run(1));

    let lines = render_full_frame(&mut app, 80, 90);
    let rule = "─".repeat(80);
    let tree_y = lines
        .iter()
        .position(|line| line.contains("Root"))
        .expect("tree row should render above split");
    let split_y = lines
        .iter()
        .position(|line| line.contains("split transcript body"))
        .expect("split transcript should render below separator");
    let separator_y = lines
        .iter()
        .enumerate()
        .find_map(|(idx, line)| (idx > tree_y && idx < split_y && line == &rule).then_some(idx))
        .expect("separator row should render between main panel and split");

    assert!(tree_y < separator_y && separator_y < split_y);
}

#[test]
fn interactive_split_owned_input_still_renders_footer_sheet() {
    let mut run = run_record(7, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_text(7, "waiting for operator")],
    );
    app.current_run_id = Some(7);
    app.state.current_phase = crate::state::Phase::BrainstormRunning;
    app.split_target = Some(super::super::split::SplitTarget::Run(7));
    app.input_mode = true;
    crate::runner::request_run_label_interactive_input_for_test("[Run 7]");

    let text = full_frame_text(&mut app, 80, 24);

    assert!(text.contains("type to agents..."), "{text}");
    assert!(text.contains("Esc close"), "{text}");
    assert!(text.contains("Enter submit"), "{text}");
}

#[test]
fn idea_split_renders_captured_text_from_target_not_selected_row() {
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Done)],
        Vec::new(),
    );
    app.state.idea_text = Some("captured idea belongs in split".to_string());
    app.state.current_phase = Phase::SpecReviewPaused;
    app.selected = 2;
    app.split_target = Some(super::super::split::SplitTarget::Idea);

    let text = full_frame_text(&mut app, 80, 90);

    assert!(text.contains("captured idea belongs in split"), "{text}");
}

#[test]
fn idea_input_split_suppresses_competing_bottom_sheet() {
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Running)],
        Vec::new(),
    );
    app.state.current_phase = Phase::IdeaInput;
    app.selected = 2;
    app.split_target = Some(super::super::split::SplitTarget::Idea);
    app.input_mode = true;
    app.input_buffer = "draft the split idea".to_string();
    app.input_cursor = app.input_buffer.chars().count();

    let text = full_frame_text(&mut app, 80, 90);

    assert!(text.contains("draft the split idea"), "{text}");
    assert!(
        !text.contains("> draft the split idea"),
        "Idea split input must not also render the footer input sheet: {text}"
    );
}

#[test]
fn interactive_run_main_panel_excludes_acp_output_but_shows_running_tail() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let mut app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_text(1, "Do you approve?")],
    );
    app.current_run_id = Some(1);
    app.run_launched = true;
    app.state.current_phase = Phase::BrainstormRunning;

    let lines = render_full_frame(&mut app, 80, 24);
    let text = lines.join("\n");

    assert!(
        !text.contains("Do you approve?"),
        "ACP output must not render in the main panel: {lines:#?}"
    );
    assert!(
        text.contains("⠋") || text.contains("⠙"),
        "running tail spinner should render in the main panel: {lines:#?}"
    );
}

fn impl_round_2_running_app() -> App {
    let nodes = vec![node(
        "Implementation",
        NodeKind::Stage,
        NodeStatus::Running,
        vec![node(
            "Builder",
            NodeKind::Mode,
            NodeStatus::Running,
            Vec::new(),
            Some(42),
            None,
        )],
        None,
        None,
    )];
    let mut app = test_app(
        nodes,
        vec![run_record(42, RunStatus::Running)],
        // No historical messages — the test only needs the live running
        // transcript leaf so the assertion surface is deterministic.
        Vec::new(),
    );
    app.state.current_phase = Phase::ImplementationRound(2);
    app.live_summary_cached_text =
        "wiring full-screen tests | adding render-level snapshot coverage".to_string();
    app.current_run_id = Some(42);
    app
}

/// Render at a width that fits the full default keymap (so `q quit`
/// appears verbatim on the last line, anchoring the assertion).
const FULL_FRAME_WIDTH: u16 = 200;

/// Replace wall-clock timestamps with stable placeholders so that
/// full-frame vector assertions are deterministic regardless of when or
/// where the test runs.
fn normalize_frame(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| {
            // Running transcript leaf: "HH:MM:SS ⠋ ..."
            if line.len() >= 9
                && line.as_bytes()[0].is_ascii_digit()
                && line.as_bytes()[1].is_ascii_digit()
                && line.as_bytes()[2] == b':'
                && line.as_bytes()[3].is_ascii_digit()
                && line.as_bytes()[4].is_ascii_digit()
                && line.as_bytes()[5] == b':'
                && line.as_bytes()[6].is_ascii_digit()
                && line.as_bytes()[7].is_ascii_digit()
                && line.as_bytes()[8] == b' '
            {
                format!("XX:XX:XX{}", &line[8..])
            } else {
                line
            }
        })
        .collect()
}

#[test]
fn full_screen_idea_input_renders_top_rule_body_bottom_rule_and_keymap() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::IdeaInput;

    let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
    let rule = "─".repeat(200);
    // Default `FocusCaps` has all capabilities disabled, so per the
    // glyph-only-disabled rule `Space` and `Enter` render without their
    // action labels and the omitted width is reclaimed by the fill.
    let keymap = format!(
        "↑↓ move · Space · PgUp/PgDn page  ·  Enter · : palette  ·  {}Esc quit",
        " ".repeat(133)
    );

    assert_eq!(
        lines,
        vec![
            "codexize─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────Idea Input · awaiting input",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &rule,
            &keymap,
        ]
    );
}

#[test]
fn full_screen_brainstorm_running_renders_running_state_in_top_rule() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::BrainstormRunning;

    let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
    let rule = "─".repeat(200);
    // Default `FocusCaps` has all capabilities disabled, so per the
    // glyph-only-disabled rule `Space` and `Enter` render without their
    // action labels and the omitted width is reclaimed by the fill.
    let keymap = format!(
        "↑↓ move · Space · PgUp/PgDn page  ·  Enter · : palette  ·  {}Esc quit",
        " ".repeat(133)
    );

    assert_eq!(
        lines,
        vec![
            "codexize─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────Brainstorming · running",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &rule,
            &keymap,
        ]
    );
}

#[test]
fn full_screen_render_does_not_reserve_or_draw_chrome_live_status_row() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::IdeaInput;

    let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
    let rule = "─".repeat(FULL_FRAME_WIDTH as usize);

    assert_eq!(
        app.body_inner_height, 21,
        "body should receive the row formerly reserved for chrome live status"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("XX:XX:XX")),
        "normal chrome should not draw a standalone live-status row: {lines:#?}"
    );
    assert_eq!(lines[22], rule);
}

/// Pads a modal content string to fit the dialog inner width and wraps it
/// with the bordered block's vertical bars.
fn modal_row(inner_width: u16, text: &str) -> String {
    let inner_width = inner_width as usize;
    let pad = inner_width.saturating_sub(text.chars().count());
    format!("│{}{}│", text, " ".repeat(pad))
}

#[test]
fn spec_review_modal_is_centered_with_content_driven_height() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SpecReviewPaused;

    let width = 100;
    let height = 30;
    let buf = render_full_frame_buf(&mut app, width, height);
    let dialog = expected_dialog_rect(width, height, 1);
    let inner_width = dialog.width.saturating_sub(2);

    assert_eq!(dialog, Rect::new(10, 12, 80, 6));
    assert!(raw_line_text(&buf, 0, width).contains("Spec Review · paused"));
    let top_line = raw_line_text(&buf, dialog.y, width);
    assert!(top_line.contains("┌"));
    assert!(top_line.contains("┐"));
    assert!(top_line.contains("Spec review complete"));
    assert_eq!(
        raw_line_text(&buf, dialog.y + 1, width),
        format!(
            "{}{}{}",
            " ".repeat(dialog.x as usize),
            modal_row(inner_width, "Spec review complete"),
            " ".repeat((width - dialog.x - dialog.width) as usize)
        )
    );
    assert!(
        raw_line_text(&buf, dialog.y + dialog.height - 2, width).contains("Enter continue"),
        "keymap should occupy the last inner row"
    );
    assert_eq!(
        raw_line_text(&buf, dialog.y + dialog.height - 1, width),
        format!(
            "{}└{}┘{}",
            " ".repeat(dialog.x as usize),
            "─".repeat(inner_width as usize),
            " ".repeat((width - dialog.x - dialog.width) as usize)
        )
    );
}

#[test]
fn modal_dialog_uses_black_background_and_light_text() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SpecReviewPaused;

    let width = 100;
    let height = 30;
    let buf = render_full_frame_buf(&mut app, width, height);
    let dialog = expected_dialog_rect(width, height, 1);

    for y in dialog.y..dialog.y + dialog.height {
        for x in dialog.x..dialog.x + dialog.width {
            let cell = &buf[(x, y)];
            assert_eq!(
                cell.bg,
                Color::Black,
                "dialog cell ({x},{y}) should have black background"
            );
            if cell.symbol().trim().is_empty() {
                continue;
            }
            assert!(
                !matches!(cell.fg, Color::Black | Color::DarkGray),
                "visible dialog text at ({x},{y}) should be light, got {:?} for {:?}",
                cell.fg,
                cell.symbol()
            );
        }
    }
}

#[test]
fn stage_error_modal_wraps_long_text_inside_centered_dialog() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SpecReviewRunning;
    app.state.agent_error = Some(
        "model timeout while fetching a response from the remote reviewer after multiple retries"
            .to_string(),
    );

    let width = 100;
    let height = 30;
    let buf = render_full_frame_buf(&mut app, width, height);
    let dialog = expected_dialog_rect(width, height, 4);

    assert_eq!(dialog, Rect::new(10, 10, 80, 9));
    assert!(raw_line_text(&buf, dialog.y + 1, width).contains("Spec review failed"));
    assert_eq!(
        raw_line_text(&buf, dialog.y + 3, width),
        format!(
            "{}{}{}",
            " ".repeat(dialog.x as usize),
            modal_row(
                dialog.width.saturating_sub(2),
                "model timeout while fetching a response from the remote reviewer after"
            ),
            " ".repeat((width - dialog.x - dialog.width) as usize)
        )
    );
    assert_eq!(
        raw_line_text(&buf, dialog.y + 4, width),
        format!(
            "{}{}{}",
            " ".repeat(dialog.x as usize),
            modal_row(dialog.width.saturating_sub(2), "multiple retries"),
            " ".repeat((width - dialog.x - dialog.width) as usize)
        )
    );
    assert!(
        raw_line_text(&buf, dialog.y + dialog.height - 2, width).contains("r retry"),
        "keymap should remain visible after wrapped error content"
    );
}

#[test]
fn skip_to_impl_modal_wraps_rationale_and_keeps_label_on_its_own_line() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SkipToImplPending;
    app.state.skip_to_impl_rationale = Some(
            "This change only touches the centered dialog rendering path and preserves the existing footer controls."
                .to_string(),
        );

    let width = 100;
    let height = 30;
    let buf = render_full_frame_buf(&mut app, width, height);
    let dialog = expected_dialog_rect(width, height, 5);

    assert_eq!(dialog, Rect::new(10, 10, 80, 10));
    assert!(raw_line_text(&buf, dialog.y + 1, width).contains("The brainstorm agent proposes"));
    assert_eq!(
        raw_line_text(&buf, dialog.y + 3, width),
        format!(
            "{}{}{}",
            " ".repeat(dialog.x as usize),
            modal_row(dialog.width.saturating_sub(2), "Rationale: "),
            " ".repeat((width - dialog.x - dialog.width) as usize)
        )
    );
    assert_eq!(
        raw_line_text(&buf, dialog.y + 4, width)
            .trim()
            .trim_matches('│')
            .trim_end(),
        "This change only touches the centered dialog rendering path and preserves the"
    );
    assert_eq!(
        raw_line_text(&buf, dialog.y + 5, width)
            .trim()
            .trim_matches('│')
            .trim_end(),
        "existing footer controls."
    );
}

#[test]
fn spec_review_modal_clamps_width_on_narrow_terminals() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::SpecReviewPaused;

    let width = 30;
    let height = 20;
    let buf = render_full_frame_buf(&mut app, width, height);
    let dialog = expected_dialog_rect(width, height, 1);

    assert_eq!(dialog, Rect::new(2, 7, 26, 6));
    let top_line = raw_line_text(&buf, dialog.y, width);
    assert!(top_line.contains("┌"));
    assert!(top_line.contains("┐"));
    assert!(top_line.contains("Spec review complete"));
}

#[test]
fn full_screen_implementation_round_2_with_active_live_summary() {
    let mut app = impl_round_2_running_app();

    let lines = normalize_frame(render_full_frame(&mut app, FULL_FRAME_WIDTH, 24));
    let rule = "─".repeat(200);
    // The Builder row is expandable, so `Space expand` keeps its label;
    // `Enter input` is disabled outside the Idea row and renders glyph-only.
    let keymap = format!(
        "↑↓ move · Space expand · PgUp/PgDn page  ·  Enter · : palette  ·  {}Esc quit",
        " ".repeat(126)
    );

    // The main panel restores live-summary tails for running runs. Historical
    // ACP output remains in the split, but the leaf live-agent-message line
    // belongs to the main-panel transcript surface.
    assert_eq!(
        lines,
        vec![
            "codexize─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────[Run 42] · wiring full-screen tests",
            " ▌ ▾ Implementation · running",
            "  └─ ▾ Builder · running",
            "XX:XX:XX ⠋ Wiring full-screen tests",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            "",
            &rule,
            &keymap,
        ]
    );
}

fn footer_line_count(lines: &[String]) -> usize {
    // Footer rows are non-empty rows below the bottom rule. The bottom rule
    // is the row immediately above either the status line (if present) or
    // the keymap. We count the trailing run of non-empty rows starting from
    // the keymap row upward.
    lines.iter().rev().take_while(|l| !l.is_empty()).count()
}

#[test]
fn pushing_status_message_adds_one_extra_footer_line_then_ttl_hides_it() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::IdeaInput;

    let baseline = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
    let baseline_footer = footer_line_count(&baseline);

    // Push with a 5-second TTL so it survives the immediate `tick` inside draw().
    app.push_status(
        "transient status".to_string(),
        super::super::status_line::Severity::Warn,
        Duration::from_secs(5),
    );

    let with_status = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
    assert!(
        with_status.iter().any(|l| l.contains("transient status")),
        "status message visible in frame: {with_status:#?}"
    );
    assert_eq!(
        footer_line_count(&with_status),
        baseline_footer + 1,
        "status push adds exactly one footer line"
    );

    // TTL=0 forces immediate expiry on the next render's tick.
    app.push_status(
        "about to expire".to_string(),
        super::super::status_line::Severity::Warn,
        Duration::from_millis(0),
    );

    let after_expiry = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
    assert!(
        !after_expiry.iter().any(|l| l.contains("about to expire")),
        "expired status hidden: {after_expiry:#?}"
    );
    assert_eq!(
        footer_line_count(&after_expiry),
        baseline_footer,
        "footer shrinks back after TTL expiry"
    );
}

#[test]
fn frame_status_line_severity_priority_info_then_error_wins() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::IdeaInput;

    app.push_status(
        "info first".to_string(),
        super::super::status_line::Severity::Info,
        Duration::from_secs(10),
    );
    app.push_status(
        "error wins".to_string(),
        super::super::status_line::Severity::Error,
        Duration::from_secs(10),
    );

    let lines = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
    assert!(lines.iter().any(|l| l.contains("error wins")));
    assert!(!lines.iter().any(|l| l.contains("info first")));
}

#[test]
fn frame_status_line_severity_priority_error_then_info_keeps_error() {
    let mut app = test_app(Vec::new(), Vec::new(), Vec::new());
    app.state.current_phase = Phase::IdeaInput;

    app.push_status(
        "error stays".to_string(),
        super::super::status_line::Severity::Error,
        Duration::from_secs(10),
    );
    app.push_status(
        "info ignored".to_string(),
        super::super::status_line::Severity::Info,
        Duration::from_secs(10),
    );

    let lines = render_full_frame(&mut app, FULL_FRAME_WIDTH, 24);
    assert!(lines.iter().any(|l| l.contains("error stays")));
    assert!(!lines.iter().any(|l| l.contains("info ignored")));
}

#[test]
fn push_status_routes_through_status_line_with_severity_priority() {
    let app = test_app(Vec::new(), Vec::new(), Vec::new());

    app.push_status(
        "info-msg".to_string(),
        super::super::status_line::Severity::Warn,
        Duration::from_secs(5),
    );
    let rendered = app
        .status_line
        .borrow()
        .render()
        .expect("status line should hold the warn message");
    assert_eq!(rendered.to_string(), "info-msg");

    // Lower severity must not silently overwrite a higher-severity message.
    app.push_status(
        "later-info".to_string(),
        super::super::status_line::Severity::Info,
        Duration::from_secs(5),
    );
    let still = app.status_line.borrow().render().unwrap();
    assert_eq!(still.to_string(), "info-msg");
}

/// Expanded penultimate sibling with visible children must render └─ for that
/// sibling (since it's the last at its depth), not ├─.
#[test]
fn expanded_penultimate_sibling_renders_last_child_connector() {
    // Tree: Root (running) → Task A (first child), Task B (second/last child)
    // When Task A is expanded and has children, Task B should render └─.
    let nodes = vec![node(
        "Root",
        NodeKind::Stage,
        NodeStatus::Running,
        vec![
            node(
                "Task A",
                NodeKind::Task,
                NodeStatus::Running,
                vec![node(
                    "Builder A",
                    NodeKind::Mode,
                    NodeStatus::Running,
                    Vec::new(),
                    Some(1),
                    None,
                )],
                None,
                None,
            ),
            node(
                "Task B",
                NodeKind::Task,
                NodeStatus::Pending,
                Vec::new(),
                None,
                None,
            ),
        ],
        None,
        None,
    )];
    let app = test_app(nodes, vec![run_record(1, RunStatus::Running)], Vec::new());

    let lines = render_lines(&app, 10);

    // Task A has a child below it, but Task B follows Task A at the same depth,
    // so Task A should render ├─ (not last sibling).
    assert!(
        lines
            .iter()
            .any(|l| l.contains("├─") && l.contains("Task A")),
        "Task A should render ├─ (not last at its depth)"
    );
    // Task B is the last child at depth 1, so it should render └─.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("└─") && l.contains("Task B")),
        "Task B should render └─ (last at its depth)"
    );
}

fn render_pipeline_buf(app: &App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    PipelineWidget { app }.render(area, &mut buf);
    buf
}

fn row_text_count(lines: &[String], needle: &str) -> usize {
    lines.iter().filter(|line| line.contains(needle)).count()
}

fn flat_stage_app(labels: &[(&str, NodeStatus)]) -> App {
    test_app(
        labels
            .iter()
            .map(|(label, status)| node(label, NodeKind::Stage, *status, Vec::new(), None, None))
            .collect(),
        Vec::new(),
        Vec::new(),
    )
}

#[test]
fn loop_header_uses_standard_status_format() {
    let app = test_app(
        vec![node(
            "Loop",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Task 1",
                NodeKind::Task,
                NodeStatus::Pending,
                Vec::new(),
                None,
                None,
            )],
            None,
            None,
        )],
        Vec::new(),
        Vec::new(),
    );
    let node = app.node_for_row(0).expect("loop row");

    let expanded = line_to_string(&app.node_header(0, true, node));
    let collapsed = line_to_string(&app.node_header(0, false, node));

    assert!(
        expanded.contains("▾ Loop · running"),
        "expanded loop header should keep standard format: {expanded:?}"
    );
    assert!(
        collapsed.contains("▸ Loop · running"),
        "collapsed loop header should keep standard format: {collapsed:?}"
    );
}

#[test]
fn non_background_headers_keep_status_format() {
    let app = flat_stage_app(&[("Planning", NodeStatus::Running)]);
    let node = app.node_for_row(0).expect("planning row");

    let header = line_to_string(&app.node_header(0, true, node));

    assert!(
        header.contains("▾ Planning · running"),
        "ordinary headers should keep marker, label, and status: {header:?}"
    );
}

#[test]
fn sticky_running_stage_header_pins_above_scrolled_viewport() {
    let mut app = test_app(
        vec![node(
            "Running Stage",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![
                node(
                    "Task A",
                    NodeKind::Task,
                    NodeStatus::Done,
                    Vec::new(),
                    None,
                    None,
                ),
                node(
                    "Task B",
                    NodeKind::Task,
                    NodeStatus::Pending,
                    Vec::new(),
                    None,
                    None,
                ),
            ],
            None,
            None,
        )],
        Vec::new(),
        Vec::new(),
    );
    app.viewport_top = 2;

    let buf = render_pipeline_buf(&app, 80, 2);

    assert!(
        line_text(&buf, 0, 80).contains("Running Stage"),
        "running stage header should pin to row 0"
    );
    let pinned_style = buf[(0, 0)].style();
    assert!(
        !pinned_style.add_modifier.contains(Modifier::UNDERLINED),
        "pinned running stage header should not be underlined"
    );
    // The status color block lives at col 0 and should carry the running bg.
    let block_style = buf[(0, 0)].style();
    assert_eq!(
        block_style.bg,
        Some(Color::Cyan),
        "running color block should carry the status highlight bg"
    );
    assert!(
        line_text(&buf, 1, 80).contains("Task B"),
        "normal scrolled content should start below the pinned row"
    );
}

#[test]
fn sticky_running_stage_header_deactivates_while_naturally_visible() {
    let mut app = test_app(
        vec![node(
            "Running Stage",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Task A",
                NodeKind::Task,
                NodeStatus::Done,
                Vec::new(),
                None,
                None,
            )],
            None,
            None,
        )],
        Vec::new(),
        Vec::new(),
    );
    app.viewport_top = 0;

    let lines = render_lines(&app, 4);

    assert_eq!(
        row_text_count(&lines, "Running Stage"),
        1,
        "naturally visible running stage must not be duplicated"
    );
    assert!(
        lines[1].contains("Task A"),
        "when unpinned, content should still begin at the natural next row"
    );
}

#[test]
fn focus_scroll_uses_effective_height_with_sticky_header() {
    let mut app = flat_stage_app(&[
        ("Running Stage", NodeStatus::Running),
        ("One", NodeStatus::Done),
        ("Two", NodeStatus::Done),
        ("Three", NodeStatus::Done),
        ("Four", NodeStatus::Done),
        ("Focused", NodeStatus::Pending),
    ]);
    app.body_inner_height = 5;
    app.viewport_top = 1;
    app.selected = 5;
    app.selected_key = Some(app.visible_rows[5].key.clone());
    app.set_follow_tail(false);
    app.explicit_viewport_scroll = false;

    app.clamp_viewport();

    assert_eq!(
        app.viewport_top, 2,
        "row 0 is pinned, so the selected row needs a four-line content viewport"
    );
    let lines = render_lines(&app, app.body_inner_height as u16);
    assert!(
        lines[0].contains("Running Stage"),
        "running stage should remain pinned"
    );
    assert!(
        lines[4].contains("Focused"),
        "focused row should be visible below the pinned header"
    );
}

#[test]
fn tail_follow_uses_effective_height_with_sticky_header() {
    let mut app = flat_stage_app(&[
        ("Running Stage", NodeStatus::Running),
        ("One", NodeStatus::Done),
        ("Two", NodeStatus::Done),
        ("Three", NodeStatus::Done),
        ("Four", NodeStatus::Done),
        ("Tail", NodeStatus::Pending),
    ]);
    app.body_inner_height = 5;

    app.clamp_viewport();

    assert_eq!(
        app.viewport_top, 2,
        "tail-follow should scroll to the bottom of the reduced content viewport"
    );
    assert_eq!(app.viewport_top, app.max_viewport_top());
    let lines = render_lines(&app, app.body_inner_height as u16);
    assert!(lines[0].contains("Running Stage"));
    assert!(lines[4].contains("Tail"));
}

/// Depth-0 row layout: `[color block][focus] > Title · status`.
/// - col 0: status color block
/// - col 1: focus glyph `▌` or blank (no bg)
/// - col 2: gap (no bg)
/// - col 3: chevron `▾`/`▸` (no bg)
/// - col 4: gap (no bg)
/// - col 5+: title (no bg)
fn assert_depth_0_color_block(status: NodeStatus, expected_bg: Color, focused: bool) {
    let mut app = test_app(
        vec![
            node("Stage", NodeKind::Stage, status, Vec::new(), None, None),
            node(
                "Other",
                NodeKind::Stage,
                NodeStatus::Pending,
                Vec::new(),
                None,
                None,
            ),
        ],
        Vec::new(),
        Vec::new(),
    );
    if !focused {
        app.selected = 1;
        app.selected_key = Some(app.visible_rows[1].key.clone());
    }

    let buf = render_pipeline_buf(&app, 80, 5);
    // No underline anywhere on depth-0 rows.
    for col in 0u16..80 {
        assert!(
            !buf[(col, 0)]
                .style()
                .add_modifier
                .contains(Modifier::UNDERLINED),
            "no cell on a depth-0 row should be underlined; col={col}"
        );
    }
    // Color block (col 0) carries the status bg.
    assert_eq!(
        buf[(0, 0)].style().bg,
        Some(expected_bg),
        "color block at col 0 should carry the status highlight bg"
    );
    // Focus glyph (col 1) never carries the status bg.
    assert!(
        buf[(1, 0)].style().bg != Some(expected_bg),
        "focus glyph cell must not carry the status highlight bg"
    );
    // Chevron (col 3) and label (cols 5..=9 for "Stage") do NOT carry the bg.
    for col in [2u16, 3, 4, 5, 6, 7, 8, 9] {
        assert!(
            buf[(col, 0)].style().bg != Some(expected_bg),
            "cell at col {col} should not carry the status highlight bg; \
                 got {:?}",
            buf[(col, 0)].style().bg
        );
    }
}

#[test]
fn depth_0_running_color_block() {
    assert_depth_0_color_block(NodeStatus::Running, Color::Cyan, true);
    assert_depth_0_color_block(NodeStatus::Running, Color::Cyan, false);
}

#[test]
fn depth_0_done_color_block() {
    assert_depth_0_color_block(NodeStatus::Done, Color::Green, true);
    assert_depth_0_color_block(NodeStatus::Done, Color::Green, false);
}

#[test]
fn depth_0_failed_color_block() {
    assert_depth_0_color_block(NodeStatus::Failed, Color::Red, true);
    assert_depth_0_color_block(NodeStatus::Failed, Color::Red, false);
}

#[test]
fn depth_0_failed_unverified_color_block() {
    assert_depth_0_color_block(NodeStatus::FailedUnverified, Color::LightYellow, true);
    assert_depth_0_color_block(NodeStatus::FailedUnverified, Color::LightYellow, false);
}

#[test]
fn depth_0_pending_row_has_no_highlight_or_underline() {
    let app = test_app(
        vec![node(
            "Stage",
            NodeKind::Stage,
            NodeStatus::Pending,
            Vec::new(),
            None,
            None,
        )],
        Vec::new(),
        Vec::new(),
    );

    let buf = render_pipeline_buf(&app, 80, 5);
    for col in 0u16..20 {
        let style = buf[(col, 0)].style();
        assert!(
            !matches!(
                style.bg,
                Some(Color::Cyan)
                    | Some(Color::Green)
                    | Some(Color::Red)
                    | Some(Color::LightYellow)
            ),
            "Pending row should not carry a status highlight bg at col {col}; got {:?}",
            style.bg
        );
        assert!(
            !style.add_modifier.contains(Modifier::UNDERLINED),
            "Pending row should not be underlined at col {col}"
        );
    }
}

#[test]
fn depth_1_running_row_has_no_background_highlight() {
    // Background highlights are only for depth-0 rows.
    let app = test_app(
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Child",
                NodeKind::Task,
                NodeStatus::Running,
                Vec::new(),
                None,
                None,
            )],
            None,
            None,
        )],
        Vec::new(),
        Vec::new(),
    );

    let lines = render_lines(&app, 5);
    let child_line_idx = lines
        .iter()
        .position(|l| l.contains("Child"))
        .expect("Child row present");

    let buf = render_pipeline_buf(&app, 80, 5);
    let bg = buf[(0, child_line_idx as u16)].style().bg;
    // Depth-1 rows should not have the stage background highlight.
    assert!(
        bg != Some(Color::Blue),
        "Depth-1 Running row should not have blue background"
    );
}

/// Builds a Root → Child fixture and renders a buffer wide enough that the
/// child row's spans (focus glyph, indent, marker, label, separator, status
/// label) all land within columns 0..=20. Returns the buffer and the y-row
/// of the child line.
fn render_depth_1_child(child_status: NodeStatus) -> (Buffer, u16) {
    let app = test_app(
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Done,
            vec![node(
                "Child",
                NodeKind::Task,
                child_status,
                Vec::new(),
                None,
                None,
            )],
            None,
            None,
        )],
        Vec::new(),
        Vec::new(),
    );
    let lines = render_lines(&app, 5);
    let child_line_idx = lines
        .iter()
        .position(|l| l.contains("Child"))
        .expect("Child row should be present in rendered lines") as u16;
    let buf = render_pipeline_buf(&app, 80, 5);
    (buf, child_line_idx)
}

fn assert_depth_1_no_underline(status: NodeStatus) {
    let (buf, row) = render_depth_1_child(status);
    for col in 0u16..20 {
        let style = buf[(col, row)].style();
        assert!(
            !style.add_modifier.contains(Modifier::UNDERLINED),
            "depth-1 row should not be underlined at col {col}; style={style:?}",
        );
    }
}

#[test]
fn depth_1_rows_are_never_underlined() {
    for status in [
        NodeStatus::Running,
        NodeStatus::Done,
        NodeStatus::Failed,
        NodeStatus::FailedUnverified,
        NodeStatus::Pending,
    ] {
        assert_depth_1_no_underline(status);
    }
}

// ---------------------------------------------------------------------------
// Split-view tree-body cleanup regression tests
// ---------------------------------------------------------------------------

#[test]
fn expanded_agent_row_renders_main_panel_transcript_and_excludes_acp_output() {
    let app = test_app(
        nested_transcript_tree(),
        vec![run_record(1, RunStatus::Running)],
        vec![
            message(1, "summary line"),
            agent_text(1, "raw agent text"),
            user_input(1, "user says hello"),
        ],
    );
    let lines = render_lines(&app, 12);
    assert!(
        lines.iter().any(|l| l.contains("Summary line")),
        "summary message belongs in the main panel: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("raw agent text")),
        "ACP output must stay in the split panel: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("user says hello")),
        "user input echo belongs in the main panel: {lines:#?}"
    );
}

#[test]
fn expanded_agent_row_does_not_render_thinking_text() {
    let mut run = run_record(1, RunStatus::Running);
    run.modes.interactive = true;
    let app = test_app(
        nested_transcript_tree(),
        vec![run],
        vec![agent_thought(1, "deep reasoning chain")],
    );
    let lines = render_lines(&app, 8);
    assert!(
        !lines.iter().any(|l| l.contains("deep reasoning chain")),
        "thinking text must not render in tree body"
    );
}

#[test]
fn expanded_agent_row_renders_live_summary_tail_in_main_panel() {
    let mut app = test_app(
        leaf_only_tree(),
        vec![run_record(7, RunStatus::Running)],
        Vec::new(),
    );
    app.live_summary_cached_text = "live summary tail text".to_string();
    app.state.current_phase = Phase::BrainstormRunning;

    let lines = render_lines(&app, 8);
    assert!(
        lines.iter().any(|l| l.contains("Live summary tail text")),
        "live summary tail belongs in the main panel: {lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("⠋") || l.contains("⠙")),
        "spinner should render in main panel: {lines:#?}"
    );
}

#[test]
fn expanded_agent_row_does_not_render_running_placeholder() {
    let app = test_app(
        vec![node(
            "Root",
            NodeKind::Stage,
            NodeStatus::Running,
            vec![node(
                "Builder",
                NodeKind::Mode,
                NodeStatus::Running,
                Vec::new(),
                Some(1),
                None,
            )],
            None,
            Some(1),
        )],
        vec![run_record(1, RunStatus::Running)],
        Vec::new(),
    );
    let lines = render_lines(&app, 8);
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("running") && l.contains("⠋")),
        "container placeholder must not render in tree body"
    );
}

#[test]
fn expanded_idea_row_does_not_render_captured_text() {
    let mut state = SessionState::new("idea-test".to_string());
    state.idea_text = Some("my brilliant idea".to_string());
    let nodes = vec![node(
        "Idea",
        NodeKind::Stage,
        NodeStatus::Done,
        Vec::new(),
        None,
        None,
    )];
    let mut app = test_app(nodes, Vec::new(), Vec::new());
    app.state = state;

    let lines = render_lines(&app, 8);
    assert!(
        !lines.iter().any(|l| l.contains("my brilliant idea")),
        "captured Idea text must not render in tree body"
    );
    assert!(
        !lines.iter().any(|l| l.contains("╭ idea")),
        "Idea frame must not render in tree body"
    );
}

#[test]
fn expanded_idea_row_does_not_render_input_box() {
    let nodes = vec![node(
        "Idea",
        NodeKind::Stage,
        NodeStatus::WaitingUser,
        Vec::new(),
        None,
        None,
    )];
    let mut app = test_app(nodes, Vec::new(), Vec::new());
    app.input_mode = true;
    app.input_buffer = "typing something".to_string();

    let lines = render_lines(&app, 8);
    assert!(
        !lines.iter().any(|l| l.contains("typing something")),
        "Idea input text must not render in tree body"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("╭ working") || l.contains("╭ input")),
        "Idea input frame must not render in tree body"
    );
}

// ---------------------------------------------------------------------------
// Restored main-panel transcript surface (parity across run modes)
// ---------------------------------------------------------------------------

#[test]
fn main_panel_shows_full_lifecycle_for_both_run_modes() {
    for interactive in [false, true] {
        let mut run = run_record(1, RunStatus::Running);
        run.modes.interactive = interactive;
        let mut app = test_app(
            nested_transcript_tree(),
            vec![run],
            vec![
                kind_message(1, MessageKind::Started, "kicked off run"),
                kind_message(1, MessageKind::Brief, "engineering brief"),
                user_input(1, "ship it"),
                agent_text(1, "acp output"),
                agent_thought(1, "internal reasoning"),
                kind_message(1, MessageKind::Summary, "wrapped up cleanly"),
                kind_message(1, MessageKind::SummaryWarn, "watch the cache"),
                kind_message(1, MessageKind::End, "run finished"),
            ],
        );
        app.live_summary_cached_text = "drafting plan | running".to_string();
        let lines = render_lines(&app, 24);
        let body = lines.join("\n");

        for visible in [
            "Kicked off run",
            "engineering brief",
            "› ship it",
            "Wrapped up cleanly",
            "Watch the cache",
            "Run finished",
        ] {
            assert!(
                body.contains(visible),
                "interactive={interactive}: expected `{visible}` in main panel: {lines:#?}"
            );
        }
        for hidden in ["acp output", "internal reasoning"] {
            assert!(
                !body.contains(hidden),
                "interactive={interactive}: ACP/thought `{hidden}` must stay in split: {lines:#?}"
            );
        }
    }
}

#[test]
fn main_panel_shows_live_summary_tail_for_both_run_modes() {
    for interactive in [false, true] {
        let mut run = run_record(7, RunStatus::Running);
        run.modes.interactive = interactive;
        let mut app = test_app(leaf_only_tree(), vec![run], Vec::new());
        app.live_summary_cached_text = "drafting parity test | covers both modes".to_string();
        app.state.current_phase = Phase::PlanningRunning;

        let lines = render_lines(&app, 12);
        assert!(
            lines.iter().any(|l| l.contains("Drafting parity test")),
            "interactive={interactive}: live summary tail must render in main panel: {lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("⠋") || l.contains("⠙")),
            "interactive={interactive}: spinner must render in main panel: {lines:#?}"
        );
    }
}
