//! The gitoxide backend, and the hybrid that routes between it and the binary.
//!
//! The seam is BY CAPABILITY, not taste, and it is a type boundary on purpose:
//! `grep "impl GitRead for GitGix"` answers "what does gix own?" exactly.
//!
//! * [`GitGix`] owns the 18 methods gix can answer CORRECTLY (measured in
//!   `docs/sodactl-git-backend.md` against the live repo).
//! * The `git` binary keeps exactly two -- `status` and `missing_by_patch_id`
//!   -- and `GitGix` returns a typed [`GitError::Unsupported`] for them, so
//!   the hole is honest and can never read as a benign answer. The rationale
//!   lives on those two methods below; do NOT "finish the migration".
//! * [`GitHybrid`] is the production type: pure per-method delegation.
//!
//! `GitCli` stays COMPLETE and becomes the *reference backend*: the oracle the
//! differential harness compares this backend against. Behaviour changes land
//! there first and gix must then match.
//!
//! # The rule, restated for a library backend
//!
//! **`Ok(None)` may only come from a STRUCTURAL gix signal, never from
//! matching error text.** This is the same rule that killed the German-stderr
//! classifier, one layer up. Where gix has a `try_*` API the signal is native;
//! where it does not (the rev-spec grammar, whose 0.85 errors are erased
//! `message!` prose -- see `rev_parse`), absence is established by resolving
//! the spec's ANCHOR through typed lookups, and anything unclassifiable is
//! `Err`, the loud direction.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::{AheadBehind, GitCli, GitError, GitRead, Oid, Pin, Reapply, Result, StatusEntry};

/// Read-only gitoxide backend rooted at one repository.
///
/// # Repository handle discipline
///
/// * **Lazy, error-cached open.** `new` is infallible like `GitCli::new`; the
///   `gix::open` result is cached in a `OnceLock`, so a non-repo path errors
///   IDENTICALLY on every method call rather than differently per call -- the
///   exact discipline `GitCli::scratch_odb` established.
/// * **Snapshot TOCTOU.** An opened `gix::Repository` holds a packed-refs and
///   index view; `build.sh`'s `submodule update --force` can rewrite the
///   gitlink mid-run. Rule: a `GitGix` must not outlive one command
///   invocation. We hold the `ThreadSafeRepository` and `.to_thread_local()`
///   per method call, which gives each call a fresh ref snapshot -- the
///   TOCTOU window is per-call, i.e. no *wider* than the binary's.
pub struct GitGix {
    repo: PathBuf,
    opened: OnceLock<std::result::Result<gix::ThreadSafeRepository, String>>,
}

/// Flatten an error's source chain into one line. gix errors are deeply
/// nested and the root cause is usually three levels down; `Display` on the
/// top frame alone routinely says nothing actionable.
fn chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}

impl GitGix {
    pub fn new(repo: impl AsRef<Path>) -> Self {
        Self { repo: repo.as_ref().to_path_buf(), opened: OnceLock::new() }
    }

    fn open(&self) -> Result<gix::Repository> {
        let r = self.opened.get_or_init(|| {
            gix::ThreadSafeRepository::open(&self.repo).map_err(|e| chain(&e))
        });
        match r {
            Ok(tsr) => Ok(tsr.to_thread_local()),
            Err(e) => Err(self.err("open", e.clone())),
        }
    }

    fn err(&self, op: &'static str, detail: impl Into<String>) -> GitError {
        GitError::Gix { op, cwd: self.repo.display().to_string(), detail: detail.into() }
    }

    fn gerr(&self, op: &'static str, e: impl std::error::Error + 'static) -> GitError {
        self.err(op, chain(&e))
    }

    /// Strict per-field decode, mirroring `GitCli::decode`: invalid UTF-8 in a
    /// value that flows into findings is an ERROR that names the bytes, never
    /// a lossy string (U+FFFD is a *plausible* value, so the failure would be
    /// silent).
    fn decode(&self, op: &'static str, field: &[u8]) -> Result<String> {
        std::str::from_utf8(field)
            .map(str::to_owned)
            .map_err(|_| self.err(op, format!("output is not valid UTF-8: {}", field.escape_ascii())))
    }

