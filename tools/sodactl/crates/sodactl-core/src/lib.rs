//! sodactl-core: model, reconcile. No clap, no rmcp, no terminal, no stdout.
//!
//! Three sources of truth, strictly ranked:
//!   1. git      -- the only authority on where refs are. Read live, every time.
//!   2. manifest -- what git cannot know: policy, topology edges, constants.
//!   3. ledger   -- what happened. A claim to be verified, never a fact. (v3)

pub mod checks;
pub mod explain;
pub mod finding;
pub mod graph;
pub mod manifest;
pub mod plan;
pub mod present;
pub mod reconcile;
pub mod rrcache;
pub mod tags;
pub mod treestate;
pub mod verbs;

pub use checks::{Policy, SkewPolicy, Workspace};
pub use sodactl_git::{GitCli, GitRead};
pub use finding::{Finding, Report, Severity};
pub use manifest::{workspace_root, Manifest};
pub use reconcile::{reconcile, reconcile_workspace};
pub use rrcache::{RrEntry, RrSurvey};
pub use tags::TagName;
