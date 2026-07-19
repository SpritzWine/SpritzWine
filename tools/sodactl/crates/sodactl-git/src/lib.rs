//! All git access, behind one trait -- with THREE impls and named roles.
//!
//! The DECISION (2026-07-19, Florian): go HYBRID. A bespoke wrapper around the
//! `git` binary is a library you maintain, just an undocumented one whose bugs
//! are yours alone -- this cycle alone paid for a leading space eaten from a
//! filename, error classification by matching GERMAN stderr text, `ls-files
//! -s` parsed positionally, and a whole-buffer lossy decode corrupting a path
//! before the splitter saw it. Typed results make that class unwritable. So
//! the read path moved to gitoxide -- BY CAPABILITY, not wholesale:
//!
//! * [`GitGix`] (`gix_backend.rs`) owns the 18 methods gix answers CORRECTLY,
//!   measured method-by-method in `docs/sodactl-git-backend.md`.
//! * The binary keeps exactly TWO: `missing_by_patch_id` (gitoxide has no
//!   patch-id anywhere across its 48 crates, and the ~130 LOC reconstruction
//!   produced a SILENT FALSE CLEAN -- it merged "unlink frame elements" with
//!   "unlink iframe elements" and suppressed a real `STITCH_STALE`; never
//!   reimplement it) and `status` (gix's dirwalk DIVERGES on this repo today:
//!   empty directories git cannot represent, plus a double-counted gitlink,
//!   and `checks/rr.rs` reports `dirty.len()`).
//! * [`GitHybrid`] is the production type: pure per-method delegation.
//! * [`GitCli`] below stays COMPLETE and is the REFERENCE backend: the oracle
//!   the differential harness (`differential.rs`, `sodactl selfcheck
//!   --differential`) compares gix against on every fixture and on the live
//!   repos. Behaviour changes land here first; gix must then match. Do NOT
//!   delete it as dead code -- without it any gix divergence gets "explained
//!   away" because there is nothing to compare against; the harness is what
//!   makes the deliberate 18-method duplication safe where the old GitRead/
//!   GitQ duplication (see the trait doc) was fatal.
//!
//! Perf was NOT the reason to migrate -- the wine tree is 12,434 tracked
//! files, `git status --porcelain=v2 -z` completes in ~33 ms warm, and the
//! "6.5 GB tree" of an earlier premise was pack data `status` never walks.
//! Maintainability was; the measured wins (`tags_with_targets` 132.8 -> 17.6
//! ms) are a side effect, and the binary is even FASTER on two methods it no
//! longer owns (`tags_pointing_at`, `branches_containing`) -- accepted.
//!
//! The trait is still the part that matters: it keeps the seam swappable and
//! lets the reconciler be tested against a fake with no repo on disk at all.
//!
//! Writes will go through the `git` binary permanently. `index.lock` is the real
//! mutex, and a terminal, an editor's git integration and `build.sh`'s
//! `submodule update --force` all write the same tree.
//!
//! # The rule this module now enforces
//!
//! **"I could not determine this" is never encoded as a definite value.**
//!
//! The v0 audit found nine defects that were all one mistake: `git` was run,
//! any nonzero exit was treated as the benign answer, and a read failure came
//! back as `false` / `vec![]` / `0`. Because the benign answer is usually
//! "everything is fine", broken checks failed *silently*; because one of them
//! feeds a `git rebase` remedy, a failed read could print an instruction that
//! rewrites an authored tier.
//!
//! So: every method below distinguishes git's *answer* exit codes (1 means "no")
//! from its *failure* exit codes (128 means "I could not tell you"), and the
//! latter always becomes `Err`. Verified against git's real behaviour on the
//! live repo -- see the per-method comments, which record the observed codes.

use std::path::{Path, PathBuf};
use std::process::Command;

pub mod differential;
pub mod gix_backend;

pub use differential::{Differential, DifferentialReport, Disagreement};
pub use gix_backend::{GitGix, GitHybrid};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git {args:?} failed in {cwd}: {stderr}")]
    Command { args: Vec<String>, cwd: String, stderr: String },
    #[error("git {args:?} in {cwd}: unparseable output: {detail}")]
    Parse { args: Vec<String>, cwd: String, detail: String },
    /// Carries the cwd, because the failure this actually reports in practice is
    /// "the working directory does not exist", and a bare `os error 2` naming
    /// neither the command nor the directory is indistinguishable from a missing
    /// `git` binary. That ambiguity is how the empty-`root` CLI bug survived.
    #[error("spawning `git {args}` in {cwd} failed: {source}")]
    Spawn { args: String, cwd: String, source: std::io::Error },
    /// A backend that cannot answer this question at all -- a test fake, or a
    /// partial backend (`GitGix` states its two holes this way, exactly as
    /// this comment always anticipated). Distinct from `Command`, which means
    /// git ran and failed: this means nobody even tried. It is an ERROR and
    /// never a benign default, so a check asking a fake something it was never
    /// taught cannot silently pass. Replaces the `anyhow::bail!("… not
    /// modelled")` arms the three test doubles used before `GitRead` and
    /// `GitQ` were folded together.
    #[error("`{method}` is not implemented by this git backend")]
    Unsupported { method: &'static str },
    /// gix refused or failed. Mirrors `Command` (git ran and failed): carries
    /// the operation and cwd because a bare gix error names neither. The
    /// source chain is flattened into `detail` at construction -- gix errors
    /// are deeply nested and the root cause is usually three levels down, and
    /// thiserror's `Display` would only ever show the top frame.
    #[error("gix {op} in {cwd}: {detail}")]
    Gix { op: &'static str, cwd: String, detail: String },
}

pub type Result<T> = std::result::Result<T, GitError>;

/// A commit id. Kept as a string: we only ever compare and display them.
pub type Oid = String;

/// Which porcelain-v2 record shape a `StatusEntry` came from.
///
/// Kept typed rather than stringly. v1 spells "untracked" `??` and v2 spells it
/// `?`, so a caller that string-matches one of them silently stops working when
/// the mode changes -- and the mode HAS changed once already, which is how this
/// enum came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StatusKind {
    /// `1` -- an ordinary tracked change.
    Tracked,
    /// `2` -- a rename or copy. Its source path follows as its own record.
    Renamed,
    /// `u` -- unmerged, i.e. a conflict.
    Unmerged,
    /// `?` -- untracked.
    Untracked,
    /// `!` -- ignored. Only ever emitted under `--ignored`, which we never
    /// pass. Modelled anyway, because the alternative is the parser's "unknown
    /// record" error firing on a flag somebody adds later.
    Ignored,
}

/// One porcelain-v2 status record, already un-quoted (we read `-z`).
#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub kind: StatusKind,
    /// The two-letter XY code, or empty for `?` and `!` records, which have
    /// none. Read `kind` rather than this field to classify.
    pub xy: String,
    pub path: String,
}

impl StatusEntry {
    pub fn is_untracked(&self) -> bool {
        self.kind == StatusKind::Untracked
    }
}

/// Whether a target already carries a commit's change, decided by reapplying it.
///
/// This exists because **`git patch-id` is context-sensitive**, and that is a
/// live correctness problem here rather than a theoretical one. patch-id
/// normalises whitespace but still hashes the diff hunks, so when a tier is
/// replayed onto a newer Wine base and surrounding lines drifted -- or when
/// **git-rerere resolves a conflict during the replay**, which is this
/// project's central mechanism -- the replayed commit hashes differently from
/// the original despite being the same change. patch-id then reports "not
/// applied" for precisely the commits the rerere pipeline worked hardest on.
///
/// StGit solves this by reapplying the patch and asking whether the result is
/// empty; `git-machete` independently landed on the same fast/exact split. Two
/// mature unrelated tools converging is reasonable evidence the shape is right.
///
/// Measured scope, so nobody over-trusts this: the reapply is CLEAN and
/// decisive for same-base questions (a stitch against its source tiers), and
/// CONFLICTS for cross-base ones (a consumer against its soda tip), where it
/// yields `Unknown` and the honest answer remains "not knowable without a
/// generation ledger".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reapply {
    /// Reapplying is a no-op: the change is already there.
    Present,
    /// Reapplying changes the tree: genuinely absent.
    Missing,
    /// The reapply conflicted. NOT evidence of absence -- see `reapply_verdict`.
    Unknown,
}

/// How one ref relates to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

/// What the superproject records for the wine submodule.
///
/// This exists because the audit's sharpest defect was a *wrong subject*, not a
/// wrong primitive: the pinned-consumer check asked about the consumer's local
/// branch tip, when the thing that breaks `git submodule update` is the
/// **gitlink** -- the oid the superproject's tree/index records. BRANCHING.md is
/// explicit that local consumer tips "routinely sit ahead of their tags", so the
/// two are different commits by design, and reading the branch made the check
/// right only by coincidence on the day it was written.
///
/// `committed` and `staged` are kept apart on purpose. A staged-but-uncommitted
/// gitlink bump is the naive `git add wine` caught mid-flight: nothing is broken
/// yet, but a `git commit` away from an unfetchable pointer. Collapsing them
/// would lose the one fact whose remedy is "unstage".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Pin {
    /// The gitlink could not be read, with the reason. NOT "there is no pin" --
    /// callers must report this rather than pass the check.
    Unknown { reason: String },
    Known {
        /// `HEAD:<path>` -- what a fresh clone gets.
        committed: Option<Oid>,
        /// The index entry. Differs from `committed` exactly when staged.
        staged: Option<Oid>,
    },
}

impl Pin {
    /// The oid a `git commit` right now would publish: the staged value if the
    /// gitlink is staged, else the committed one. This is the commit the pin
    /// checks must be run against.
    pub fn effective(&self) -> Option<&str> {
        match self {
            Pin::Unknown { .. } => None,
            Pin::Known { committed, staged } => staged.as_deref().or(committed.as_deref()),
        }
    }

    /// True when the gitlink is staged at a different oid than `HEAD` records.
    pub fn is_staged(&self) -> bool {
        match self {
            Pin::Unknown { .. } => false,
            Pin::Known { committed, staged } => staged.is_some() && staged != committed,
        }
    }
}

