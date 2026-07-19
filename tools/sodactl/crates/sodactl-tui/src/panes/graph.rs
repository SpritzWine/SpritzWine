//! The graph pane: the modelled topology as a tree.
//!
//! Home base at the root, tiers nested by their DECLARED parent edge (the one
//! piece of geometry the manifest owns), stitches and consumers as sibling
//! top-level groups -- they are replayed artifacts, not descendants, and
//! nesting them under a tier would draw exactly the ancestry relationship the
//! checks refuse to reason with (checks/mod.rs rule two). Labels come from
//! `sodactl_core::present`, so a cell can never render differently here than
//! in `sodactl status`.

use crate::gitlog::Context;
use crate::theme;
use crate::worker::Snapshot;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use sodactl_core::finding::{ConsumerState, Severity};
use sodactl_core::graph::Node;
use sodactl_core::manifest::Stitch;
use sodactl_core::present;
use std::collections::HashSet;
use tui_tree_widget::{Tree, TreeItem, TreeState};

/// Node identity, by name -- same identifier-path model as the findings pane
/// (a tree that changes shape under the cursor cannot leave a stale index).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GraphId {
    Home,
    Tier(String),
    Stitches,
    Stitch(String),
    Consumers,
    Consumer(String),
}

/// What the cursor rests on, resolved against the snapshot the caller is
/// drawing -- the pane stores identities, never data (the findings-pane rule).
pub enum GraphSel<'s> {
    Home,
    Tier(&'s Node),
    Stitch(&'s Stitch),
    Consumer(&'s ConsumerState),
    StitchGroup,
    ConsumerGroup,
}

pub struct GraphPane {
    pub state: TreeState<GraphId>,
    /// First-apply flag. The topology is opened wholesale ONCE -- it is small
    /// and the whole point of the pane is seeing it at a glance -- and after
    /// that the user's opens/closes are their own. Unlike findings there is no
    /// per-severity "reappearance" to reopen on: the node set is the manifest,
    /// which only moves when Florian edits it.
    opened_once: bool,
}

impl GraphPane {
    pub fn new() -> Self {
        Self { state: TreeState::default(), opened_once: false }
    }

    /// Reconcile with a fresh snapshot: open everything on first sight,
    /// re-anchor a dangling selection to the root.
    pub fn apply_snapshot(&mut self, snap: &Snapshot) {
        let items = items(snap);
        let mut paths = Vec::new();
        collect_paths(&items, &mut Vec::new(), &mut paths);

        if !self.opened_once {
            for p in &paths {
                self.state.open(p.clone());
            }
            self.opened_once = true;
        }

        // A selection naming a node the new snapshot no longer has (a tier
        // renamed away, a consumer dropped) would render a live tree beside a
        // detail pane about nothing -- re-anchor to the root, which always
        // exists, rather than leaving the dangle to read as "nothing to say".
        let alive = paths.iter().any(|p| p.as_slice() == self.state.selected());
        if !alive {
            self.state.select(vec![GraphId::Home]);
        }
    }

    /// Resolve the selection against the snapshot. `None` only when the
    /// referent vanished mid-frame (apply re-anchors on the next snapshot).
    pub fn selection<'s>(&self, snap: &'s Snapshot) -> Option<GraphSel<'s>> {
        match self.state.selected() {
            [] => None,
            [GraphId::Home] => Some(GraphSel::Home),
            [GraphId::Stitches] => Some(GraphSel::StitchGroup),
            [GraphId::Consumers] => Some(GraphSel::ConsumerGroup),
            [.., GraphId::Tier(t)] => {
                snap.dag.nodes.iter().find(|n| &n.name == t).map(GraphSel::Tier)
            }
            [.., GraphId::Stitch(s)] => {
                snap.manifest.stitch.iter().find(|x| &x.name == s).map(GraphSel::Stitch)
            }
            [.., GraphId::Consumer(c)] => {
                snap.report.consumers.iter().find(|x| &x.name == c).map(GraphSel::Consumer)
            }
            _ => None,
        }
    }

    /// Enter: the log context this selection means. Groups and the root mean
    /// "show me everything" -- Overview -- rather than nothing.
    pub fn log_context(&self) -> Context {
        match self.state.selected() {
            [.., GraphId::Tier(t)] => Context::Tier(t.clone()),
            [.., GraphId::Stitch(s)] => Context::Stitch(s.clone()),
            [.., GraphId::Consumer(c)] => Context::Consumer(c.clone()),
            _ => Context::Overview,
        }
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, snap: &Snapshot, focused: bool) {
        let items = items(snap);
        let tree = Tree::new(&items)
            .expect("top-level identifiers are unique: Home/Stitches/Consumers variants")
            // Tab words in the title: the bracketed one is on screen. The
            // log pane titles itself "graph / [log]" -- the pair is the whole
            // tab bar, no separate widget to drift.
            .block(theme::pane_block("[graph] / log", focused))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(tree, area, &mut self.state);
    }
}

