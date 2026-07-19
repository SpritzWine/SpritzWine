//! The help overlay, rendered FROM `keys::BINDINGS`.
//!
//! No hand-written key list: every row is a table entry, and the key names
//! are formatted from the combinations the event loop actually matches
//! (`crokey::KeyCombinationFormat`), so a rebinding shows up here without
//! anyone remembering to update prose. That is the whole Stage-7 contract:
//! help that CANNOT drift from the bindings.

use crate::keys;
use crokey::KeyCombinationFormat;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

/// Lifted verbatim from gitui `src/ui/mod.rs` (`centered_rect`, MIT license,
/// gitui-org/gitui), reformatted only: a rect of the given percentages,
/// centered in `r` by symmetric margins.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(popup_layout[1])[1]
}

/// One line per table row: right-aligned key names, then the help text. Pure,
/// so the anti-drift claim is testable as strings.
fn lines() -> Vec<Line<'static>> {
    let fmt = KeyCombinationFormat::default();
    let mut out = Vec::with_capacity(keys::BINDINGS.len() + 2);
    for b in keys::BINDINGS {
        let combos =
            b.keys.iter().map(|k| fmt.to_string(*k)).collect::<Vec<_>>().join(" / ");
        out.push(Line::from(vec![
            // 14 fits the widest current combo pair; a wider future one
            // overflows visibly rather than truncating.
            Span::styled(format!("{combos:>14}  "), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(b.help),
        ]));
    }
    out.push(Line::from(""));
    out.push(Line::from("any other key closes this help"));
    out
}

/// Draw the overlay over the whole frame. `Clear` first: without it the
/// underlying panes bleed through every cell the paragraph leaves blank.
/// 80/80: sized so the full table fits an 80x24 terminal -- and wrapped, so a
/// row that outgrows the width someday breaks its alignment VISIBLY instead
/// of truncating its help text (a help row silently cut mid-sentence is this
/// project's least favourite failure).
pub fn render(f: &mut Frame, area: Rect) {
    let rect = centered_rect(80, 80, area);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines())
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::bordered().title("help")),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// The overlay, pinned: every binding row present, keys formatted from the
    /// combinations. A rebinding in keys.rs fails THIS snapshot -- which is
    /// the anti-drift property demonstrated, not just claimed.
    #[test]
    fn overlay_renders_every_binding_row() {
        // 80x24 -- the smallest terminal worth designing for; every row and
        // the closing hint must fit it UNWRAPPED (wrapping is the render's
        // overflow behaviour, not its normal one; the string test below and
        // this snapshot both fail if a help text outgrows the width).
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| render(f, f.area())).unwrap();
        insta::assert_snapshot!(term.backend());
    }

    /// The structural claim as strings: one rendered line per table row, and
    /// each row contains its own help text -- no row can be skipped or
    /// reordered without this failing.
    #[test]
    fn every_table_row_reaches_the_overlay_in_order() {
        let rendered = lines();
        for (i, b) in keys::BINDINGS.iter().enumerate() {
            let line = rendered[i].to_string();
            assert!(line.contains(b.help), "row {i} must carry its help text: {line}");
            // 62 = the overlay's usable width on 80 cols (80% minus borders).
            // A longer row wraps on the smallest supported terminal, which is
            // the overflow path, not the design -- trim the help text instead.
            assert!(line.chars().count() <= 62, "row {i} outgrew an 80-col terminal: {line}");
        }
    }
}