/// The read surface the reconciler and the checks need.
///
/// This was two traits -- `GitRead` here and `GitQ` in `sodactl_core::checks` --
/// with two `git`-binary backends behind them, and they DRIFTED. Only this one
/// got hardened, so `ahead_behind` grew a stderr substring match on one side and
/// a blanket swallow on the other, `submodule_index_oid` parsed `-z` records
/// here and `.lines()` there, and `reapply_verdict` built its own `Command` with
/// no `LC_ALL=C`. Two implementations of one question is the mechanism that
/// produced those bugs; folding them is the fix, and re-splitting them would
/// reintroduce it.
///
/// Every fallible query returns either a definite answer or an error, and
/// `Option` means *git said no such thing*, never *the query failed*.
pub trait GitRead {
    /// `None` iff git reports the ref does not exist (exit 1). Any other
    /// failure -- not a repo, ambiguous refname, corrupt object -- is an error.
    fn rev_parse(&self, rev: &str) -> Result<Option<Oid>>;
    /// `rev_parse` WITHOUT the `^{commit}` peel.
    ///
    /// Needed for exactly one thing: `HEAD:wine`. A gitlink names a commit that
    /// lives in the *submodule*, so peeling it in the superproject fails to
    /// find the object and the pin -- the single most important fact this tool
    /// reads -- would come back as "no such ref".
    ///
    /// Deliberately not `rev_parse`: that appends `^{commit}`, and a gitlink's
    /// target usually does not exist as an object in the SUPERPROJECT at all.
    /// Peeling would fail and the pin would read as absent, which is precisely
    /// the "unknown became a definite value" mistake this module exists to
    /// prevent. (Both halves of this doc were written independently, once from
    /// the caller's side and once from the implementer's, back when this method
    /// existed twice. That they agree is the evidence the split was costing
    /// something.)
    ///
    /// Spelled `--verify --quiet`, so a spec naming nothing is `Ok(None)` at
    /// exit 1 rather than the bare form's `Err` at 128. That is the LOOSER of
    /// the two behaviours this method had before the fold, kept on purpose: for
    /// the gitlink spec `HEAD:<path>`, exit 1 means "the superproject records
    /// nothing at this path", which is an answer, not a failure to answer. The
    /// caller must route it to `PIN_ABSENT` (a Blocker) and never to a clean
    /// pin -- `Pin::effective()` returning `None` is what carries that. The fold
    /// also removed a disagreement here: `checks/pin.rs` was already on the
    /// `--verify` form while `superproject_pin` was not.
    fn rev_parse_raw(&self, rev: &str) -> Result<Option<Oid>>;
    /// The real gitdir. For a submodule `wine/.git` is a *file* containing
    /// `gitdir: ../.git/modules/wine`, so anything that pokes at rerere state
    /// or rebase state by path must resolve this first or it silently inspects
    /// a directory that does not exist and always passes.
    fn git_dir(&self) -> Result<PathBuf>;
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool>;
    /// Commits in `head` (relative to `limit`) that are NOT present in
    /// `upstream` **by patch-id**.
    ///
    /// This is the only correct way to ask "does this replay carry that tier?".
    /// Stitches are built by cherry-pick and consumers by rebase, so the
    /// original commits are never ancestors of the result -- `is_ancestor` gives
    /// a confident wrong answer here.
    ///
    /// Precondition the caller must establish: `limit` is an ancestor of both
    /// `upstream` and `head`. `git cherry`'s limit silently truncates the
    /// comparison set otherwise, and the same question then returns different
    /// answers with exit 0 both times. `check_stitches` asserts it.
    fn missing_by_patch_id(&self, upstream: &str, head: &str, limit: &str) -> Result<Vec<String>>;
    /// Does `onto` already carry `commit`'s change? Determined by REAPPLYING
    /// rather than hashing -- see `Reapply` for why that is not redundant with
    /// `missing_by_patch_id`.
    fn reapply_verdict(&self, commit: &str, onto: &str) -> Result<Reapply>;
    fn ahead_behind(&self, base: &str, head: &str) -> Result<Option<AheadBehind>>;
    fn count_commits(&self, range: &str) -> Result<u32>;
    fn local_branches(&self) -> Result<Vec<String>>;
    fn tags(&self) -> Result<Vec<String>>;
    /// All tags with their peeled commit.
    fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>>;
    /// Branches containing `oid`. `remote` selects `refs/remotes` over
    /// `refs/heads`.
    fn branches_containing(&self, oid: &str, remote: bool) -> Result<Vec<String>>;
    /// `(oid, subject)` oldest first.
    fn log_subjects(&self, range: &str) -> Result<Vec<(Oid, String)>>;
    /// Merge commits in a range. A tier must be a straight line or
    /// `rebase --onto` replay silently changes what consumers get.
    fn merges_in(&self, range: &str) -> Result<Vec<Oid>>;
    fn status(&self, include_untracked: bool) -> Result<Vec<StatusEntry>>;
    /// The gitlink oid the superproject's index records for `path`. `None` when
    /// the path is not in the index; an error when it is in the index but is
    /// *not* a single stage-0 gitlink -- a blob's oid returned as a submodule
    /// pointer would feed a Blocker.
    fn submodule_index_oid(&self, path: &str) -> Result<Option<Oid>>;
    /// The branch HEAD is on, or `None` when detached.
    fn head_branch(&self) -> Result<Option<String>>;
    /// A key from an arbitrary config file, e.g. `.gitmodules`. Parsed by git,
    /// not by us: `.gitmodules` is git-config format and a regex over it gets
    /// continuations and quoting wrong.
    fn config_file_get(&self, file: &str, key: &str) -> Result<Option<String>>;

