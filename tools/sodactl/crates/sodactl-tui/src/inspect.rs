//! Shell-out inspection: Enter on a commit hands the terminal to `tig show`
//! or `git show` and takes it back.
//!
//! The child inherits stdio and drives its own pager -- borrowing git's
//! renderer again, exactly like `gitlog` borrows `--graph`: no diff viewer to
//! own here, nothing to drift from what git would show. The TUI's part is only
//! the terminal handover (`terminal::with_suspended`) and saying the exact
//! command line when the child fails.

use std::path::{Path, PathBuf};

/// Startup probe: is `tig` on PATH? A manual walk, deliberately not a
/// subprocess spawn (`tig --version` et al.) -- a probe that spawns has all
/// the failure modes the probe exists to avoid, and its exit status would
/// still need interpreting. Executable-bit-on-a-regular-file is the same
/// answer execvp itself would compute.
///
/// Probed ONCE at startup (see `App::new`): a per-Enter probe would stat the
/// whole PATH on every keypress, and "tig appeared mid-session" is not a case
/// worth serving -- the user can restart a dashboard.
pub fn tig_on_path() -> bool {
    std::env::var_os("PATH")
        .map(|path| find_in(&path, "tig").is_some())
        .unwrap_or(false)
}

/// The walk itself, over an explicit PATH value so the tests can hand in a
/// constructed one instead of mutating the process environment (`set_var` is
/// unsound under the parallel test harness).
fn find_in(path: &std::ffi::OsStr, bin: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        // An empty PATH element legacy-means "current directory"; honouring
        // that here would make the probe's answer depend on the cwd the TUI
        // happened to start in. Skipped: a missed tig degrades to `git show`,
        // which is correct anyway.
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(bin))
        .find(|cand| is_executable(cand))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // Any execute bit: execvp does the precise ownership check itself, and a
    // false positive here just means the spawn fails with the exact command
    // line in the banner -- the loud failure, not the silent one.
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// The inspection argv, pure (the gitlog rule: everything argv-shaped is
/// unit-tested on exact strings). Both forms use `-C` rather than a spawn-time
/// current_dir so the failure banner's command line reproduces standalone --
/// the reader's next move is to paste it into a shell.
pub fn show_argv(tig: bool, wine: &Path, oid: &str) -> Vec<String> {
    let tool = if tig { "tig" } else { "git" };
    [tool, "-C", &wine.to_string_lossy(), "show", oid].iter().map(|s| s.to_string()).collect()
}

/// Run the inspection with inherited stdio -- the caller has already restored
/// the real screen (`terminal::with_suspended`), so the child's pager owns the
/// terminal until it exits. A spawn failure or nonzero exit comes back with
/// the exact command line; the child's own stderr already went to the real
/// screen, so the banner's job is naming what failed, not repeating why.
///
/// Cause FIRST, command line last -- the gitlog mapping-error rule: the
/// footer renders this on one unwrapped line, and the command (the longest
/// element by far) is the part that survives truncation worst. A banner
/// clipped to "...exited with exi" was the paid-for live finding here.
///
/// No `LC_ALL=C` pin, unlike `gitlog::exec`: that discipline exists for
/// CAPTURED stderr that gets parsed and grepped. Nothing here is captured,
/// and forcing English onto Florian's interactive pager would change output
/// with no consumer of the change.
///
/// AUDITED READ-ONLY CONSTRUCTOR: the only commands this module runs are
/// `tig show <oid>` and `git show <oid>` (built by `show_argv` above). The
/// guardrail test in lib.rs greps every `Command::new` in this crate against
/// the audited sites; this is the second of two.
pub fn run_show(argv: &[String]) -> Result<(), String> {
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .map_err(|e| format!("could not spawn ({e}) -- `{}`", argv.join(" ")))?;
    if !status.success() {
        return Err(format!("{status} -- `{}`", argv.join(" ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn show_argv_exact_strings_for_both_tools() {
        let wine = Path::new("/repo/wine");
        let oid = "aaaaaaaa94d5eb2f423a1d194ce67dae0f5db878";
        assert_eq!(
            show_argv(true, wine, oid),
            ["tig", "-C", "/repo/wine", "show", oid].map(String::from)
        );
        assert_eq!(
            show_argv(false, wine, oid),
            ["git", "-C", "/repo/wine", "show", oid].map(String::from)
        );
    }

    /// The PATH walk against a constructed PATH: found iff a regular file with
    /// an execute bit; a non-executable file of the right name is skipped.
    #[test]
    fn path_walk_finds_executables_only() {
        let base = std::env::temp_dir().join(format!("sodactl-tui-pathprobe-{}", std::process::id()));
        let with_exec = base.join("bin");
        let without_exec = base.join("noexec");
        std::fs::create_dir_all(&with_exec).unwrap();
        std::fs::create_dir_all(&without_exec).unwrap();
        std::fs::write(with_exec.join("tig"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(with_exec.join("tig"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(without_exec.join("tig"), "not a program").unwrap();
        std::fs::set_permissions(without_exec.join("tig"), std::fs::Permissions::from_mode(0o644))
            .unwrap();

        // Non-executable dir first: the walk must pass over it, not stop.
        let path = std::env::join_paths([&without_exec, &with_exec]).unwrap();
        assert_eq!(find_in(&path, "tig"), Some(with_exec.join("tig")));

        let only_bad = std::env::join_paths([&without_exec]).unwrap();
        assert_eq!(find_in(&only_bad, "tig"), None);

        // The empty element ("::" in PATH) must not resolve against the cwd.
        let with_empty = std::ffi::OsString::from("");
        assert_eq!(find_in(&with_empty, "tig"), None);

        std::fs::remove_dir_all(&base).unwrap();
    }

    /// The failure banner: cause first (it must survive footer truncation),
    /// exact command line after -- both failure kinds. `false` is
    /// POSIX-guaranteed present and exits 1; the unspawnable path is a name
    /// no PATH resolves.
    #[test]
    fn run_show_failures_lead_with_the_cause_then_the_exact_command_line() {
        let nonzero = run_show(&["false".into()]).unwrap_err();
        assert!(nonzero.starts_with("exit status: 1 -- `false`"), "{nonzero}");

        let unspawnable =
            run_show(&["sodactl-no-such-binary".into(), "show".into(), "abc".into()]).unwrap_err();
        assert!(unspawnable.starts_with("could not spawn ("), "{unspawnable}");
        assert!(unspawnable.ends_with("-- `sodactl-no-such-binary show abc`"), "{unspawnable}");
    }
}
