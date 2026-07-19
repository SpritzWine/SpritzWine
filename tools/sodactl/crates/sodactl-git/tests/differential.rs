//! The differential harness, hermetic half: throwaway fixture repos, every
//! compared method, zero disagreements between the binary and gix.
//!
//! This is the paid-for-bug suite running against the gix backend for free --
//! rename, both-modified conflict, annotated + lightweight tags,
//! gitlink-with-space, absorbed gitdir, conflicted gitlink -- and it is what
//! makes a gix version bump reviewable in CI without the real repo.
//!
//! The LIVE half (the real SpritzWine checkout) is the hidden
//! `sodactl selfcheck --differential` subcommand; the `#[ignore]`d test at
//! the bottom runs the same engine. The gate is STRUCTURAL (`#[ignore]`, so
//! the default run reports it as ignored) rather than a printed skip: libtest
//! captures `eprintln!` from passing tests, so an env-gated early return
//! rendered as a bare `ok` -- indistinguishable from a real pass, a green
//! lie. Run it with `--ignored` and `SODACTL_LIVE_REPO` set.

use std::path::{Path, PathBuf};
use std::process::Command;

use sodactl_git::{Differential, GitCli, GitGix, GitHybrid, GitRead, Reapply};

/// A well-formed full oid no fixture repo will ever contain.
const MISSING_HEX: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run git for FIXTURE SETUP only -- assertions go through the backends.
/// LC_ALL=C to match what the backends themselves pin.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .expect("spawning git for fixture setup");
    assert!(
        out.status.success(),
        "fixture setup `git {args:?}` failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Pid-scoped like the unit-test fixtures: a shared path plus `remove_dir_all`
/// on entry means a concurrent second run deletes the first run's live repos.
fn scratch_dir(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("sodactl-diff-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
}

fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", msg]);
}

/// The everything-fixture: linear history with a side branch and a real merge,
/// both tag kinds, a submodule WITH A SPACE in its path (absorbed gitdir, like
/// wine's layout), a `.gitmodules` with a quoted padded value, and a commit
/// subject containing a tab.
fn rich_fixture(name: &str) -> Scratch {
    let s = scratch_dir(name);
    let dir = &s.0;
    init_repo(dir);
    commit_file(dir, "a.txt", "a\n", "one");
    commit_file(dir, "b.txt", "b\n", "two\tstill two"); // tab in subject
    // Side branch + merge, so merges_in and ahead_behind have something real.
    git(dir, &["checkout", "-qb", "side"]);
    commit_file(dir, "s.txt", "s\n", "side work");
    git(dir, &["checkout", "-q", "main"]);
    commit_file(dir, "m.txt", "m\n", "main work");
    git(dir, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    // A branch that is strictly behind, for is_ancestor/branches_containing.
    git(dir, &["branch", "old", "HEAD~2"]);
    // Both tag kinds -- 847 annotated and 5769 lightweight live in the real
    // repo, so both paths run on every invocation there.
    git(dir, &["tag", "light", "HEAD"]);
    git(dir, &["tag", "-a", "-m", "annotated", "heavy", "HEAD~1"]);
    // Submodule with a space, absorbed gitdir (`.git` FILE) -- wine's layout.
    let subdir = dir.join("my mod");
    std::fs::create_dir_all(&subdir).unwrap();
    init_repo(&subdir);
    commit_file(&subdir, "x.txt", "1\n", "s1");
    git(dir, &["-c", "protocol.file.allow=always", "submodule", "-q", "add", "./my mod", "my mod"]);
    git(dir, &["commit", "-qm", "add sub"]);
    git(dir, &["submodule", "absorbgitdirs"]);
    // A `.gitmodules`-style value with quoted padding, for config_file_get.
    git(dir, &["config", "-f", ".gitmodules", "submodule.my mod.pad", " x "]);
    s
}

/// Drive every COMPARED method through the harness with representative
/// arguments, including the directed probes -- the arms a healthy repo never
/// exercises, where the Ok(None)/Err discipline actually lives.
fn exercise(d: &Differential<'_>, sub_path: &str) {
    let head = d.rev_parse("HEAD").unwrap().unwrap();

    // -- group a: object/ref resolution --------------------------------------
    let _ = d.rev_parse("main");
    let _ = d.rev_parse("heavy"); // annotated tag -> must peel to the commit
    let _ = d.rev_parse("light");
    let _ = d.rev_parse(&head); // full oid round-trip
    let _ = d.rev_parse("sodactl-selfcheck-no-such-ref"); // Ok(None), not Err
    // The full-hex echo: `--verify --quiet <40-hex>` answers the hex itself
    // WITHOUT an odb lookup, even for an object the repo never contained --
    // so raw must be Ok(Some(hex)) and only the PEELED form says Ok(None).
    // (The quiet-direction divergence found 2026-07-19.)
    let _ = d.rev_parse_raw(MISSING_HEX);
    let _ = d.rev_parse(MISSING_HEX);
    let _ = d.rev_parse_raw("heavy"); // NO peel -> the tag object itself
    let _ = d.rev_parse_raw(&format!("HEAD:{sub_path}")); // the gitlink read
    let _ = d.rev_parse_raw("HEAD:no/such/path"); // the PIN_ABSENT arm
    let _ = d.rev_parse_raw("HEAD:a.txt"); // a blob through the same spec
    let _ = d.git_dir();
    let _ = d.head_branch();

    // -- group b: refs --------------------------------------------------------
    let _ = d.local_branches();
    let _ = d.tags();
    let _ = d.tags_with_targets();
    let _ = d.tags_pointing_at(&head);
    let parent = d.rev_parse("HEAD~1").unwrap().unwrap();
    let _ = d.tags_pointing_at(&parent); // annotated tag sits here
    let grandparent = d.rev_parse("HEAD~2").unwrap().unwrap();
    let _ = d.tags_pointing_at(&grandparent); // known-untagged -> Ok([])
    let _ = d.branches_containing(&grandparent, false);
    let _ = d.branches_containing(&head, false);
    let _ = d.branches_containing(&head, true); // no remotes -> Ok([])

    // -- group c: index + config ---------------------------------------------
    let _ = d.submodule_index_oid(sub_path);
    let _ = d.submodule_index_oid("no/such/path"); // Ok(None)
    let _ = d.config_file_get(".gitmodules", &format!("submodule.{sub_path}.path"));
    let _ = d.config_file_get(".gitmodules", &format!("submodule.{sub_path}.pad")); // quoted " x "
    let _ = d.config_file_get(".gitmodules", "submodule.nope.branch"); // Ok(None)
    let _ = d.superproject_pin();

    // -- group d: history -----------------------------------------------------
    let _ = d.is_ancestor("old", "main");
    let _ = d.is_ancestor("main", "old");
    let _ = d.is_ancestor("main", "main"); // a ref is its own ancestor
    let _ = d.ahead_behind("old", "main");
    let _ = d.ahead_behind("side", "main");
    let _ = d.ahead_behind("sodactl-selfcheck-no-such-ref", "HEAD"); // "never pushed" -> Ok(None)
    let _ = d.count_commits("old..main");
    let _ = d.count_commits("main..old"); // empty range -> 0
    let _ = d.count_commits("main"); // single-rev form
    let _ = d.log_subjects("old..main");
    let _ = d.log_subjects("HEAD~2..HEAD~1"); // the tab-subject commit
    let _ = d.log_subjects("HEAD..HEAD"); // empty -> []
    let _ = d.merges_in("old..main"); // contains the real merge
    let _ = d.merges_in("HEAD..HEAD");

    // -- group e: the verdict -------------------------------------------------
    let side = d.rev_parse("side").unwrap().unwrap();
    let _ = d.reapply_verdict(&side, "main"); // merged already -> Present
    let _ = d.reapply_verdict(&side, &side); // carries itself -> Present
    let _ = d.reapply_verdict(&side, "old"); // absent there -> Missing

    // -- the excluded two, so the report proves they route to the binary ------
    let _ = d.status(true);
    let _ = d.missing_by_patch_id("main", "side", "old");
}

#[test]
fn hermetic_differential_agrees_on_the_rich_fixture() {
    let s = rich_fixture("rich");
    let cli = GitCli::new(&s.0);
    let gix = GitGix::new(&s.0);
    let d = Differential::new(&cli, &gix);
    exercise(&d, "my mod");
    let r = d.finish();
    assert!(r.is_clean(), "{r}");
    // The floor: a refactor that silently stopped exercising methods must not
    // stay green on an empty scorecard.
    assert!(r.calls >= 35, "only {} compared calls -- probes went missing:\n{r}", r.calls);
    assert!(r.agreements == r.calls);
    // The report itself must carry the exclusions.
    let text = r.to_string();
    assert!(text.contains("status") && text.contains("missing_by_patch_id"), "{text}");
}

#[test]
fn hermetic_differential_agrees_on_a_conflicting_pair() {
    // The three-valued verdict across backends: a cross-base pair that
    // CONFLICTS must be Unknown on both sides -- Unknown collapsing into
    // Missing on the gix side is a disagreement here, not a debate.
    let s = scratch_dir("conflict");
    let dir = &s.0;
    init_repo(dir);
    commit_file(dir, "f.txt", "base\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    commit_file(dir, "f.txt", "side change\n", "side");
    git(dir, &["checkout", "-q", "main"]);
    commit_file(dir, "f.txt", "main change\n", "main");

    let cli = GitCli::new(dir);
    let gix = GitGix::new(dir);
    let d = Differential::new(&cli, &gix);
    let side = d.rev_parse("side").unwrap().unwrap();
    let verdict = d.reapply_verdict(&side, "main").unwrap();
    let _ = d.reapply_verdict("no-such-commit", "main"); // Err on both
    let _ = d.reapply_verdict(&side, "no-such-target"); // Unknown on both
    let _ = d.is_ancestor("nope-a", "main"); // Err on both, never Ok(false)
    let r = d.finish();
    assert!(r.is_clean(), "{r}");
    assert_eq!(verdict, Reapply::Unknown, "a conflict is not evidence of absence");
}

#[test]
fn hermetic_differential_agrees_on_a_broken_repo() {
    // Not-a-repo: every method must refuse on BOTH backends (Err/Err counts
    // as agreement of classification), never answer.
    let s = scratch_dir("notarepo");
    let cli = GitCli::new(&s.0);
    let gix = GitGix::new(&s.0);
    let d = Differential::new(&cli, &gix);
    let _ = d.rev_parse("main");
    let _ = d.head_branch();
    let _ = d.tags();
    let _ = d.is_ancestor("a", "b");
    let _ = d.ahead_behind("a", "b");
    let _ = d.count_commits("a..b");
    let r = d.finish();
    assert!(r.is_clean(), "{r}");
    assert_eq!(r.err_err, r.calls, "a non-repo must refuse everything on both sides:\n{r}");
}

// ---- the paid-for conformance tests, three-way ------------------------------
//
// The scratch-repo tests in lib.rs pinned the binary's discipline; these are
// the same disciplines the gix backend must now independently satisfy, run
// against all three impls so the hybrid cannot route around them.

fn each_backend(dir: &Path, f: impl Fn(&dyn GitRead, &str)) {
    let cli = GitCli::new(dir);
    let gix = GitGix::new(dir);
    let hybrid = GitHybrid::new(dir);
    f(&cli, "GitCli");
    f(&gix, "GitGix");
    f(&hybrid, "GitHybrid");
}

#[test]
fn a_missing_ref_is_none_but_a_broken_repo_is_an_error_on_every_backend() {
    let s = scratch_dir("threeway-missing");
    init_repo(&s.0);
    commit_file(&s.0, "a.txt", "a\n", "one");
    each_backend(&s.0, |g, which| {
        assert!(g.rev_parse("main").unwrap().is_some(), "{which}");
        assert_eq!(g.rev_parse("no-such-branch").unwrap(), None, "{which}");
    });
    let empty = scratch_dir("threeway-notarepo");
    each_backend(&empty.0, |g, which| {
        assert!(g.rev_parse("main").is_err(), "{which}: non-repo must be Err");
    });
}

#[test]
fn a_bad_ref_is_an_error_not_a_false_on_every_backend() {
    let s = scratch_dir("threeway-badref");
    init_repo(&s.0);
    commit_file(&s.0, "a.txt", "a\n", "one");
    each_backend(&s.0, |g, which| {
        assert!(g.is_ancestor("main", "main").unwrap(), "{which}: a ref is its own ancestor");
        assert!(
            g.is_ancestor("nope-a", "main").is_err(),
            "{which}: unknown ref must be Err, not Ok(false) -- false raises TIER_CHILD_STALE"
        );
    });
}

#[test]
fn a_conflict_is_unknown_and_a_clean_reapply_is_decisive_on_every_backend() {
    let s = scratch_dir("threeway-verdict");
    let dir = &s.0;
    init_repo(dir);
    commit_file(dir, "f.txt", "base\n", "base");
    git(dir, &["checkout", "-qb", "side"]);
    commit_file(dir, "f.txt", "side change\n", "side");
    git(dir, &["checkout", "-q", "main"]);
    commit_file(dir, "f.txt", "main change\n", "main");
    git(dir, &["checkout", "-qb", "other", "main"]);
    commit_file(dir, "new.txt", "n\n", "adds new.txt");
    git(dir, &["checkout", "-q", "main"]);

    let cli = GitCli::new(dir);
    let side = cli.rev_parse("side").unwrap().unwrap();
    let onto = cli.rev_parse("main").unwrap().unwrap();
    let added = cli.rev_parse("other").unwrap().unwrap();
    each_backend(dir, |g, which| {
        assert_eq!(g.reapply_verdict(&side, &onto).unwrap(), Reapply::Unknown, "{which}");
        assert_eq!(g.reapply_verdict(&added, &onto).unwrap(), Reapply::Missing, "{which}");
        assert_eq!(g.reapply_verdict(&added, &added).unwrap(), Reapply::Present, "{which}");
    });
}

#[test]
fn a_conflicted_gitlink_is_an_error_not_stage_one_on_every_backend() {
    let s = scratch_dir("threeway-gitlink");
    let dir = &s.0;
    init_repo(dir);
    commit_file(dir, "a.txt", "a\n", "one");
    let subdir = dir.join("mod");
    std::fs::create_dir_all(&subdir).unwrap();
    init_repo(&subdir);
    commit_file(&subdir, "x.txt", "1\n", "s1");
    git(dir, &["-c", "protocol.file.allow=always", "submodule", "-q", "add", "./mod", "mod"]);
    git(dir, &["commit", "-qm", "add sub"]);
    each_backend(dir, |g, which| {
        assert!(g.submodule_index_oid("mod").unwrap().is_some(), "{which}: clean gitlink reads");
        // A trailing slash means the same directory to a pathspec, but the
        // index stores the entry WITHOUT it -- gix's exact/prefix lookups
        // both missed and answered a false "not in index" (the one found
        // divergence in the QUIET direction, 2026-07-19). Every backend must
        // read the gitlink through either spelling.
        assert_eq!(
            g.submodule_index_oid("mod/").unwrap(),
            g.submodule_index_oid("mod").unwrap(),
            "{which}: a trailing slash must not turn the gitlink invisible"
        );
        assert_eq!(g.submodule_index_oid("not-in-index").unwrap(), None, "{which}");
    });

    // Diverge the gitlink on two branches and merge -> stages 1/2/3.
    git(dir, &["checkout", "-qb", "side"]);
    commit_file(&subdir, "x.txt", "2\n", "s2");
    git(dir, &["add", "mod"]);
    git(dir, &["commit", "-qm", "bump side"]);
    git(dir, &["checkout", "-q", "main"]);
    git(&subdir, &["reset", "-q", "--hard", "HEAD~1"]);
    commit_file(&subdir, "x.txt", "3\n", "s3");
    git(dir, &["add", "mod"]);
    git(dir, &["commit", "-qm", "bump main"]);
    let merge = Command::new("git")
        .current_dir(dir)
        .env("LC_ALL", "C")
        .args(["merge", "side"])
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1), "the fixture must actually conflict");

    each_backend(dir, |g, which| {
        assert!(
            g.submodule_index_oid("mod").is_err(),
            "{which}: a conflicted gitlink must refuse, not return stage 1"
        );
    });
    // A plain directory of files must refuse too, on every backend.
    git(dir, &["merge", "--abort"]);
    std::fs::create_dir_all(dir.join("plaindir")).unwrap();
    commit_file(dir, "plaindir/f.txt", "f\n", "plain dir");
    each_backend(dir, |g, which| {
        assert!(
            g.submodule_index_oid("plaindir").is_err(),
            "{which}: a blob must not read as a gitlink"
        );
    });
}

#[test]
fn the_two_excluded_methods_stay_typed_holes_on_gix() {
    // The seam's tripwire. `status` and `missing_by_patch_id` are binary-only
    // BY DECISION (empty-dir/gitlink divergence; no patch-id in gitoxide and
    // the reconstruction was silently wrong). Nothing else fails if someone
    // "finishes the migration": the three-way tests never call these two on
    // `GitGix`, and `Differential`'s excluded bodies never touch the
    // candidate. So this test IS the guard -- implementing either on `GitGix`
    // must break it, forcing the implementer to meet the recorded rationale
    // head-on instead of deleting a routing line in `GitHybrid`.
    let s = scratch_dir("excluded-holes");
    init_repo(&s.0);
    commit_file(&s.0, "a.txt", "a\n", "one");
    let gix = GitGix::new(&s.0);
    assert!(
        matches!(gix.status(true), Err(sodactl_git::GitError::Unsupported { method: "status" })),
        "GitGix::status must stay a typed hole -- see the exclusion rationale"
    );
    assert!(
        matches!(
            gix.missing_by_patch_id("a", "b", "c"),
            Err(sodactl_git::GitError::Unsupported { method: "missing_by_patch_id" })
        ),
        "GitGix::missing_by_patch_id must stay a typed hole -- see the exclusion rationale"
    );
    // And the hybrid must ANSWER both (routed to the binary), not surface the
    // hole: a rerouted GitHybrid would return Unsupported here and fail.
    let hybrid = GitHybrid::new(&s.0);
    assert!(hybrid.status(true).is_ok(), "GitHybrid must route status to the binary");
    assert!(
        hybrid.missing_by_patch_id("HEAD", "HEAD", "HEAD").is_ok(),
        "GitHybrid must route missing_by_patch_id to the binary"
    );
}

#[test]
fn symref_branches_keep_their_own_names() {
    // The converse test for the `branches_containing` symref fix. gix's
    // `.peeled()` iterator resolves a symref and yields it under the TARGET's
    // name, so `refs/remotes/<r>/HEAD` came out as a duplicate of
    // `<r>/<branch>` and the symref's own short name (`<r>`, which is how
    // `for-each-ref --contains` lists it) vanished. Found live 2026-07-19:
    // reference ["codeberg", "codeberg/rust"] vs candidate
    // ["codeberg/rust", "codeberg/rust"]. Both shapes are pinned here: a
    // local symref branch (`alias` -> `main`) and the remote-HEAD layout the
    // live repo actually has.
    let s = scratch_dir("symref");
    let dir = &s.0;
    init_repo(dir);
    commit_file(dir, "a.txt", "a\n", "one");
    git(dir, &["symbolic-ref", "refs/heads/alias", "refs/heads/main"]);
    // The remote-HEAD layout, built with plumbing: a tracking branch plus the
    // `HEAD` symref every `git clone` writes for the remote's default branch.
    git(dir, &["update-ref", "refs/remotes/origin/main", "main"]);
    git(dir, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);

    let cli = GitCli::new(dir);
    let gix = GitGix::new(dir);
    let head = cli.rev_parse("HEAD").unwrap().unwrap();

    // Pin the ORACLE's shape first, so this test cannot degrade into "both
    // backends wrong the same way": the symref appears under its OWN short
    // name, next to its target, no duplicates.
    assert_eq!(cli.branches_containing(&head, false).unwrap(), vec!["alias", "main"]);
    assert_eq!(cli.branches_containing(&head, true).unwrap(), vec!["origin", "origin/main"]);

    // And the candidate must independently match it, plus every other
    // ref-listing method, scored by the harness.
    let d = Differential::new(&cli, &gix);
    let _ = d.branches_containing(&head, false);
    let _ = d.branches_containing(&head, true);
    let _ = d.local_branches();
    let _ = d.head_branch();
    let r = d.finish();
    assert!(r.is_clean(), "{r}");
}

#[test]
fn a_missing_full_hex_echoes_raw_and_peels_to_none_on_every_backend() {
    // The converse pair for the full-hex fix. `rev-parse --verify --quiet
    // <40-hex>` echoes the hex WITHOUT an odb lookup (exit 0, verified on git
    // 2.55), so the raw form answers Ok(Some(hex)) even for an object the
    // repo never contained -- gix used to manufacture Ok(None) there, the
    // quiet direction (a "names this" silently became "names nothing", and
    // rev_parse_raw's None is exactly what routes to PIN_ABSENT). The PEELED
    // form must KEEP answering Ok(None) -- that half is what stops this fix
    // from becoming a mute button that invents objects.
    let s = scratch_dir("fullhex");
    init_repo(&s.0);
    commit_file(&s.0, "a.txt", "a\n", "one");
    let upper = MISSING_HEX.to_ascii_uppercase();
    each_backend(&s.0, |g, which| {
        assert_eq!(
            g.rev_parse_raw(MISSING_HEX).unwrap().as_deref(),
            Some(MISSING_HEX),
            "{which}: a full hex echoes itself, no odb lookup"
        );
        // git normalizes uppercase hex to lowercase on output; so must we.
        assert_eq!(g.rev_parse_raw(&upper).unwrap().as_deref(), Some(MISSING_HEX), "{which}");
        assert_eq!(
            g.rev_parse(MISSING_HEX).unwrap(),
            None,
            "{which}: the peeled form needs the object and must stay None"
        );
    });
}

#[test]
fn tags_pointing_at_a_missing_oid_errs_on_both_backends() {
    // Formerly the ONE recorded gix-vs-binary divergence; closed 2026-07-19 on
    // Florian's rule that a clear error beats a silent breakdown. Raw
    // `git tag --points-at <full-hex-not-in-repo>` exits 0 with EMPTY output
    // (nonzero only for malformed/unresolvable NAMES), which reads as
    // "untagged" -- and the one oid in the pipeline not guaranteed to exist is
    // the gitlink pin, where "untagged" feeds a Blocker. So `GitCli` now
    // pre-checks existence with `cat-file -e` (NOT `rev_parse_raw`, which
    // echoes a full hex without an odb lookup and would bless exactly the oid
    // the guard exists to catch) and both backends Err.
    let s = scratch_dir("points-at-missing");
    init_repo(&s.0);
    commit_file(&s.0, "a.txt", "a\n", "one");
    git(&s.0, &["tag", "t1", "HEAD"]);
    let cli = GitCli::new(&s.0);
    let gix = GitGix::new(&s.0);
    assert!(
        cli.tags_pointing_at(MISSING_HEX).is_err(),
        "binary: a missing oid must be loud, not 0-empty"
    );
    assert!(gix.tags_pointing_at(MISSING_HEX).is_err(), "gix: loud, per the pin-bias rule");
    // The converse, so the guard cannot become a mute button: on an oid that
    // EXISTS both backends still answer, and identically.
    let head = cli.rev_parse("HEAD").unwrap().unwrap();
    let tags = cli.tags_pointing_at(&head).unwrap();
    assert_eq!(tags, gix.tags_pointing_at(&head).unwrap());
    assert_eq!(tags, vec!["t1".to_string()], "the existing-oid arm still finds the tag");
}

#[test]
fn a_bare_repo_pin_is_unknown_with_one_voice() {
    // `Pin::Unknown` compares `reason` under `PartialEq`, so the two backends
    // must refuse a bare repo in the SAME words -- the binary's
    // `--show-superproject-working-tree` prints empty there (verified on git
    // 2.55, see the GitCli impl), and gix's no-workdir arm phrases it
    // identically. A prose-only fork here would surface as a differential
    // disagreement on any bare repo.
    let s = scratch_dir("barepin");
    let dir = s.0.join("bare.git");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "--bare"]);
    let cli = GitCli::new(&dir);
    let gix = GitGix::new(&dir);
    let d = Differential::new(&cli, &gix);
    let pin = d.superproject_pin().unwrap();
    assert!(matches!(pin, sodactl_git::Pin::Unknown { .. }), "bare repo must be Unknown, never a clean pin");
    let r = d.finish();
    assert!(r.is_clean(), "{r}");
}

