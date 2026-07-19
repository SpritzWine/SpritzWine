//! Terminal lifecycle: raw mode + alternate screen + focus events.
//!
//! Every exit path -- clean quit, error, panic, Ctrl-Z -- must hand the shell
//! back usable. The panic path is the one worth paranoia: a hook that prints
//! before restoring writes the message INTO the alternate screen, which is
//! then torn down, and the user gets a dead terminal and no explanation.

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableFocusChange, EnableFocusChange};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use std::io::{stdout, Stdout};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> Result<Term> {
    // Hook first, then raw mode: if anything between here and the first draw
    // panics, the cleanup must already be armed. Restore precedes the default
    // hook so the panic message prints to the REAL screen, not the vanishing
    // one.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        default_hook(info);
    }));
    enable_raw_mode()?;
    // EnableFocusChange: focus-gain is one of the two refresh triggers (manual
    // `r` + terminal focus -- Florian's spec). Without it the terminal never
    // emits Event::FocusGained and half the refresh design silently does
    // nothing, which is exactly the failure mode this project forbids.
    //
    // Raw mode is ON from the line above: an entry failure here must take it
    // back down before surfacing, because this error leaves through `run`'s
    // `init()?` -- the one exit path with no restore behind it -- and an error
    // printed into raw mode is staircase text on a dead-feeling shell.
    if let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableFocusChange) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout()))?)
}

/// Inverse of `init`, in reverse order. Safe to call twice (the escape codes
/// are idempotent), which the panic hook and `run`'s unconditional restore
/// both rely on.
pub fn restore() -> Result<()> {
    // Raw mode comes down even when the escape-code write fails: the ioctl
    // does not depend on stdout being writable, and a cooked-mode shell with a
    // stray alternate screen beats a raw-mode shell either way. The screen
    // error still surfaces -- after the part that matters most has run.
    let screen = execute!(stdout(), DisableFocusChange, LeaveAlternateScreen);
    disable_raw_mode()?;
    screen?;
    Ok(())
}

/// Ctrl-Z: give the terminal back, stop, re-enter on `fg`.
///
/// Raw mode swallows the tty's own job control, so suspension must be an
/// in-app keybinding or Ctrl-Z is simply dead. Shape lifted from
/// ratatui/templates @ cd2b97b, `component/template/src/tui.rs`
/// (`Tui::suspend`/`resume`, MIT license): restore the terminal, raise SIGTSTP
/// against ourselves, and re-init when the shell resumes us. The template
/// raises through the `signal-hook` crate; a whole dependency for one libc
/// wrapper is not worth it, so raise(2) is declared here directly.
pub fn suspend(term: &mut Term) -> Result<()> {
    restore()?;
    extern "C" {
        fn raise(sig: std::ffi::c_int) -> std::ffi::c_int;
    }
    // 20 is SIGTSTP on the Linux ABIs this host tool runs on (x86_64,
    // aarch64). Not portable to the BSDs' numbering; does not need to be.
    const SIGTSTP: std::ffi::c_int = 20;
    // Execution stops on this call and continues past it on SIGCONT (`fg`).
    if unsafe { raise(SIGTSTP) } != 0 {
        anyhow::bail!("raise(SIGTSTP) failed; terminal left restored");
    }
    reenter(term)
}

/// Hand the terminal to a foreground child and take it back: restore, run the
/// closure on the REAL screen (inherited stdio -- the child's pager owns the
/// tty until it exits), re-enter. Stage 6's `tig show`/`git show` path.
///
/// The closure's own outcome is returned to the caller only AFTER the
/// terminal is back: whatever went wrong in the child, the dashboard must
/// come back first so there is a screen to say it on.
pub fn with_suspended<T>(term: &mut Term, f: impl FnOnce() -> T) -> Result<T> {
    restore()?;
    let out = f();
    reenter(term)?;
    Ok(out)
}

/// The shared back-half of `suspend` and `with_suspended`.
fn reenter(term: &mut Term) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableFocusChange)?;
    // Full repaint by CONSTRUCTION, not by `Terminal::clear()`: 0.30's clear
    // round-trips a cursor-position query (ESC[6n) that only a live terminal
    // emulator answers -- right after SIGCONT (or a just-quit pager) the reply
    // can be slow or lost and crossterm then fails with "cursor position could
    // not be read". A fresh Terminal has an empty previous buffer, so the next
    // draw rewrites every cell of the (blank, freshly entered) alternate
    // screen with zero round-trips.
    *term = Terminal::new(CrosstermBackend::new(stdout()))?;
    Ok(())
}