    /// Tags whose target commit *is* `oid` -- peeled, so annotated tags count.
    ///
    /// Defaulted rather than required so implementations that already answer
    /// `tags` + `rev_parse` (the test fake) stay correct for free. The default
    /// is O(tags) process spawns for `GitCli`, which on the live repo's 6616
    /// tags measured 9.7s -- so `GitCli` overrides it with a single call. Note
    /// the `?`: a tag that fails to peel is an error, not a silent non-match,
    /// because a swallowed peel biases this toward "untagged" and that is the
    /// direction that produces a false Blocker.
    ///
    /// This DEFAULT returns `Ok(vec![])` for an oid nothing points at,
    /// including one that does not exist -- it cannot ask the odb without a
    /// repo handle the trait does not have. Both real backends override with
    /// an existence check and `Err` on a missing oid (the 2026-07-19
    /// loud-over-silent rule); only the test fake keeps this arm, where an
    /// unregistered oid biases toward RAISING a finding, never muting one.
    fn tags_pointing_at(&self, oid: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for t in self.tags()? {
            if self.rev_parse(&t)?.as_deref() == Some(oid) {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// What the superproject pins this repo at.
    ///
    /// Defaulted to `Unknown` so a fake with no superproject model compiles and
    /// -- importantly -- reports honestly rather than answering about some other
    /// commit. `GitCli` discovers the superproject from the submodule itself.
    ///
    /// FOLLOW-UP: this is a second, independent expression of "two repos", and
    /// `Workspace` is the first. Once every caller carries a `Workspace`, this
    /// becomes deletable and `reconcile()` loses the back door it uses to read
    /// the gitlink. Not done here because it changes `reconcile()`'s behaviour.
    ///
    /// Note that only TWO methods on this trait are defaulted, and both are
    /// defaulted because a correct answer is *derivable* from other required
    /// methods. That is the bar. A blanket `Err(Unsupported)` default for the
    /// rest is tempting -- it would delete ~20 stub lines from each of three
    /// fakes -- and is wrong: it lets a future real backend silently not
    /// implement `status`, turning a compile error into a runtime one. Forcing a
    /// backend to STATE what it cannot answer is the same discipline as "unknown
    /// is never a definite value", one level up.
    fn superproject_pin(&self) -> Result<Pin> {
        Ok(Pin::Unknown { reason: "this GitRead does not model a superproject".into() })
    }
}

/// One porcelain-v2 record -> one `StatusEntry`, or a reason it could not be.
///
/// A free function so the malformed case is testable without constructing a
/// repository that git would refuse to produce. The rename SOURCE record is not
/// handled here -- it has no leading token and is consumed by the caller.
/// NUL-SEPARATED records, dropping the trailing empty piece.
///
/// git `-z` output is NUL-TERMINATED rather than separated, so a naive split
/// yields a final empty record; and an empty range yields zero bytes rather than
/// one blank record. Both are handled here so no parser has to remember.
fn split_nul(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    buf.split(|b| *b == 0).filter(|r| !r.is_empty())
}

/// The trimmed field, if it can only be an object id.
///
/// A free function so the malformed case is testable: git 2.55.0 always prints
/// the tree oid when `merge-tree --write-tree` exits 0, so the failing input
/// cannot be produced by driving a real repository -- and "the git we have
/// always prints X" is not a property this crate owns. The caller is
/// `reapply_verdict`, where `Missing` is the ONE verdict that reaches reconcile
/// as confirmed; without this gate an empty read compared unequal to the target
/// tree and manufactured that confirmation out of a failure to read.
fn looks_like_oid(field: &str) -> Option<&str> {
    let f = field.trim();
    (!f.is_empty() && f.chars().all(|c| c.is_ascii_hexdigit())).then_some(f)
}

fn parse_status_record(rec: &str) -> std::result::Result<StatusEntry, String> {
    // Field counts verified against git 2.55.0, one record per kind:
    //   1 .M N... 100644 100644 100644 <hH> <hI> <path>              -> 9
    //   2 R. N... 100644 100644 100644 <hH> <hI> R100 <path>         -> 10
    //   u UU N... 100644 100644 100644 100644 <h1> <h2> <h3> <path>  -> 11
    //   ? <path> / ! <path>                                          -> 2
    let (kind, nfields) = match rec.as_bytes().first() {
        Some(b'1') => (StatusKind::Tracked, 9),
        Some(b'2') => (StatusKind::Renamed, 10),
        Some(b'u') => (StatusKind::Unmerged, 11),
        Some(b'?') => (StatusKind::Untracked, 2),
        Some(b'!') => (StatusKind::Ignored, 2),
        _ => return Err(format!("unknown status record {rec:?}")),
    };
    // The path is the LAST field, and `splitn` stops splitting once it has n-1
    // pieces, so everything after the final delimiter -- leading spaces,
    // embedded spaces, an embedded tab -- arrives verbatim.
    //
    // What is deliberately NOT done here is slicing from a fixed offset. The v1
    // parser did `split_at(2)` then `trim_start()`, which ate the separator AND
    // the filename's own first space: `" leading-space.txt"` came back as
    // `"leading-space.txt"` (reproduced, scratch repo, git 2.55.0) -- a path
    // that matches a different `build_artifacts` entry than the one on disk.
    // Porting that to v2 as `get(3..)` would be worse, not better: `? ` is a
    // TWO-character prefix, so it would eat the first character of every
    // untracked path.
    let path = match rec.splitn(nfields, ' ').nth(nfields - 1) {
        Some(p) if !p.is_empty() => p.to_string(),
        // A short record is not a file that happens to be uninteresting -- it is
        // git output this parser does not understand, and skipping it is how a
        // modified path goes unreported and a Blocker is not raised. `continue`
        // was the old behaviour and it is exactly the "unknown became a benign
        // definite value" mistake this crate exists to prevent. Signed off by
        // the repo owner: it errors now.
        _ => return Err(format!("short status record {rec:?}")),
    };
    // `?` and `!` carry no XY at all; `kind` is the classifier.
    let xy = match kind {
        StatusKind::Untracked | StatusKind::Ignored => String::new(),
        _ => rec.get(2..4).unwrap_or_default().to_string(),
    };
    Ok(StatusEntry { kind, xy, path })
}

pub struct GitCli {
    repo: PathBuf,
    /// A private object database for the one command that WRITES: see
    /// `reapply_verdict`. Created lazily on first use, shared across calls (so
    /// merge-tree's writes dedupe and `rr-health`'s per-postimage loop does not
    /// churn mkdir/rm), and removed on drop.
    ///
    /// The Err is CACHED too, deliberately: a setup failure must report
    /// identically on every subsequent call rather than retrying and producing a
    /// different message each time. A failure is an error and never a silent
    /// fallback to the unredirected command -- falling back would restore
    /// exactly the pollution this removes, precisely on the machines where /tmp
    /// is constrained.
    scratch_odb: std::sync::OnceLock<std::result::Result<PathBuf, String>>,
}

/// One completed `git` invocation. `code` is `None` only when git died to a
/// signal, which is a failure, never an answer.
struct Exec {
    code: Option<i32>,
    /// RAW. Not decoded here, on purpose: git's stdout is a CONTAINER of fields,
    /// and the container's framing (`\0`, `\t`, `\n`) is pure ASCII while an
    /// individual field -- only ever a pathname -- may not be UTF-8 at all.
    /// Decoding the whole buffer replaces those bytes with U+FFFD *before the
    /// splitter runs*, so the parser sees a plausible, WRONG path and every
    /// later glob and comparison silently misses. Reproduced: a working-tree
    /// change to `lat\xe9in1.txt` came back as `lat\u{fffd}in1.txt`, which then
    /// failed every `build_artifacts` glob naming the real file and took the
    /// tracked branch -- a false SUBMODULE_DIRTY Blocker. `-z` does not help
    /// here; the corruption happens before the splitter ever runs.
    ///
    /// So: split first, decode per field, and let the decode fail where it can
    /// be attributed.
    stdout: Vec<u8>,
    /// Still lossy, and that is correct. Nothing PARSES stderr any more (see
    /// `ahead_behind`) -- it is interpolated into a message for a human. An
    /// error about being unable to decode an error message is strictly worse
    /// than a message with a replacement character in it.
    stderr: String,
}

impl GitCli {
    pub fn new(repo: impl AsRef<Path>) -> Self {
        Self { repo: repo.as_ref().to_path_buf(), scratch_odb: std::sync::OnceLock::new() }
    }

    /// The scratch object database, created on first use.
    ///
    /// Lives in `$TMPDIR`, NOT inside `.git`: putting it in the repository
    /// would re-create the disease in a tidier directory. /tmp is tmpfs here, so
    /// the objects never reach the disk and a hard kill leaves nothing durable.
    ///
    /// `info/alternates` rather than `GIT_ALTERNATE_OBJECT_DIRECTORIES`, even
    /// though both work: the env var is a PATH-style colon-separated list, so an
    /// odb path containing `:` would silently truncate into two bogus entries.
    /// The file is one path per line and has no colon hazard.
    fn scratch_odb(&self) -> Result<&Path> {
        let r = self.scratch_odb.get_or_init(|| {
            // The real odb, from git rather than from `$GIT_DIR` -- for the wine
            // submodule the latter is a gitlink FILE, and a relative `--git-dir`
            // would resolve against the wrong root.
            let real = self.git_dir().map_err(|e| e.to_string())?.join("objects");
            let base = std::env::temp_dir();
            let mut last = String::new();
            for attempt in 0..8 {
                let dir = base.join(format!(
                    "sodactl-odb-{}-{}-{attempt}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ));
                // `create_dir` is atomically exclusive, so this needs no
                // tempfile dependency.
                match std::fs::create_dir(&dir) {
                    Ok(()) => {
                        let info = dir.join("info");
                        let r = std::fs::create_dir(&info).and_then(|()| {
                            std::fs::write(
                                info.join("alternates"),
                                format!("{}\n", real.display()),
                            )
                        });
                        return match r {
                            Ok(()) => Ok(dir),
                            Err(e) => {
                                let _ = std::fs::remove_dir_all(&dir);
                                Err(format!("preparing {}: {e}", dir.display()))
                            }
                        };
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        last = format!("{} already exists", dir.display());
                    }
                    Err(e) => return Err(format!("creating {}: {e}", dir.display())),
                }
            }
            Err(format!("could not create a scratch object directory ({last})"))
        });
        match r {
            Ok(p) => Ok(p.as_path()),
            Err(e) => Err(self.err(&["merge-tree"], e)),
        }
    }

    /// Run git and hand back the exit code untouched.
    ///
    /// Everything else in this impl is built on top of this, because the whole
    /// bug class the audit found came from callers that never saw the code --
    /// they saw `success(): bool` and had no way to tell "no" from "I could not
    /// look". Raw stdout is kept untrimmed for the `-z` parsers.
    fn exec(&self, args: &[&str]) -> Result<Exec> {
        self.exec_env(args, &[])
    }

    /// `exec` with extra environment for THIS call only.
    ///
    /// Per-call and never on the struct: `GIT_OBJECT_DIRECTORY` set on a
    /// `GitCli` would redirect every subsequent read's object lookups, and there
    /// is no reason to widen the blast radius past the one command that writes.
    fn exec_env(&self, args: &[&str], env: &[(&str, &Path)]) -> Result<Exec> {
        // LC_ALL=C is load-bearing, not tidiness. git translates its
        // diagnostics, so anything that reads stderr to tell "no such ref" from
        // "a real failure" silently breaks on a non-English machine. On the
        // German-locale machine this was developed on, a missing ref says
        // "unbekannter Commit oder Pfad existiert nicht", the English match
        // failed, and a routine never-pushed tier became a hard error that
        // aborted the whole run. Any stderr or porcelain-adjacent parsing
        // anywhere below inherits the same hazard, so the invocation is pinned
        // once, here, and no future caller has to remember.
        //
        // Context on the SPAWN failure too, not just on git's own errors:
        // without it an unreadable working directory surfaces as a bare
        // "No such file or directory (os error 2)" naming neither the command
        // nor the directory -- which is exactly how the empty-`root` bug stayed
        // invisible through two review passes.
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.repo).env("LC_ALL", "C").args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().map_err(|source| {
            GitError::Spawn {
                args: args.join(" "),
                cwd: self.repo.display().to_string(),
                source,
            }
        })?;
        Ok(Exec {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }

    fn err(&self, args: &[&str], stderr: &str) -> GitError {
        GitError::Command {
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: self.repo.display().to_string(),
            stderr: stderr.to_string(),
        }
    }

    fn parse_err(&self, args: &[&str], detail: impl Into<String>) -> GitError {
        GitError::Parse {
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: self.repo.display().to_string(),
            detail: detail.into(),
        }
    }

    /// True iff git cannot resolve one of `revs` to an object.
    ///
    /// One spawn regardless of how many revs (`rev-parse --verify` rejects a
    /// second rev, verified), and the verdict is a TOKEN rather than prose, so
    /// it survives translation -- which is the entire point, see `ahead_behind`.
    ///
    /// Exit is 0 even when everything is missing, so a nonzero here means the
    /// repository itself is unreadable and must NOT be read as "the ref is
    /// absent". That distinction is the whole reason this helper exists.
    fn any_rev_missing(&self, revs: &[&str]) -> Result<bool> {
        use std::io::Write;
        let args = ["cat-file", "--batch-check=%(objecttype)"];
        let mut child = Command::new("git")
            .current_dir(&self.repo)
            .env("LC_ALL", "C")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|source| GitError::Spawn {
                args: args.join(" "),
                cwd: self.repo.display().to_string(),
                source,
            })?;
        // `^{commit}` so this asks the same question `rev_parse` does: a name
        // that resolves to a tree or a blob is not an endpoint `rev-list` can
        // use, and reporting it as present would push the caller into the `Err`
        // arm with a misleading diagnosis.
        let stdin: Vec<u8> =
            revs.iter().map(|r| format!("{r}^{{commit}}\n")).collect::<String>().into_bytes();
        let mut pipe = child.stdin.take().expect("stdin was piped");
        let wrote = pipe.write_all(&stdin).and_then(|_| pipe.flush());
        drop(pipe);
        let out = child.wait_with_output().map_err(|source| GitError::Spawn {
            args: args.join(" "),
            cwd: self.repo.display().to_string(),
            source,
        })?;
        wrote.map_err(|source| GitError::Spawn {
            args: args.join(" "),
            cwd: self.repo.display().to_string(),
            source,
        })?;
        if out.status.code() != Some(0) {
            return Err(self.err(&args, &String::from_utf8_lossy(&out.stderr)));
        }
        // The record for an unresolvable name is `<input> missing`, NOT a bare
        // `missing` -- git echoes what it was asked about whatever the
        // `--batch-check` format says. A resolvable one is just the format's
        // output (`commit`), which cannot end in ` missing` because
        // `%(objecttype)` has no space in it.
        Ok(out.stdout.split(|b| *b == b'\n').any(|l| l.ends_with(b" missing")))
    }

    /// The per-field decode boundary. One place, so the failure reads the same
    /// wherever it comes from.
    ///
    /// Invalid UTF-8 is an ERROR, not a lossy string and not raw bytes:
    ///
    /// * Lossy is the status quo and is the defect -- U+FFFD is a *plausible*
    ///   path, so the failure is silent and lands as a false Blocker.
    /// * Raw bytes (`Vec<u8>`/`OsString` in `StatusEntry.path`) is the
    ///   technically correct answer and is NOT done here for a stated reason
    ///   rather than by omission: `path` flows into `Finding.subject`,
    ///   `Finding.summary`, `glob_match(&str, &str)` and serde -> JSON, where
    ///   invalid UTF-8 has no representation at all. Doing it properly is a
    ///   cross-crate retype of the finding pipeline.
    /// * Erroring is consistent with this module's rule, names the offending
    ///   bytes so the user can act, and is loud.
    ///
    /// The cost is honest and written down: one undecodable path aborts the
    /// whole run. Note that `build_artifacts` is NOT the escape hatch -- the
    /// allowlist is matched on the decoded string, so it cannot rescue a path
    /// that failed to decode. If this ever fires in the wild, the fix is to
    /// widen `StatusEntry.path` to bytes and push decoding to the render
    /// boundary; do not reach for the allowlist.
    fn decode(&self, field: &[u8], args: &[&str]) -> Result<String> {
        std::str::from_utf8(field).map(str::to_owned).map_err(|_| {
            self.parse_err(
                args,
                format!("output is not valid UTF-8: {}", field.escape_ascii()),
            )
        })
    }

    /// Success or error, decoded. For commands whose entire output git
    /// guarantees is ASCII: oids, counts, refnames, modes. Invalid UTF-8 here is
    /// not an exotic filename, it is something genuinely wrong.
    fn run(&self, args: &[&str]) -> Result<String> {
        let e = self.exec(args)?;
        match e.code {
            Some(0) => Ok(self.decode(&e.stdout, args)?.trim_end().to_string()),
            _ => Err(self.err(args, &e.stderr)),
        }
    }

    /// Success or error, RAW. For path-bearing commands: the caller owns the
    /// framing and decodes each field itself.
    fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        let e = self.exec(args)?;
        match e.code {
            Some(0) => Ok(e.stdout),
            _ => Err(self.err(args, &e.stderr)),
        }
    }

    /// Exit 0 -> `Some`, exit 1 -> `None`, anything else -> `Err`.
    ///
    /// Exit 1 is git's "the ref does not exist" for `rev-parse --verify
    /// --quiet`; 128 is "I could not read the repository at all" (verified: 1
    /// for a missing ref, 128 when run outside a work tree). The previous code
    /// mapped every nonzero to `None`, so pointing the tool at a path that is
    /// not a repo produced one confident `TIER_MISSING` Blocker per tier, each
    /// carrying the remedy "create it, or remove the [[tier]] entry" -- i.e. it
    /// advised deleting the model because it could not open a directory.
    fn run_opt(&self, args: &[&str]) -> Result<Option<String>> {
        let e = self.exec(args)?;
        match e.code {
            Some(0) => Ok(Some(self.decode(&e.stdout, args)?.trim_end().to_string())),
            Some(1) => Ok(None),
            _ => Err(self.err(args, &e.stderr)),
        }
    }

    /// Exit 0 -> `true`, exit 1 -> `false`, anything else -> `Err`.
    ///
    /// `merge-base --is-ancestor` answers with 1 for "no" and 128 for a bad
    /// ref, an ambiguous name or a corrupt object (verified: 0 / 1 / 128). The
    /// old `status.success()` collapsed all three, and the consequence was not
    /// cosmetic: `false` here raises `TIER_CHILD_STALE`, whose remedy is a
    /// `git rebase --onto` on a hand-authored tier. A tool must not propose
    /// rewriting history because a read failed.
    ///
    /// (The deleted `GitExec::run_tristate` said the same thing in its own
    /// words, and both wordings are kept because they were written
    /// independently: mapping 128 to `false` "makes the tool print a
    /// `git rebase --onto` at a user because it could not read a ref".)
    fn run_bool(&self, args: &[&str]) -> Result<bool> {
        let e = self.exec(args)?;
        match e.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(self.err(args, &e.stderr)),
        }
    }
}

/// Removes the scratch object database.
///
/// Errors are ignored: a failed cleanup must not mask a real verdict. `Drop`
/// runs during unwind, so a panic anywhere in the reconciler still cleans up
/// under the default `panic=unwind`. `panic=abort`, SIGKILL or a power cut leak
/// one directory in tmpfs -- bounded, RAM-backed, gone at reboot, and the
/// `sodactl-odb-` prefix makes any stray identifiable. That is the right amount
/// of engineering for this failure mode; it does not need a reaper.
impl Drop for GitCli {
    fn drop(&mut self) {
        if let Some(Ok(p)) = self.scratch_odb.get() {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

impl GitRead for GitCli {
    fn rev_parse(&self, rev: &str) -> Result<Option<Oid>> {
        self.run_opt(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
            .map(|o| o.filter(|s| !s.is_empty()))
    }

    fn rev_parse_raw(&self, rev: &str) -> Result<Option<Oid>> {
        Ok(self.run_opt(&["rev-parse", "--verify", "--quiet", rev])?.filter(|s| !s.is_empty()))
    }

    fn git_dir(&self) -> Result<PathBuf> {
        // `--absolute-git-dir` rather than `--git-dir` + a join. The join was
        // not wrong, but its correctness rested on an unstated invariant --
        // that `current_dir` is always `self.repo` -- and git will do it
        // itself. Contrary to the design note this does NOT exit 128 in a bare
        // repo (verified on git 2.55.0: `git init --bare X` then
        // `rev-parse --absolute-git-dir` returns the path at exit 0); 128
        // appears only for a nonexistent cwd, which `GitError::Spawn` already
        // names. So this is a simplification, not a bug fix.
        Ok(PathBuf::from(self.run(&["rev-parse", "--absolute-git-dir"])?))
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        self.run_bool(&["merge-base", "--is-ancestor", ancestor, descendant])
    }

    fn reapply_verdict(&self, commit: &str, onto: &str) -> Result<Reapply> {
        // StGit's primitive, borrowed: do not hash the patch -- REAPPLY it and
        // ask whether the result is empty. `git merge-tree` does that without
        // touching the worktree, index or any ref (it writes loose objects,
        // which are unreachable and get collected).
        //
        // Three-way: base = the commit's parent, ours = the target, theirs =
        // the commit. base->theirs is exactly the change; if the target already
        // carries it, the merged tree equals the target's tree.
        let onto_tree = match self.tree_oid(onto)? {
            Some(t) => t,
            None => return Ok(Reapply::Unknown),
        };
        //
        // `--write-tree` really does write: the merged trees and blobs land in
        // the repository as unreachable loose objects and sit there until a `gc`
        // nobody runs. `rr-health` calls this once per rerere postimage, so a
        // routine read-only invocation quietly grew the user's odb. So the write
        // is redirected into a private scratch database whose `info/alternates`
        // points back at the real one: merge-tree can READ every object it
        // needs, and everything it CREATES is discarded on drop.
        //
        // If the redirect is broken (alternates missing, unwritable, pointing
        // nowhere) merge-tree cannot read the base tree and exits 128 --
        // rejected by the `_ => Err` arm below. So a broken redirect can only
        // manufacture an honest error, never a false `Missing`. That property is
        // what makes this safe to do at all.
        let odb = self.scratch_odb()?;
        let merge_base = format!("--merge-base={commit}^");
        // `-z` swaps the terminator after the tree oid and NUL-terminates the
        // conflicted-file records (whose last field is a PATH). We never parse
        // those records, but taking the first NUL-delimited field is now the
        // same operation in the clean and the conflicted case.
        let args = ["merge-tree", "--write-tree", "-z", &merge_base, onto, commit];
        // Through the shared runner, which is what pins LC_ALL=C. This used to
        // build its own `Command` and therefore did NOT -- a correctness gap
        // that existed only because there were two git backends to keep in step
        // and only one of them was maintained.
        let out = self.exec_env(&args, &[("GIT_OBJECT_DIRECTORY", odb)])?;

        // Exit 1 is a CONFLICT, and a conflict is not a verdict. The tree on
        // stdout is a conflicted tree; comparing it to anything answers a
        // question nobody asked. Collapsing it into "missing" is the exact
        // error this crate forbids everywhere else, and it is not hypothetical:
        // measured against the live repo, every cross-base consumer check
        // conflicts here, because BOTH sides introduce the commit's change --
        // the consumer's copy adjusted by rerere during the replay, the
        // original unadjusted -- which is a textbook both-added-differently
        // conflict rather than evidence of absence.
        //
        // Match the exit code FIRST and parse only inside the `Some(0)` arm.
        // With `-z` a conflicted merge's stdout ALSO begins with a valid tree
        // oid, so any tidy-up that hoists "read the first field and compare"
        // above this match would silently start returning `Missing` for every
        // conflict. `Some(1)` and `_` stay distinct for the same reason: 1 is a
        // conflict, while 128 (bad rev, root commit, broken redirect -- all
        // verified) is a failure to answer.
        match out.code {
            Some(0) => {
                let merged =
                    self.decode(split_nul(&out.stdout).next().unwrap_or_default(), &args)?;
                let Some(merged) = looks_like_oid(&merged) else {
                    return Err(
                        self.parse_err(&args, format!("expected a tree oid, got {merged:?}"))
                    );
                };
                Ok(if merged == onto_tree { Reapply::Present } else { Reapply::Missing })
            }
            Some(1) => Ok(Reapply::Unknown),
            _ => Err(self.err(&args, &out.stderr)),
        }
    }

    fn missing_by_patch_id(&self, upstream: &str, head: &str, limit: &str) -> Result<Vec<String>> {
        // `git cherry` compares by patch-id: '+' = not present upstream,
        // '-' = an equivalent patch is already there.
        //
        // This deliberately does NOT swallow failures. `git cherry` exits 128 on
        // an unknown ref (verified), and the old code turned that into an empty
        // vec -- which reads as "nothing is missing", which suppresses
        // STITCH_STALE entirely. A one-character typo in sodactl.toml therefore
        // disabled stitch verification for every tier with no diagnostic at all.
        // This was the single most dangerous swallow in the codebase: it
        // silenced the one check that protects the cherry-pick artifact.
        let out = self.run(&["cherry", upstream, head, limit])?;
        Ok(out
            .lines()
            .filter(|l| l.starts_with('+'))
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect())
    }

    fn ahead_behind(&self, base: &str, head: &str) -> Result<Option<AheadBehind>> {
        let spec = format!("{base}...{head}");
        let args = ["rev-list", "--left-right", "--count", &spec];
        let e = self.exec(&args)?;
        if e.code != Some(0) {
            // A missing tracking ref genuinely IS the answer here ("never
            // pushed"), and the caller distinguishes it from `Some(0,0)`. It is
            // also the ONLY nonzero this method may absorb: the discarded half
            // of this method swallowed *every* nonzero into `Ok(None)`, which
            // renders "I could not open the repository" as "never pushed".
            //
            // It used to be told apart by matching git's stderr for "unknown
            // revision" -- which is INERT on this host, whose git says
            // "Schwerwiegend: mehrdeutiges Argument ... unbekannter Commit".
            // That match worked only because `exec` pins LC_ALL=C, i.e. a
            // correctness property was resting on an unrelated tidiness flag one
            // layer down. Ask git structurally instead: `cat-file
            // --batch-check` reports an unresolvable name with the literal
            // token `missing`, at exit 0, in every locale (verified under
            // de_AT.UTF-8).
            //
            // Deliberately AFTER the failure, not before it. On a healthy repo
            // every call site succeeds, so an eager pre-check would spend two
            // extra spawns per call (~19% of a run) to answer a question nobody
            // asked. It is also strictly more accurate this way: a rev can
            // resolve and `rev-list` still fail, and only this order reports
            // that as the failure it is.
            if self.any_rev_missing(&[base, head])? {
                return Ok(None);
            }
            return Err(self.err(&args, &e.stderr));
        }
        // FIELD ORDER, verified against the live repo before touching this:
        //   $ git rev-list --left-right --count wine-11.12...wine-11.13
        //   0<TAB>198
        //   $ git rev-list --count wine-11.13..wine-11.12   -> 0    (left-only)
        //   $ git rev-list --count wine-11.12..wine-11.13   -> 198  (right-only)
        // Left is left-only = in `base`, not in `head` = BEHIND. Right is ahead.
        // The existing assignment was already correct; the audit flagged it as
        // worth confirming rather than as a defect, and it is recorded here so
        // nobody "fixes" it into a swap.
        let stdout = self.decode(&e.stdout, &args)?;
        let mut it = stdout.split_whitespace();
        match (it.next().and_then(|s| s.parse().ok()), it.next().and_then(|s| s.parse().ok())) {
            (Some(behind), Some(ahead)) => Ok(Some(AheadBehind { ahead, behind })),
            _ => Err(self.parse_err(&args, format!("expected `<behind>\\t<ahead>`, got {stdout:?}"))),
        }
    }

    fn count_commits(&self, range: &str) -> Result<u32> {
        let args = ["rev-list", "--count", range];
        let out = self.run(&args)?;
        // `unwrap_or(0)` here used to turn unreadable output into a confident
        // zero. The call site already renders `-` for an error, so the honest
        // path existed; the default just bypassed it and printed a number that
        // actually meant "I could not read this".
        out.trim()
            .parse()
            .map_err(|_| self.parse_err(&args, format!("expected a count, got {out:?}")))
    }

    fn local_branches(&self) -> Result<Vec<String>> {
        Ok(self
            .run(&["for-each-ref", "--format=%(refname:short)", "refs/heads"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn tags(&self) -> Result<Vec<String>> {
        Ok(self.run(&["tag", "-l"])?.lines().map(str::to_string).collect())
    }

    /// One process instead of one per tag.
    ///
    /// Also deliberately left on line-splitting: `git tag --points-at` emits one
    /// refname per line, peeling is git's, and refnames forbid space and
    /// newline. There is nothing `-z` would fix.
    ///
    /// `--points-at` peels annotated tags itself, which matters because ours are
    /// annotated and the vendored Valve/upstream ones are lightweight -- a
    /// hand-rolled comparison has to handle both and the default impl only does
    /// because `rev_parse` peels via `^{commit}`.
    fn tags_pointing_at(&self, oid: &str) -> Result<Vec<String>> {
        // Verify the oid exists FIRST. `git tag --points-at <full-hex>` exits 0
        // with EMPTY output for an oid the repo does not contain (it only exits
        // nonzero for malformed names), which reads as "untagged" -- and the one
        // oid in this pipeline not guaranteed to exist is the gitlink pin, where
        // "untagged" feeds a Blocker. Florian's rule (2026-07-19): a clear error
        // beats a silent breakdown. This also closes the last recorded
        // gix-vs-binary deviation: both backends now Err on a missing oid.
        //
        // NOT `rev_parse_raw` -- that echoes a full 40-hex without an odb lookup
        // (on both backends, by design), so it would bless exactly the oid this
        // guard exists to catch. `cat-file -e` asks the odb: 0 = exists,
        // 1 = missing, anything else (malformed = 128) is already an Err.
        if !self.run_bool(&["cat-file", "-e", oid])? {
            return Err(GitError::Parse {
                args: vec!["tag".into(), "--points-at".into(), oid.into()],
                cwd: self.repo.display().to_string(),
                detail: format!("oid {oid} does not exist in this repository"),
            });
        }
        Ok(self
            .run(&["tag", "--points-at", oid])?
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
        // `%(objectname)` is the TAG object for an annotated tag, not the
        // commit; `%(*objectname)` is the peel and is empty for a lightweight
        // one. Ours are annotated and Valve's are not, so both fields are
        // needed and the peel wins where present -- concatenating them yields
        // an 80-char non-oid that every later query rejects.
        //
        // `%00` inside the format, because `for-each-ref` has NO `-z` (verified:
        // "Fehler: Unbekannter Schalter `z'"). Line-splitting the RECORDS is
        // safe where it would not be elsewhere: a refname categorically cannot
        // contain a space or a newline (`git check-ref-format`), so the only
        // thing at risk was the name/oid boundary, and `%00` fixes exactly that.
        //
        // `%(objecttype)` is the new field and it earns its place. Without it
        // there is no way to tell "lightweight, so the peel is legitimately
        // empty" from "annotated, peel MISSING" -- and the latter used to fall
        // through to the tag object's own oid, i.e. an oid no commit query can
        // match, rendering a tag as pointing nowhere. This repo has both kinds
        // (verified: 847 annotated, 5769 lightweight), so the fallback runs on
        // every invocation and cannot be reasoned away.
        let args = [
            "for-each-ref",
            "--format=%(objecttype)%00%(refname:short)%00%(objectname)%00%(*objectname)",
            "refs/tags",
        ];
        // A whole-buffer decode is right here because the SPLITTER is `%00` and
        // `\n`, neither of which a refname may contain -- not because refnames
        // are ASCII, which they need not be (git permits non-ASCII UTF-8
        // refnames; this repo happens to have none). Decoding before splitting
        // is only a bug when a replacement char could be mistaken for a
        // delimiter, and U+FFFD is neither of these two.
        let out = self.run(&args)?;
        let mut v = Vec::new();
        for l in out.lines().filter(|l| !l.is_empty()) {
            let f: Vec<&str> = l.split('\0').collect();
            let [ty, name, obj, peel] = f.as_slice() else {
                // A `filter_map` used to drop a short record silently, and a
                // vanished tag reads as an untagged consumer.
                return Err(self
                    .parse_err(&args, format!("expected 4 NUL-separated fields, got {l:?}")));
            };
            let oid = match (*ty, peel.is_empty()) {
                ("tag", true) => {
                    return Err(self
                        .parse_err(&args, format!("annotated tag `{name}` has no peeled target")))
                }
                ("tag", false) => *peel,
                _ => *obj,
            };
            v.push((name.to_string(), oid.to_string()));
        }
        Ok(v)
    }

    /// Deliberately left on plain line-splitting, stated rather than left to
    /// be rediscovered: `%(refname:short)` is the only field, and a refname
    /// cannot contain a space or a newline. `-z` would buy nothing here, and an
    /// untargeted "add -z everywhere" pass would reopen a parser that works.
    fn branches_containing(&self, oid: &str, remote: bool) -> Result<Vec<String>> {
        let scope = if remote { "refs/remotes" } else { "refs/heads" };
        let out = self.run(&["for-each-ref", "--format=%(refname:short)", "--contains", oid, scope])?;
        Ok(out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
    }

    fn log_subjects(&self, range: &str) -> Result<Vec<(Oid, String)>> {
        Ok(self.commit_records(&["--reverse"], range)?.into_iter().map(|(o, _, s)| (o, s)).collect())
    }

    fn merges_in(&self, range: &str) -> Result<Vec<Oid>> {
        // `log --merges` rather than `rev-list --merges`. Identical commit set
        // (verified over `--all`: 1 == 1), but it shares COMMIT_FMT, so the two
        // questions can no longer be answered by two commands that drift apart
        // -- which is the whole subject of this pass.
        Ok(self.commit_records(&["--merges"], range)?.into_iter().map(|(o, _, _)| o).collect())
    }

    fn status(&self, include_untracked: bool) -> Result<Vec<StatusEntry>> {
        // `-z` rather than the default porcelain output, for one reason: without
        // it git C-quotes any path containing a non-ASCII or special byte
        // ("dlls/…") and renders a rename as `R  old -> new` in one field. Both
        // forms would fail to match a `build_artifacts` glob, so the allowlist
        // would silently stop working on exactly the files most likely to be
        // generated -- and the dirty-tree guard would refuse a tree it should
        // have accepted. With `-z` the records are NUL-terminated and never
        // quoted, and a rename's source arrives as its own record -- which must
        // be consumed, not read as a status line.
        let u = if include_untracked { "--untracked-files=normal" } else { "--untracked-files=no" };
        let args = ["status", "--porcelain=v2", "-z", u];
        let out = self.run_bytes(&args)?;
        // Split on NUL FIRST, decode each record after. A Latin-1 filename would
        // otherwise become U+FFFD before the splitter ever saw it -- see
        // `Exec::stdout`.
        let mut recs = split_nul(&out);
        let mut v = Vec::new();
        while let Some(rec) = recs.next() {
            let rec = self.decode(rec, &args)?;
            let e = parse_status_record(&rec).map_err(|d| self.parse_err(&args, d))?;
            // A rename/copy emits the ORIGINAL path as its own bare NUL record
            // with no leading token. Consume it, or the next iteration reads a
            // filename as a record kind and now ERRORS rather than mangling it
            // -- louder, but still wrong, so this skip stays load-bearing.
            if e.kind == StatusKind::Renamed {
                let _ = recs.next();
            }
            v.push(e);
        }
        Ok(v)
    }

    fn head_branch(&self) -> Result<Option<String>> {
        self.run_opt(&["symbolic-ref", "--quiet", "--short", "HEAD"])
    }

    fn config_file_get(&self, file: &str, key: &str) -> Result<Option<String>> {
        // git exits 1 for BOTH "key absent" and "file absent" (verified:
        // `git config -f nosuchfile --get a.b` -> exit 1, no stderr, in any
        // locale), and no flag separates them. So the file's existence has to be
        // established out of band; otherwise a moved, renamed or mis-rooted
        // `.gitmodules` reads as "the branch key is simply not set" -- and the
        // sole caller treats that `None` as a soft outcome while it gates
        // GITMODULES_BRANCH_MISMATCH. That is this module's forbidden pattern in
        // the one place it costs a real finding.
        let args = ["config", "-z", "-f", file, "--get", key];
        let p = self.repo.join(file);
        if !p.exists() {
            return Err(self.err(&args, &format!("{} does not exist", p.display())));
        }
        // `-z` so the value is NUL-terminated rather than newline-terminated:
        // the old `.trim()` also ate a value's own leading and trailing
        // whitespace, which git-config preserves inside quotes.
        let e = self.exec(&args)?;
        match e.code {
            Some(0) => {
                let v = e.stdout.strip_suffix(b"\0").unwrap_or(&e.stdout);
                Ok(Some(self.decode(v, &args)?))
            }
            Some(1) => Ok(None), // now unambiguously "key absent"
            _ => Err(self.err(&args, &e.stderr)),
        }
    }

    fn submodule_index_oid(&self, path: &str) -> Result<Option<Oid>> {
        // Once this feeds a Blocker it has to be exact, and the old
        // `split_whitespace().nth(1)` was not:
        //   * pointed at a plain directory, `ls-files -s` prints one line PER
        //     FILE and it returned the first blob's oid as though it were a
        //     submodule pointer (verified: `ls-files -s -- tools` yields
        //     `100755 a4cf433e… tools/regenerate.sh`);
        //   * during a conflict, stages 1/2/3 each produce a line and it
        //     silently took stage 1 -- the PRE-merge value.
        // So: require exactly one record, mode 160000, stage 0. Anything else is
        // a refusal to answer, never a guess.
        // `--format` rather than `-s`, for one reason: `-s` lays out
        // `<mode> <oid> <stage>\t<path>`, which forces a split on WHITESPACE to
        // reach the three metadata fields -- so the parser's correctness depends
        // on git never widening a field. `--format` makes every separator a tab
        // chosen here, and `splitn(4, '\t')` puts the path in the last piece
        // untouched. `%x09` is accepted (verified, git 2.55.0).
        let args = [
            "ls-files",
            "-z",
            "--format=%(objectmode)%x09%(objectname)%x09%(stage)%x09%(path)",
            "--",
            path,
        ];
        let out = self.run_bytes(&args)?;
        let recs: Vec<&[u8]> = split_nul(&out).collect();
        match recs.as_slice() {
            [] => Ok(None),
            [one] => {
                let one = self.decode(one, &args)?;
                let f: Vec<&str> = one.splitn(4, '\t').collect();
                // Arity FIRST. `[mode, ..]` matched every non-empty slice, which
                // made the `_` arm dead and reported a truncated record as
                // "mode X, not a gitlink" -- an error either way, but one that
                // names the wrong fault and would send a reader looking at the
                // index instead of at the format string.
                match f.as_slice() {
                    [mode, oid, stage, _path] => match (*mode, *stage) {
                        ("160000", "0") => Ok(Some((*oid).to_string())),
                        ("160000", stage) => {
                            Err(self.parse_err(&args, format!("gitlink is at merge stage {stage}")))
                        }
                        (mode, _) => Err(self.parse_err(
                            &args,
                            format!("`{path}` is mode {mode}, not a gitlink (160000)"),
                        )),
                    },
                    _ => Err(self.parse_err(
                        &args,
                        format!("expected 4 tab-separated fields, got {one:?}"),
                    )),
                }
            }
            many => Err(self.parse_err(
                &args,
                format!("`{path}` matched {} index entries; not a single gitlink", many.len()),
            )),
        }
    }

    fn superproject_pin(&self) -> Result<Pin> {
        // Discover the superproject from the submodule rather than requiring it
        // to be passed in. `--show-superproject-working-tree` prints empty (exit
        // 0) when this is not a submodule checkout, which is the "not modelled"
        // case and must surface as Unknown, not as a clean pass.
        let root = self.run(&["rev-parse", "--show-superproject-working-tree"])?;
        if root.trim().is_empty() {
            return Ok(Pin::Unknown {
                reason: format!("{} is not a submodule of any superproject", self.repo.display()),
            });
        }
        let root = PathBuf::from(root.trim());

        // The superproject records the submodule under its path relative to the
        // superproject root, so both sides must be canonical before stripping --
        // `self.repo` is built by joining a manifest-relative string and can
        // contain `..` segments that a textual strip would get wrong.
        // A path that will not canonicalize is "I could not look", never a clean
        // pin -- so it becomes `Pin::Unknown` with the reason, which is exactly
        // what `PIN_UNVERIFIABLE` is for. (It used to ride the blanket
        // `#[from] io::Error`, which produced a bare `os error 2` naming
        // neither path.)
        let (Ok(croot), Ok(cself)) = (root.canonicalize(), self.repo.canonicalize()) else {
            return Ok(Pin::Unknown {
                reason: format!(
                    "could not canonicalize {} or {}",
                    root.display(),
                    self.repo.display()
                ),
            });
        };
        let Ok(rel) = cself.strip_prefix(&croot) else {
            return Ok(Pin::Unknown {
                reason: format!(
                    "{} is not under its superproject {}",
                    cself.display(),
                    croot.display()
                ),
            });
        };
        let Some(rel) = rel.to_str() else {
            return Ok(Pin::Unknown { reason: "submodule path is not valid UTF-8".into() });
        };

        let sup = GitCli::new(&croot);
        let committed = sup.rev_parse_raw(&format!("HEAD:{rel}"))?;
        let staged = sup.submodule_index_oid(rel)?;
        Ok(Pin::Known { committed, staged })
    }
}

impl GitCli {
    /// The tree a rev names.
    ///
    /// Inherent rather than on the trait: its only caller is `reapply_verdict`
    /// just above, and every fake had to answer it dishonestly (`Ok(None)`)
    /// purely to satisfy the trait. A method no caller outside this impl needs
    /// is not part of the read surface.
    fn tree_oid(&self, rev: &str) -> Result<Option<Oid>> {
        self.run_opt(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{tree}}")])
            .map(|o| o.filter(|s| !s.is_empty()))
    }

    /// `(oid, parents, subject)` for a range, oldest-first only if `extra` says
    /// so. The ONE place a commit record's shape is written down, so writer and
    /// parser cannot drift -- there is only one of each.
    ///
    /// `splitn(3, '\t')` rather than `split` plus an arity assertion, and the
    /// distinction is load-bearing: a commit SUBJECT may legally contain a tab,
    /// so a plain 3-way split would reject a valid commit. The first two tabs
    /// are guaranteed to be the real delimiters -- `%H` is exactly 40 hex bytes
    /// and `%P` is oids separated by SPACES (verified on a real merge:
    /// `5b7b606d...\t5e5950d9... 8373146e...\tOn spritzwine: Auto stash...`, and
    /// empty for a root commit, which still leaves arity 3) -- so everything
    /// after the second tab is subject, whatever it contains.
    ///
    /// Fewer than 3 pieces means git did not emit this format, and that is an
    /// error, never a skipped commit: the old `filter_map(split_once)` dropped
    /// unparseable lines silently, and a dropped commit reads as a tier that
    /// does not contain it.
    ///
    /// `%s` already folds a multi-line message to one line, so the old
    /// line-based split was not wrong about SUBJECTS; it was wrong about the
    /// framing around them. Do not "fix" it back on multi-line grounds.
    fn commit_records(&self, extra: &[&str], range: &str) -> Result<Vec<(Oid, Vec<Oid>, String)>> {
        const COMMIT_FMT: &str = "--format=%H%x09%P%x09%s";
        let mut args = vec!["log", "-z", COMMIT_FMT];
        args.extend_from_slice(extra);
        args.push(range);
        // `log -z` NUL-TERMINATES (verified: the buffer ends `\0`, not `\n`), so
        // `split_nul` drops the trailing empty piece, and an empty range yields
        // zero bytes rather than one blank record.
        let out = self.run_bytes(&args)?;
        let mut v = Vec::new();
        for rec in split_nul(&out) {
            let rec = self.decode(rec, &args)?;
            let f: Vec<&str> = rec.splitn(3, '\t').collect();
            let [oid, parents, subject] = f.as_slice() else {
                return Err(self.parse_err(&args, format!("expected 3 tab fields, got {rec:?}")));
            };
            v.push((
                (*oid).to_string(),
                parents.split_whitespace().map(str::to_string).collect(),
                (*subject).to_string(),
            ));
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_pin_prefers_the_staged_gitlink() {
        // A staged bump is what `git commit` would publish, so it is the commit
        // the tag check must run against -- checking HEAD would clear a pin that
        // is one keystroke from being unfetchable.
        let p = Pin::Known { committed: Some("aaa".into()), staged: Some("bbb".into()) };
        assert_eq!(p.effective(), Some("bbb"));
        assert!(p.is_staged());
    }

    #[test]
    fn an_unstaged_pin_is_not_reported_as_staged() {
        let p = Pin::Known { committed: Some("aaa".into()), staged: Some("aaa".into()) };
        assert_eq!(p.effective(), Some("aaa"));
        assert!(!p.is_staged());
    }

    #[test]
    fn unknown_yields_no_oid_so_callers_cannot_silently_pass() {
        let p = Pin::Unknown { reason: "no superproject".into() };
        assert_eq!(p.effective(), None);
    }

    // ---- against a real (tiny, throwaway) repo -----------------------------
    //
    // These exercise the exact confusions the audit found. They need a real
    // `git` because the whole point is git's OWN exit codes and output shapes --
    // a fake would just re-encode whatever the author already believed, which is
    // how the original defects passed review.

    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// What the deleted `dirty_paths()` returned: tracked modifications only.
    ///
    /// The method had no production caller once `status` existed, but its two
    /// regression tests below are paid-for-bug tests and are retargeted rather
    /// than deleted -- they are the guard rail the `status` parser gets rewritten
    /// under.
    fn dirty_paths(g: &GitCli) -> Vec<String> {
        g.status(false).unwrap().into_iter().map(|e| e.path).collect()
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A one-commit repo with a deterministic identity.
    ///
    /// The pid is NOT decoration. The path used to be `{name}` alone while entry
    /// does `remove_dir_all`, so a second concurrent run of this suite -- CI,
    /// `cargo nextest`, a file watcher, a second agent -- deleted the first run's
    /// LIVE repos mid-test. Measured: sequential 0/30 runs failed, ten concurrent
    /// copies 10/10 failed, up to 34 of 44 tests at once. The two odb tests fail
    /// first and their message is "merge-tree polluted the real odb" -- a FALSE
    /// accusation of the very redirect they exist to guard, which is how a guard
    /// gets ignored. `GitCli::scratch_odb` already got this right (pid + nanos);
    /// only the fixtures lagged.
    fn scratch(name: &str) -> (Scratch, GitCli) {
        let dir =
            std::env::temp_dir().join(format!("sodactl-git-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let g = GitCli::new(&dir);
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            g.run(a).unwrap();
        }
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("sub/b.txt"), "b\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "one"]).unwrap();
        (Scratch(dir.clone()), GitCli::new(&dir))
    }

    #[test]
    fn a_bad_ref_is_an_error_not_a_false() {
        // `merge-base --is-ancestor` exits 1 for "no" and 128 for a bad ref. The
        // old `status.success()` collapsed both to `false`, and `false` is what
        // raises TIER_CHILD_STALE -- whose remedy rewrites an authored tier. A
        // failed read must never propose a rebase.
        let (_s, g) = scratch("badref");
        assert!(g.is_ancestor("main", "main").unwrap(), "a ref is its own ancestor");
        assert!(g.is_ancestor("nope-a", "main").is_err(), "unknown ref must be Err, not Ok(false)");
    }

    #[test]
    fn a_missing_ref_is_none_but_a_broken_repo_is_an_error() {
        // rev-parse --verify --quiet: exit 1 = "no such ref" (an answer),
        // exit 128 = "this is not a repository" (a failure). Mapping both to
        // None produced one confident TIER_MISSING Blocker per tier, each
        // advising deletion of the model, whenever the path was wrong.
        let (_s, g) = scratch("missing");
        assert!(g.rev_parse("main").unwrap().is_some());
        assert_eq!(g.rev_parse("no-such-branch").unwrap(), None);

        // Pid-scoped for the same reason `scratch` is.
        let empty =
            std::env::temp_dir().join(format!("sodactl-git-test-notarepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(GitCli::new(&empty).rev_parse("main").is_err(), "non-repo must be Err");
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn a_typo_in_a_cherry_argument_is_loud() {
        // The sharpest swallow: `git cherry` exits 128 on an unknown ref, the
        // old code returned vec![], and an empty vec reads as "nothing missing"
        // -- so a one-character typo in sodactl.toml silently disabled stitch
        // staleness verification with no diagnostic whatsoever.
        let (_s, g) = scratch("cherry");
        assert!(g.missing_by_patch_id("main", "no-such-tier", "main").is_err());
    }

    #[test]
    fn a_plain_directory_is_not_mistaken_for_a_gitlink() {
        // `ls-files -s -- <dir>` prints one line PER FILE. Taking field 1 of the
        // first line returned a blob oid as though it were a submodule pointer.
        // Once this feeds a Blocker it has to refuse rather than guess.
        let (_s, g) = scratch("gitlink");
        assert!(g.submodule_index_oid("sub").is_err(), "a blob must not read as a gitlink");
        assert_eq!(g.submodule_index_oid("not-in-index").unwrap(), None);
    }

    #[test]
    fn a_rename_does_not_mangle_the_next_path() {
        // porcelain `-z` emits a rename as TWO records: `R  <new>\0<old>\0`.
        // The source record carries no XY prefix, so failing to consume it makes
        // the next iteration slice three characters off a real filename. The
        // non-`-z` form is worse still: it C-quotes unusual paths and packs
        // renames as `old -> new`, neither of which can match a build_artifacts
        // glob -- so the dirty-tree guard would refuse a tree it should accept.
        let (s, g) = scratch("rename");
        std::fs::rename(s.0.join("a.txt"), s.0.join("renamed.txt")).unwrap();
        g.run(&["add", "-A"]).unwrap();
        let dirty = dirty_paths(&g);
        assert!(dirty.contains(&"renamed.txt".to_string()), "got {dirty:?}");
        assert!(!dirty.iter().any(|p| p.contains("->")), "rename arrow leaked: {dirty:?}");
    }

    #[test]
    fn a_path_with_a_space_survives_the_status_parser() {
        let (s, g) = scratch("space");
        std::fs::write(s.0.join("a.txt"), "changed\n").unwrap();
        std::fs::write(s.0.join("two words.txt"), "x\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        let dirty = dirty_paths(&g);
        assert!(dirty.contains(&"two words.txt".to_string()), "got {dirty:?}");
        assert!(!dirty.iter().any(|p| p.starts_with('"')), "C-quoting leaked: {dirty:?}");
    }

    #[test]
    #[cfg(unix)]
    fn a_non_utf8_path_is_an_error_not_a_replacement_character() {
        // The one defect `-z` does NOT fix. `String::from_utf8_lossy` over the
        // whole buffer turns `lat\xe9in1.txt` into `lat\u{fffd}in1.txt` BEFORE
        // the splitter runs, so the parser reports a plausible path that no
        // build_artifacts glob can match -- and an allowlisted generated file
        // silently becomes a SUBMODULE_DIRTY Blocker.
        use std::os::unix::ffi::OsStrExt;
        let (s, g) = scratch("latin1");
        let bad = s.0.join(std::ffi::OsStr::from_bytes(b"lat\xe9in1.txt"));
        std::fs::write(&bad, "x\n").unwrap();
        let e = g.status(true).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("not valid UTF-8"), "wrong error: {msg}");
        assert!(msg.contains("lat\\xe9"), "the offending bytes must be named: {msg}");
    }

    #[test]
    fn a_non_ascii_utf8_path_round_trips_intact() {
        // The converse. "reject invalid UTF-8" must not degrade into "reject
        // anything non-ASCII" -- wine's tree is full of legitimate UTF-8 and
        // rejecting it would break every run rather than the exotic case.
        let (s, g) = scratch("unicode");
        std::fs::write(s.0.join("ünïcode.c"), "x\n").unwrap();
        let paths: Vec<String> = g.status(true).unwrap().into_iter().map(|e| e.path).collect();
        assert_eq!(paths, vec!["ünïcode.c".to_string()], "got {paths:?}");
    }

    #[test]
    fn decode_failure_names_the_bytes() {
        // A diagnostic nobody can act on is the same bug one layer up.
        let (_s, g) = scratch("decode");
        let e = g.decode(b"a\xe9b", &["some", "command"]).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("a\\xe9b"), "bytes not shown: {msg}");
        assert_eq!(g.decode(b"plain", &["x"]).unwrap(), "plain");
    }

    #[test]
    fn stderr_stays_readable_when_a_command_fails() {
        // The deliberate asymmetry: stdout is raw and decoded per field, stderr
        // is lossy. An error ABOUT decoding an error message would be strictly
        // worse than an error message containing a replacement character.
        let (_s, g) = scratch("stderrlossy");
        let e = g.run(&["rev-parse", "--verify", "no-such-thing-at-all"]).unwrap_err();
        assert!(matches!(e, GitError::Command { .. }), "expected a git failure, got {e:?}");
    }

    #[test]
    fn a_malformed_status_record_is_an_error() {
        // Signed off: a record this parser does not understand is a failure to
        // read the tree, not a file that happens to be uninteresting. Silently
        // skipping it is how a modified path goes unreported and the
        // SUBMODULE_DIRTY Blocker is not raised.
        assert!(parse_status_record("1 .M N... 100644").is_err(), "short record must be Err");
        assert!(parse_status_record("x whatever").is_err(), "unknown kind must be Err");
        assert!(parse_status_record("").is_err(), "empty record must be Err");
        assert!(parse_status_record("? ").is_err(), "empty path must be Err");
    }

    #[test]
    fn a_path_with_leading_whitespace_survives_verbatim() {
        // THE signed-off behaviour change. `split_at(2)` + `trim_start()` ate
        // the separator and the filename's own leading space together, so
        // `" leading-space.txt"` came back as `"leading-space.txt"` -- a path
        // that matches a different build_artifacts glob than the file on disk.
        let (s, g) = scratch("leadspace");
        std::fs::write(s.0.join(" leading-space.txt"), "y\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        let dirty = dirty_paths(&g);
        assert!(dirty.contains(&" leading-space.txt".to_string()), "space trimmed: {dirty:?}");
    }

    #[test]
    fn a_normal_path_is_not_given_a_phantom_prefix() {
        // The converse. "return everything after a fixed offset" could satisfy
        // the test above by shifting EVERY path one character the other way.
        let (s, g) = scratch("nophantom");
        std::fs::write(s.0.join("a.txt"), "changed\n").unwrap();
        let dirty = dirty_paths(&g);
        assert_eq!(dirty, vec!["a.txt".to_string()], "got {dirty:?}");
    }

    #[test]
    fn an_untracked_path_with_a_leading_space_keeps_it() {
        // v2's `? ` is a TWO-character prefix, which is precisely what a
        // `get(3..)` port of the v1 parse would corrupt -- it would return
        // "untracked lead.txt" for a file actually named " untracked lead.txt".
        let (s, g) = scratch("untrackedlead");
        std::fs::write(s.0.join(" untracked lead.txt"), "z\n").unwrap();
        let paths: Vec<String> =
            g.status(true).unwrap().into_iter().filter(|e| e.is_untracked()).map(|e| e.path).collect();
        assert_eq!(paths, vec![" untracked lead.txt".to_string()], "got {paths:?}");
    }

    #[test]
    fn untracked_is_detected_without_matching_a_status_string() {
        // v1 spelled this `??` and v2 spells it `?`. A caller comparing the XY
        // string would have silently stopped detecting untracked files at the
        // v1->v2 switch, and `xy` is now legitimately EMPTY for these records.
        let (s, g) = scratch("untrackedkind");
        std::fs::write(s.0.join("new.txt"), "n\n").unwrap();
        let e = g.status(true).unwrap();
        let u: Vec<&StatusEntry> = e.iter().filter(|e| e.is_untracked()).collect();
        assert_eq!(u.len(), 1, "got {e:?}");
        assert_eq!(u[0].kind, StatusKind::Untracked);
        assert!(u[0].xy.is_empty(), "v2 untracked records carry no XY: {:?}", u[0]);
    }

    #[test]
    fn every_real_record_kind_parses() {
        // The converse to "a malformed record errors": that rule is a mute
        // button unless the parser demonstrably accepts every shape git really
        // emits. Builds a modification, a rename, an untracked file and a
        // genuine unmerged conflict in one tree.
        let (s, g) = scratch("allkinds");
        // A real both-modified conflict: two branches change the same file.
        std::fs::write(s.0.join("c.txt"), "base\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "c"]).unwrap();
        g.run(&["checkout", "-qb", "side"]).unwrap();
        std::fs::write(s.0.join("c.txt"), "side\n").unwrap();
        g.run(&["commit", "-qam", "side"]).unwrap();
        g.run(&["checkout", "-q", "main"]).unwrap();
        std::fs::write(s.0.join("c.txt"), "main\n").unwrap();
        g.run(&["commit", "-qam", "main"]).unwrap();
        let e = g.exec(&["merge", "side"]).unwrap();
        assert_eq!(e.code, Some(1), "the fixture must actually conflict");

        std::fs::write(s.0.join("a.txt"), "changed\n").unwrap();
        std::fs::rename(s.0.join("sub/b.txt"), s.0.join("sub/renamed.txt")).unwrap();
        g.run(&["add", "sub"]).unwrap();
        std::fs::write(s.0.join("untracked.txt"), "u\n").unwrap();

        let st = g.status(true).unwrap();
        let kinds: BTreeSet<StatusKind> = st.iter().map(|e| e.kind).collect();
        for want in [
            StatusKind::Tracked,
            StatusKind::Renamed,
            StatusKind::Unmerged,
            StatusKind::Untracked,
        ] {
            assert!(kinds.contains(&want), "kind {want:?} never parsed: {st:#?}");
        }
        assert!(st.iter().any(|e| e.path == "sub/renamed.txt"));
        assert!(st.iter().any(|e| e.path == "c.txt" && e.kind == StatusKind::Unmerged));
        assert!(!st.iter().any(|e| e.path.contains("->")), "rename arrow leaked: {st:#?}");
    }

    #[test]
    fn ahead_behind_puts_the_left_column_in_behind() {
        // Field order, pinned by a test because it is invisible at the call site
        // and a swap would silently invert every divergence remedy.
        // `rev-list --left-right --count <base>...<head>` prints
        // `<left-only>\t<right-only>`; left-only is in base and not in head =
        // BEHIND.
        let (s, g) = scratch("aheadbehind");
        g.run(&["checkout", "-qb", "side"]).unwrap();
        for n in ["s1", "s2"] {
            std::fs::write(s.0.join(format!("{n}.txt")), "x\n").unwrap();
            g.run(&["add", "-A"]).unwrap();
            g.run(&["commit", "-qm", n]).unwrap();
        }
        // head=side is 2 ahead of base=main, 0 behind.
        let ab = g.ahead_behind("main", "side").unwrap().unwrap();
        assert_eq!((ab.ahead, ab.behind), (2, 0));
        // Reversed: main is 0 ahead, 2 behind side.
        let ab = g.ahead_behind("side", "main").unwrap().unwrap();
        assert_eq!((ab.ahead, ab.behind), (0, 2));
    }

    #[test]
    fn a_missing_endpoint_is_none_not_an_error() {
        // "never pushed" is a real answer to this question and the only nonzero
        // `ahead_behind` may absorb.
        let (_s, g) = scratch("ab-missing");
        assert_eq!(g.ahead_behind("no-such-ref", "main").unwrap(), None);
        assert_eq!(g.ahead_behind("main", "no-such-ref").unwrap(), None);
    }

    #[test]
    fn endpoint_classification_does_not_read_translated_stderr() {
        // THE converse, and the one that matters. The old classifier matched
        // git's stderr for "unknown revision". Under a translated locale git
        // emits no such substring -- on this host it says "Schwerwiegend:
        // mehrdeutiges Argument ... unbekannter Commit" -- so the match was
        // inert except where LC_ALL=C happened to be pinned one layer down, and
        // a routine never-pushed tier became a hard error that aborted the run.
        //
        // Asserting through `ahead_behind` alone would NOT discriminate: `exec`
        // pins LC_ALL=C, so the substring path would pass too. So this tests the
        // classifier directly, with the environment forced the other way. It is
        // locale-independent because `cat-file --batch-check` answers with a
        // token, not prose.
        let (_s, g) = scratch("ab-locale");

        // First: establish that git's prose really is not what we rely on. Run
        // the failing rev-list under a translated locale and confirm that IF the
        // host has translations, the old substring is absent -- i.e. the old
        // implementation would have misclassified here.
        let out = Command::new("git")
            .current_dir(&g.repo)
            .env_remove("LC_ALL")
            .env("LANGUAGE", "de")
            .env("LANG", "de_AT.UTF-8")
            .env("LC_MESSAGES", "de_AT.UTF-8")
            .args(["rev-list", "--left-right", "--count", "no-such-ref...main"])
            .output()
            .unwrap();
        let translated = String::from_utf8_lossy(&out.stderr).contains("unbekannt");

        // Second, and unconditionally: the structural classifier is right about
        // both directions, and it never looked at that string.
        assert!(g.any_rev_missing(&["no-such-ref"]).unwrap());
        assert!(g.any_rev_missing(&["main", "no-such-ref"]).unwrap(), "one missing is enough");
        assert!(!g.any_rev_missing(&["main"]).unwrap(), "a resolvable rev must not read as missing");
        assert_eq!(g.ahead_behind("no-such-ref", "main").unwrap(), None);

        if translated {
            // The host proved the hazard is live rather than theoretical.
            assert!(
                !String::from_utf8_lossy(&out.stderr).contains("unknown revision"),
                "a translated stderr must not contain the substring the old code matched"
            );
        }
    }

    #[test]
    fn a_broken_repo_is_an_error_not_a_missing_endpoint() {
        // The direction the discarded implementation got wrong: it mapped every
        // nonzero to `Ok(None)`, so "I could not open the repository" rendered
        // as "this tier was never pushed" -- a clean-looking finding produced by
        // a total failure to read anything.
        // Pid-scoped for the same reason `scratch` is.
        let empty = std::env::temp_dir()
            .join(format!("sodactl-git-test-ab-notarepo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        assert!(GitCli::new(&empty).ahead_behind("a", "b").is_err());
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn tags_pointing_at_agrees_with_the_generic_default() {
        // GitCli overrides `tags_pointing_at` for speed (6616 spawns -> 1). The
        // override must agree with the defaulted implementation the fake uses,
        // including on ANNOTATED tags, which need peeling.
        let (_s, g) = scratch("tags");
        let head = g.rev_parse("main").unwrap().unwrap();
        g.run(&["tag", "light", "main"]).unwrap();
        g.run(&["tag", "-a", "-m", "annotated", "heavy", "main"]).unwrap();

        let mut fast = g.tags_pointing_at(&head).unwrap();
        fast.sort();
        // The default impl, invoked explicitly for comparison.
        let mut slow: Vec<String> = g
            .tags()
            .unwrap()
            .into_iter()
            .filter(|t| g.rev_parse(t).unwrap().as_deref() == Some(head.as_str()))
            .collect();
        slow.sort();
        assert_eq!(fast, slow);
        assert_eq!(fast, vec!["heavy".to_string(), "light".to_string()]);
    }

    #[test]
    fn a_subject_containing_a_tab_is_not_truncated() {
        // `splitn(3, '\t')` rather than a plain 3-way split with an arity
        // assertion: a subject may legally contain a tab, and only the FIRST
        // two tabs are the real delimiters (%H is fixed-width, %P is
        // space-separated).
        let (s, g) = scratch("subjtab");
        std::fs::write(s.0.join("t.txt"), "t\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "fix\tthing"]).unwrap();
        let subs = g.log_subjects("HEAD~1..HEAD").unwrap();
        assert_eq!(subs.len(), 1, "{subs:?}");
        assert_eq!(subs[0].1, "fix\tthing", "subject truncated at its own tab");
    }

    #[test]
    fn a_normal_subject_splits_at_the_right_tab() {
        // The converse: the fix must not shift the boundary the other way.
        let (s, g) = scratch("subjnormal");
        std::fs::write(s.0.join("t.txt"), "t\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "plain subject"]).unwrap();
        let subs = g.log_subjects("HEAD~1..HEAD").unwrap();
        assert_eq!(subs[0].0.len(), 40, "oid must be a bare 40-char sha: {:?}", subs[0].0);
        assert_eq!(subs[0].1, "plain subject");
    }

    #[test]
    fn a_merge_records_all_its_parents_and_a_root_commit_none() {
        // Arity 3 has to hold at both extremes: a merge whose %P field carries
        // two space-separated oids, and a root commit whose %P is EMPTY.
        let (s, g) = scratch("parents");
        g.run(&["checkout", "-qb", "side"]).unwrap();
        std::fs::write(s.0.join("s.txt"), "s\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "side"]).unwrap();
        g.run(&["checkout", "-q", "main"]).unwrap();
        std::fs::write(s.0.join("m.txt"), "m\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "main"]).unwrap();
        g.run(&["merge", "-q", "--no-ff", "-m", "merge it", "side"]).unwrap();

        let recs = g.commit_records(&["--merges"], "HEAD~2..HEAD").unwrap();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].1.len(), 2, "merge must report both parents: {recs:?}");

        let root = g.commit_records(&["--max-parents=0"], "HEAD").unwrap();
        assert_eq!(root.len(), 1, "{root:?}");
        assert!(root[0].1.is_empty(), "root commit has no parents: {root:?}");
        assert_eq!(root[0].2, "one");
    }

    #[test]
    fn merges_in_agrees_with_rev_list_merges() {
        // The A/B that would have caught the original drift: `merges_in` used
        // `rev-list --merges` while `log_subjects` used `log`, two commands
        // answering one overlapping question.
        let (s, g) = scratch("mergesab");
        g.run(&["checkout", "-qb", "side"]).unwrap();
        std::fs::write(s.0.join("s.txt"), "s\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "side"]).unwrap();
        g.run(&["checkout", "-q", "main"]).unwrap();
        std::fs::write(s.0.join("m.txt"), "m\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "main"]).unwrap();
        g.run(&["merge", "-q", "--no-ff", "-m", "merge it", "side"]).unwrap();

        let ours = g.merges_in("HEAD~2..HEAD").unwrap();
        let theirs: Vec<String> = g
            .run(&["rev-list", "--merges", "HEAD~2..HEAD"])
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(ours, theirs, "log --merges and rev-list --merges disagree");
        assert_eq!(ours.len(), 1);
    }

    #[test]
    fn an_empty_range_yields_no_records() {
        // `log -z` NUL-TERMINATES, so a naive split would produce one blank
        // record; an empty range must produce zero, not one unparseable one.
        let (_s, g) = scratch("emptyrange");
        assert!(g.log_subjects("HEAD..HEAD").unwrap().is_empty());
        assert!(g.merges_in("HEAD..HEAD").unwrap().is_empty());
    }

    #[test]
    fn an_annotated_and_a_lightweight_tag_both_resolve_to_their_commit() {
        // Both kinds are live in the real repo (847 annotated, 5769
        // lightweight), so `%(objectname)` vs `%(*objectname)` is exercised on
        // every run and cannot be reasoned away.
        let (_s, g) = scratch("tagtargets");
        let head = g.rev_parse("main").unwrap().unwrap();
        g.run(&["tag", "light", "main"]).unwrap();
        g.run(&["tag", "-a", "-m", "annotated", "heavy", "main"]).unwrap();

        let mut t = g.tags_with_targets().unwrap();
        t.sort();
        assert_eq!(
            t,
            vec![("heavy".to_string(), head.clone()), ("light".to_string(), head.clone())],
            "both tag kinds must resolve to the COMMIT"
        );
    }

    #[test]
    fn an_annotated_tag_never_reports_its_own_tag_object() {
        // The converse. "prefer the peel" passes trivially if the peel is
        // silently the tag object; concatenating the two fields used to yield an
        // 80-char non-oid that every later query rejects.
        let (_s, g) = scratch("tagobj");
        g.run(&["tag", "-a", "-m", "annotated", "heavy", "main"]).unwrap();
        let tag_object = g.run(&["rev-parse", "refs/tags/heavy"]).unwrap();
        let commit = g.rev_parse("main").unwrap().unwrap();
        assert_ne!(tag_object, commit, "fixture: an annotated tag is its own object");

        let t = g.tags_with_targets().unwrap();
        let heavy = &t.iter().find(|(n, _)| n == "heavy").unwrap().1;
        assert_eq!(heavy, &commit);
        assert_ne!(heavy, &tag_object, "reported the tag object instead of its target");
        assert_eq!(heavy.len(), 40, "not an 80-char concatenation");
    }

    #[test]
    fn a_conflicted_gitlink_is_an_error_not_stage_one() {
        // A conflicted path lists stages 1/2/3, and taking the first silently
        // returns the PRE-merge value -- presented as the pin. Constructed with
        // a real conflicting change to a submodule gitlink.
        let (s, g) = scratch("gitlinkconflict");
        let subdir = s.0.join("mod");
        std::fs::create_dir_all(&subdir).unwrap();
        let sg = GitCli::new(&subdir);
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            sg.run(a).unwrap();
        }
        std::fs::write(subdir.join("x.txt"), "1\n").unwrap();
        sg.run(&["add", "-A"]).unwrap();
        sg.run(&["commit", "-qm", "s1"]).unwrap();
        g.run(&["-c", "protocol.file.allow=always", "submodule", "-q", "add", "./mod", "mod"])
            .unwrap();
        g.run(&["commit", "-qm", "add sub"]).unwrap();
        assert!(g.submodule_index_oid("mod").unwrap().is_some(), "clean gitlink reads at stage 0");

        // Two branches move the gitlink to different commits.
        g.run(&["checkout", "-qb", "side"]).unwrap();
        std::fs::write(subdir.join("x.txt"), "2\n").unwrap();
        sg.run(&["commit", "-qam", "s2"]).unwrap();
        g.run(&["add", "mod"]).unwrap();
        g.run(&["commit", "-qm", "bump side"]).unwrap();
        g.run(&["checkout", "-q", "main"]).unwrap();
        // Back to s1 before branching the submodule the other way: s2 and s3
        // must DIVERGE, or git resolves the gitlink merge by fast-forward and
        // the fixture never conflicts.
        sg.run(&["reset", "-q", "--hard", "HEAD~1"]).unwrap();
        std::fs::write(subdir.join("x.txt"), "3\n").unwrap();
        sg.run(&["commit", "-qam", "s3"]).unwrap();
        g.run(&["add", "mod"]).unwrap();
        g.run(&["commit", "-qm", "bump main"]).unwrap();
        let m = g.exec(&["merge", "side"]).unwrap();
        assert_eq!(m.code, Some(1), "the fixture must actually conflict");

        let r = g.submodule_index_oid("mod");
        assert!(r.is_err(), "a conflicted gitlink must refuse, not return stage 1: {r:?}");
    }

    #[test]
    fn a_submodule_path_with_a_space_parses() {
        // What the `-s` -> `--format` move is FOR: the metadata fields must
        // still split correctly when `%(path)` contains spaces.
        let (s, g) = scratch("gitlinkspace");
        let subdir = s.0.join("my mod");
        std::fs::create_dir_all(&subdir).unwrap();
        let sg = GitCli::new(&subdir);
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            sg.run(a).unwrap();
        }
        std::fs::write(subdir.join("x.txt"), "1\n").unwrap();
        sg.run(&["add", "-A"]).unwrap();
        sg.run(&["commit", "-qm", "s1"]).unwrap();
        let head = sg.rev_parse("main").unwrap().unwrap();
        g.run(&["-c", "protocol.file.allow=always", "submodule", "-q", "add", "./my mod", "my mod"])
            .unwrap();
        assert_eq!(g.submodule_index_oid("my mod").unwrap(), Some(head));
    }

    #[test]
    fn git_dir_resolves_a_submodule_gitdir_file() {
        // `wine/.git` is a FILE containing `gitdir: ...`. Anything that stats
        // `rebase-merge` or `rr-cache` under an unresolved gitdir inspects a
        // directory that does not exist and passes forever.
        let (s, g) = scratch("gitdirsub");
        let subdir = s.0.join("mod");
        std::fs::create_dir_all(&subdir).unwrap();
        let sg = GitCli::new(&subdir);
        for a in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ] {
            sg.run(a).unwrap();
        }
        std::fs::write(subdir.join("x.txt"), "1\n").unwrap();
        sg.run(&["add", "-A"]).unwrap();
        sg.run(&["commit", "-qm", "s1"]).unwrap();
        g.run(&["-c", "protocol.file.allow=always", "submodule", "-q", "add", "./mod", "mod"])
            .unwrap();
        g.run(&["commit", "-qm", "add sub"]).unwrap();
        // Move the gitdir into `.git/modules/mod` and leave a `gitdir:` FILE
        // behind -- which is exactly wine's layout, and the thing this method
        // has to resolve.
        g.run(&["submodule", "absorbgitdirs"]).unwrap();
        assert!(s.0.join("mod/.git").is_file(), "fixture: .git must be a file here");

        let gd = GitCli::new(s.0.join("mod")).git_dir().unwrap();
        assert!(gd.is_absolute(), "{gd:?}");
        assert!(gd.is_dir(), "the gitdir must be a real directory: {gd:?}");
        assert_ne!(gd, s.0.join("mod/.git"), "that path is a FILE, not the gitdir");
    }

    #[test]
    fn git_dir_of_a_plain_repo_is_its_dot_git() {
        // The converse, so resolving the indirection did not break the ordinary
        // case.
        let (s, g) = scratch("gitdirplain");
        assert_eq!(g.git_dir().unwrap(), s.0.canonicalize().unwrap().join(".git"));
    }

    #[test]
    fn a_missing_config_file_is_an_error_not_an_absent_key() {
        // git exits 1 for both, so a moved or mis-rooted `.gitmodules` used to
        // read as "the branch key is simply not set" -- and the caller treats
        // that None as benign while it gates GITMODULES_BRANCH_MISMATCH.
        let (_s, g) = config_repo("cfg-missing");
        assert!(
            g.config_file_get("nosuchfile", "submodule.wine.branch").is_err(),
            "a missing config FILE must not read as an absent key"
        );
    }

    #[test]
    fn an_absent_key_in_a_present_file_is_none() {
        // The converse: the fix must not over-correct into erroring on every
        // unset key, which is a legitimate and load-bearing `None`.
        let (_s, g) = config_repo("cfg-absent");
        assert_eq!(g.config_file_get(".gitmodules", "submodule.nope.branch").unwrap(), None);
        assert_eq!(
            g.config_file_get(".gitmodules", "submodule.wine.branch").unwrap(),
            Some("soda-full".to_string())
        );
    }

    #[test]
    fn a_quoted_config_value_keeps_its_whitespace() {
        // The `-z` half. The old `.trim()` ate a value's own leading and
        // trailing whitespace, which git-config preserves inside quotes.
        let (_s, g) = config_repo("cfg-quoted");
        assert_eq!(g.config_file_get(".gitmodules", "submodule.wine.pad").unwrap(), Some(" x ".to_string()));
    }

    /// A repo with a `.gitmodules` carrying one plain and one padded value.
    fn config_repo(name: &str) -> (Scratch, GitCli) {
        let (s, g) = scratch(name);
        std::fs::write(
            s.0.join(".gitmodules"),
            "[submodule \"wine\"]\n\tbranch = soda-full\n\tpad = \" x \"\n",
        )
        .unwrap();
        (s, g)
    }

    /// Loose objects in a repo's own odb, ignoring anything the alternates
    /// chain makes merely readable.
    fn loose_count(repo: &Path) -> usize {
        let objects = GitCli::new(repo).git_dir().unwrap().join("objects");
        let mut n = 0;
        for d in std::fs::read_dir(&objects).unwrap().flatten() {
            let name = d.file_name();
            let name = name.to_string_lossy();
            if name.len() == 2 && name.chars().all(|c| c.is_ascii_hexdigit()) {
                n += std::fs::read_dir(d.path()).unwrap().count();
            }
        }
        n
    }

    /// A repo where `commit` conflicts when reapplied onto `onto`.
    fn conflicting_reapply(name: &str) -> (Scratch, GitCli, String, String) {
        let (s, g) = scratch(name);
        std::fs::write(s.0.join("f.txt"), "base\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "base"]).unwrap();
        g.run(&["checkout", "-qb", "side"]).unwrap();
        std::fs::write(s.0.join("f.txt"), "side change\n").unwrap();
        g.run(&["commit", "-qam", "side"]).unwrap();
        let commit = g.rev_parse("side").unwrap().unwrap();
        g.run(&["checkout", "-q", "main"]).unwrap();
        std::fs::write(s.0.join("f.txt"), "main change\n").unwrap();
        g.run(&["commit", "-qam", "main"]).unwrap();
        let onto = g.rev_parse("main").unwrap().unwrap();
        (s, g, commit, onto)
    }

    #[test]
    fn a_reapply_writes_nothing_into_the_real_odb() {
        // The whole point of the scratch odb: `merge-tree --write-tree` really
        // does write, and every unreachable object it left behind landed in the
        // user's repository and stayed until a `gc` nobody runs.
        let (s, g, commit, onto) = conflicting_reapply("odb-redirect");
        let before = loose_count(&s.0);
        let v = g.reapply_verdict(&commit, &onto).unwrap();
        assert_eq!(v, Reapply::Unknown, "fixture must conflict");
        assert_eq!(loose_count(&s.0), before, "merge-tree polluted the real odb");

        // The converse half: a change that quietly stopped invoking merge-tree
        // at all would satisfy the assertion above trivially.
        let scratch_dir = g.scratch_odb().unwrap().to_path_buf();
        let wrote = std::fs::read_dir(&scratch_dir)
            .unwrap()
            .flatten()
            .filter(|d| {
                let n = d.file_name();
                let n = n.to_string_lossy();
                n.len() == 2 && n.chars().all(|c| c.is_ascii_hexdigit())
            })
            .count();
        assert!(wrote > 0, "nothing was written to the scratch odb -- did merge-tree run?");
    }

    #[test]
    fn a_broken_redirect_is_an_error_not_a_missing_verdict() {
        // Pins the property the redirect rests on: if the alternates file does
        // not resolve, merge-tree cannot read the base tree and exits 128, which
        // the `_ => Err` arm rejects. A broken redirect can only manufacture an
        // honest error, never a false `Missing`.
        let (s, g, commit, onto) = conflicting_reapply("odb-broken");
        let odb = g.scratch_odb().unwrap().to_path_buf();
        std::fs::write(odb.join("info/alternates"), "/nonexistent/sodactl/odb\n").unwrap();
        let r = g.reapply_verdict(&commit, &onto);
        assert!(r.is_err(), "a broken redirect must be Err, got {r:?}");
        drop(s);
    }

    #[test]
    fn an_unreadable_merge_tree_field_is_not_a_missing_verdict() {
        // The converse of the `looks_like_oid` gate. `Missing` is the only
        // verdict reconcile renders as "(confirmed by reapply)", so it must be
        // unreachable from anything that is not an oid we read. Before the gate
        // an empty field simply compared unequal to the target tree, and the
        // failure to read came out the far end as a measurement.
        let good = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
        assert_eq!(looks_like_oid(&format!("  {good}\n")).unwrap(), good);
        for bad in ["", "   ", "\n", "warning: something", "not-an-oid", "4b825dc6z"] {
            assert!(looks_like_oid(bad).is_none(), "{bad:?} must not read as an oid");
        }
    }

    #[test]
    fn fixture_scratch_paths_are_scoped_to_this_process() {
        // The converse of the pid suffix. `scratch` does `remove_dir_all` on
        // entry, so a shared path means a second concurrent run of this suite
        // deletes the first run's LIVE repos -- measured 10/10 failing runs with
        // ten concurrent copies, and the odb tests fail first with "merge-tree
        // polluted the real odb", falsely accusing the redirect they guard.
        let (s, _g) = scratch("pid-scope");
        let name = s.0.file_name().unwrap().to_str().unwrap().to_string();
        assert!(
            name.ends_with(&format!("-{}", std::process::id())),
            "fixture path {name:?} must carry the pid"
        );
    }

    #[test]
    fn a_conflict_is_unknown_and_a_clean_reapply_is_decisive() {
        // Three-valued and it must stay that way: a conflict is NOT evidence of
        // absence. `Some(1)` and the `_` arm are deliberately distinct.
        let (s, g, commit, onto) = conflicting_reapply("odb-verdicts");
        assert_eq!(g.reapply_verdict(&commit, &onto).unwrap(), Reapply::Unknown);

        // Clean and genuinely absent: a change to an untouched file.
        g.run(&["checkout", "-qb", "other", "main"]).unwrap();
        std::fs::write(s.0.join("new.txt"), "n\n").unwrap();
        g.run(&["add", "-A"]).unwrap();
        g.run(&["commit", "-qm", "adds new.txt"]).unwrap();
        let added = g.rev_parse("other").unwrap().unwrap();
        assert_eq!(g.reapply_verdict(&added, &onto).unwrap(), Reapply::Missing);
        // ...and against a target that already carries it, a no-op.
        assert_eq!(g.reapply_verdict(&added, &added).unwrap(), Reapply::Present);
    }

    #[test]
    fn the_scratch_odb_is_removed_on_drop() {
        let (s, g, commit, onto) = conflicting_reapply("odb-cleanup");
        let _ = g.reapply_verdict(&commit, &onto).unwrap();
        let odb = g.scratch_odb().unwrap().to_path_buf();
        assert!(odb.is_dir());
        drop(g);
        assert!(!odb.exists(), "scratch odb leaked: {odb:?}");
        drop(s);
    }

    #[test]
    fn reapply_errors_are_pinned_to_the_c_locale() {
        // This call path used to build its own `Command` with no LC_ALL=C, so
        // its diagnostics were whatever the host's locale produced -- the exact
        // gap that the two-backends split kept alive. Routing it through the
        // shared runner is what fixes it.
        let (_s, g, _c, onto) = conflicting_reapply("odb-locale");
        let e = g.reapply_verdict("no-such-commit", &onto).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("fatal:"), "stderr not in the C locale: {msg}");
    }

    #[test]
    fn a_standalone_repo_reports_an_unknown_pin_not_a_clean_one() {
        // No superproject is "I cannot tell you", never "the pin is fine".
        let (_s, g) = scratch("nopin");
        assert!(matches!(g.superproject_pin().unwrap(), Pin::Unknown { .. }));
    }
}
