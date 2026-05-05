// Re-organized from tests_prompts.rs — see commit history.

use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    state::{self as session_state, Phase, PipelineItemStatus, RunRecord, RunStatus, SessionState},
    tasks,
};

pub(super) fn assert_prompt_insta_snapshot(name: &str, actual: &str) {
    insta::with_settings!({
        description => "Prompt output snapshot",
        omit_expression => true,
    }, {
        insta::assert_snapshot!(name, actual);
    });
}

mod chunk_00_tests;
mod chunk_01_tests;
