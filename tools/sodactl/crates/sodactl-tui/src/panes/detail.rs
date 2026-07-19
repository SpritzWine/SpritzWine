//! The detail pane: context-following prose for whatever is selected.
//!
//! Line builders are pure -- they return `Vec<Line>` and never touch a
//! terminal -- so the content contract ("the remedy is always there", "a
//! missing catalogue entry is STATED") is testable as strings, not buffer
//! diffs. `render` only wraps and frames. The graph-side builders (tier,
//! stitch, consumer, home base) speak `present::` cells for the same reason
//! the tree labels do: one rendering per tri-state, everywhere.

use crate::theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;
use sodactl_core::explain;
use sodactl_core::finding::{ConsumerState, Finding, Severity};
use sodactl_core::graph::Node;
use sodactl_core::manifest::{HomeBase, Stitch};
use sodactl_core::present;

/// A labelled catalogue section: underlined header, prose, breathing room.
fn section(out: &mut Vec<Line<'static>>, title: &'static str, prose: &'static str) {
    out.push(Line::styled(title, Style::default().add_modifier(Modifier::UNDERLINED)));
    out.push(Line::from(prose));
    out.push(Line::from(""));
}

/// The finding branch: headline, summary, remedy, then the catalogue prose.
pub fn finding_lines(fd: &Finding) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(vec![
            Span::styled(fd.code, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(fd.severity.label(), theme::severity_style(fd.severity)),
            Span::raw("  "),
            Span::raw(fd.subject.clone()),
        ]),
        Line::from(""),
        Line::from(fd.summary.clone()),
        Line::from(""),
        // The copy-paste line: the finding's OWN remedy, exact for this
        // subject, set off bold under its own label so the eye lands on it
        // before the prose. Distinct from the catalogue's remedy below, which
        // is general and includes what NOT to do -- both render on purpose.
        Line::styled("remedy", Style::default().add_modifier(Modifier::UNDERLINED)),
        Line::styled(fd.remedy.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Line::from(""),
    ];
    match explain::lookup(fd.code) {
        Some(doc) => {
            section(&mut out, "what this means", doc.means);
            section(&mut out, "why it matters", doc.why_it_matters);
            section(&mut out, "how to clear it", doc.remedy);
        }
        // A code the catalogue does not know is a build-discipline failure
        // (the entry lands in the same commit as the emitter). Render the gap
        // out loud -- an omitted section reads as "nothing more to say", which
        // is a different and wrong claim.
        None => out.push(Line::from(format!("no catalogue entry for {}", fd.code))),
    }
    out
}

/// A severity group is selected: say what it is and how to go deeper, rather
/// than leaving the pane at its previous content or blank.
pub fn group_lines(sev: Severity, count: usize) -> Vec<Line<'static>> {
    vec![
        Line::styled(format!("{} ({count})", sev.label()), theme::severity_style(sev)),
        Line::from(""),
        Line::from("select a finding beneath for its remedy and catalogue prose"),
    ]
}

/// Nothing is selected. Two distinct truths, each said explicitly.
pub fn nothing_lines(report_is_empty: bool) -> Vec<Line<'static>> {
    vec![Line::from(if report_is_empty {
        "no findings -- nothing to detail"
    } else {
        "no finding selected"
    })]
}

/// A `label  value` fact row, label dimmed so the values column reads first.
fn fact(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<14}"), theme::ticked_style()),
        Span::raw(value),
    ])
}

/// The literal "where to do what next": tiers needing a restack, parents
/// strictly before children (`graph::Dag::restack_order` -- order is the
/// whole value there, so it is repeated verbatim, never re-sorted here).
/// Appended to every tier-side detail while non-empty, because the reader
/// looking at ANY stale tier needs the sequence, not just the verdict.
fn restack_section(out: &mut Vec<Line<'static>>, order: &[String]) {
    if order.is_empty() {
        return;
    }
    out.push(Line::from(""));
    out.push(Line::styled(
        "restack in this order",
        Style::default().add_modifier(Modifier::UNDERLINED),
    ));
    out.push(Line::styled(
        order.join(" -> "),
        Style::default().add_modifier(Modifier::BOLD),
    ));
}

/// The home-base root: the one constant that is NOT derivable, shown with its
/// `describes`/`derivation` so a reader can tell a deliberate bump from a typo
/// (the manifest stores them for exactly that reason).
pub fn home_lines(hb: &HomeBase, restack_order: &[String]) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(Span::styled("home base", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
        // Full oid, not shortened: this is the copy-paste anchor for every
        // range in the family.
        fact("commit", hb.commit.clone()),
        fact("describes", hb.describes.clone()),
        fact("derivation", hb.derivation.clone()),
    ];
    restack_section(&mut out, restack_order);
    out
}