    /// Resolve a rev-spec to the object it names WITHOUT peeling, `Ok(None)`
    /// iff the spec names nothing -- the gix analogue of
    /// `rev-parse --verify --quiet` exit 1.
    ///
    /// The `rev:path` shape is handled STRUCTURALLY before the grammar ever
    /// runs: split at the first `:` (a refname categorically cannot contain
    /// one), resolve the anchor, walk the tree. That is what makes
    /// `HEAD:no/such/path -> Ok(None)` a typed `Option`, not an error-text
    /// match -- and it is the shape `superproject_pin` reads the gitlink
    /// through, the single most important fact this tool reads.
    fn resolve_raw(&self, repo: &gix::Repository, rev: &str) -> Result<Option<gix::ObjectId>> {
        const OP: &str = "rev-parse";
        // A spec that is exactly one full hex oid echoes back WITHOUT an odb
        // lookup, because that is what the oracle does: `rev-parse --verify
        // --quiet <40-hex>` exits 0 and prints the hex even for an object the
        // repo has never contained (verified on git 2.55). Without this,
        // `rev_parse_single` fails the lookup and `anchor_is_absent` would
        // manufacture `Ok(None)` -- the QUIET direction, where "names this"
        // silently became "names nothing" (found 2026-07-19). The PEELED
        // `rev_parse` is unaffected on purpose: `peel_object`'s
        // `try_find_object -> None` keeps `<hex>^{commit}` at `Ok(None)`,
        // which is also what the binary's `^{commit}` form reports.
        if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
            let id = gix::ObjectId::from_hex(rev.to_ascii_lowercase().as_bytes())
                .map_err(|e| self.err(OP, format!("hex spec {rev:?}: {e}")))?;
            return Ok(Some(id));
        }
        if let Some((anchor, path)) = rev.split_once(':') {
            if !anchor.is_empty() && !path.is_empty() {
                let Some(id) = self.resolve_raw(repo, anchor)? else {
                    return Ok(None); // anchor names nothing -> the spec does too
                };
                // Peel the anchor to a tree. A non-treeish anchor is what the
                // binary reports as exit 1 under --quiet: the spec verifies to
                // nothing.
                let tree = match self.peel_object(repo, id, gix::object::Kind::Tree, OP)? {
                    Some(t) => t.into_tree(),
                    None => return Ok(None),
                };
                let entry = tree
                    .lookup_entry_by_path(path)
                    .map_err(|e| self.gerr(OP, e))?;
                return Ok(entry.map(|e| e.object_id()));
            }
        }
        match repo.rev_parse_single(rev) {
            Ok(id) => Ok(Some(id.detach())),
            // gix 0.85's spec errors are erased `message!` prose, NOT the
            // typed enum an earlier design assumed -- so the error itself is
            // unclassifiable without matching text, which is forbidden here.
            // Instead: establish absence POSITIVELY, by resolving the spec's
            // anchor through typed lookups. Unclassifiable stays `Err`, the
            // loud direction (identical polarity to the exit-code rule: only
            // a whitelisted signal is an answer; unknown is a failure).
            Err(e) => {
                if self.anchor_is_absent(repo, rev)? {
                    Ok(None)
                } else {
                    Err(self.gerr(OP, e))
                }
            }
        }
    }

    /// Is the spec's ANCHOR -- the leading name before any navigation
    /// (`^ ~ : @{`) -- neither a findable ref nor an existing full object id?
    ///
    /// This is deliberately NOT a re-implementation of the grammar:
    /// resolution still goes through `rev_parse_single` above, and this only
    /// classifies an already-failed parse. A misclassification errs toward
    /// `Err` (loud) -- e.g. a spec whose anchor exists but whose navigation
    /// failed (`main~1000`) is `Err` here where the binary's `--quiet` says
    /// `None`; no caller writes such specs (they pass tier names, tags, full
    /// oids, and `HEAD:<path>` -- grepped, and the differential harness
    /// watches the live distribution).
    ///
    /// Residual, stated: a SHORT hex prefix that exists as an object but
    /// failed for another reason would read as absent. Callers never pass
    /// abbreviated oids -- every oid in the pipeline came from a git answer
    /// and is full-width.
    fn anchor_is_absent(&self, repo: &gix::Repository, spec: &str) -> Result<bool> {
        let end = spec
            .char_indices()
            .find(|(i, c)| matches!(c, '^' | '~' | ':') || (*c == '@' && spec[i + 1..].starts_with('{')))
            .map(|(i, _)| i)
            .unwrap_or(spec.len());
        let anchor = match &spec[..end] {
            "" | "@" => "HEAD",
            a => a,
        };
        // A ref by that (partial) name? `try_find_reference` implements the
        // refs/{,tags/,heads/,remotes/} search path, so "soda-core" and
        // "origin/main" both resolve the way rev-parse would.
        match repo.try_find_reference(anchor) {
            Ok(Some(_)) => return Ok(false), // exists; the failure was elsewhere
            Ok(None) => {}
            // Could not even look (unreadable packed-refs, invalid name...):
            // NOT absence. The caller will surface the original parse error.
            Err(_) => return Ok(false),
        }
        if anchor.len() == 40 && anchor.bytes().all(|b| b.is_ascii_hexdigit()) {
            let id = match gix::ObjectId::from_hex(anchor.as_bytes()) {
                Ok(id) => id,
                Err(_) => return Ok(true),
            };
            return match repo.try_find_object(id) {
                Ok(Some(_)) => Ok(false),
                Ok(None) => Ok(true),
                Err(e) => Err(self.gerr("rev-parse", e)),
            };
        }
        Ok(true)
    }

    /// Peel `id` to `kind`. `Ok(None)` for the two structural "the spec
    /// verifies to nothing" cases -- the object is absent, or it exists but
    /// cannot peel to that kind -- which is exactly what
    /// `rev-parse --verify --quiet '<rev>^{<kind>}'` reports as exit 1. Any
    /// other failure (odb IO, corrupt object) stays `Err`.
    fn peel_object<'r>(
        &self,
        repo: &'r gix::Repository,
        id: gix::ObjectId,
        kind: gix::object::Kind,
        op: &'static str,
    ) -> Result<Option<gix::Object<'r>>> {
        use gix::object::find::existing::Error as FindExisting;
        use gix::object::peel::to_kind::Error as Peel;
        let obj = match repo.try_find_object(id).map_err(|e| self.gerr(op, e))? {
            Some(o) => o,
            None => return Ok(None),
        };
        match obj.peel_to_kind(kind) {
            Ok(o) => Ok(Some(o)),
            // Kind mismatch: `<tag-of-a-blob>^{commit}` etc.
            Err(Peel::NotFound { .. }) => Ok(None),
            // A link in the peel chain names an object the odb does not have
            // (the gitlink case: `HEAD:wine^{commit}` in the superproject).
            Err(Peel::FindExistingObject(FindExisting::NotFound { .. })) => Ok(None),
            Err(e) => Err(self.gerr(op, e)),
        }
    }

    /// Resolve to a COMMIT id, `Ok(None)` iff the spec names nothing (the
    /// `rev_parse` question, kept as `ObjectId` for internal use).
    fn commit_id(&self, repo: &gix::Repository, rev: &str) -> Result<Option<gix::ObjectId>> {
        let Some(id) = self.resolve_raw(repo, rev)? else { return Ok(None) };
        Ok(self
            .peel_object(repo, id, gix::object::Kind::Commit, "rev-parse")?
            .map(|o| o.id))
    }

    /// A rev that MUST resolve to a commit -- the `Err`-on-absence discipline
    /// of `run_bool`/`run` (git exit 128), for methods where a bad ref must
    /// never read as a benign answer.
    fn required_commit(
        &self,
        repo: &gix::Repository,
        rev: &str,
        op: &'static str,
    ) -> Result<gix::ObjectId> {
        self.commit_id(repo, rev)?.ok_or_else(|| {
            self.err(op, format!("bad revision '{rev}': unknown revision or not a commit"))
        })
    }

    /// `("A", "B")` for `A..B`, treating an empty side as HEAD like git does.
    /// `None` for a spec without `..`. Three-dot specs are refused LOUDLY: no
    /// caller writes them (grepped -- every range is `format!("{a}..{b}")`),
    /// and silently mis-reading `A...B` as `A..B` would miscount.
    fn split_range<'s>(&self, range: &'s str, op: &'static str) -> Result<Option<(&'s str, &'s str)>> {
        if range.contains("...") {
            return Err(self.err(op, format!("symmetric-difference range {range:?} is not supported by the gix backend")));
        }
        Ok(range.split_once("..").map(|(l, r)| {
            (if l.is_empty() { "HEAD" } else { l }, if r.is_empty() { "HEAD" } else { r })
        }))
    }

    /// Commits reachable from `tip` and not from `hide`, oldest first when
    /// `oldest_first` (== `git log --reverse`).
    ///
    /// Sorting is `ByCommitTime(NewestFirst)` -- the same priority-queue
    /// order git's revision walk uses. Identical on linear ranges, which is
    /// the dominant case here since tiers are *enforced* linear
    /// (`TIER_NOT_LINEAR`); on merge-bearing ranges git's generation lists
    /// and gix's queue may interleave differently, which the live harness
    /// adjudicates -- do not "fix" that by sorting output, which would change
    /// golden bytes.
    fn walk<'r>(
        &self,
        repo: &'r gix::Repository,
        tip: gix::ObjectId,
        hide: Option<gix::ObjectId>,
        op: &'static str,
    ) -> Result<Vec<gix::revision::walk::Info<'r>>> {
        // Borrow gymnastics: Info borrows the repo, so collect eagerly.
        let platform = repo
            .rev_walk(Some(tip))
            .sorting(gix::revision::walk::Sorting::ByCommitTime(
                gix::traverse::commit::simple::CommitTimeOrder::NewestFirst,
            ))
            .with_hidden(hide);
        let mut out = Vec::new();
        for item in platform.all().map_err(|e| self.gerr(op, e))? {
            out.push(item.map_err(|e| self.gerr(op, e))?);
        }
        Ok(out)
    }

    /// Short (git-style) UTF-8 name of a ref, decoded strictly.
    fn short_name(&self, name: &gix::refs::FullNameRef, op: &'static str) -> Result<String> {
        self.decode(op, name.shorten())
    }

    /// All tags as `(short_name, peeled_target)`, the shared engine of
    /// `tags_with_targets` and `tags_pointing_at`, so the two cannot drift.
    fn tags_peeled(&self, repo: &gix::Repository) -> Result<Vec<(String, gix::ObjectId)>> {
        const OP: &str = "for-each-ref refs/tags";
        let platform = repo.references().map_err(|e| self.gerr(OP, e))?;
        let iter = platform.tags().map_err(|e| self.gerr(OP, e))?;
        let iter = iter.peeled().map_err(|e| self.gerr(OP, e))?;
        let mut v = Vec::new();
        for r in iter {
            // A tag that fails to read or peel is an ERROR, not a silent
            // non-match: a swallowed peel biases toward "untagged", the
            // direction that produces a false Blocker.
            let r = r.map_err(|e| self.err(OP, chain(e.as_ref())))?;
            let name = self.short_name(r.name(), OP)?;
            let id = r
                .target()
                .try_id()
                .ok_or_else(|| self.err(OP, format!("tag `{name}` did not peel to an object")))?
                .to_owned();
            v.push((name, id));
        }
        // gix's loose+packed overlay already iterates full-refname bytewise
        // ascending, which is for-each-ref's default sort -- aligned by
        // construction. The explicit sort is a no-op TODAY (so it cannot
        // change golden bytes) and kills the whole disagreement class if
        // gix's iteration order ever shifts. Short-name order == full-name
        // order because the `refs/tags/` prefix is constant.
        v.sort_unstable();
        Ok(v)
    }

    /// The id at the end of a ref's symref chain, WITHOUT renaming the ref --
    /// the caller keeps `r.name()` from before the walk. `Ok(None)` for the
    /// two structural "names no commit" cases git also skips: an unborn
    /// symref (no target at all) and a dangling one (its target ref does not
    /// exist -- the typed `existing::Error::NotFound`). A cycle or an
    /// over-deep chain is `Err`: git itself refuses symref chains past depth
    /// 5, and a corrupt ref store must be loud, not an empty answer.
    fn follow_to_id(
        &self,
        r: gix::Reference<'_>,
        op: &'static str,
    ) -> Result<Option<gix::ObjectId>> {
        use gix::refs::file::find::existing::Error as FindExisting;
        let mut cur = r;
        for _ in 0..=5 {
            if let Some(id) = cur.target().try_id() {
                return Ok(Some(id.to_owned()));
            }
            match cur.follow() {
                Some(Ok(next)) => cur = next,
                Some(Err(FindExisting::NotFound { .. })) => return Ok(None),
                Some(Err(e)) => return Err(self.gerr(op, e)),
                None => return Ok(None), // symbolic but unborn: nothing to follow
            }
        }
        Err(self.err(op, format!("symref chain from `{}` exceeds depth 5 (cycle?)", cur.name().as_bstr())))
    }
}

