//! The findings pane: the failure checklist.
//!
//! Severity groups with counts, `[ ]/[x] CODE subject` leaves. A tick is
//! session-local working state -- "dealt with this one" -- never data: it
//! persists nowhere and mutates nothing. A ticked finding stays VISIBLE and
//! dimmed, because hiding it would turn the checklist into a filter, and a
//! filtered Blocker is exactly the silent breakdown this tool exists to
//! prevent.

use crate::theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use sodactl_core::finding::{Finding, Report, Severity};
use std::collections::HashSet;
use tui_tree_widget::{Tree, TreeItem, TreeState};

/// Tree node identity. Selection is keyed on WHAT is selected, not where it
/// sits: tui-tree-widget tracks identifier paths, so a tree that shrinks under
/// the cursor cannot leave a stale index behind (the abort class the reuse
/// survey rejected tui-widget-list over). `(code, subject)` is the finding
/// identity the whole tool agrees on -- the same pair `reconcile_workspace`
/// dedups on and a tick is keyed by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeId {
    Sev(Severity),
    F { code: &'static str, subject: String },
}

/// Worst-first, matching `Report::sort`. The groups iterate in this order so
/// the top of the pane is always the most urgent thing known.
const SEVERITIES: [Severity; 3] = [Severity::Blocker, Severity::Drift, Severity::Hygiene];

pub struct FindingsPane {
    pub state: TreeState<NodeId>,
    /// Session-local tick set, keyed on `(severity, code, subject)` -- the
    /// finding identity PLUS the severity it carried when ticked. Severity is
    /// in the key because it is not constant per code: reconcile.rs:521 emits
    /// PINNED_CONSUMER_UNTAGGED as Drift while the pin bump is staged-only
    /// and Blocker once committed. A tick keyed on identity alone would ride
    /// through that escalation and the Blocker would arrive pre-dimmed --
    /// the filtered-Blocker breakdown this pane's header comment forbids.
    ticks: HashSet<(Severity, &'static str, String)>,
    /// Severities present in the last applied report. A group auto-opens when
    /// its severity APPEARS (cold start included): the pane is a failure
    /// checklist, and a collapsed group of findings the user has never seen
    /// would hide new evidence behind a closed arrow. A group the user closed
    /// stays closed across refreshes as long as its severity stays present --
    /// only appearance reopens.
    present: HashSet<Severity>,
}

impl FindingsPane {
    pub fn new() -> Self {
        Self { state: TreeState::default(), ticks: HashSet::new(), present: HashSet::new() }
    }

