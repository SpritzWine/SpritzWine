//! ONE static table: key combination(s), action, help text.
//!
//! The event loop resolves keys through `action_for` and the help overlay
//! (`panes::help`) renders every row of the SAME table, key names formatted
//! from the combinations themselves -- so the help overlay structurally cannot
//! drift from the bindings: there is no second list to forget, and a binding
//! added without help text does not compile.

use crokey::{key, KeyCombination};

/// What a key MEANS, decoupled from which pane acts on it: the app dispatches
/// global actions in `run` and per-focus ones in `handle_action`. One nav
/// vocabulary across all three panes (the tree keys are simply inert in the
/// log) keeps the table small enough to read in one screen of help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Close,
    Open,
    Tick,
    Enter,
    CycleFocus,
    Refresh,
    Suspend,
    Help,
    Quit,
}

pub struct Binding {
    pub keys: &'static [KeyCombination],
    pub action: Action,
    pub help: &'static str,
}

/// Table order IS help order: navigation first (what a new reader needs to
/// move), lifecycle last. `?` is bound twice because terminals disagree on
/// whether a shifted symbol reports the SHIFT modifier -- legacy encodings
/// send a bare `?`, the kitty-style ones add SHIFT -- and a help key that only
/// works on some terminals is the least discoverable failure a help key can
/// have. Same reason ctrl-c backs up `q`: raw mode swallowed SIGINT.
pub static BINDINGS: &[Binding] = &[
    Binding { keys: &[key!(up), key!(k)], action: Action::Up, help: "move up" },
    Binding { keys: &[key!(down), key!(j)], action: Action::Down, help: "move down" },
    Binding { keys: &[key!(left)], action: Action::Close, help: "close the selected tree group" },
    Binding { keys: &[key!(right)], action: Action::Open, help: "open the selected tree group" },
    Binding {
        keys: &[key!(space)],
        action: Action::Tick,
        help: "tick/untick the selected finding",
    },
    Binding {
        keys: &[key!(enter)],
        action: Action::Enter,
        // Terse on purpose: help rows must fit the overlay's width unwrapped
        // (the render wraps an overflow visibly, but a wrapped row breaks the
        // key column); the detail pane spells out what Enter does in context.
        help: "findings/graph: jump the log; log: inspect",
    },
    Binding {
        keys: &[key!(tab)],
        action: Action::CycleFocus,
        help: "cycle focus: findings -> graph -> log",
    },
    Binding {
        keys: &[key!(r)],
        action: Action::Refresh,
        help: "refresh (reconcile + re-derive the log)",
    },
    Binding {
        keys: &[key!(ctrl - z)],
        action: Action::Suspend,
        help: "suspend to the shell (fg resumes)",
    },
    Binding {
        keys: &[key!('?'), key!(shift - '?')],
        action: Action::Help,
        help: "toggle this help",
    },
    Binding { keys: &[key!(q), key!(ctrl - c)], action: Action::Quit, help: "quit" },
];

/// The event loop's key resolution. Linear scan: eleven rows, run at
/// keypress rate -- a lookup map would be a cache of nothing.
pub fn action_for(kc: KeyCombination) -> Option<Action> {
    BINDINGS.iter().find(|b| b.keys.contains(&kc)).map(|b| b.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn table_resolves_representative_keys() {
        assert_eq!(action_for(key!(q)), Some(Action::Quit));
        assert_eq!(action_for(key!(ctrl - c)), Some(Action::Quit));
        assert_eq!(action_for(key!(k)), Some(Action::Up));
        assert_eq!(action_for(key!(space)), Some(Action::Tick));
        assert_eq!(action_for(key!(x)), None, "an unbound key must resolve to nothing");
    }

    /// The terminal-disagreement case `?` is double-bound for: a shifted `?`
    /// (kitty-style modifier reporting) and a bare one (legacy) must BOTH
    /// reach Help, through the same normalization the live event loop uses
    /// (`KeyCombination::from(KeyEvent)`).
    #[test]
    fn help_opens_from_both_question_mark_encodings() {
        let bare = KeyCombination::from(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let shifted = KeyCombination::from(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert_eq!(action_for(bare), Some(Action::Help));
        assert_eq!(action_for(shifted), Some(Action::Help));
    }

    /// Every Action variant appears in the table exactly once. An action bound
    /// twice would render two help rows claiming the same meaning; an action
    /// bound zero times is dead code the compiler cannot see (it is only ever
    /// CONSTRUCTED here).
    #[test]
    fn every_action_has_exactly_one_table_row() {
        use Action::*;
        for action in
            [Up, Down, Close, Open, Tick, Enter, CycleFocus, Refresh, Suspend, Help, Quit]
        {
            let rows = BINDINGS.iter().filter(|b| b.action == action).count();
            assert_eq!(rows, 1, "{action:?} must have exactly one binding row");
        }
    }
}