// ---- the live half ----------------------------------------------------------

/// Same engine as `sodactl selfcheck --differential`, as a test. `#[ignore]`
/// rather than an env-gated early return, because libtest captures the
/// "skipped" eprintln from a passing test and renders a bare `ok` -- a green
/// lie. The acceptance procedure runs it explicitly:
/// `SODACTL_LIVE_REPO=<checkout> cargo test --release --test differential -- --ignored`
/// and an explicit run WITHOUT the env var fails loudly instead of passing.
#[test]
#[ignore = "live half: set SODACTL_LIVE_REPO to the SpritzWine checkout and run with -- --ignored"]
fn live_differential_if_repo_available() {
    let root = std::env::var_os("SODACTL_LIVE_REPO").expect(
        "this test was run explicitly (--ignored) but SODACTL_LIVE_REPO is unset -- \
         point it at the SpritzWine checkout; passing without looking would be a green lie",
    );
    let root = PathBuf::from(root);
    let wine = root.join("wine");
    for repo in [&root, &wine] {
        let cli = GitCli::new(repo);
        let gix = GitGix::new(repo);
        let d = Differential::new(&cli, &gix);
        let head = d.rev_parse("HEAD").unwrap().unwrap();
        let _ = d.head_branch();
        let _ = d.git_dir();
        let _ = d.local_branches();
        let _ = d.tags();
        let _ = d.tags_with_targets();
        let _ = d.tags_pointing_at(&head);
        let _ = d.branches_containing(&head, false);
        let _ = d.branches_containing(&head, true);
        let _ = d.superproject_pin();
        let _ = d.rev_parse("sodactl-selfcheck-no-such-ref");
        let _ = d.rev_parse_raw("HEAD:no/such/path");
        let _ = d.ahead_behind("sodactl-selfcheck-no-such-ref", "HEAD");
        let _ = d.submodule_index_oid("no/such/path");
        let r = d.finish();
        assert!(r.is_clean(), "live differential in {}:\n{r}", repo.display());
    }
}
