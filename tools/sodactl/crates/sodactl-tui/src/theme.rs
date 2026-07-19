//! The single Severity -> Color map.
//!
//! One map, consumed by every pane and (Stage 7) the footer, because two
//! hand-picked palettes would eventually disagree on exactly one severity --
//! and colour is the first thing the eye trusts. It stays reinforcement, never
//! the only carrier: every rendering that uses these styles also prints the
//! severity label in words, so pipes, snapshots and colour-blind eyes lose
//! nothing.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Block;
use sodactl_core::finding::Severity;

pub fn severity_color(sev: Severity) -> Color {
    match sev {
        Severity::Blocker => Color::Red,
        Severity::Drift => Color::Yellow,
        Severity::Hygiene => Color::DarkGray,
    }
}

/// Severity as an eye-catcher: group headers, detail headlines.
pub fn severity_style(sev: Severity) -> Style {
    Style::default().fg(severity_color(sev)).add_modifier(Modifier::BOLD)
}

/// A ticked finding: dimmed, NOT hidden. Plain `DarkGray` foreground rather
/// than `Modifier::DIM` -- DIM support is spotty enough that some emulators
/// render it invisible, and "dimmed but VISIBLE" is the contract (a hidden
/// Blocker is a filter, not a checklist).
pub fn ticked_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// One bordered block per pane, with focus on the border. Focus is the one
/// state colour may carry alone: it is interaction chrome, not a fact -- the
/// cursor answers the same question on the next keypress, and every FACT the
/// panes render (stale, staged, failed) still arrives in words per the
/// standing rule. Centralised so no pane invents its own idea of "focused".
pub fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title.to_string());
    if focused {
        block.border_style(Style::default().fg(Color::Cyan))
    } else {
        block
    }
}
