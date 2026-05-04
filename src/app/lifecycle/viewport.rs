use std::collections::BTreeSet;

use crate::app::{App, ExpansionOverride, effective_expansion, split::SplitTarget};
use crate::app::tree::{
    NodeKey, active_path_keys, build_tree, current_node_index, deepest_path_for_run,
    flatten_visible_rows, node_at_path, node_key_at_path,
};
use crate::state::{Node, NodeStatus, Phase, RunStatus};

impl App {
    pub(crate) fn current_node(&self) -> usize {
        current_node_index(&self.nodes)
    }

    #[cfg(test)]
    pub(crate) fn current_row(&self) -> usize {
        let current = self.current_node();
        self.visible_rows
            .iter()
            .position(|row| row.depth == 0 && row.path.first().copied() == Some(current))
            .unwrap_or(0)
    }

    pub(crate) fn node_for_row(&self, index: usize) -> Option<&Node> {
        let row = self.visible_rows.get(index)?;
        node_at_path(&self.nodes, &row.path)
    }

    pub(crate) fn default_selected_key(&self) -> Option<NodeKey> {
        let current = self.current_node();
        node_key_at_path(&self.nodes, &[current])
    }

    pub(crate) fn active_path_keys(&self) -> BTreeSet<NodeKey> {
        active_path_keys(&self.nodes, &self.state.agent_runs)
    }

    pub(crate) fn rebuild_visible_rows(&mut self) {
        let active_keys = self.active_path_keys();
        let current = self.current_node();
        let overrides = self.collapsed_overrides.clone();
        self.visible_rows = flatten_visible_rows(&self.nodes, |row| {
            effective_expansion(row, current, &active_keys, &overrides)
        });
    }

    pub(crate) fn restore_selection(
        &mut self,
        preferred_key: Option<NodeKey>,
        previous_selected: usize,
    ) {
        self.explicit_viewport_scroll = false;
        let target = preferred_key.or_else(|| self.default_selected_key());
        if let Some(key) = target {
            if let Some(index) = self.visible_rows.iter().position(|row| row.key == key) {
                self.selected = index;
                self.selected_key = Some(key);
                return;
            }
            if let Some(index) = key
                .ancestors()
                .find_map(|ancestor| self.visible_rows.iter().position(|row| row.key == ancestor))
            {
                self.selected = index;
                self.selected_key = self.visible_rows.get(index).map(|row| row.key.clone());
                return;
            }
        }

        self.selected = previous_selected.min(self.visible_rows.len().saturating_sub(1));
        self.selected_key = self
            .visible_rows
            .get(self.selected)
            .map(|row| row.key.clone());
    }

    pub(crate) fn rebuild_tree_view(&mut self, preferred_key: Option<NodeKey>) {
        let previous_selected = self.selected;
        let preferred_key = preferred_key.or_else(|| self.selected_key.clone());

        self.nodes = build_tree(&self.state);
        self.rebuild_visible_rows();
        self.restore_selection(preferred_key, previous_selected);
        self.synchronize_split_target();
    }

    /// Validate the current split target against the latest tree and session
    /// state. Closes the split when its run id disappears after rebuild/retry,
    /// and clamps the scroll offset.
    ///
    /// Interactive ACP prompts force-open the split for the active run and
    /// focus the split input box without waiting for another keypress. The
    /// `interactive_run_waiting_for_input` guard checks `run.modes.interactive`,
    /// so non-interactive runs never trigger auto-open, auto-switch, or forced
    /// input focus from this path — only the stale-target cleanup below applies
    /// to them.
    pub(crate) fn synchronize_split_target(&mut self) {
        if self.interactive_run_waiting_for_input()
            && let Some(run_id) = self.current_run_id
        {
            let target = SplitTarget::Run(run_id);
            if self.split_target != Some(target) {
                self.open_split_target(target);
            }
            // Force input mode for interactive prompts
            self.input_mode = true;
            self.clamp_split_scroll(self.current_split_content_height());
            return;
        }

        if self.state.current_phase == Phase::IdeaInput {
            let target = SplitTarget::Idea;
            if self.split_target != Some(target) {
                self.open_split_target(target);
            }
            // Force input mode for Idea input
            self.input_mode = true;
            self.clamp_split_scroll(self.current_split_content_height());
            return;
        }

        let Some(target) = self.split_target else {
            return;
        };
        match target {
            SplitTarget::Run(run_id) => {
                let still_exists = self.state.agent_runs.iter().any(|run| run.id == run_id);
                if !still_exists {
                    self.split_target = None;
                    self.split_scroll_offset = 0;
                    self.split_follow_tail = true;
                }
            }
            SplitTarget::Idea => {
                // Idea is always valid as long as the session exists.
            }
        }
        self.clamp_split_scroll(self.current_split_content_height());
    }

