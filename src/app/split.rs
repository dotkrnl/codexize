/// Identifies what content the bottom split pane is displaying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitTarget {
    /// An agent run transcript identified by its run id.
    Run(u64),
    /// The Idea node's captured text or active input surface.
    Idea,
}
