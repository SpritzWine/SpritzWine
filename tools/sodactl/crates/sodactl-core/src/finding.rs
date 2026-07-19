//! Findings: the product surface.
//!
//! Everything the tool knows how to tell you is a `Finding`. The CLI renders
//! them as a table, `--json` serialises them, MCP returns them as
//! `structuredContent`, and a future TUI draws them. One schema, N surfaces.
//!
//! Every finding carries a `remedy` -- the exact thing to do about it. A finding
//! you cannot act on is noise, and this type makes that omission impossible to
//! ship by accident.

use serde::Serialize;
use sodactl_git::Pin;

// `Hash` because the TUI keys tree nodes on `Severity` (tui-tree-widget wants
// `Identifier: Hash`); it is derivable for free on a fieldless enum and implies
// nothing about ordering, which `Ord` already owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Actively shipping wrong bits, or will corrupt history if acted on.
    Blocker,
    /// The graph is inconsistent; a regeneration would produce stale output.
    Drift,
    /// Untidy, not wrong.
    Hygiene,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Blocker => "BLOCKER",
            Severity::Drift => "DRIFT",
            Severity::Hygiene => "hygiene",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// Stable machine-readable code, e.g. `TIER_CHILD_STALE`.
    pub code: &'static str,
    pub severity: Severity,
    /// The ref or path this is about.
    pub subject: String,
    pub summary: String,
    /// What to do. Exact commands where there is one.
    pub remedy: String,
}

impl Finding {
    pub fn new(
        code: &'static str,
        severity: Severity,
        subject: impl Into<String>,
        summary: impl Into<String>,
        remedy: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            subject: subject.into(),
            summary: summary.into(),
            remedy: remedy.into(),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub tiers: Vec<TierState>,
    pub consumers: Vec<ConsumerState>,
}

#[derive(Debug, Serialize)]
pub struct TierState {
    pub name: String,
    pub oid: Option<String>,
    /// Derived, never declared.
    pub commits_on_parent: Option<u32>,
    pub parent: String,
    pub ahead_of_remote: Option<u32>,
    pub behind_remote: Option<u32>,
    pub contains_parent_tip: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ConsumerState {
    pub name: String,
    pub oid: Option<String>,
    pub base: String,
    pub soda_tip: String,
    /// Whether the consumer's replay still reflects its declared soda tip.
    /// `None` when it cannot be determined without a ledger -- and saying
    /// "unknown" is the honest answer, not a guess.
    pub current: Option<bool>,
    pub pinned_by_superproject: bool,
    /// What the superproject actually records for the wine submodule.
    ///
    /// Carried on the state, not just folded into a finding, because the branch
    /// tip and the gitlink are different commits by design and any reader
    /// eyeballing a pin problem needs to see both to believe the diagnosis.
    pub pin: Option<Pin>,
}

impl Report {
    pub fn sort(&mut self) {
        self.findings.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.code.cmp(b.code)));
    }

    pub fn count(&self, sev: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == sev).count()
    }

    pub fn worst(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).min()
    }
}