    /// Clamp the split scroll offset to a maximum value. Called after
    /// terminal resize and after content changes.
    #[allow(dead_code)]
    pub(crate) fn clamp_split_scroll(&mut self, content_height: usize) {
        let viewport_height = self.split_viewport_height();
        if viewport_height == 0 {
            self.split_scroll_offset = 0;
            return;
        }

        let max_offset = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            viewport_height,
        );
        if self.split_follow_tail {
            self.split_scroll_offset = max_offset;
            return;
        }

        self.split_scroll_offset = self.split_scroll_offset.min(max_offset);
        // If content shrink or a larger viewport leaves the operator at the
        // tail anyway, re-engage follow mode so later transcript growth streams
        // normally instead of appearing frozen at a stale offset.
        if self.split_scroll_offset >= max_offset {
            self.split_follow_tail = true;
        }
    }

    /// Derive the preferred row for automatic progress follow.
    ///
    /// Resolution order: deepest node backing the current run id when that
    /// run is still `Running`, then the current top-level pipeline stage.
    /// The status check matters during rewinds (`go_back`) and other paths
    /// that finalize the run before clearing `current_run_id` — without it,
    /// a refocus event fired in that window would land on the just-aborted
    /// row instead of the new active stage. Returns `None` only when the
    /// pipeline has no live stage (everything `Done`/`Skipped`), which lets
    /// callers leave `selected_key` alone on terminal phases.
    pub(crate) fn progress_focus_key(&self) -> Option<NodeKey> {
        if let Some(run_id) = self.current_run_id
            && self
                .state
                .agent_runs
                .iter()
                .any(|run| run.id == run_id && run.status == RunStatus::Running)
            && let Some(path) = deepest_path_for_run(&self.nodes, run_id)
            && let Some(key) = node_key_at_path(&self.nodes, &path)
        {
            return Some(key);
        }
        let current = self.current_node();
        let active = self
            .nodes
            .get(current)
            .is_some_and(|node| !matches!(node.status, NodeStatus::Done | NodeStatus::Skipped));
        if active {
            return node_key_at_path(&self.nodes, &[current]);
        }
        None
    }

    /// Move focus to the row returned by `progress_focus_key` when progress
    /// follow is active. Reuses `restore_selection` so the collapsed-ancestor
    /// fallback matches normal selection recovery.
    pub(crate) fn maybe_refocus_to_progress(&mut self) {
        if !self.progress_follow_active {
            return;
        }
        let Some(target) = self.progress_focus_key() else {
            return;
        };
        let previous_selected = self.selected;
        self.restore_selection(Some(target), previous_selected);
    }

    /// Re-enable progress-follow focus and immediately refocus. Called from
    /// the phase-transition and run-launch boundaries the spec treats as
    /// natural reset points after manual navigation.
    pub(crate) fn enable_progress_follow_and_refocus(&mut self) {
        self.progress_follow_active = true;
        self.maybe_refocus_to_progress();
    }

    pub(crate) fn can_focus_input(&self) -> bool {
        self.is_expanded(self.selected)
            && self.state.current_phase == Phase::IdeaInput
            && self
                .node_for_row(self.selected)
                .is_some_and(|node| node.label == "Idea")
    }

    pub(crate) fn split_owns_input(&self) -> bool {
        self.is_split_open()
            && (matches!(self.split_target, Some(SplitTarget::Idea))
                && self.state.current_phase == Phase::IdeaInput
                || self.interactive_run_waiting_for_input())
    }

    pub(crate) fn split_viewport_height(&self) -> usize {
        if !self.is_split_open() || self.body_inner_height == 0 {
            return 0;
        }
        if self.split_fullscreen {
            return self.body_inner_height;
        }
        let content_height = self.body_inner_height.saturating_sub(1);
        let tree_height = (content_height / 3).max(1).min(content_height);
        content_height.saturating_sub(tree_height)
    }

    pub(crate) fn current_split_content_height(&self) -> usize {
        let Some(target) = self.split_target else {
            return 0;
        };
        match target {
            SplitTarget::Run(run_id) => {
                let Some(run) = self.state.agent_runs.iter().find(|run| run.id == run_id) else {
                    return 0;
                };
                let msgs: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|m| m.run_id == run_id)
                    .filter(|m| {
                        crate::app::split::run_split_panel_message_visible(
                            run,
                            m.kind,
                            self.state.show_thinking_texts,
                        )
                    })
                    .cloned()
                    .collect();

                let local_offset = chrono::Local::now().fixed_offset().offset().to_owned();
                crate::app::chat_widget::message_lines(
                    &msgs,
                    run,
                    &local_offset,
                    self.split_transcript_tail_line(run),
                    self.body_inner_width.max(1),
                    self.spinner_tick,
                    true,
                )
                .len()
            }
            // Idea content currently does not participate in transcript-style
            // scrolling, so rebuild/sync clamps it as a fixed viewport.
            SplitTarget::Idea => 0,
        }
    }

    pub(crate) fn header_y_offsets(&self) -> (Vec<usize>, usize) {
        let mut ys = Vec::with_capacity(self.visible_rows.len());
        let mut y = 0usize;
        for i in 0..self.visible_rows.len() {
            ys.push(y);
            y += 1;
            if self.is_expanded_body(i) {
                y += self.node_body(i).len();
            }
        }
        (ys, y)
    }

    pub(crate) fn running_depth_0_header(&self) -> Option<(usize, usize)> {
        let (ys, _) = self.header_y_offsets();
        let mut candidates = self
            .visible_rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.depth == 0)
            .filter_map(|(index, _)| {
                let node = self.node_for_row(index)?;
                (node.status == NodeStatus::Running).then_some((index, ys[index]))
            });
        let candidate = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        Some(candidate)
    }

    pub(crate) fn pinned_running_header(&self, viewport_top: usize) -> Option<(usize, usize)> {
        self.running_depth_0_header()
            .filter(|(_, header_y)| *header_y < viewport_top)
    }

    pub(crate) fn effective_body_height_for_top(
        &self,
        viewport_top: usize,
        body_height: usize,
    ) -> usize {
        if self.pinned_running_header(viewport_top).is_some() {
            body_height.saturating_sub(1)
        } else {
            body_height
        }
    }

    pub(crate) fn effective_body_inner_height(&self) -> usize {
        self.effective_body_height_for_top(self.viewport_top, self.body_inner_height)
    }

    pub(crate) fn max_viewport_top_for_height(&self, body_height: usize) -> usize {
        if body_height == 0 {
            return 0;
        }
        let (_, total) = self.header_y_offsets();
        let normal_max = total.saturating_sub(body_height);
        if self
            .running_depth_0_header()
            .is_some_and(|(_, header_y)| header_y < normal_max)
        {
            total.saturating_sub(body_height.saturating_sub(1))
        } else {
            normal_max
        }
    }

    pub(crate) fn clamp_viewport(&mut self) {
        let area_h = self.body_inner_height;
        if area_h == 0 {
            self.viewport_top = 0;
            return;
        }
        let (ys, total) = self.header_y_offsets();
        let max_top = self.max_viewport_top_for_height(area_h);
        if self.follow_tail {
            self.viewport_top = max_top;
            self.explicit_viewport_scroll = false;
            return;
        }
        if !self.explicit_viewport_scroll
            && let Some(&header_y) = ys.get(self.selected)
        {
            let section_bottom = ys.get(self.selected + 1).copied().unwrap_or(total);
            // A first adjustment can move the viewport above a running header,
            // which activates pinning and reduces the content height by one.
            for _ in 0..2 {
                let effective_h = self.effective_body_height_for_top(self.viewport_top, area_h);
                // Keep any line of the selected section visible. This lets the user
                // scroll viewport_top through a tall body without the viewport snapping
                // back to the header on every render.
                if section_bottom <= self.viewport_top {
                    self.viewport_top = section_bottom.saturating_sub(1);
                } else if header_y >= self.viewport_top + effective_h {
                    self.viewport_top = header_y + 1 - effective_h;
                } else {
                    break;
                }
            }
        }
        self.viewport_top = self.viewport_top.min(max_top);
        if self.viewport_top >= max_top {
            self.set_follow_tail(true);
            self.explicit_viewport_scroll = false;
        }
    }

    pub(crate) fn max_viewport_top(&self) -> usize {
        self.max_viewport_top_for_height(self.body_inner_height)
    }

    pub(crate) fn scroll_viewport(&mut self, delta: isize, explicit: bool) {
        self.explicit_viewport_scroll = explicit;
        let max_top = self.max_viewport_top() as isize;
        let next = (self.viewport_top as isize + delta).clamp(0, max_top.max(0));
        self.viewport_top = next as usize;
        self.set_follow_tail(self.viewport_top as isize >= max_top);
        // Explicit paging (PageUp/PageDown today, equivalent mouse handlers
        // tomorrow) signals operator-driven browsing. Implicit scrolls from
        // arrow-key handoff or clamp_viewport do not.
        if explicit {
            self.progress_follow_active = false;
        }
    }

    /// Single writer for `follow_tail`. Tracks the message-count baseline so
    /// the unread-counter badge can compute "messages since detach".
    pub(crate) fn set_follow_tail(&mut self, follow: bool) {
        if follow == self.follow_tail {
            return;
        }
        self.follow_tail = follow;
        self.tail_detach_baseline = if follow {
            None
        } else {
            Some(self.messages.len())
        };
        if follow {
            self.explicit_viewport_scroll = false;
        }
    }

    /// Pin every row that's currently effectively expanded as an explicit
    /// Expanded override. Called once per render so that whatever the user
    /// is currently looking at stays expanded across later state changes
    /// (e.g., the active stage rolling over to Done before a phase advance,
    /// which would otherwise drop it off the auto-expand active path).
    pub(crate) fn latch_visible_expansions(&mut self) {
        let to_pin: Vec<NodeKey> = (0..self.visible_rows.len())
            .filter(|&i| self.is_expanded(i))
            .filter_map(|i| self.visible_rows.get(i).map(|row| row.key.clone()))
            .filter(|key| !self.collapsed_overrides.contains_key(key))
            .collect();
        for key in to_pin {
            self.collapsed_overrides
                .insert(key, ExpansionOverride::Expanded);
        }
    }

    pub(crate) fn unread_below_count(&self) -> usize {
        match self.tail_detach_baseline {
            Some(baseline) => self.messages.len().saturating_sub(baseline),
            None => 0,
        }
    }

    pub(crate) fn first_unread_rendered_line(&self) -> Option<usize> {
        let baseline = self.tail_detach_baseline?;
        if baseline >= self.messages.len() {
            return None;
        }

        let local_offset = chrono::Local::now().fixed_offset().offset().to_owned();
        let available_width = self.body_inner_width.max(1);
        let (ys, _) = self.header_y_offsets();

        (0..self.visible_rows.len())
            .filter(|&index| self.is_expanded_body(index))
            .filter_map(|index| {
                let node = self.node_for_row(index)?;
                let run_id = node.run_id.or(node.leaf_run_id)?;
                let run = self.state.agent_runs.iter().find(|run| run.id == run_id)?;
                // Match main-panel rendering: messages that are not visible
                // in the main panel must not contribute to the unread offset
                // because the pipeline widget never renders them.
                let visible = |message: &&crate::state::Message| {
                    crate::app::split::run_main_panel_message_visible(
                        run,
                        message.kind,
                        self.state.show_thinking_texts,
                    )
                };
                let old_messages: Vec<_> = self
                    .messages
                    .iter()
                    .take(baseline)
                    .filter(|message| message.run_id == run_id)
                    .filter(visible)
                    .cloned()
                    .collect();
                let all_messages: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|message| message.run_id == run_id)
                    .filter(visible)
                    .cloned()
                    .collect();

                if old_messages.len() == all_messages.len() {
                    return None;
                }

                let old_line_count = crate::app::chat_widget::message_lines(
                    &old_messages,
                    run,
                    &local_offset,
                    None,
                    available_width,
                    0,
                    false,
                )
                .len();
                let all_line_count = crate::app::chat_widget::message_lines(
                    &all_messages,
                    run,
                    &local_offset,
                    None,
                    available_width,
                    0,
                    false,
                )
                .len();

                (all_line_count > old_line_count).then_some(ys[index] + 1 + old_line_count)
            })
            .min()
    }
}
