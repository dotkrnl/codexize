/// Identifies what content the bottom split pane is displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitTarget {
    /// An agent run transcript identified by its run id.
    Run(u64),
    /// The Idea node's captured text or active input surface.
    Idea,
}

// Main-panel renderers are wired in a later slice; keep this helper available now.
#[allow(dead_code)]
pub(crate) fn run_main_panel_message_visible(
    _run: &crate::state::RunRecord,
    kind: crate::state::MessageKind,
    _show_thinking_texts: bool,
) -> bool {
    use crate::state::MessageKind;

    matches!(
        kind,
        MessageKind::Started
            | MessageKind::Brief
            | MessageKind::Summary
            | MessageKind::SummaryWarn
            | MessageKind::End
            | MessageKind::UserInput
    )
}

pub(crate) fn run_split_panel_message_visible(
    _run: &crate::state::RunRecord,
    kind: crate::state::MessageKind,
    show_thinking_texts: bool,
) -> bool {
    use crate::state::MessageKind;

    match kind {
        MessageKind::AgentText => true,
        // User input is the approved exception to the split's ACP/debug focus.
        MessageKind::UserInput => true,
        MessageKind::AgentThought => show_thinking_texts,
        MessageKind::Started
        | MessageKind::Brief
        | MessageKind::Summary
        | MessageKind::SummaryWarn
        | MessageKind::End => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LaunchModes, MessageKind, RunRecord, RunStatus};

    const ALL_MESSAGE_KINDS: [MessageKind; 8] = [
        MessageKind::Started,
        MessageKind::Brief,
        MessageKind::UserInput,
        MessageKind::AgentText,
        MessageKind::AgentThought,
        MessageKind::Summary,
        MessageKind::SummaryWarn,
        MessageKind::End,
    ];

    fn run_record(interactive: bool) -> RunRecord {
        RunRecord {
            id: if interactive { 1 } else { 2 },
            stage: "stage".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "model".to_string(),
            vendor: "vendor".to_string(),
            window_name: "window".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: Default::default(),
            modes: LaunchModes {
                interactive,
                ..Default::default()
            },
            hostname: None,
            mount_device_id: None,
        }
    }

    #[test]
    fn main_panel_visibility_is_mode_and_verbose_independent() {
        for interactive in [true, false] {
            let run = run_record(interactive);
            for show_thinking_texts in [true, false] {
                for kind in ALL_MESSAGE_KINDS {
                    let expected = matches!(
                        kind,
                        MessageKind::Started
                            | MessageKind::Brief
                            | MessageKind::UserInput
                            | MessageKind::Summary
                            | MessageKind::SummaryWarn
                            | MessageKind::End
                    );

                    assert_eq!(
                        run_main_panel_message_visible(&run, kind, show_thinking_texts),
                        expected,
                        "interactive={interactive}, show_thinking_texts={show_thinking_texts}, kind={kind:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn split_panel_visibility_is_mode_independent_and_verbose_gates_thoughts() {
        for interactive in [true, false] {
            let run = run_record(interactive);
            for show_thinking_texts in [true, false] {
                for kind in ALL_MESSAGE_KINDS {
                    let expected = match kind {
                        MessageKind::AgentText | MessageKind::UserInput => true,
                        MessageKind::AgentThought => show_thinking_texts,
                        MessageKind::Started
                        | MessageKind::Brief
                        | MessageKind::Summary
                        | MessageKind::SummaryWarn
                        | MessageKind::End => false,
                    };

                    assert_eq!(
                        run_split_panel_message_visible(&run, kind, show_thinking_texts),
                        expected,
                        "interactive={interactive}, show_thinking_texts={show_thinking_texts}, kind={kind:?}"
                    );
                }
            }
        }
    }
}
