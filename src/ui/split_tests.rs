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
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: LaunchModes {
            interactive,
            ..Default::default()
        },
        hostname: None,
        mount_device_id: None,
        section_path: None,
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
                    MessageKind::AgentText
                    | MessageKind::UserInput
                    | MessageKind::Started
                    | MessageKind::End => true,
                    MessageKind::AgentThought => show_thinking_texts,
                    MessageKind::Brief | MessageKind::Summary | MessageKind::SummaryWarn => false,
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
