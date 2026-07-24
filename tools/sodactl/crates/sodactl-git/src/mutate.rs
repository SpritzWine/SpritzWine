//! The mutating half of the git seam -- one struct, one spawn site, no push.
//!
//! Everything in `lib.rs` is a QUERY; this module is the only place sodactl is
//! allowed to change a repository, and the allowance is deliberately narrow:
//!
//! * Writes go through the `git` binary permanently -- the lib.rs module doc
//!   records why: `index.lock` is the real mutex, and a terminal, an editor's
//!   git integration and `build.sh`'s `submodule update --force` all write the
//!   same tree. There is no gix write path and there must never be one.
//! * ONE `Command::new("git")` site (`GitMut::exec`), mirroring
//!   `GitCli::exec_env` on every property that matters: `current_dir` is the
//!   repo (never `-C`, never inherited), `LC_ALL=C` pinned at the spawn,
//!   raw-bytes stdout, lossy stderr, the same `GitError`. The spawn-audit test
//!   in this file enumerates every spawn site in the crate, so a convenient
//!   `Command` added anywhere else fails the build.
//! * NO push method, provably: `no_git_subcommand_is_ever_push` below fails
//!   if the argv literal ever appears in non-test code. Florian pushes; the
//!   tool prints commands and stops.
//!
//! The verbs are THIN: explicit argv, exit 0 = done, anything else = `Err`.
//! In particular a rebase or cherry-pick that STOPS on a conflict surfaces as
//! `Err` here -- callers that need to distinguish "waiting for resolution"
//! from "failed" must probe the gitdir structurally (`rebase-merge` /
//! `rebase-apply` / `CHERRY_PICK_HEAD`, the same probe `checks/tree.rs` uses)
//! and never match the stderr prose. That keeps the crate's rule intact: no
//! behaviour ever hangs off translated text.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Exec, GitError, Result};

/// git's spelling of "this ref must not currently exist" in the `<old>`
/// position of `update-ref`. Named so the create-only arm of `update_ref_cas`
/// reads as what it is rather than as forty magic characters. Public because
/// `sodactl-core`'s plan renderer must show the LITERAL argv this module will
/// pass -- a display twin that spells the create-only sentinel differently is
/// a plan the user reviewed but did not see.
pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Which side of a conflicted path a resolution takes. A type rather than a
/// bare string so a call site cannot transpose or typo the flag -- the wrong
/// side here silently keeps the wrong blob and the replay commits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

/// The one mutating git handle.
///
/// Deliberately a separate type from `GitCli` rather than more methods on it:
/// `Workspace` hands `&dyn GitRead` to every check, and keeping mutation off
/// that trait means no check can acquire a write capability by accident --
/// the type system carries the read-only promise the differential harness and
/// the guardrail tests then enforce mechanically.
pub struct GitMut {
    repo: PathBuf,
}

impl GitMut {
    pub fn new(repo: impl AsRef<Path>) -> Self {
        Self { repo: repo.as_ref().to_path_buf() }
    }