impl GitRead for GitGix {
    fn rev_parse(&self, rev: &str) -> Result<Option<Oid>> {
        let repo = self.open()?;
        Ok(self.commit_id(&repo, rev)?.map(|id| id.to_string()))
    }

    fn rev_parse_raw(&self, rev: &str) -> Result<Option<Oid>> {
        let repo = self.open()?;
        Ok(self.resolve_raw(&repo, rev)?.map(|id| id.to_string()))
    }

    fn git_dir(&self) -> Result<PathBuf> {
        // gix has already resolved the submodule `.git`-FILE indirection at
        // open time; canonicalize to match `--absolute-git-dir`, which the
        // conformance test pins against a symlinked $TMPDIR.
        let repo = self.open()?;
        repo.git_dir()
            .canonicalize()
            .map_err(|e| self.err("git-dir", format!("canonicalize {}: {e}", repo.git_dir().display())))
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        // Resolve both endpoints FIRST: either missing is `Err`, matching the
        // binary's 128 discipline -- a bad ref must never read as `false`,
        // which raises TIER_CHILD_STALE whose remedy rewrites an authored
        // tier. Then `merge_base`, whose no-common-ancestor variant is the
        // structural `Ok(false)`. This is `run_bool`'s 0/1/128 split, rebuilt
        // from typed parts.
        const OP: &str = "merge-base --is-ancestor";
        let repo = self.open()?;
        let a = self.required_commit(&repo, ancestor, OP)?;
        let d = self.required_commit(&repo, descendant, OP)?;
        match repo.merge_base(a, d) {
            Ok(base) => Ok(base.detach() == a),
            Err(gix::repository::merge_base::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(self.gerr(OP, e)),
        }
    }

    /// Binary-only, permanently -- see `GitHybrid` and the module header.
    /// gitoxide has no patch-id (zero hits across all 48 gix crates); the
    /// ~130 LOC reconstruction was tried and produced a SILENT FALSE CLEAN
    /// (it merged "unlink frame elements" with "unlink iframe elements" and
    /// suppressed a real STITCH_STALE). Never reimplement this on gix.
    fn missing_by_patch_id(&self, _: &str, _: &str, _: &str) -> Result<Vec<String>> {
        Err(GitError::Unsupported { method: "missing_by_patch_id" })
    }

    fn reapply_verdict(&self, commit: &str, onto: &str) -> Result<Reapply> {
        // Same shape as the binary's merge-tree dance, from typed parts:
        // base = the commit's parent tree, ours = the target tree, theirs =
        // the commit's tree; base->theirs is exactly the change, and if the
        // target already carries it the merged tree equals the target's.
        //
        // `with_object_memory()` is the whole scratch-odb apparatus in one
        // call: every object the merge writes lands in memory and is
        // discarded with `repo`, and the backend doc measured ZERO loose
        // objects against the binary's 61->67. There is nothing to clean up
        // and no redirect that could break.
        const OP: &str = "merge-trees";
        let repo = self.open()?.with_object_memory();

        // An unresolvable TARGET is `Unknown` (mirrors `tree_oid -> None` in
        // the binary impl); an unresolvable COMMIT is `Err` (the binary's
        // merge-tree exits 128). The asymmetry is inherited deliberately.
        let onto_tree = match self.resolve_raw(&repo, onto)? {
            Some(id) => match self.peel_object(&repo, id, gix::object::Kind::Tree, OP)? {
                Some(t) => t.id,
                None => return Ok(Reapply::Unknown),
            },
            None => return Ok(Reapply::Unknown),
        };
        let commit_id = self.required_commit(&repo, commit, OP)?;
        let commit_obj = repo
            .find_object(commit_id)
            .map_err(|e| self.gerr(OP, e))?
            .into_commit();
        let their_tree = commit_obj.tree_id().map_err(|e| self.gerr(OP, e))?.detach();
        // `<commit>^`: a root commit has no parent and the binary's
        // `--merge-base=<commit>^` fails with 128 -- an error, not a verdict.
        let parent_id = commit_obj
            .parent_ids()
            .next()
            .ok_or_else(|| self.err(OP, format!("'{commit}^': root commit has no parent")))?;
        let base_tree = parent_id
            .object()
            .map_err(|e| self.gerr(OP, e))?
            .into_commit()
            .tree_id()
            .map_err(|e| self.gerr(OP, e))?
            .detach();

        let options = repo.tree_merge_options().map_err(|e| self.gerr(OP, e))?;
        let mut outcome = repo
            .merge_trees(base_tree, onto_tree, their_tree, Default::default(), options)
            .map_err(|e| self.gerr(OP, e))?;
        // Three-valued and it must stay that way: a conflict is NOT evidence
        // of absence -- `Unknown` must NEVER collapse into `Missing`, the one
        // verdict reconcile renders as "(confirmed by reapply)".
        // `TreatAsUnresolved::git()` is what makes this the same question as
        // the binary's exit 1.
        if outcome.has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::git()) {
            return Ok(Reapply::Unknown);
        }
        let merged = outcome.tree.write().map_err(|e| self.err(OP, chain(&e)))?.detach();
        Ok(if merged == onto_tree { Reapply::Present } else { Reapply::Missing })
    }