    /// Reconcile session state with a fresh report. Called on every applied
    /// snapshot, BEFORE the next draw.
    pub fn apply_snapshot(&mut self, report: &Report) {
        // Tick GC. A tick records a judgment about THIS occurrence of the
        // finding. When the finding vanishes, the tick's referent is gone, so
        // the tick goes with it -- and when the same (code, subject) later
        // reappears, it arrives UNticked: the state changed twice, which is
        // new evidence, not the occurrence that was already dealt with. A
        // severity change without vanishing is the same rule through the
        // richer key (see the `ticks` field doc): the escalated form is a new
        // occurrence, so it too arrives unticked.
        let live: HashSet<(Severity, &'static str, &str)> = report
            .findings
            .iter()
            .map(|f| (f.severity, f.code, f.subject.as_str()))
            .collect();
        self.ticks.retain(|(sev, code, subject)| live.contains(&(*sev, *code, subject.as_str())));

        let now: HashSet<Severity> = report.findings.iter().map(|f| f.severity).collect();
        for sev in SEVERITIES {
            if now.contains(&sev) && !self.present.contains(&sev) {
                self.state.open(vec![NodeId::Sev(sev)]);
            }
        }
        self.present = now;

        // Selection must always point at something the new report contains: a
        // dangling selection would render a live tree beside an empty detail
        // pane, which reads as "nothing to say" -- a silent breakdown.
        // Liveness is judged on the FULL path, severity included: after a
        // severity flip the leaf exists but under a different group, so the
        // old path is dead in the new tree -- keeping it would render a
        // highlight-less pane (the widget finds no such path to paint)
        // beside a detail pane that still explains the finding.
        let selection_alive = match self.state.selected() {
            [NodeId::Sev(sev)] => self.present.contains(sev),
            [NodeId::Sev(sev), NodeId::F { code, subject }] => {
                live.contains(&(*sev, *code, subject.as_str()))
            }
            _ => false,
        };
        if !selection_alive {
            match report.findings.first() {
                // Cold start and re-anchor both select the WORST finding
                // (`findings[0]` -- `Report::sort` orders worst-first) by its
                // explicit identifier path. `select_first()` would be a no-op
                // here: it reads `last_identifiers`, which only a render
                // populates, and on cold start no render has happened yet.
                // The same gotcha is why every headless nav test does one
                // warm-up render before driving keys.
                Some(worst) => {
                    self.state.select(vec![
                        NodeId::Sev(worst.severity),
                        NodeId::F { code: worst.code, subject: worst.subject.clone() },
                    ]);
                }
                None => {
                    self.state.select(Vec::new());
                }
            }
        }
    }

    /// Space. Only leaves tick; on a group Space does nothing rather than
    /// something surprising (bulk-ticking a severity would let one keypress
    /// dim every Blocker at once).
    pub fn toggle_tick(&mut self) {
        // The full path, not `[.., F]`: the group element carries the
        // severity half of the tick key.
        if let [NodeId::Sev(sev), NodeId::F { code, subject }] = self.state.selected() {
            let key = (*sev, *code, subject.clone());
            if !self.ticks.remove(&key) {
                self.ticks.insert(key);
            }
        }
    }

    /// The finding under the cursor, resolved against the report the caller is
    /// drawing -- the pane never stores findings, only their identities.
    pub fn selected_finding<'r>(&self, report: &'r Report) -> Option<&'r Finding> {
        // Severity matched too: the path names one occurrence, and resolving
        // a Drift-path selection to the finding's Blocker form would explain
        // a node the tree is not highlighting.
        if let [NodeId::Sev(sev), NodeId::F { code, subject }] = self.state.selected() {
            report
                .findings
                .iter()
                .find(|f| f.severity == *sev && f.code == *code && &f.subject == subject)
        } else {
            None
        }
    }

    /// The selected severity group, when a group rather than a leaf is
    /// selected.
    pub fn selected_group(&self) -> Option<Severity> {
        match self.state.selected() {
            [NodeId::Sev(sev)] => Some(*sev),
            _ => None,
        }
    }