/// The tier label: name, tip, own-commit count, and the parent-tip verdict --
/// `present::parent_tip_cell`'s exact words, so "NO -- stale" and the honest
/// `?` render identically to the status table. A tier stale only transitively
/// (its own edge is fine, an ancestor moved) says so in words: the marker must
/// distinguish "you moved" from "everything under your parent is unshippable".
fn tier_label(n: &Node) -> Line<'static> {
    let mut spans = vec![
        Span::styled(n.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {}", present::short_opt(&n.tip))),
        Span::raw(format!(
            "  {} own",
            n.own_commits.map(|c| c.to_string()).unwrap_or_else(|| "-".into())
        )),
    ];
    let stale_style = theme::severity_style(Severity::Drift);
    match (n.on_parent_tip, n.needs_restack) {
        (Some(false), _) => {
            spans.push(Span::styled(
                format!("  {}", present::parent_tip_cell(Some(false))),
                stale_style,
            ));
        }
        // The propagated case: correct on its parent, but an ancestor moved.
        (_, true) => spans.push(Span::styled("  STALE (inherited)".to_string(), stale_style)),
        (verdict, false) => {
            spans.push(Span::raw(format!("  on tip: {}", present::parent_tip_cell(verdict))));
        }
    }
    Line::from(spans)
}

/// The consumer label. The pin cell is IN-TREE deliberately: a STAGED
/// divergence (an uncommitted gitlink move, one `git commit` from an
/// unfetchable pointer) must be visible without opening the detail pane.
fn consumer_label(c: &ConsumerState) -> Line<'static> {
    let pin = present::pin_cell(&c.pin);
    // The loud pin forms get the loud style. Keyed off the rendered cell, not
    // a re-derivation of its match: `pin_cell` is the single authority on what
    // counts as loud, and re-implementing its STAGED test here is how the two
    // would drift.
    let pin_style = if pin.contains("STAGED") || pin.contains("UNKNOWN") {
        theme::severity_style(Severity::Drift)
    } else {
        Style::default()
    };
    let cur = present::current_cell(c.current);
    let cur_style = if cur == "STALE" {
        theme::severity_style(Severity::Drift)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(c.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {}", present::short_opt(&c.oid))),
        Span::raw("  pin ".to_string()),
        Span::styled(pin, pin_style),
        Span::raw("  ".to_string()),
        Span::styled(cur, cur_style),
    ])
}

/// The stitch label is manifest facts only -- name, base, replay order. The
/// tool derives no per-stitch state that is not already a finding
/// (STITCH_STALE et al.), and inventing a green checkmark here from nothing
/// would be a fabricated verdict.
fn stitch_label(s: &Stitch) -> Line<'static> {
    let mut label = format!("{}  <- {}  [{}]", s.name, s.base_tier, s.order.join(" + "));
    if s.local_only {
        label.push_str("  local-only");
    }
    Line::from(label)
}