    fn ahead_behind(&self, base: &str, head: &str) -> Result<Option<AheadBehind>> {
        // Either endpoint missing is the one absorbable non-answer ("never
        // pushed"), same as the binary's `any_rev_missing` path -- and it is
        // typed here rather than classified from a failure.
        const OP: &str = "rev-list --left-right --count";
        let repo = self.open()?;
        let (Some(b), Some(h)) = (self.commit_id(&repo, base)?, self.commit_id(&repo, head)?) else {
            return Ok(None);
        };
        let ahead = self.walk(&repo, h, Some(b), OP)?.len();
        let behind = self.walk(&repo, b, Some(h), OP)?.len();
        let clamp = |n: usize| -> Result<u32> {
            u32::try_from(n).map_err(|_| self.err(OP, format!("count {n} exceeds u32")))
        };
        Ok(Some(AheadBehind { ahead: clamp(ahead)?, behind: clamp(behind)? }))
    }

    fn count_commits(&self, range: &str) -> Result<u32> {
        const OP: &str = "rev-list --count";
        let repo = self.open()?;
        let n = match self.split_range(range, OP)? {
            Some((l, r)) => {
                let l = self.required_commit(&repo, l, OP)?;
                let r = self.required_commit(&repo, r, OP)?;
                self.walk(&repo, r, Some(l), OP)?.len()
            }
            None => {
                let tip = self.required_commit(&repo, range, OP)?;
                self.walk(&repo, tip, None, OP)?.len()
            }
        };
        u32::try_from(n).map_err(|_| self.err(OP, format!("count {n} exceeds u32")))
    }