/// A tier: the full derived state behind the tree label's summary cells.
pub fn tier_lines(n: &Node, restack_order: &[String]) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(vec![
            Span::styled(n.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  tier"),
        ]),
        Line::from(""),
        fact("tip", present::short_opt(&n.tip)),
        // Declared edge AND what it resolved to: `$home_base` is a manifest
        // convention, and showing only the substituted commit would hide
        // which of the two kinds of edge this is.
        fact(
            "parent",
            if n.parent == n.parent_ref {
                n.parent.clone()
            } else {
                format!("{}  = {}", n.parent, present::shorten(&n.parent_ref))
            },
        ),
        fact("parent tip", present::short_opt(&n.parent_tip)),
        fact(
            "own commits",
            n.own_commits.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
        ),
        fact("on parent tip", present::parent_tip_cell(n.on_parent_tip).to_string()),
    ];
    if n.needs_restack && n.on_parent_tip != Some(false) {
        // The propagated verdict, in words: this tier did nothing wrong and
        // is unshippable anyway -- the blast-radius fact the dag exists to
        // surface (see `graph::Node::needs_restack`).
        out.push(Line::styled(
            "stale by inheritance: an ancestor tier moved",
            theme::severity_style(Severity::Drift),
        ));
    }
    restack_section(&mut out, restack_order);
    out
}

/// A stitch: declared facts only -- name, base, replay order (order is
/// load-bearing: overlapping tiers replay in sequence or conflict).
pub fn stitch_lines(s: &Stitch) -> Vec<Line<'static>> {
    let mut out = vec![
        Line::from(vec![
            Span::styled(s.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  stitch"),
        ]),
        Line::from(""),
        fact("base tier", s.base_tier.clone()),
        fact("order", s.order.join(" -> ")),
    ];
    if s.local_only {
        out.push(fact("remote", "local-only by declaration".into()));
    }
    out
}

/// A consumer: the full state including the pin -- committed AND staged sides,
/// because the tip and the gitlink are different commits by design and a
/// reader eyeballing a pin problem needs both (`present::pin_cell`).
pub fn consumer_lines(c: &ConsumerState) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled(c.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  consumer"),
        ]),
        Line::from(""),
        fact("tip", present::short_opt(&c.oid)),
        fact("base", c.base.clone()),
        fact("soda tip", c.soda_tip.clone()),
        fact("current", present::current_cell(c.current).to_string()),
        fact("pin", present::pin_cell(&c.pin)),
        fact(
            "pinned",
            if c.pinned_by_superproject {
                "yes -- the superproject records this consumer's gitlink; it must stay tagged".into()
            } else {
                "no".into()
            },
        ),
    ]
}

/// The stitch/consumer group headers: say what the group is, like
/// `group_lines` does for severities.
pub fn graph_group_lines(what: &'static str, count: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("{what} ({count})")),
        Line::from(""),
        Line::from("select an entry beneath for its full state"),
    ]
}

/// The log cursor: the commit under it, or the explicit reason there is none
/// -- two different truths ("between commits" vs "the mapping is down") that
/// must not share one blank rendering. `tig` names the tool Enter will
/// actually suspend into (the startup probe's verdict): "inspect" alone would
/// leave the user to discover which pager owns their terminal by surprise.
pub fn commit_lines(oid: Option<&str>, mapping_broken: bool, tig: bool) -> Vec<Line<'static>> {
    match oid {
        Some(oid) => vec![
            fact("commit", oid.to_string()),
            Line::from(""),
            Line::from(if tig {
                "Enter to inspect -- suspends into `tig show` (q returns)"
            } else {
                "Enter to inspect -- suspends into `git show` (tig not on PATH; q returns)"
            }),
        ],
        None if mapping_broken => vec![Line::from(
            "no commit selectable -- the line->commit mapping failed (see the log pane)",
        )],
        None => vec![Line::from("no commit under the cursor")],
    }
}

