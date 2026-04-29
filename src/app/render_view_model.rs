use super::tree::VisibleNodeRow;

/// Determines whether the row at `index` is the last sibling at its depth.
///
/// Scans forward from `index + 1` until a row with depth <= current depth is
/// found. Returns true if no such row exists or if the found row has depth less
/// than the current depth.
pub(super) fn is_last_sibling(visible_rows: &[VisibleNodeRow], index: usize) -> bool {
    let cur_depth = visible_rows[index].depth;
    visible_rows[index + 1..]
        .iter()
        .find(|r| r.depth <= cur_depth)
        .map(|r| r.depth < cur_depth)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tree::NodeKey,
        state::{NodeKind, NodeStatus},
    };

    fn row(depth: usize) -> VisibleNodeRow {
        VisibleNodeRow {
            depth,
            path: Vec::new(),
            key: NodeKey::new(Vec::new()),
            kind: NodeKind::Stage,
            status: NodeStatus::Done,
            has_children: false,
            has_transcript: false,
            has_body: false,
            backing_leaf_run_id: None,
        }
    }

    #[test]
    fn row_is_not_last_sibling_when_next_peer_has_same_depth() {
        let rows = vec![row(0), row(1), row(2), row(1)];

        assert!(!is_last_sibling(&rows, 1));
    }

    #[test]
    fn row_is_last_sibling_when_next_boundary_is_ancestor() {
        let rows = vec![row(0), row(1), row(2), row(0)];

        assert!(is_last_sibling(&rows, 1));
    }
}