    fn local_branches(&self) -> Result<Vec<String>> {
        const OP: &str = "for-each-ref refs/heads";
        let repo = self.open()?;
        let platform = repo.references().map_err(|e| self.gerr(OP, e))?;
        let mut v = Vec::new();
        for r in platform.local_branches().map_err(|e| self.gerr(OP, e))? {
            let r = r.map_err(|e| self.err(OP, chain(e.as_ref())))?;
            v.push(self.short_name(r.name(), OP)?);
        }
        v.sort_unstable(); // no-op today; see `tags_peeled` for why it stays
        Ok(v)
    }

    fn tags(&self) -> Result<Vec<String>> {
        const OP: &str = "tag -l";
        let repo = self.open()?;
        let platform = repo.references().map_err(|e| self.gerr(OP, e))?;
        let mut v = Vec::new();
        for r in platform.tags().map_err(|e| self.gerr(OP, e))? {
            let r = r.map_err(|e| self.err(OP, chain(e.as_ref())))?;
            v.push(self.short_name(r.name(), OP)?);
        }
        v.sort_unstable();
        Ok(v)
    }

    fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
        let repo = self.open()?;
        Ok(self
            .tags_peeled(&repo)?
            .into_iter()
            .map(|(n, id)| (n, id.to_string()))
            .collect())
    }

    fn tags_pointing_at(&self, oid: &str) -> Result<Vec<String>> {
        // A missing oid is an ERROR on BOTH backends since 2026-07-19
        // (Florian: a clear error beats a silent breakdown). This side always
        // was -- the one oid in the pipeline not guaranteed to exist in the
        // queried repo is the gitlink pin (`pin.rs`), and answering `Ok([])`
        // for an unreadable pin biases toward a false PIN_UNTAGGED, exactly
        // the "empty reads as untagged" direction this module refuses
        // everywhere else. `GitCli` was the deviant (raw `tag --points-at`
        // exits 0-empty for a missing full-hex) and now pre-checks with
        // `cat-file -e`. Agreement pinned in `tests/differential.rs`
        // (`tags_pointing_at_a_missing_oid_errs_on_both_backends`).
        const OP: &str = "tag --points-at";
        let repo = self.open()?;
        let target = self.required_commit(&repo, oid, OP)?;
        Ok(self
            .tags_peeled(&repo)?
            .into_iter()
            .filter(|(_, id)| *id == target)
            .map(|(n, _)| n)
            .collect())
    }

    fn branches_containing(&self, oid: &str, remote: bool) -> Result<Vec<String>> {
        const OP: &str = "for-each-ref --contains";
        let repo = self.open()?;
        let needle = self.required_commit(&repo, oid, OP)?;
        let platform = repo.references().map_err(|e| self.gerr(OP, e))?;
        let iter = if remote { platform.remote_branches() } else { platform.local_branches() }
            .map_err(|e| self.gerr(OP, e))?;
        // NOT `.peeled()`. The peeled iterator resolves a symref -- e.g.
        // `refs/remotes/<r>/HEAD` -> `refs/remotes/<r>/rust` -- and then
        // yields it under the TARGET's name, so the output listed the target
        // twice and the symref's own short name (`codeberg`, which is how
        // `for-each-ref --contains` lists it) not at all. Found live
        // 2026-07-19: reference `["codeberg", "codeberg/rust"]` vs candidate
        // `["codeberg/rust", "codeberg/rust"]`. Each ref keeps ITS OWN name
        // and the tip id is found by following the chain separately below.
        //
        // ONE `Graph` shared across the whole per-ref loop, which is exactly
        // what `merge_base_with_graph` exists for: the naive
        // `repo.merge_base()` builds a fresh graph per call, and per-call cost
        // on this repo's 49 remote branches measured 381 ms against the
        // binary's 2.8 -- a 0.9 s production run instead of 0.2 s. With the
        // shared graph the parsed-commit work is paid once per invocation.
        let cache = repo.commit_graph_if_enabled().map_err(|e| self.gerr(OP, e))?;
        let mut graph = repo.revision_graph(cache.as_ref());
        let mut v = Vec::new();
        for r in iter {
            let r = r.map_err(|e| self.err(OP, chain(e.as_ref())))?;
            // git shortens `refs/remotes/<r>/HEAD` to just `<r>` -- it is one
            // of rev-parse's standard lookup patterns -- and that is the name
            // `for-each-ref --contains` lists a remote's HEAD under. gix's
            // `shorten()` only strips the category prefix and would say
            // `<r>/HEAD`, a name git never prints.
            let full = r.name().as_bstr();
            let name = match full
                .strip_prefix(b"refs/remotes/".as_slice())
                .and_then(|s| s.strip_suffix(b"/HEAD".as_slice()))
            {
                Some(remote) => self.decode(OP, remote)?,
                None => self.short_name(r.name(), OP)?,
            };
            let Some(tip) = self.follow_to_id(r, OP)? else {
                continue; // an unborn/dangling symref names no commit and contains nothing
            };
            // "contains" == the needle is an ancestor of the tip.
            let contains = match repo.merge_base_with_graph(needle, tip, &mut graph) {
                Ok(base) => base.detach() == needle,
                Err(gix::repository::merge_base_with_graph::Error::NotFound { .. }) => false,
                Err(e) => return Err(self.gerr(OP, e)),
            };
            if contains {
                v.push(name);
            }
        }
        v.sort_unstable();
        Ok(v)
    }

    fn log_subjects(&self, range: &str) -> Result<Vec<(Oid, String)>> {
        // `git log --reverse` == walk newest-first, collect, reverse.
        const OP: &str = "log --reverse";
        let repo = self.open()?;
        let (hide, tip) = match self.split_range(range, OP)? {
            Some((l, r)) => {
                (Some(self.required_commit(&repo, l, OP)?), self.required_commit(&repo, r, OP)?)
            }
            None => (None, self.required_commit(&repo, range, OP)?),
        };
        let mut v = Vec::new();
        for info in self.walk(&repo, tip, hide, OP)? {
            let commit = info.object().map_err(|e| self.gerr(OP, e))?;
            // `summary()` folds a multi-line title to one line, which is what
            // `%s` does -- the binary's parser has the same note.
            let subject = self.decode(OP, &commit.message().map_err(|e| self.gerr(OP, e))?.summary())?;
            v.push((info.id.to_string(), subject));
        }
        v.reverse();
        Ok(v)
    }

    fn merges_in(&self, range: &str) -> Result<Vec<Oid>> {
        // `git log --merges`: newest-first, NO --reverse -- note the
        // asymmetry with `log_subjects`, inherited from the binary impl.
        const OP: &str = "log --merges";
        let repo = self.open()?;
        let (hide, tip) = match self.split_range(range, OP)? {
            Some((l, r)) => {
                (Some(self.required_commit(&repo, l, OP)?), self.required_commit(&repo, r, OP)?)
            }
            None => (None, self.required_commit(&repo, range, OP)?),
        };
        Ok(self
            .walk(&repo, tip, hide, OP)?
            .into_iter()
            .filter(|info| info.parent_ids.len() > 1)
            .map(|info| info.id.to_string())
            .collect())
    }

    /// Binary-only -- see `GitHybrid` and the module header. gix's status
    /// DIVERGES on this repo today (10 items vs git's 8): its dirwalk emits
    /// `.brv` (7 empty directories, 0 files -- git has no concept of an empty
    /// directory under ANY `--untracked-files` setting), and the `wine`
    /// gitlink double-counts as TreeIndex + IndexWorktree where git emits one
    /// record. `checks/rr.rs` reports `dirty.len()`, so a port would inflate
    /// it. Decided 2026-07-19; revisit only with a measured gix status that
    /// matches git here.
    fn status(&self, _: bool) -> Result<Vec<StatusEntry>> {
        Err(GitError::Unsupported { method: "status" })
    }

    fn submodule_index_oid(&self, path: &str) -> Result<Option<Oid>> {
        // The same refusals as the binary impl, now typed: require exactly one
        // index record for the pathspec, at stage 0, mode 160000. A blob's oid
        // returned as a submodule pointer would feed a Blocker; a conflicted
        // gitlink silently reporting stage 1 is the PRE-merge value.
        const OP: &str = "ls-files";
        use gix::bstr::{BStr, ByteSlice};
        // The binary's pathspec matches `wine/` to the `wine` gitlink (a
        // pathspec with a trailing slash means "the directory"), but the
        // index stores the entry WITHOUT the slash, so the exact/prefix
        // lookups below would both miss and report a false "not in index" --
        // the quiet direction, found 2026-07-19. Trim before looking. No
        // caller adds slashes today (paths come verbatim from the manifest);
        // this exists so the first one that does cannot mis-read the index.
        let path = path.trim_end_matches('/');
        let repo = self.open()?;
        let index = repo.index_or_empty().map_err(|e| self.gerr(OP, e))?;
        // The binary's `-- <path>` pathspec matches the path itself AND
        // everything under `path/` -- both must be counted or a plain
        // directory stops erroring and the guard rots.
        let exact = index
            .entry_range(BStr::new(path))
            .map(|r| index.entries()[r].to_vec())
            .unwrap_or_default();
        let prefix = format!("{path}/");
        let below = index
            .prefixed_entries_range(BStr::new(&prefix))
            .map(|r| index.entries()[r].to_vec())
            .unwrap_or_default();
        match (exact.as_slice(), below.as_slice()) {
            ([], []) => Ok(None),
            ([one], []) => {
                let stage = one.stage() as u8;
                if one.mode != gix::index::entry::Mode::COMMIT {
                    Err(self.err(
                        OP,
                        format!("`{path}` is mode {:06o}, not a gitlink (160000)", one.mode.bits()),
                    ))
                } else if stage != 0 {
                    Err(self.err(OP, format!("gitlink is at merge stage {stage}")))
                } else {
                    Ok(Some(one.id.to_string()))
                }
            }
            ([], [one]) => {
                // One file below the path: the binary reports its mode, not a
                // gitlink -- e.g. `ls-files -- tools` yielding a blob.
                let p = index.path_backing();
                let name = one.path_in(p).to_str_lossy();
                Err(self.err(OP, format!("`{name}` is mode {:06o}, not a gitlink (160000)", one.mode.bits())))
            }
            (e, b) => Err(self.err(
                OP,
                format!("`{path}` matched {} index entries; not a single gitlink", e.len() + b.len()),
            )),
        }
    }

    fn head_branch(&self) -> Result<Option<String>> {
        const OP: &str = "symbolic-ref HEAD";
        let repo = self.open()?;
        match repo.head_name().map_err(|e| self.gerr(OP, e))? {
            Some(name) => Ok(Some(self.short_name(name.as_ref(), OP)?)),
            None => Ok(None), // detached -- native, no classification needed
        }
    }

    fn config_file_get(&self, file: &str, key: &str) -> Result<Option<String>> {
        const OP: &str = "config -f";
        // The out-of-band existence pre-check is kept VERBATIM even though
        // gix could distinguish file-missing structurally: the trait contract
        // (missing file = Err, absent key = Ok(None)) must not fork per
        // backend.
        let p = self.repo.join(file);
        if !p.exists() {
            return Err(self.err(OP, format!("{} does not exist", p.display())));
        }
        // The include-RESOLVING constructor, on purpose: `git config -f`
        // follows `include`/`includeIf`, and `from_path_no_includes` would
        // silently fork the contract. `.gitmodules` has no includes today, so
        // no harness run would catch the fork -- it is decided by spec here.
        let meta = gix::config::file::Metadata::try_from_path(&p, gix::config::Source::Local)
            .map_err(|e| self.err(OP, format!("{}: {e}", p.display())))?;
        let opts = gix::config::file::init::Options {
            includes: gix::config::file::includes::Options::follow(
                Default::default(),
                Default::default(),
            ),
            ..Default::default()
        };
        let f = gix::config::File::from_paths_metadata(Some(meta), opts)
            .map_err(|e| self.err(OP, chain(&e)))?
            .ok_or_else(|| self.err(OP, format!("{} vanished mid-read", p.display())))?;
        // `string()` is git's lookup: last value wins, quotes normalized --
        // which preserves a quoted value's own whitespace, the `-z` property
        // the binary impl documents.
        match f.string(key) {
            Some(v) => Ok(Some(self.decode(OP, v.as_ref())?)),
            None => Ok(None),
        }
    }

    fn superproject_pin(&self) -> Result<Pin> {
        // Discover the superproject from the submodule, like the binary's
        // `--show-superproject-working-tree`: the repo in the parent
        // directory whose INDEX records a gitlink at our relative path. No
        // repo above, or no gitlink, is "I cannot tell you" -- Unknown, never
        // a clean pass.
        let repo = self.open()?;
        // A bare repo has no workdir and therefore no superproject. Same
        // REASON STRING as the binary's empty-`--show-superproject-working-tree`
        // arm on purpose: `Pin::Unknown` compares `reason` under `PartialEq`,
        // so a prose-only fork here would read as a backend disagreement in
        // the differential harness (and the binary really does print empty in
        // a bare repo -- verified on git 2.55, see the GitCli impl).
        let Some(workdir) = repo.workdir().map(Path::to_path_buf) else {
            return Ok(Pin::Unknown {
                reason: format!("{} is not a submodule of any superproject", self.repo.display()),
            });
        };
        let (Ok(cself), Some(parent)) = (workdir.canonicalize(), workdir.parent().map(Path::to_path_buf))
        else {
            return Ok(Pin::Unknown {
                reason: format!("could not canonicalize {}", workdir.display()),
            });
        };
        let sup_workdir = match gix::discover(&parent) {
            Ok(sup) => match sup.workdir() {
                Some(d) => d.to_path_buf(),
                None => {
                    return Ok(Pin::Unknown {
                        reason: format!("{} is not a submodule of any superproject", self.repo.display()),
                    })
                }
            },
            Err(_) => {
                return Ok(Pin::Unknown {
                    reason: format!("{} is not a submodule of any superproject", self.repo.display()),
                })
            }
        };
        let Ok(croot) = sup_workdir.canonicalize() else {
            return Ok(Pin::Unknown {
                reason: format!("could not canonicalize {}", sup_workdir.display()),
            });
        };
        let Ok(rel) = cself.strip_prefix(&croot) else {
            return Ok(Pin::Unknown {
                reason: format!("{} is not under its superproject {}", cself.display(), croot.display()),
            });
        };
        let Some(rel) = rel.to_str() else {
            return Ok(Pin::Unknown { reason: "submodule path is not valid UTF-8".into() });
        };

        let sup = GitGix::new(&croot);
        // The gitlink in the superproject's index is what makes this a
        // submodule at all -- git's own discovery checks the same fact.
        let staged = sup.submodule_index_oid(rel)?;
        if staged.is_none() {
            return Ok(Pin::Unknown {
                reason: format!("{} is not a submodule of any superproject", self.repo.display()),
            });
        }
        let committed = sup.rev_parse_raw(&format!("HEAD:{rel}"))?;
        Ok(Pin::Known { committed, staged })
    }
}

