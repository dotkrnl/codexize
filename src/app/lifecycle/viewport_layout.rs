//! Pinned-header layout helpers for the body viewport.
//!
//! Owns the math that drives "running stage stays pinned at the top while
//! you scroll" so both `viewport.rs` clamping and the renderer can call it
//! through the same surface.
use crate::app::App;
use crate::state::NodeStatus;
use crate::ui::render::frame_cache::{cached_header_y_offsets, cached_running_depth_0_header};
impl App {
    pub(crate) fn header_y_offsets(&self) -> (Vec<usize>, usize) {
        cached_header_y_offsets(|| self.compute_header_y_offsets())
    }
    fn compute_header_y_offsets(&self) -> (Vec<usize>, usize) {
        let mut ys = Vec::with_capacity(self.visible_rows.len());
        let mut y = 0usize;
        for i in 0..self.visible_rows.len() {
            ys.push(y);
            y += 1;
            if self.is_expanded_body(i) {
                // `node_body_len` reads from the per-row body cache without
                // cloning every `Line` just to take `.len()`.
                y += self.node_body_len(i);
            }
        }
        (ys, y)
    }
    pub(crate) fn running_depth_0_header(&self) -> Option<(usize, usize)> {
        cached_running_depth_0_header(|| self.compute_running_depth_0_header())
    }
    fn compute_running_depth_0_header(&self) -> Option<(usize, usize)> {
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
}
