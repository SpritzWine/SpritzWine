//! Panes: each renders one facet of a `Snapshot`.
//!
//! The split mirrors the layout, not the data: `findings` owns the checklist
//! tree and its session state (selection, ticks), `graph` owns the topology
//! tree, `log` owns commit-chain selection and scroll, and `detail` is pure
//! line builders that follow whatever the focused pane selects. Dispatch
//! between focused panes stays a plain `enum Focus` + `match` in the app --
//! honest at this N, per the design record.

pub mod detail;
pub mod findings;
pub mod graph;
pub mod help;
pub mod log;