/// Build the whole tree. A free function over the snapshot rather than a
/// method: everything here is derived per-render from snapshot data, and
/// keeping it stateless is what lets `apply_snapshot` reuse it for liveness.
fn items(snap: &Snapshot) -> Vec<TreeItem<'static, GraphId>> {
    let hb = &snap.manifest.repo.home_base;

    // parent -> children, from the DERIVED dag (never the manifest directly:
    // the dag already resolved the edges and carries the live per-node state).
    let mut visited: HashSet<&str> = HashSet::new();
    let roots: Vec<TreeItem<'static, GraphId>> = snap
        .dag
        .nodes
        .iter()
        .filter(|n| n.parent == "$home_base")
        .filter_map(|n| build_tier(n, &snap.dag.nodes, &mut visited))
        .collect();

    // Tiers the walk from the roots never reached: a typo'd parent or a
    // parent cycle in a hand-edited manifest. Silently dropping them from the
    // picture is the exact breakdown class this tool forbids -- append them at
    // top level with the reason in words.
    let orphans: Vec<TreeItem<'static, GraphId>> = snap
        .dag
        .nodes
        .iter()
        // `insert` is the reached-check AND the mark in one: false for
        // anything the root walk built, and for the second sighting of a
        // cycle member (A -> B -> A emits each once).
        .filter(|n| visited.insert(&n.name))
        .map(|n| {
            let mut l = tier_label(n);
            l.push_span(Span::styled(
                format!("  parent `{}` UNREACHABLE", n.parent),
                theme::severity_style(Severity::Drift),
            ));
            TreeItem::new_leaf(GraphId::Tier(n.name.clone()), l)
        })
        .collect();

    let mut home_children = roots;
    home_children.extend(orphans);
    let home = TreeItem::new(
        GraphId::Home,
        Line::from(format!(
            "home base  {}  {}",
            present::shorten(&hb.commit),
            hb.describes
        )),
        home_children,
    )
    .expect("tier names are sibling-unique: deduplicated in build_tier's visited set");

    let mut top = vec![home];

    // An absent group is absent, not "(0)" -- the findings-pane rationale: a
    // permanent empty header trains the eye to skip the word.
    if !snap.manifest.stitch.is_empty() {
        let leaves: Vec<_> = snap
            .manifest
            .stitch
            .iter()
            .map(|s| TreeItem::new_leaf(GraphId::Stitch(s.name.clone()), stitch_label(s)))
            .collect();
        top.push(
            TreeItem::new(
                GraphId::Stitches,
                Line::from(format!("stitches ({})", leaves.len())),
                leaves,
            )
            .expect("stitch names are unique in a parseable manifest"),
        );
    }
    if !snap.report.consumers.is_empty() {
        let leaves: Vec<_> = snap
            .report
            .consumers
            .iter()
            .map(|c| TreeItem::new_leaf(GraphId::Consumer(c.name.clone()), consumer_label(c)))
            .collect();
        top.push(
            TreeItem::new(
                GraphId::Consumers,
                Line::from(format!("consumers ({})", leaves.len())),
                leaves,
            )
            .expect("consumer names are unique in a parseable manifest"),
        );
    }
    top
}

/// One tier and its subtree. The visited set is the cycle guard AND the
/// sibling-unique guard: a name already built is never built again, so a
/// cyclic or duplicated manifest yields a finite tree instead of a stack
/// overflow or a `Tree::new` refusal.
fn build_tier<'n>(
    n: &'n Node,
    all: &'n [Node],
    visited: &mut HashSet<&'n str>,
) -> Option<TreeItem<'static, GraphId>> {
    if !visited.insert(&n.name) {
        return None;
    }
    let children: Vec<_> = all
        .iter()
        .filter(|c| c.parent == n.name)
        .filter_map(|c| build_tier(c, all, visited))
        .collect();
    Some(
        TreeItem::new(GraphId::Tier(n.name.clone()), tier_label(n), children)
            .expect("children deduplicated through the visited set"),
    )
}