    fn items(&self, report: &Report) -> Vec<TreeItem<'static, NodeId>> {
        let mut groups = Vec::new();
        for sev in SEVERITIES {
            let mut seen = HashSet::new();
            let leaves: Vec<TreeItem<'static, NodeId>> = report
                .findings
                .iter()
                .filter(|f| f.severity == sev)
                // Tree identifiers must be sibling-unique or the widget
                // refuses to render. (code, subject) IS the finding identity,
                // so a duplicate pair is the same fact restated --
                // `reconcile_workspace` already dedups on it; this guards the
                // pane against any future emitter that forgets.
                .filter(|f| seen.insert((f.code, f.subject.clone())))
                .map(|f| {
                    let ticked =
                        self.ticks.contains(&(f.severity, f.code, f.subject.clone()));
                    let style =
                        if ticked { theme::ticked_style() } else { Style::default() };
                    let tick = if ticked { "[x]" } else { "[ ]" };
                    TreeItem::new_leaf(
                        NodeId::F { code: f.code, subject: f.subject.clone() },
                        Line::styled(format!("{tick} {} {}", f.code, f.subject), style),
                    )
                })
                .collect();
            // An absent severity is the absence of findings, not a suppressed
            // one -- a permanent "BLOCKER (0)" header would train the eye to
            // skip the word BLOCKER.
            if leaves.is_empty() {
                continue;
            }
            let header =
                Line::styled(format!("{} ({})", sev.label(), leaves.len()), theme::severity_style(sev));
            groups.push(
                TreeItem::new(NodeId::Sev(sev), header, leaves)
                    .expect("leaf identifiers are sibling-unique: deduplicated on (code, subject) above"),
            );
        }
        groups
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, report: &Report, focused: bool) {
        let block = theme::pane_block("findings", focused);
        if report.findings.is_empty() {
            // The empty state says so in words. An empty tree under a border
            // is indistinguishable from a pane that failed to draw.
            f.render_widget(
                Paragraph::new("no findings -- the workspace reconciles clean").block(block),
                area,
            );
            return;
        }
        let items = self.items(report);
        let tree = Tree::new(&items)
            .expect("group identifiers are unique: at most one node per Severity variant")
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(tree, area, &mut self.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn fd(code: &'static str, sev: Severity, subject: &str) -> Finding {
        Finding::new(code, sev, subject, format!("{code} observed"), format!("fix {code}"))
    }

    /// Reports reach the pane already sorted (`Report::sort` runs inside
    /// reconcile); sorting here keeps the fixtures honest about that contract
    /// -- `apply_snapshot`'s "worst = findings[0]" leans on it.
    fn report(findings: Vec<Finding>) -> Report {
        let mut r = Report::default();
        r.findings = findings;
        r.sort();
        r
    }

    fn draw(pane: &mut FindingsPane, report: &Report) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(48, 10)).unwrap();
        term.draw(|f| pane.render(f, f.area(), report, false)).unwrap();
        term
    }

    fn rows(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol()).collect())
            .collect()
    }

    fn mixed() -> Report {
        // Deliberately shuffled: report() re-sorts, and the pane must render
        // groups worst-first regardless of arrival order.
        report(vec![
            fd("RR_NO_POSTIMAGE", Severity::Hygiene, "rr/cafe"),
            fd("PINNED_CONSUMER_UNTAGGED", Severity::Blocker, "spritzwine"),
            fd("TIER_CHILD_STALE", Severity::Drift, "soda-ntdll"),
            fd("STITCH_STALE", Severity::Drift, "soda-all"),
        ])
    }

    #[test]
    fn empty_report_says_so_in_words() {
        let mut pane = FindingsPane::new();
        let r = report(vec![]);
        pane.apply_snapshot(&r);
        insta::assert_snapshot!(draw(&mut pane, &r).backend());
    }

    #[test]
    fn mixed_severities_render_worst_first_with_counts() {
        let mut pane = FindingsPane::new();
        let r = mixed();
        pane.apply_snapshot(&r);
        insta::assert_snapshot!(draw(&mut pane, &r).backend());
    }

    #[test]
    fn cold_start_selects_the_worst_finding_before_any_render() {
        let mut pane = FindingsPane::new();
        pane.apply_snapshot(&mixed());
        // No render has happened -- this is exactly where select_first() would
        // silently do nothing.
        assert_eq!(
            pane.state.selected(),
            &[
                NodeId::Sev(Severity::Blocker),
                NodeId::F { code: "PINNED_CONSUMER_UNTAGGED", subject: "spritzwine".into() },
            ]
        );
    }

    #[test]
    fn space_ticks_dims_and_unticks_the_selected_leaf() {
        let mut pane = FindingsPane::new();
        let r = mixed();
        pane.apply_snapshot(&r);
        // Warm-up render: populates last_identifiers so later nav/tick
        // interplay behaves as it would live.
        let _ = draw(&mut pane, &r);

        pane.toggle_tick();
        let term = draw(&mut pane, &r);
        let all = rows(&term);
        let y = all
            .iter()
            .position(|row| row.contains("[x] PINNED_CONSUMER_UNTAGGED"))
            .expect("the ticked leaf must stay visible");
        // Dimmed: the leaf's cells carry the ticked foreground. Probe a cell
        // inside the code text (border + highlight symbol offset the row).
        let x = all[y].find("PINNED").unwrap() as u16;
        let cell = term.backend().buffer().cell((x, y as u16)).unwrap();
        assert_eq!(cell.fg, Color::DarkGray, "ticked leaf must render dimmed");

        pane.toggle_tick();
        let term = draw(&mut pane, &r);
        let all = rows(&term);
        let y = all
            .iter()
            .position(|row| row.contains("[ ] PINNED_CONSUMER_UNTAGGED"))
            .expect("unticking must restore the empty box");
        let x = all[y].find("PINNED").unwrap() as u16;
        let cell = term.backend().buffer().cell((x, y as u16)).unwrap();
        assert_ne!(cell.fg, Color::DarkGray, "unticked leaf must not stay dimmed");
    }

    /// The severity flip is REACHABLE, not hypothetical (found by adversarial
    /// verification): reconcile.rs:521 emits PINNED_CONSUMER_UNTAGGED as
    /// Drift while the pin bump is staged-only and Blocker once committed --
    /// same (code, subject), in the primary live workflow. A tick on the
    /// Drift form must not dim the escalated Blocker, and the old
    /// Drift-group path must not linger as a highlight-less selection.
    #[test]
    fn severity_escalation_unticks_and_keeps_a_visible_selection() {
        let mut pane = FindingsPane::new();
        let before = report(vec![
            fd("PINNED_CONSUMER_UNTAGGED", Severity::Drift, "spritzwine"),
            fd("RR_NO_POSTIMAGE", Severity::Hygiene, "rr/cafe"),
        ]);
        pane.apply_snapshot(&before);
        let _ = draw(&mut pane, &before); // warm-up (see cold-start comment)
        pane.toggle_tick(); // the pre-selected worst: the Drift form
        assert_eq!(pane.ticks.len(), 1);

        let after = report(vec![
            fd("PINNED_CONSUMER_UNTAGGED", Severity::Blocker, "spritzwine"),
            fd("RR_NO_POSTIMAGE", Severity::Hygiene, "rr/cafe"),
        ]);
        pane.apply_snapshot(&after);
        assert!(
            pane.ticks.is_empty(),
            "a tick on the Drift form must not dim the escalated Blocker"
        );
        // The dead Drift path re-anchored to the worst finding's new path...
        assert_eq!(
            pane.state.selected(),
            &[
                NodeId::Sev(Severity::Blocker),
                NodeId::F { code: "PINNED_CONSUMER_UNTAGGED", subject: "spritzwine".into() },
            ]
        );
        // ...so exactly one rendered row carries the selection highlight.
        let term = draw(&mut pane, &after);
        let buf = term.backend().buffer();
        let highlighted = (0..buf.area.height)
            .filter(|y| {
                (0..buf.area.width).any(|x| {
                    buf.cell((x, *y)).unwrap().modifier.contains(Modifier::REVERSED)
                })
            })
            .count();
        assert_eq!(highlighted, 1, "the highlight must survive the escalation");
    }

    #[test]
    fn tick_gc_a_vanished_finding_reappears_unticked() {
        let mut pane = FindingsPane::new();
        let r = mixed();
        pane.apply_snapshot(&r);
        let _ = draw(&mut pane, &r); // warm-up (see cold-start comment)

        // Tick a Drift leaf, not the pre-selected Blocker, so the re-anchor
        // assertion below is distinguishable from "selection never moved".
        pane.state.select(vec![
            NodeId::Sev(Severity::Drift),
            NodeId::F { code: "STITCH_STALE", subject: "soda-all".into() },
        ]);
        pane.toggle_tick();
        assert_eq!(pane.ticks.len(), 1);

        // Refresh without the ticked finding: tick AND selection referent are
        // gone. The tick is GCed; the selection re-anchors to the worst.
        let without = report(vec![
            fd("RR_NO_POSTIMAGE", Severity::Hygiene, "rr/cafe"),
            fd("PINNED_CONSUMER_UNTAGGED", Severity::Blocker, "spritzwine"),
            fd("TIER_CHILD_STALE", Severity::Drift, "soda-ntdll"),
        ]);
        pane.apply_snapshot(&without);
        assert!(pane.ticks.is_empty(), "a vanished finding's tick must be GCed");
        assert_eq!(
            pane.state.selected(),
            &[
                NodeId::Sev(Severity::Blocker),
                NodeId::F { code: "PINNED_CONSUMER_UNTAGGED", subject: "spritzwine".into() },
            ],
            "a dangling selection must re-anchor to the worst finding"
        );

        // The same (code, subject) returns: state changed twice is new
        // evidence -- it must arrive UNticked.
        let back = mixed();
        pane.apply_snapshot(&back);
        assert!(pane.ticks.is_empty());
        let term = draw(&mut pane, &back);
        assert!(
            rows(&term).iter().any(|row| row.contains("[ ] STITCH_STALE")),
            "the reappeared finding must render unticked"
        );
    }
}