    /// AUDITED MUTATING CONSTRUCTOR: the one place this crate spawns git with
    /// intent to write. Mirrors `GitCli::exec_env` -- see the long comment
    /// there for why `LC_ALL=C` is load-bearing (translated stderr broke ref
    /// classification on a German-locale machine) and why stdout stays raw
    /// bytes (a whole-buffer lossy decode corrupts a path BEFORE any splitter
    /// runs). The spawn-audit test below counts this site.
    fn exec(&self, args: &[&str]) -> Result<Exec> {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.repo).env("LC_ALL", "C").args(args);
        let out = cmd.output().map_err(|source| GitError::Spawn {
            args: args.join(" "),
            cwd: self.repo.display().to_string(),
            source,
        })?;
        Ok(Exec {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }

    /// Exit 0 -> done, anything else -> `Err`. Mutations have no benign
    /// nonzero: unlike the read side there is no "the ref simply is not
    /// there" answer to preserve, so the exit-code split collapses to one arm
    /// and the stderr travels in the error for the human.
    fn run0(&self, args: &[&str]) -> Result<()> {
        let e = self.exec(args)?;
        match e.code {
            Some(0) => Ok(()),
            _ => Err(GitError::Command {
                args: args.iter().map(|s| s.to_string()).collect(),
                cwd: self.repo.display().to_string(),
                stderr: e.stderr,
            }),
        }
    }

    /// The compare-and-swap primitive. Every branch move goes through this
    /// where git allows it: git verifies `<old>` against the ref UNDER ITS OWN
    /// REF LOCK, so the check and the write are atomic -- a read-then-write
    /// from here would race every other writer of the same tree.
    ///
    /// `expected_old = None` means CREATE-ONLY (the all-zero oid): used for
    /// backup refs, where clobbering an earlier backup would destroy exactly
    /// the state the backup exists to preserve.
    pub fn update_ref_cas(
        &self,
        refname: &str,
        new: &str,
        expected_old: Option<&str>,
    ) -> Result<()> {
        self.run0(&["update-ref", refname, new, expected_old.unwrap_or(ZERO_OID)])
    }

    pub fn fetch(&self, remote: &str, refspecs: &[&str]) -> Result<()> {
        let mut args = vec!["fetch", remote];
        args.extend_from_slice(refspecs);
        self.run0(&args)
    }

    /// `git rebase --onto <newbase> <upstream> <branch>`.
    ///
    /// The rebase moves the branch ref ITSELF, so there is no `<old>` slot for
    /// git to CAS on -- the protection is procedural and lives in the caller:
    /// verify the expected tip immediately before, with the backup ref and the
    /// journal already written. A conflict stops the rebase in flight and
    /// surfaces as `Err` (see the module doc for how callers tell the two
    /// apart without reading prose).
    pub fn rebase_onto(&self, newbase: &str, upstream: &str, branch: &str) -> Result<()> {
        self.run0(&["rebase", "--onto", newbase, upstream, branch])
    }

    pub fn rebase_continue(&self) -> Result<()> {
        self.run0(&["rebase", "--continue"])
    }

    pub fn rebase_abort(&self) -> Result<()> {
        self.run0(&["rebase", "--abort"])
    }

    pub fn cherry_pick(&self, range: &str) -> Result<()> {
        self.run0(&["cherry-pick", range])
    }

    pub fn cherry_pick_abort(&self) -> Result<()> {
        self.run0(&["cherry-pick", "--abort"])
    }

    /// `git checkout -B <branch> <start>`: create-or-reset plus checkout in
    /// one step, which is exactly the stitch/regen opening move.
    pub fn checkout_branch_reset(&self, branch: &str, start: &str) -> Result<()> {
        self.run0(&["checkout", "-B", branch, start])
    }

    pub fn checkout_detach(&self, rev: &str) -> Result<()> {
        self.run0(&["checkout", "--detach", rev])
    }

    /// Resolve a conflicted path by taking one side wholesale, then stage it.
    /// Two commands because that is what git makes of it: `checkout
    /// --ours/--theirs` rewrites the worktree copy from the index stage, and
    /// only the `add` marks the conflict resolved.
    pub fn resolve_take(&self, path: &str, side: Side) -> Result<()> {
        let flag = match side {
            Side::Ours => "--ours",
            Side::Theirs => "--theirs",
        };
        self.run0(&["checkout", flag, "--", path])?;
        self.run0(&["add", "--", path])
    }

    /// Resolve a conflicted path by deleting it (`[[binary_resolution]]`
    /// strategy = "delete"): `git rm` on an unmerged path both removes the
    /// file and records the deletion as the resolution.
    pub fn rm_cached_conflict(&self, path: &str) -> Result<()> {
        self.run0(&["rm", "--", path])
    }

    pub fn add(&self, paths: &[&str]) -> Result<()> {
        let mut args = vec!["add", "--"];
        args.extend_from_slice(paths);
        self.run0(&args)
    }

    /// `-m`, never an editor: every mutation here must run to completion
    /// without a terminal to hand a $EDITOR to.
    pub fn commit(&self, message: &str) -> Result<()> {
        self.run0(&["commit", "-m", message])
    }

    /// `git commit -m <msg> -- <paths>`: commit ONLY the named paths, whatever
    /// else sits in the index. This is how `regen`'s rr-cache capture and
    /// `rr gc`'s prune commit stay sweep-proof -- a plain `commit -m` commits
    /// the whole index, so anything the user had staged for an unrelated
    /// commit would ride along invisibly (`pin` refuses that state outright;
    /// these two verbs run long and conditionally, so scoping the commit
    /// beats gating it).
    pub fn commit_paths(&self, message: &str, paths: &[&str]) -> Result<()> {
        let mut args = vec!["commit", "-m", message, "--"];
        args.extend_from_slice(paths);
        self.run0(&args)
    }

    /// `git config <key> <value>` -- repo-local config, the rerere enablement
    /// `regen` performs (`regenerate.sh`'s step 1). Not `config_file_set`: the
    /// wine submodule's config lives behind the `.git`-file indirection, and
    /// hardcoding a path there is the resolved-gitdir mistake `checks/rr.rs`
    /// documents; git itself knows where its config is.
    pub fn config_set(&self, key: &str, value: &str) -> Result<()> {
        self.run0(&["config", key, value])
    }

    /// Annotated only -- a lightweight tag carries no message and the release
    /// discipline requires one. `force` re-points an existing name; without it
    /// a collision is git's own refusal, passed through untouched.
    pub fn tag_annotated(&self, name: &str, target: &str, message: &str, force: bool) -> Result<()> {
        let mut args = vec!["tag", "-a"];
        if force {
            args.push("-f");
        }
        args.extend_from_slice(&["-m", message, name, target]);
        self.run0(&args)
    }

    /// `git config -f <file> <key> <value>` -- the `.gitmodules` `branch=`
    /// writer. The read twin (`GitCli::config_file_get`) errors when the file
    /// is missing; the writer legitimately CREATES the file, so no existence
    /// check here.
    pub fn config_file_set(&self, file: &str, key: &str, value: &str) -> Result<()> {
        self.run0(&["config", "-f", file, key, value])
    }

    pub fn rerere_gc(&self) -> Result<()> {
        self.run0(&["rerere", "gc"])
    }

    /// `git rerere clear`: drop the metadata of the CURRENT (finished or
    /// abandoned) conflicted operation -- the `MERGE_RR` file plus the
    /// preimage-only recordings it lists. Measured: entries that gained a
    /// postimage are untouched (they left `MERGE_RR` when rerere captured
    /// them), so this removes exactly the recordings that can never be
    /// consulted again -- a conflict `regen` resolved by rule (deletion,
    /// binary take) leaves one behind after EVERY pour, and git itself never
    /// cleans it up on rebase completion.
    pub fn rerere_clear(&self) -> Result<()> {
        self.run0(&["rerere", "clear"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GitCli, GitRead, StatusKind};

    // ---- guardrails: the crate's spawn surface, enumerated -----------------

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// A file's lines up to its first `#[cfg(test)]` line. Test modules build
    /// scratch repos and are allowed to run git however they like; only
    /// production code is held to the audited-site list. The cutoff is the
    /// attribute LINE itself, which is how this crate writes its test modules
    /// -- `require_cutoff` pins that convention where it is load-bearing, so
    /// the cutoff cannot silently rot into scanning nothing or everything.
    fn non_test_lines(text: &str, cfg_test: &str) -> Vec<(usize, String)> {
        text.lines()
            .enumerate()
            .take_while(|(_, l)| l.trim() != cfg_test)
            .map(|(n, l)| (n + 1, l.to_string()))
            .collect()
    }

    /// The tui guardrail's mould (`sodactl-tui/src/lib.rs`): the read/write
    /// split as a test, not a convention. Exactly three spawn sites may exist
    /// in this crate's production code -- `GitCli::exec_env` and
    /// `GitCli::any_rev_missing` in lib.rs (the read side), `GitMut::exec`
    /// here (the write side) -- and the write side must carry its audit
    /// marker. A fourth site anywhere, including inside an already-audited
    /// file, fails and forces the audit conversation instead of skipping it.
    #[test]
    fn every_spawn_goes_through_an_audited_constructor() {
        // Built from fragments so this test's own source does not match
        // itself (the shared rationale: an exemption marker on a guardrail is
        // the thing that eventually gets used on a real offender).
        let spawn = "Command::".to_string() + "new";
        let cfg_test = "#[cfg(te".to_string() + "st)]";
        let marker = "AUDITED MUTATING ".to_string() + "CONSTRUCTOR";
        // (file, allowed spawn sites in non-test code)
        let audited = [("lib.rs", 2usize), ("mutate.rs", 1usize)];
        // Files whose test modules DO spawn git for fixtures: the cutoff must
        // exist there or the audit would count fixture setup as offenders --
        // which is precisely the rot this assertion prevents.
        let require_cutoff = ["lib.rs", "mutate.rs"];

        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(files.len() >= 4, "the source walk must actually find this crate");

        let mut offenders = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            if require_cutoff.contains(&name.as_str()) {
                assert!(
                    text.lines().any(|l| l.trim() == cfg_test),
                    "{name} lost its `{cfg_test}` line -- the audit's test-code \
                     cutoff just became a no-op; restore the module or retune \
                     the audit"
                );
            }
            let hits: Vec<usize> = non_test_lines(&text, &cfg_test)
                .iter()
                // Comments mention the pattern (this crate documents its own
                // guardrail); only code can spawn.
                .filter(|(_, l)| !l.trim_start().starts_with("//"))
                .filter(|(_, l)| l.contains(&spawn))
                .map(|(n, _)| *n)
                .collect();
            match audited.iter().find(|(a, _)| *a == name.as_str()) {
                Some((_, want)) => {
                    assert_eq!(
                        hits.len(),
                        *want,
                        "{name} must contain exactly its {want} audited spawn \
                         site(s), found lines {hits:?}"
                    );
                    if name == "mutate.rs" {
                        assert!(
                            text.contains(&marker),
                            "{name} lost its audit marker -- re-audit before \
                             renaming anything"
                        );
                    }
                }
                None => {
                    for line in hits {
                        offenders.push(format!("{}:{line}", f.display()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "spawn sites outside the audited constructors -- every mutation \
             must go through GitMut::exec and every read through \
             GitCli::exec_env:\n{}",
            offenders.join("\n")
        );
    }

    /// The no-push promise as a test. `push --print` prints commands for
    /// Florian to run; nothing in this crate may ever pass "push" to git as a
    /// subcommand. Scans the argv LITERAL (quotes included) in non-test,
    /// non-comment lines -- like the spawn audit it catches the realistic
    /// case, a push verb added under deadline, not a dependency or an
    /// obfuscated format!().
    #[test]
    fn no_git_subcommand_is_ever_push() {
        // Fragments, same rationale as above: the guardrail must not match
        // its own source.
        let subcmd = "\"pu".to_string() + "sh\"";
        let cfg_test = "#[cfg(te".to_string() + "st)]";

        let mut files = Vec::new();
        rs_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        assert!(files.len() >= 4, "the source walk must actually find this crate");

        let mut offenders = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).unwrap();
            for (n, line) in non_test_lines(&text, &cfg_test) {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if line.contains(&subcmd) {
                    offenders.push(format!("{}:{n}", f.display()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the literal {subcmd} appears in non-test code -- there is no \
             push verb and there will not be one; the user pushes:\n{}",
            offenders.join("\n")
        );
    }

    // ---- against real (tiny, throwaway) repos ------------------------------
    //
    // Same reasoning as the lib.rs suite: these exercise git's OWN semantics
    // (ref locks, sequencer state, conflict stages), which a fake would just
    // re-encode from the author's beliefs.

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A one-commit repo plus BOTH handles: `GitMut` under test, `GitCli` to
    /// read the results back (and for fixture setup its private `run` is
    /// crate-visible, same as the lib.rs suite uses).
    ///
    /// Pid-scoped for the reason documented on the lib.rs `scratch`: entry
    /// does `remove_dir_all`, so an unscoped path lets a concurrent run
    /// delete this run's live repos mid-test.
    fn scratch(name: &str) -> (Scratch, GitMut, GitCli) {
        let dir =
            std::env::temp_dir().join(format!("sodactl-mut-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = GitCli::new(&dir);
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            g.run(a).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "one"]).unwrap();
        (Scratch(dir.clone()), GitMut::new(&dir), GitCli::new(&dir))
    }

    /// Write, stage and commit one file via the handles under test; returns
    /// the new HEAD.
    fn put(m: &GitMut, r: &GitCli, dir: &Path, file: &str, content: &str, msg: &str) -> String {
        std::fs::write(dir.join(file), content).unwrap();
        m.add(&[file]).unwrap();
        m.commit(msg).unwrap();
        r.rev_parse("HEAD").unwrap().unwrap()
    }

    #[test]
    fn update_ref_cas_moves_only_when_old_matches() {
        let (s, m, r) = scratch("cas");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();
        let c2 = put(&m, &r, &s.0, "b.txt", "b\n", "two");

        // Stale expectation: main is at c2, the caller believes c1. git must
        // refuse under its ref lock and leave the ref untouched -- this Err is
        // the entire point of routing branch moves through update-ref.
        assert!(m.update_ref_cas("refs/heads/main", &c1, Some(&c1)).is_err());
        assert_eq!(r.rev_parse("main").unwrap().unwrap(), c2, "a failed CAS must not move the ref");

        // Correct expectation: the move lands.
        m.update_ref_cas("refs/heads/main", &c1, Some(&c2)).unwrap();
        assert_eq!(r.rev_parse("main").unwrap().unwrap(), c1);
    }

    #[test]
    fn create_only_refuses_to_clobber() {
        let (_s, m, r) = scratch("createonly");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();

        // The backup-ref pattern: first write creates, an identical second
        // write is a COLLISION, not idempotence -- overwriting a backup
        // destroys exactly what it exists to preserve.
        let backup = "refs/sodactl/backup/20260724T000000Z/main";
        m.update_ref_cas(backup, &c1, None).unwrap();
        assert_eq!(r.rev_parse_raw(backup).unwrap().unwrap(), c1);
        assert!(m.update_ref_cas(backup, &c1, None).is_err(), "create-only must refuse an existing ref");

        // And against an existing branch, create-only is always a refusal.
        assert!(m.update_ref_cas("refs/heads/main", &c1, None).is_err());
    }

    #[test]
    fn rebase_onto_replays_the_branch() {
        let (s, m, r) = scratch("rebase");
        let base = r.rev_parse("HEAD").unwrap().unwrap();

        m.checkout_branch_reset("topic", &base).unwrap();
        put(&m, &r, &s.0, "t.txt", "t\n", "topic work");

        m.checkout_branch_reset("main", "main").unwrap();
        put(&m, &r, &s.0, "mn.txt", "m\n", "main work");

        m.rebase_onto("main", &base, "topic").unwrap();
        assert!(r.is_ancestor("main", "topic").unwrap(), "topic must now sit on main's tip");
        assert_eq!(r.count_commits("main..topic").unwrap(), 1, "exactly the one own commit replayed");
    }

    #[test]
    fn tag_annotated_refuses_collisions_unless_forced() {
        let (s, m, r) = scratch("tag");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();

        m.tag_annotated("v1", "HEAD", "first", false).unwrap();
        // rev_parse peels ^{commit}, so this compares commit to commit even
        // though the tag object sits in between.
        assert_eq!(r.rev_parse("v1").unwrap().unwrap(), c1);

        assert!(
            m.tag_annotated("v1", "HEAD", "again", false).is_err(),
            "an existing tag without --force is git's own refusal, passed through"
        );

        let c2 = put(&m, &r, &s.0, "b.txt", "b\n", "two");
        m.tag_annotated("v1", "HEAD", "moved", true).unwrap();
        assert_eq!(r.rev_parse("v1").unwrap().unwrap(), c2);
    }

    #[test]
    fn add_and_commit_advance_head_and_leave_a_clean_tree() {
        let (s, m, r) = scratch("commit");
        let before = r.rev_parse("HEAD").unwrap().unwrap();
        let after = put(&m, &r, &s.0, "c.txt", "c\n", "add c");
        assert_ne!(before, after);
        let subjects = r.log_subjects("HEAD~1..HEAD").unwrap();
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].1, "add c");
        assert!(r.status(false).unwrap().is_empty(), "nothing may be left unstaged or unmerged");
    }

    #[test]
    fn config_file_set_round_trips_through_the_reader() {
        let (_s, m, r) = scratch("config");
        // The reader errors on a missing file; the writer creates it. This is
        // the `.gitmodules` `branch=` write `pin` performs.
        m.config_file_set(".gitmodules", "submodule.wine.branch", "spritzwine").unwrap();
        assert_eq!(
            r.config_file_get(".gitmodules", "submodule.wine.branch").unwrap().as_deref(),
            Some("spritzwine")
        );
        // Overwrite, not append: config -f replaces a single-valued key.
        m.config_file_set(".gitmodules", "submodule.wine.branch", "spritzvalve").unwrap();
        assert_eq!(
            r.config_file_get(".gitmodules", "submodule.wine.branch").unwrap().as_deref(),
            Some("spritzvalve")
        );
    }

    #[test]
    fn resolve_take_settles_a_real_conflict() {
        let (s, m, r) = scratch("resolve");

        m.checkout_branch_reset("other", "main").unwrap();
        let other = put(&m, &r, &s.0, "a.txt", "theirs\n", "their change");

        m.checkout_branch_reset("main", "main").unwrap();
        put(&m, &r, &s.0, "a.txt", "ours\n", "our change");

        // Both sides rewrote the same line of a.txt: a textbook conflict. The
        // stopped pick is an Err by design (module doc) -- the STRUCTURAL
        // conflict evidence is the unmerged index entry, not the message.
        assert!(m.cherry_pick(&other).is_err());
        assert!(
            r.status(false).unwrap().iter().any(|e| e.kind == StatusKind::Unmerged && e.path == "a.txt"),
            "the conflict must be visible as an unmerged index entry"
        );

        m.resolve_take("a.txt", Side::Theirs).unwrap();
        assert_eq!(std::fs::read_to_string(s.0.join("a.txt")).unwrap(), "theirs\n");

        // `commit -m` concludes a single-commit pick; afterwards nothing may
        // remain unmerged or modified.
        m.commit("took theirs").unwrap();
        assert!(r.status(false).unwrap().is_empty());
    }

    #[test]
    fn refs_under_lists_exactly_the_backup_namespace() {
        let (_s, m, r) = scratch("refsunder");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();

        // Two backups plus a deliberate near-miss OUTSIDE the namespace: the
        // prefix must select, not merely sort.
        m.update_ref_cas("refs/sodactl/backup/20260724T000000Z/main", &c1, None).unwrap();
        m.update_ref_cas("refs/sodactl/backup/20260724T000001Z/main", &c1, None).unwrap();
        m.update_ref_cas("refs/sodactl/other/20260724T000002Z/main", &c1, None).unwrap();

        let refs = r.refs_under("refs/sodactl/backup/").unwrap();
        assert_eq!(
            refs,
            vec![
                ("refs/sodactl/backup/20260724T000000Z/main".to_string(), c1.clone()),
                ("refs/sodactl/backup/20260724T000001Z/main".to_string(), c1.clone()),
            ]
        );
        // An empty namespace is an empty answer, not an error -- `undo` with
        // no history must list nothing rather than fail.
        assert_eq!(r.refs_under("refs/sodactl/nothing/").unwrap(), vec![]);
    }

    #[test]
    fn rerere_gc_runs_clean_on_a_repo_without_a_cache() {
        // The verb must be safe to run unconditionally -- `rr gc` plans it on
        // repos that may never have recorded a resolution.
        let (_s, m, _r) = scratch("rrgc");
        m.rerere_gc().unwrap();
    }

    #[test]
    fn commit_paths_leaves_a_stranger_staged_change_out() {
        // The sweep-proof property that is the method's whole reason to exist:
        // a path someone else staged for their own commit must survive the
        // scoped commit STILL staged, not ride along into it.
        let (s, m, r) = scratch("commitpaths");
        std::fs::write(s.0.join("mine.txt"), "mine\n").unwrap();
        std::fs::write(s.0.join("theirs.txt"), "theirs\n").unwrap();
        m.add(&["mine.txt", "theirs.txt"]).unwrap();

        m.commit_paths("scoped commit", &["mine.txt"]).unwrap();
        let subjects = r.log_subjects("HEAD~1..HEAD").unwrap();
        assert_eq!(subjects[0].1, "scoped commit");
        // theirs.txt is still staged and uncommitted.
        assert!(r
            .status(false)
            .unwrap()
            .iter()
            .any(|e| e.path == "theirs.txt" && e.xy.starts_with('A')));
        assert!(r.rev_parse_raw("HEAD:theirs.txt").unwrap().is_none(), "not in the commit");
        assert!(r.rev_parse_raw("HEAD:mine.txt").unwrap().is_some());
    }

    #[test]
    fn config_set_round_trips_through_git_itself() {
        let (_s, m, r) = scratch("configset");
        m.config_set("rerere.enabled", "true").unwrap();
        // Read back via the same binary; `config --get` exits 1 on absence,
        // which run_opt maps to None -- so Some proves the write landed.
        assert_eq!(r.run_opt(&["config", "--get", "rerere.enabled"]).unwrap().as_deref(), Some("true"));
    }
}
