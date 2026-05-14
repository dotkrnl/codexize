//! Concrete [`Stage`](super::Stage) implementations.
//!
//! One module per pipeline stage. Each module exports a unit struct named
//! `<StageName>Stage` plus its `impl Stage` block; nothing here is wired
//! into a [`StageRegistry`](super::StageRegistry) yet — Step 3 owns the
//! registration step. The structs and their tests exist now so the trait
//! contract is exercised before the FSM scheduler turns them on.