pub fn render(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    // trim: false -- the remedies are commands, and wrapping must not eat the
    // leading whitespace of a continuation the user will copy.
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title("detail")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line]) -> String {
        lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn finding_detail_renders_remedy_and_all_three_catalogue_sections() {
        let fd = Finding::new(
            "TIER_CHILD_STALE",
            Severity::Drift,
            "soda-ntdll",
            "not on soda-core's tip",
            "restack soda-ntdll onto soda-core",
        );
        let text = text_of(&finding_lines(&fd));
        // The finding's own remedy -- the copy-paste line -- is present...
        assert!(text.contains("restack soda-ntdll onto soda-core"), "{text}");
        // ...and so is the full catalogue triple for the code.
        assert!(text.contains("what this means"), "{text}");
        assert!(text.contains("why it matters"), "{text}");
        assert!(text.contains("how to clear it"), "{text}");
        // A phrase from the catalogue prose proves lookup is wired, not
        // just headers over empty sections.
        assert!(text.contains("central defect"), "{text}");
    }

    #[test]
    fn missing_catalogue_entry_is_stated_not_omitted() {
        let fd = Finding::new("NOT_A_CODE", Severity::Hygiene, "x", "s", "r");
        let text = text_of(&finding_lines(&fd));
        assert!(text.contains("no catalogue entry for NOT_A_CODE"), "{text}");
    }

    #[test]
    fn group_and_nothing_lines_say_their_state() {
        assert!(text_of(&group_lines(Severity::Blocker, 2)).contains("BLOCKER (2)"));
        assert!(text_of(&nothing_lines(true)).contains("no findings"));
        assert!(text_of(&nothing_lines(false)).contains("no finding selected"));
    }

    #[test]
    fn tier_detail_states_inheritance_and_the_restack_order() {
        let n = Node {
            name: "soda-ntdll".into(),
            tip: Some("39fe08f1bef0000000000000000000000000000a".into()),
            parent: "soda-core".into(),
            parent_ref: "soda-core".into(),
            parent_tip: Some("aaaa1111222233334444555566667777888899aa".into()),
            own_commits: Some(3),
            // Correct on its own edge, stale anyway: the propagated case.
            on_parent_tip: Some(true),
            needs_restack: true,
        };
        let text = text_of(&tier_lines(&n, &["soda-core".into(), "soda-ntdll".into()]));
        assert!(text.contains("on parent tip"), "{text}");
        assert!(text.contains("stale by inheritance"), "{text}");
        // The literal where-to-do-what-next, order preserved verbatim.
        assert!(text.contains("restack in this order"), "{text}");
        assert!(text.contains("soda-core -> soda-ntdll"), "{text}");
    }

    #[test]
    fn tier_detail_shows_both_sides_of_a_home_base_edge() {
        let n = Node {
            name: "soda-core".into(),
            tip: Some("bbbb1111222233334444555566667777888899bb".into()),
            parent: "$home_base".into(),
            parent_ref: "1bb0da91a5e1234567890abcdef1234567890abc".into(),
            parent_tip: Some("1bb0da91a5e1234567890abcdef1234567890abc".into()),
            own_commits: None,
            on_parent_tip: None,
            needs_restack: false,
        };
        let text = text_of(&tier_lines(&n, &[]));
        // The declared convention AND the resolved commit, side by side.
        assert!(text.contains("$home_base"), "{text}");
        assert!(text.contains("1bb0da91a5e"), "{text}");
        // The could-not-determine renderings, out loud.
        assert!(text.contains('?'), "{text}");
        assert!(!text.contains("restack in this order"), "clean order must not render: {text}");
    }

    #[test]
    fn consumer_detail_carries_the_staged_pin_and_the_pinned_consequence() {
        let c = ConsumerState {
            name: "spritzwine".into(),
            oid: Some("d3902a7f753000000000000000000000000000dd".into()),
            base: "wine-11.13".into(),
            soda_tip: "soda-all".into(),
            current: None,
            pinned_by_superproject: true,
            pin: Some(sodactl_git::Pin::Known {
                committed: Some("e3a386db584aaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                staged: Some("39fe08f1befbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            }),
        };
        let text = text_of(&consumer_lines(&c));
        assert!(text.contains("STAGED 39fe08f1bef"), "{text}");
        assert!(text.contains("unknown"), "currency tri-state must render its None arm: {text}");
        assert!(text.contains("must stay tagged"), "{text}");
    }

    #[test]
    fn stitch_home_group_and_commit_lines_say_their_facts() {
        let s = Stitch {
            name: "soda-all".into(),
            base_tier: "soda-core".into(),
            order: vec!["soda-ntdll".into(), "soda-fsync".into()],
            local_only: true,
        };
        let text = text_of(&stitch_lines(&s));
        assert!(text.contains("soda-ntdll -> soda-fsync"), "order is load-bearing: {text}");
        assert!(text.contains("local-only"), "{text}");

        let hb = HomeBase {
            commit: "1bb0da91a5e1234567890abcdef1234567890abc".into(),
            describes: "spritzwine-11.13 fork point".into(),
            derivation: "git merge-base is WRONG here; recorded by hand".into(),
        };
        let text = text_of(&home_lines(&hb, &["soda-core".into()]));
        assert!(text.contains("1bb0da91a5e1234567890abcdef1234567890abc"), "{text}");
        assert!(text.contains("restack in this order"), "{text}");

        assert!(text_of(&graph_group_lines("stitches", 2)).contains("stitches (2)"));
        assert!(text_of(&commit_lines(Some("abc123"), false, true)).contains("abc123"));
        // The tool Enter will open is NAMED, per the probe's verdict.
        assert!(text_of(&commit_lines(Some("abc123"), false, true)).contains("tig show"));
        assert!(text_of(&commit_lines(Some("abc123"), false, false)).contains("git show"));
        assert!(text_of(&commit_lines(None, false, true)).contains("no commit under the cursor"));
        assert!(text_of(&commit_lines(None, true, true)).contains("mapping failed"));
    }
}
