use super::*;

#[test]
fn empty_view_has_no_modal_or_status() {
    let view = AppView::empty("test-session");
    assert_eq!(view.session_id.as_ref(), "test-session");
    assert!(view.modal.is_none());
    assert!(view.status.is_none());
    assert!(view.agent_runs.is_empty());
    assert!(view.follow_tail);
    assert!(!view.agent_running);
    assert_eq!(view.phase, Phase::IdeaInput);
    assert_eq!(view.modes, ModeFlags::default());
}
