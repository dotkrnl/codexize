// expansion.rs
use super::*;

use super::tree::VisibleNodeRow;

impl App {
    pub(super) fn default_expanded(&self, row: &VisibleNodeRow) -> bool {
        default_expansion(row, self.current_node(), &self.active_path_keys())
    }

    pub(super) fn is_expanded(&self, index: usize) -> bool {
        let Some(row) = self.visible_rows.get(index) else {
            return false;
        };
        effective_expansion(
            row,
            self.current_node(),
            &self.active_path_keys(),
            &self.collapsed_overrides,
        )
    }

    pub(super) fn is_expanded_body(&self, index: usize) -> bool {
        self.is_expanded(index)
            && self
                .visible_rows
                .get(index)
                .is_some_and(|row| row.has_transcript || row.has_body)
    }
}