/// The production backend: one `GitCli` and one `GitGix` for the same path,
/// dispatched per method along the capability seam. ~25 lines of pure
/// delegation, and that is all it must ever be -- convenience calls crossing
/// the seam here is how it erodes.
pub struct GitHybrid {
    cli: GitCli,
    gix: GitGix,
}

impl GitHybrid {
    pub fn new(repo: impl AsRef<Path>) -> Self {
        let repo = repo.as_ref();
        Self { cli: GitCli::new(repo), gix: GitGix::new(repo) }
    }
}

impl GitRead for GitHybrid {
    // gix owns every method it answers correctly...
    fn rev_parse(&self, rev: &str) -> Result<Option<Oid>> {
        self.gix.rev_parse(rev)
    }
    fn rev_parse_raw(&self, rev: &str) -> Result<Option<Oid>> {
        self.gix.rev_parse_raw(rev)
    }
    fn git_dir(&self) -> Result<PathBuf> {
        self.gix.git_dir()
    }
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        self.gix.is_ancestor(ancestor, descendant)
    }
    fn reapply_verdict(&self, commit: &str, onto: &str) -> Result<Reapply> {
        self.gix.reapply_verdict(commit, onto)
    }
    fn ahead_behind(&self, base: &str, head: &str) -> Result<Option<AheadBehind>> {
        self.gix.ahead_behind(base, head)
    }
    fn count_commits(&self, range: &str) -> Result<u32> {
        self.gix.count_commits(range)
    }
    fn local_branches(&self) -> Result<Vec<String>> {
        self.gix.local_branches()
    }
    fn tags(&self) -> Result<Vec<String>> {
        self.gix.tags()
    }
    fn tags_with_targets(&self) -> Result<Vec<(String, Oid)>> {
        self.gix.tags_with_targets()
    }
    fn tags_pointing_at(&self, oid: &str) -> Result<Vec<String>> {
        self.gix.tags_pointing_at(oid)
    }
    fn branches_containing(&self, oid: &str, remote: bool) -> Result<Vec<String>> {
        self.gix.branches_containing(oid, remote)
    }
    fn log_subjects(&self, range: &str) -> Result<Vec<(Oid, String)>> {
        self.gix.log_subjects(range)
    }
    fn merges_in(&self, range: &str) -> Result<Vec<Oid>> {
        self.gix.merges_in(range)
    }
    fn submodule_index_oid(&self, path: &str) -> Result<Option<Oid>> {
        self.gix.submodule_index_oid(path)
    }
    fn head_branch(&self) -> Result<Option<String>> {
        self.gix.head_branch()
    }
    fn config_file_get(&self, file: &str, key: &str) -> Result<Option<String>> {
        self.gix.config_file_get(file, key)
    }
    fn superproject_pin(&self) -> Result<Pin> {
        self.gix.superproject_pin()
    }

    /// ...and the binary keeps exactly two. `status`: gix's dirwalk emits
    /// empty directories git cannot represent and double-counts the `wine`
    /// gitlink -- see `GitGix::status` for the measurements.
    fn status(&self, include_untracked: bool) -> Result<Vec<StatusEntry>> {
        self.cli.status(include_untracked)
    }

    /// `missing_by_patch_id`: gitoxide has no patch-id and the hand-rolled
    /// reconstruction produced a silent false clean -- see
    /// `GitGix::missing_by_patch_id`. Never move this off the binary.
    fn missing_by_patch_id(&self, upstream: &str, head: &str, limit: &str) -> Result<Vec<String>> {
        self.cli.missing_by_patch_id(upstream, head, limit)
    }
}
