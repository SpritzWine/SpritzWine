//! The log pane: git's own coloured `--graph` output, line-selectable.
//!
//! ansi-to-tui turns the raw ANSI into ratatui `Text`, so git remains the sole
//! author of lane layout and colour. This pane owns only selection and scroll;
//! which commit a line means comes from `gitlog::LogView`'s mapping, and when
//! that mapping is broken the pane says so and pages plainly -- selection is
//! Stage 6's `git show` input, and a selection that might name the wrong
//! commit is worse than none.

use crate::gitlog::LogView;
use crate::theme;
use ansi_to_tui::IntoText;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use sodactl_core::finding::Severity;

/// Lifted verbatim from gitui `src/ui/mod.rs` (`calc_scroll_top`, MIT
/// license, gitui-org/gitui): keep `selection` inside the `height`-line
/// window starting at `current_top`, moving the window only when the
/// selection leaves it.
const fn calc_scroll_top(current_top: usize, height_in_lines: usize, selection: usize) -> usize {
    if current_top.saturating_add(height_in_lines) <= selection {
        selection.saturating_sub(height_in_lines) + 1
    } else if current_top > selection {
        selection
    } else {
        current_top
    }
}

pub struct LogPane {
    /// Selected LINE (not commit): continuation lines select their commit via
    /// the mapping, so the cursor never skips or teleports.
    pub selected: usize,
    top: usize,
}

impl LogPane {
    pub fn new() -> Self {
        Self { selected: 0, top: 0 }
    }

    /// Clamp against a fresh snapshot: the text under the cursor changed
    /// wholesale (new range, new commits), so the only invariant worth keeping
    /// is "the cursor points at a line that exists".
    pub fn apply_snapshot(&mut self, log: &Result<LogView, String>) {
        let lines = log.as_ref().map(|v| v.text.lines().count()).unwrap_or(0);
        self.selected = self.selected.min(lines.saturating_sub(1));
    }

    pub fn key_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn key_down(&mut self, log: &Result<LogView, String>) {
        let lines = log.as_ref().map(|v| v.text.lines().count()).unwrap_or(0);
        self.selected = (self.selected + 1).min(lines.saturating_sub(1));
    }

