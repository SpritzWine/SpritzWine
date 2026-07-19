//! sodactl-tui: the failure-path dashboard.
//!
//! Read-only, like every other surface. The panes render the same `Report` and
//! `Dag` the CLI prints and MCP serves -- one schema, N surfaces -- and the
//! table cells come from `sodactl_core::present`, so a tri-state can never
//! grow a second, quietly divergent rendering here.
//!
//! The scope is deliberately the FAILURE path: pinpoint what is wrong, where,
//! and what to do next. No in-TUI merge resolver, no push gate, no mutation of
//! any kind.

pub mod app;
pub mod gitlog;
pub mod inspect;
pub mod keys;
pub mod panes;
pub mod terminal;
pub mod theme;
pub mod worker;

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Entry point: validate the manifest, then own the terminal until quit.
pub fn run(manifest: PathBuf) -> Result<()> {
    // Parse BEFORE the alternate screen: a bad --manifest must be a readable
    // stderr error, not a half-drawn frame the terminal swallows on restore.
    // The parsed value is then deliberately dropped -- this parse is only the
    // readability gate. The worker re-reads per refresh (`worker::run_once`),
    // so `r` after editing sodactl.toml shows the edited topology under the
    // same live-authority rule git already gets.
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading manifest {}", manifest.display()))?;
    sodactl_core::Manifest::parse(&text).context("parsing manifest")?;

    let mut term = terminal::init()?;
    let result = app::App::new(manifest).run(&mut term);
    // Restore on BOTH paths -- an error report printed into raw mode comes out
    // as unreadable staircase text. `.and()` keeps the app's error as the one
    // that surfaces; a restore failure only speaks when the app succeeded.
    let restored = terminal::restore();
    result.and(restored)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// The Stage-6 guardrail, in the sodactl-mcp `no_stdout_writes` mould: the
    /// read-only promise as a test, not a convention.
    ///
    /// This crate points loaded commands at Florian's LIVE repos, so every
    /// subprocess it can spawn is audited: `gitlog::exec` (the two `git log`
    /// passes) and `inspect::run_show` (`tig show`/`git show`) -- read-only
    /// constructors, marked as such in-source. The assertion is exact: those
    /// two files, ONE spawn site each, marker present. A third site anywhere
    /// -- including inside an already-audited file -- fails here and forces
    /// the audit conversation instead of skipping it.
    ///
    /// Like the mcp original this cannot catch a dependency that spawns; it
    /// catches the realistic case, a convenient `Command` added under
    /// deadline two files away from any comment saying why it must not be.
    #[test]
    fn every_spawn_goes_through_an_audited_constructor() {
        // Built from fragments so this test's own source does not match
        // itself (the mcp rationale: an exemption marker on a guardrail is
        // the thing that eventually gets used on a real offender).
        let spawn = "Command::".to_string() + "new";
        let marker = "AUDITED READ-ONLY ".to_string() + "CONSTRUCTOR";
        let audited = ["gitlog.rs", "inspect.rs"];

        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(files.len() > 5, "the source walk must actually find this crate");

        let mut offenders = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let hits: Vec<usize> = text
                .lines()
                .enumerate()
                // Comments mention the pattern (this crate documents its own
                // guardrail); only code can spawn.
                .filter(|(_, l)| !l.trim_start().starts_with("//"))
                .filter(|(_, l)| l.contains(&spawn))
                .map(|(n, _)| n + 1)
                .collect();
            if audited.contains(&name.as_str()) {
                assert_eq!(
                    hits.len(),
                    1,
                    "{name} must contain exactly its one audited spawn site, found lines {hits:?}"
                );
                assert!(
                    text.contains(&marker),
                    "{name} lost its audit marker -- re-audit before renaming anything"
                );
            } else {
                for line in hits {
                    offenders.push(format!("{}:{line}", f.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "spawn sites outside the audited constructors -- this crate's git \
             must stay read-only against the live repos:\n{}",
            offenders.join("\n")
        );
    }

    /// The sodactl-mcp `no_stdout_writes` mould, adapted for a crate where
    /// stdout IS the product: every byte on it must travel through the ratatui
    /// backend or the lifecycle escape codes, both owned by terminal.rs. A
    /// `print!` anywhere else lands INSIDE the alternate screen mid-frame and
    /// vanishes on restore -- corrupted dashboard AND lost message.
    ///
    /// Unlike the mcp original, stderr gets no pass here: while the TUI is
    /// live, raw mode turns an `eprintln!` into staircase text over the frame.
    /// Errors travel as values (footer banners, `run`'s Result) and the CLI
    /// prints them after the terminal is back -- never as writes from this
    /// crate.
    #[test]
    fn stdout_is_reached_only_through_the_terminal_lifecycle() {
        // Fragments, same rationale as above: the guardrail must not match
        // its own source.
        let p = "print";
        let everywhere: Vec<String> = vec![
            format!("{p}!"),
            format!("{p}ln!"),
            format!("e{p}!"),
            format!("e{p}ln!"),
            "dbg".to_string() + "!",
        ];
        // The raw handle itself: legitimate only in terminal.rs (backend
        // construction + escape codes), banned everywhere else.
        let handle: Vec<String> =
            vec!["std".to_string() + "out()", "std::io::std".to_string() + "out"];

        // `eprintln!` ends in `println!` (and `eprint!` in `print!`): a hit
        // counts only at a word boundary, so each variant is its own row and
        // none double-fires inside another.
        let hit = |code: &str, pat: &str| {
            code.match_indices(pat).any(|(i, _)| {
                i == 0
                    || !matches!(code.as_bytes()[i - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
            })
        };

        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(files.len() > 5, "the source walk must actually find this crate");

        let mut offenders = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let mut pats: Vec<&str> = everywhere.iter().map(String::as_str).collect();
            if name != "terminal.rs" {
                pats.extend(handle.iter().map(String::as_str));
            }
            for (n, line) in text.lines().enumerate() {
                // Comments may name the patterns (this crate documents its own
                // guardrails); only code can write.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for pat in &pats {
                    if hit(line, pat) {
                        offenders.push(format!("{}:{}: {pat}", f.display(), n + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "stdout/stderr writes outside the terminal lifecycle -- a live TUI \
             has no legitimate raw writer; route it through the footer or the \
             Result:\n{}",
            offenders.join("\n")
        );
    }
}