/// Every identifier path in the tree, for open-all and selection liveness.
fn collect_paths(
    items: &[TreeItem<'static, GraphId>],
    prefix: &mut Vec<GraphId>,
    out: &mut Vec<Vec<GraphId>>,
) {
    for item in items {
        prefix.push(item.identifier().clone());
        out.push(prefix.clone());
        collect_paths(item.children(), prefix, out);
        prefix.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_snapshot;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use sodactl_core::graph::{Dag, Edge, Node};
    use sodactl_git::Pin;

    fn node(
        name: &str,
        parent: &str,
        own: u32,
        on_tip: Option<bool>,
        needs_restack: bool,
    ) -> Node {
        Node {
            name: name.into(),
            // Tip derived from the own-commit count: distinct per fixture
            // node and readable in the buffer snapshots.
            tip: Some(own.to_string().repeat(40).chars().take(40).collect()),
            parent: parent.into(),
            parent_ref: if parent == "$home_base" { "1bb0da91a5e".into() } else { parent.into() },
            parent_tip: Some("f".repeat(40)),
            own_commits: Some(own),
            on_parent_tip: on_tip,
            needs_restack,
        }
    }

    /// The stale chain: soda-core moved off soda-build's tip (direct), so
    /// soda-ntdll -- itself correctly stacked -- is stale by inheritance.
    fn stale_snapshot() -> Snapshot {
        let mut snap = test_snapshot();
        snap.dag = Dag {
            nodes: vec![
                node("soda-build", "$home_base", 2, Some(true), false),
                node("soda-core", "soda-build", 5, Some(false), true),
                node("soda-ntdll", "soda-core", 3, Some(true), true),
            ],
            edges: vec![
                Edge { parent: "soda-build".into(), child: "soda-core".into() },
                Edge { parent: "soda-core".into(), child: "soda-ntdll".into() },
            ],
            restack_order: vec!["soda-core".into(), "soda-ntdll".into()],
        };
        snap.report.consumers = vec![ConsumerState {
            name: "spritzwine".into(),
            oid: Some("d3902a7f753".into()),
            base: "wine-11.13".into(),
            soda_tip: "soda-all".into(),
            current: Some(false),
            pinned_by_superproject: true,
            pin: Some(Pin::Known {
                committed: Some("e3a386db584aaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                staged: Some("39fe08f1befbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            }),
        }];
        snap
    }

    fn draw(pane: &mut GraphPane, snap: &Snapshot) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(84, 14)).unwrap();
        term.draw(|f| pane.render(f, f.area(), snap, false)).unwrap();
        term
    }

    /// The one snapshot the plan calls for: the stale chain, with the direct
    /// offender saying "NO -- stale", the descendant saying "STALE
    /// (inherited)", and the consumer's STAGED pin divergence visible in-tree.
    #[test]
    fn stale_chain_renders_direct_and_inherited_markers_and_the_staged_pin() {
        let mut pane = GraphPane::new();
        let snap = stale_snapshot();
        pane.apply_snapshot(&snap);
        insta::assert_snapshot!(draw(&mut pane, &snap).backend());
    }

    #[test]
    fn first_apply_opens_the_topology_and_selects_the_root() {
        let mut pane = GraphPane::new();
        let snap = stale_snapshot();
        pane.apply_snapshot(&snap);
        assert_eq!(pane.state.selected(), &[GraphId::Home]);
        // Deep nesting visible without a keypress: the inherited-stale leaf
        // sits two levels down and must already be on screen.
        let term = draw(&mut pane, &snap);
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol()).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("soda-ntdll"), "{all}");
        assert!(all.contains("STALE (inherited)"), "{all}");
        assert!(all.contains("STAGED"), "the pin divergence must be in-tree:\n{all}");
    }

    #[test]
    fn a_vanished_selection_reanchors_to_home() {
        let mut pane = GraphPane::new();
        let snap = stale_snapshot();
        pane.apply_snapshot(&snap);
        pane.state.select(vec![
            GraphId::Home,
            GraphId::Tier("soda-build".into()),
            GraphId::Tier("soda-core".into()),
        ]);
        assert!(matches!(pane.selection(&snap), Some(GraphSel::Tier(n)) if n.name == "soda-core"));

        let mut without = stale_snapshot();
        without.dag.nodes.retain(|n| n.name != "soda-core");
        without.dag.nodes[1].parent = "soda-build".into(); // keep ntdll reachable
        pane.apply_snapshot(&without);
        assert_eq!(pane.state.selected(), &[GraphId::Home]);
        // ...and a user-closed group is NOT reopened by the second apply.
        assert!(pane.opened_once);
    }

    /// A typo'd parent must not silently drop the tier from the picture.
    #[test]
    fn an_orphan_tier_renders_with_the_reason_in_words() {
        let mut snap = test_snapshot();
        snap.dag.nodes = vec![node("soda-lost", "soda-typo", 4, Some(true), false)];
        let mut pane = GraphPane::new();
        pane.apply_snapshot(&snap);
        let term = draw(&mut pane, &snap);
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol()).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("soda-lost"), "{all}");
        assert!(all.contains("UNREACHABLE"), "{all}");
    }

    #[test]
    fn enter_contexts_follow_the_selection() {
        let mut pane = GraphPane::new();
        let snap = stale_snapshot();
        pane.apply_snapshot(&snap);
        assert_eq!(pane.log_context(), Context::Overview);
        pane.state.select(vec![GraphId::Home, GraphId::Tier("soda-build".into())]);
        assert_eq!(pane.log_context(), Context::Tier("soda-build".into()));
        pane.state
            .select(vec![GraphId::Consumers, GraphId::Consumer("spritzwine".into())]);
        assert_eq!(pane.log_context(), Context::Consumer("spritzwine".into()));
    }
}