    /// The commit under the cursor, when the mapping vouches for one.
    pub fn selected_oid<'v>(&self, view: &'v LogView) -> Option<&'v str> {
        view.line_oids.get(self.selected).and_then(|o| o.as_deref())
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, log: &Result<LogView, String>, focused: bool) {
        let block = theme::pane_block("graph / [log]", focused);
        let view = match log {
            Ok(v) => v,
            // The log failed but the snapshot landed: the pane is the error.
            // Words first ("LOG FAILED"), colour second -- and the message
            // carries the exact command line (gitlog::exec builds it so).
            Err(e) => {
                f.render_widget(
                    Paragraph::new(Line::styled(
                        format!("LOG FAILED -- {e}"),
                        theme::severity_style(Severity::Blocker),
                    ))
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(block),
                    area,
                );
                return;
            }
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        // Caption first: which context this text IS, in words, so a stale
        // frame during a context-switch refresh can never be misread.
        lines.push(Line::styled(
            view.title.clone(),
            Style::default().add_modifier(Modifier::UNDERLINED),
        ));
        if let Some(err) = &view.mapping_error {
            lines.push(Line::styled(err.clone(), theme::severity_style(Severity::Drift)));
        }
        let header = lines.len();

        if view.text.is_empty() {
            // An empty range is a fact ("nothing replayed here"), and a blank
            // pane under a border is indistinguishable from a failed draw.
            lines.push(Line::from("no commits in this range"));
        } else {
            match view.text.into_text() {
                Ok(text) => lines.extend(text.lines),
                // Unparseable ANSI: show the parse error, never a blank pane.
                Err(e) => lines.push(Line::styled(
                    format!("could not parse git's ANSI output: {e}"),
                    theme::severity_style(Severity::Blocker),
                )),
            }
        }

        // Highlight the selected line -- only when the mapping is trusted;
        // in plain-pager mode a highlight would claim a selection Stage 6
        // could act on.
        if view.mapping_error.is_none() {
            if let Some(line) = lines.get_mut(header + self.selected) {
                // Span styles, not `Line::style`: ansi-to-tui emits explicit
                // `Reset` colours in its spans, and span styles PATCH over the
                // line style -- a line-level repaint is silently reset away on
                // exactly the plain lines it should be most visible on. And a
                // hard black-on-white rather than REVERSED, which can vanish
                // over git's own colour runs.
                let sel = Style::default().bg(Color::White).fg(Color::Black);
                for span in &mut line.spans {
                    span.style = sel;
                }
            }
        }

        // Scroll keeps the selection visible under the header lines; the
        // window math is gitui's (attributed above).
        let height = area.height.saturating_sub(2) as usize; // borders
        let visible = height.saturating_sub(header);
        self.top = if visible == 0 { 0 } else { calc_scroll_top(self.top, visible, self.selected) };

        f.render_widget(
            Paragraph::new(lines).scroll((self.top as u16, 0)).block(block),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn view(text: &str, oids: &[Option<&str>], mapping_error: Option<&str>) -> LogView {
        LogView {
            title: "soda-ntdll  (soda-core..soda-ntdll)".into(),
            text: text.into(),
            line_oids: oids.iter().map(|o| o.map(str::to_string)).collect(),
            mapping_error: mapping_error.map(str::to_string),
        }
    }

    fn draw(pane: &mut LogPane, log: &Result<LogView, String>) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(64, 10)).unwrap();
        term.draw(|f| pane.render(f, f.area(), log, false)).unwrap();
        term
    }

    /// Canned ANSI through the real render path: colours parsed, title
    /// captioned, selected line repainted.
    #[test]
    fn canned_ansi_renders_with_title_and_selection() {
        let text = "* \u{1b}[33mcommit aaaaaaaa94d5eb2f423a1d194ce67dae0f5db878\u{1b}[m\n\
                    |     ntdll: port the alert path\n\
                    * \u{1b}[33mcommit bbbbbbbb0714b716e10742250037ce0797712d9c\u{1b}[m";
        let v = view(
            text,
            &[
                Some("aaaaaaaa94d5eb2f423a1d194ce67dae0f5db878"),
                Some("aaaaaaaa94d5eb2f423a1d194ce67dae0f5db878"),
                Some("bbbbbbbb0714b716e10742250037ce0797712d9c"),
            ],
            None,
        );
        let mut pane = LogPane::new();
        pane.key_down(&Ok(view(text, &[], None))); // select the continuation line
        let term = draw(&mut pane, &Ok(v));
        // The buffer snapshot shows symbols only, so the selection repaint is
        // asserted on a cell: the selected line's fg/bg swap must be there.
        let cell = term.backend().buffer().cell((8, 3)).unwrap(); // inside "ntdll: port"
        assert_eq!(cell.bg, Color::White, "the selected line must be repainted");
        insta::assert_snapshot!(term.backend());
    }

    #[test]
    fn selection_resolves_through_the_mapping() {
        let v = view(
            "a\nb\nc",
            &[Some("aaaa11112222"), Some("aaaa11112222"), Some("cccc33334444")],
            None,
        );
        let mut pane = LogPane::new();
        assert_eq!(pane.selected_oid(&v), Some("aaaa11112222"));
        pane.key_down(&Ok(view("a\nb\nc", &[], None)));
        assert_eq!(pane.selected_oid(&v), Some("aaaa11112222"), "continuation -> commit above");
        pane.key_down(&Ok(view("a\nb\nc", &[], None)));
        assert_eq!(pane.selected_oid(&v), Some("cccc33334444"));
        // Clamped at the end: no phantom line below the text.
        pane.key_down(&Ok(view("a\nb\nc", &[], None)));
        assert_eq!(pane.selected, 2);
    }

    #[test]
    fn mapping_error_renders_in_words_and_pages_plainly() {
        let v = view(
            "* some line\n* another",
            &[None, None],
            Some("mapping failed: 1 vs 2 commits (--graph pass vs %H pass) -- selection disabled, plain pager"),
        );
        let mut pane = LogPane::new();
        insta::assert_snapshot!(draw(&mut pane, &Ok(v)).backend());
    }

    #[test]
    fn failed_log_is_the_pane_content() {
        let mut pane = LogPane::new();
        let term = draw(&mut pane, &Err("`git -C wine log ...` failed (exit status: 128): fatal: bad revision".into()));
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol()).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("LOG FAILED"), "{all}");
        assert!(all.contains("128"), "the exact failure must reach the screen:\n{all}");
    }

    #[test]
    fn empty_range_says_so() {
        let mut pane = LogPane::new();
        let term = draw(&mut pane, &Ok(view("", &[], None)));
        let buf = term.backend().buffer();
        let all: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol()).collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("no commits in this range"), "{all}");
    }

    /// gitui's window math, pinned: moves only when the selection leaves.
    #[test]
    fn scroll_top_follows_the_selection() {
        assert_eq!(calc_scroll_top(0, 5, 3), 0);
        assert_eq!(calc_scroll_top(0, 5, 5), 1);
        assert_eq!(calc_scroll_top(1, 5, 9), 5);
        assert_eq!(calc_scroll_top(5, 5, 2), 2);
    }
}
