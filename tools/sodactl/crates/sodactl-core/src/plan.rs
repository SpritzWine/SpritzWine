//! The plan/apply split: every mutating verb PLANS as a pure function, and one
//! shared engine APPLIES.
//!
//! The contract, from the design doc's safety guardrails:
//!
//! * A `Plan` is reviewable: literal argv per step, predicted ref mutations
//!   with the live oids they were derived from, the backup refs that will be
//!   created. The default CLI mode prints it and exits.
//! * `--apply` re-derives the plan from live state and refuses if `Plan::id`
//!   differs from what was shown -- that closes the TOCTOU window between
//!   review and execution.
//! * `apply` pre-verifies every `expected_before` against the live refs,
//!   writes the journal BEFORE the first mutation, creates the backup refs
//!   create-only, then runs the steps in order and stops on the first failure
//!   with an error naming the journal, the backups and the exact
//!   continue/abort commands.
//!
//! Error taxonomy, kept greppable on purpose: everything that means "nothing
//! was changed, fix the precondition" renders with the prefix `refused: ` and
//! is a typed value (`Refusal`, `ApplyError::RefusedExpectedMoved`) the CLI
//! maps to exit 1 (EXIT_GATE). Everything else is either a mid-apply stop
//! (state to inspect, commands printed) or "the tool could not answer".

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::checks::Workspace;
use crate::manifest::Manifest;
use crate::treestate;
use sodactl_git::{GitError, GitMut, GitRead, ZERO_OID};

/// Which of the two repositories a step or mutation targets. The wine
/// submodule is where history is rewritten; the superproject only ever GAINS
/// commits (`pin`, rr-cache capture) -- `apply` leans on that asymmetry when
/// it creates backup refs (wine side only, see `apply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoSide {
    Wine,
    Superproject,
}

/// What a ref is planned to become. `Unknown` is a real answer here exactly as
/// it is on the read side: a rebase's resulting tip is unknowable before the
/// rebase runs, and predicting a value would be inventing one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum PlannedValue {
    Oid { oid: String },
    Unknown { reason: String },
}

/// One ref a plan intends to move, with the live oid the plan was derived
/// from. `expected_before = None` means the ref must NOT exist (create-only:
/// backup refs, new tags). `apply` verifies every one of these against the
/// live repo immediately before running anything.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefMutation {
    pub repo: RepoSide,
    pub refname: String,
    pub expected_before: Option<String>,
    pub after: PlannedValue,
}

/// The typed, executable half of a step -- exactly the `GitMut` methods that
/// make sense as PLANNED work. The reactive half of `GitMut`
/// (`rebase_continue`/`rebase_abort`/`cherry_pick_abort`/`resolve_take`/
/// `rm_cached_conflict`) is deliberately absent: a plan states what SHOULD
/// happen; conflict recovery is decided at conflict time by the verb, against
/// live state, and must not be pre-committed in a reviewed plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Action {
    UpdateRefCas { refname: String, new: String, expected_old: Option<String> },
    Fetch { remote: String, refspecs: Vec<String> },
    RebaseOnto { newbase: String, upstream: String, branch: String },
    CherryPick { range: String },
    CheckoutBranchReset { branch: String, start: String },
    CheckoutDetach { rev: String },
    Add { paths: Vec<String> },
    Commit { message: String },
    /// `commit -m <msg> -- <paths>`: scoped to the named paths so a stranger's
    /// staged change cannot be swept in (regen's rr-cache capture).
    CommitPaths { message: String, paths: Vec<String> },
    TagAnnotated { name: String, target: String, message: String, force: bool },
    ConfigFileSet { file: String, key: String, value: String },
    /// Repo-local `git config <key> <value>` (regen's rerere enablement).
    ConfigSet { key: String, value: String },
    RerereGc,
}

impl Action {
    /// The literal argv the matching `GitMut` method passes to git, for
    /// display. Kept beside the dispatch below so the two are one screen
    /// apart; `ZERO_OID` is imported from `mutate.rs` so the create-only
    /// sentinel the user reviews is the one git receives.
    pub fn argv(&self) -> Vec<String> {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        match self {
            Action::UpdateRefCas { refname, new, expected_old } => {
                s(&["update-ref", refname, new, expected_old.as_deref().unwrap_or(ZERO_OID)])
            }
            Action::Fetch { remote, refspecs } => {
                let mut a = s(&["fetch", remote]);
                a.extend(refspecs.iter().cloned());
                a
            }
            Action::RebaseOnto { newbase, upstream, branch } => {
                s(&["rebase", "--onto", newbase, upstream, branch])
            }
            Action::CherryPick { range } => s(&["cherry-pick", range]),
            Action::CheckoutBranchReset { branch, start } => s(&["checkout", "-B", branch, start]),
            Action::CheckoutDetach { rev } => s(&["checkout", "--detach", rev]),
            Action::Add { paths } => {
                let mut a = s(&["add", "--"]);
                a.extend(paths.iter().cloned());
                a
            }
            Action::Commit { message } => s(&["commit", "-m", message]),
            Action::CommitPaths { message, paths } => {
                let mut a = s(&["commit", "-m", message, "--"]);
                a.extend(paths.iter().cloned());
                a
            }
            Action::TagAnnotated { name, target, message, force } => {
                let mut a = s(&["tag", "-a"]);
                if *force {
                    a.push("-f".into());
                }
                a.extend(s(&["-m", message, name, target]));
                a
            }
            Action::ConfigFileSet { file, key, value } => s(&["config", "-f", file, key, value]),
            Action::ConfigSet { key, value } => s(&["config", key, value]),
            Action::RerereGc => s(&["rerere", "gc"]),
        }
    }

    /// Execution goes through `GitMut`'s typed methods, never by re-parsing
    /// the display argv -- the argv is a rendering of this dispatch, not its
    /// input.
    fn run(&self, git: &GitMut) -> sodactl_git::Result<()> {
        match self {
            Action::UpdateRefCas { refname, new, expected_old } => {
                git.update_ref_cas(refname, new, expected_old.as_deref())
            }
            Action::Fetch { remote, refspecs } => {
                let rs: Vec<&str> = refspecs.iter().map(String::as_str).collect();
                git.fetch(remote, &rs)
            }
            Action::RebaseOnto { newbase, upstream, branch } => {
                git.rebase_onto(newbase, upstream, branch)
            }
            Action::CherryPick { range } => git.cherry_pick(range),
            Action::CheckoutBranchReset { branch, start } => git.checkout_branch_reset(branch, start),
            Action::CheckoutDetach { rev } => git.checkout_detach(rev),
            Action::Add { paths } => {
                let ps: Vec<&str> = paths.iter().map(String::as_str).collect();
                git.add(&ps)
            }
            Action::Commit { message } => git.commit(message),
            Action::CommitPaths { message, paths } => {
                let ps: Vec<&str> = paths.iter().map(String::as_str).collect();
                git.commit_paths(message, &ps)
            }
            Action::TagAnnotated { name, target, message, force } => {
                git.tag_annotated(name, target, message, *force)
            }
            Action::ConfigFileSet { file, key, value } => git.config_file_set(file, key, value),
            Action::ConfigSet { key, value } => git.config_set(key, value),
            Action::RerereGc => git.rerere_gc(),
        }
    }
}

/// One planned command: what it does in words, the literal argv, where it
/// runs. `argv` is derived from `action` in the constructor so the display can
/// never diverge from what executes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Step {
    pub description: String,
    pub argv: Vec<String>,
    pub repo: RepoSide,
    pub action: Action,
}

impl Step {
    pub fn new(description: impl Into<String>, repo: RepoSide, action: Action) -> Self {
        Self { description: description.into(), argv: action.argv(), repo, action }
    }
}

/// A reviewed unit of work. Built via `Plan::new`, which computes `id`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub verb: String,
    /// A stable digest whose ONLY job is plan/apply drift refusal: `--apply`
    /// re-derives the plan from live state and compares ids, so any change in
    /// what would run (steps) or in the state it was derived from
    /// (`expected_before`) refuses instead of executing a plan nobody saw.
    /// FNV-1a, in-crate: the adversary is time, not an attacker, so a
    /// cryptographic hash would buy nothing and cost a dependency.
    pub id: String,
    pub steps: Vec<Step>,
    pub mutations: Vec<RefMutation>,
    /// `refname -> old tip` pairs `apply` creates create-only before any step.
    pub backups: Vec<(String, String)>,
    /// Free-text narration for the human rendering; never part of the id.
    pub notes: Vec<String>,
}

impl Plan {
    pub fn new(
        verb: impl Into<String>,
        steps: Vec<Step>,
        mutations: Vec<RefMutation>,
        backups: Vec<(String, String)>,
        notes: Vec<String>,
    ) -> Self {
        let verb = verb.into();
        let id = digest(&verb, &steps, &mutations);
        Self { verb, id, steps, mutations, backups, notes }
    }
}

// ---- the id digest ---------------------------------------------------------

/// FNV-1a over a canonical byte encoding of verb + steps (repo + argv) +
/// mutations (repo + refname + expected_before + after).
///
/// Deliberately EXCLUDED, with reasons:
/// * `backups` -- their refnames embed the UTC stamp of the run that creates
///   them, so including them would make every re-derivation drift by
///   construction, and their tips are already covered by `expected_before`.
/// * `notes` and step `description`s -- prose is presentation, not identity;
///   a rewording must not invalidate a reviewed plan whose commands are
///   unchanged. The argv IS the step's identity.
///
/// Every field is length-prefixed and every list count-prefixed, so adjacent
/// fields can never be confused by concatenation (`["ab","c"]` vs
/// `["a","bc"]`), and `Option`/enum arms carry a discriminant byte so `None`
/// differs from `Some("")`.
fn digest(verb: &str, steps: &[Step], mutations: &[RefMutation]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    fn bytes(h: &mut u64, b: &[u8]) {
        for x in b {
            *h ^= *x as u64;
            *h = h.wrapping_mul(0x100000001b3);
        }
    }
    fn field(h: &mut u64, s: &str) {
        bytes(h, &(s.len() as u64).to_le_bytes());
        bytes(h, s.as_bytes());
    }
    fn count(h: &mut u64, n: usize) {
        bytes(h, &(n as u64).to_le_bytes());
    }
    fn side(h: &mut u64, r: RepoSide) {
        bytes(h, &[match r { RepoSide::Wine => 0, RepoSide::Superproject => 1 }]);
    }

    field(&mut h, verb);
    count(&mut h, steps.len());
    for s in steps {
        side(&mut h, s.repo);
        count(&mut h, s.argv.len());
        for a in &s.argv {
            field(&mut h, a);
        }
    }
    count(&mut h, mutations.len());
    for m in mutations {
        side(&mut h, m.repo);
        field(&mut h, &m.refname);
        match &m.expected_before {
            None => bytes(&mut h, &[0]),
            Some(o) => {
                bytes(&mut h, &[1]);
                field(&mut h, o);
            }
        }
        match &m.after {
            PlannedValue::Unknown { reason } => {
                bytes(&mut h, &[0]);
                field(&mut h, reason);
            }
            PlannedValue::Oid { oid } => {
                bytes(&mut h, &[1]);
                field(&mut h, oid);
            }
        }
    }
    format!("{h:016x}")
}

// ---- time and backup-ref naming --------------------------------------------

/// `YYYYMMDDTHHMMSSZ`, UTC, from `std::time` alone -- a chrono/time dependency
/// for one format string is not worth its supply chain.
pub fn utc_stamp() -> String {
    utc_stamp_at(SystemTime::now())
}

pub(crate) fn utc_stamp_at(t: SystemTime) -> String {
    // Pre-1970 is unrepresentable here and would mean a clock so broken that
    // continuing to write timestamped backup refs is worse than stopping.
    let secs = t.duration_since(UNIX_EPOCH).expect("system clock before 1970").as_secs();
    let (y, mo, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    format!("{y:04}{mo:02}{d:02}T{:02}{:02}{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// The extended form (`2026-07-24T12:00:00Z`) the ledger's `at` field uses --
/// the plan doc's schema. Same instant handed to both formatters where a
/// ledger entry and its provenance tag must agree (`regen` passes one
/// `SystemTime` to this and to `utc_stamp_at`); a ref name cannot carry the
/// colons, which is why the compact form exists at all.
pub(crate) fn utc_stamp_iso_at(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).expect("system clock before 1970").as_secs();
    let (y, mo, d) = civil_from_days((secs / 86400) as i64);
    let rem = secs % 86400;
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Days-since-epoch to (year, month, day). Howard Hinnant's civil-from-days
/// algorithm, the standard closed-form conversion -- no lookup tables, exact
/// over the whole proleptic Gregorian range.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The backup namespace `explain.rs` has referenced since v0: one ref per
/// rewritten branch per run, never auto-pruned, restored by `sodactl undo`.
///
/// A subject under `tags/` names a tag, not a branch (`tag --force`'s
/// preservation of the re-pointed tag object); `undo` maps the segment back
/// to the right ref namespace.
pub fn backup_refname(stamp: &str, branch: &str) -> String {
    format!("refs/sodactl/backup/{stamp}/{branch}")
}

/// The stamp verbs feed `backup_refname`: the compact UTC second plus the
/// sub-second nanoseconds, zero-padded so the lexicographic newest-first sort
/// `undo`'s listing relies on keeps working within a second. The fraction is
/// what makes two rewrites of the same branch in one wall-clock second (an
/// undo-and-retry loop, a scripted `--yes` run) mint DISTINCT create-only
/// refnames instead of aborting mid-apply on a backup collision -- with the
/// journal already written and claiming a started run.
pub fn backup_stamp() -> String {
    let now = SystemTime::now();
    let nanos = now
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .subsec_nanos();
    format!("{}-{nanos:09}", utc_stamp_at(now))
}

// ---- preflight -------------------------------------------------------------

/// A precondition that stops a verb before anything is touched. Typed so the
/// CLI can downcast and map to exit 1 (EXIT_GATE) rather than 2; the Display
/// prefix `refused: ` is stable and greppable on purpose -- scripts and the
/// journal both key on it.
#[derive(Debug)]
pub enum Refusal {
    /// A replay is half-done in the wine submodule.
    InFlight { markers: Vec<&'static str>, narrative: String },
    /// Tracked paths are modified in the wine submodule (after the
    /// `build_artifacts` allowlist) -- a rebase or `checkout -B` would
    /// destroy them.
    DirtyTracked { paths: Vec<String> },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::InFlight { markers, narrative } => write!(
                f,
                "refused: a replay is in progress in the wine submodule ({}){} -- finish or abort it first: `git -C wine rebase --continue`, or `git -C wine rebase --abort`",
                markers.join(", "),
                narrative
            ),
            Refusal::DirtyTracked { paths } => write!(
                f,
                "refused: {} tracked path(s) modified in the wine submodule ({}{}) -- a rebase or `checkout -B` would destroy them; `git -C wine stash` or commit them to a tier first",
                paths.len(),
                paths.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                if paths.len() > 4 { ", …" } else { "" }
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// The shared precondition gate every mutating verb runs first.
///
/// Exactly two refusals, both consuming the same `treestate` observations
/// `checks/tree.rs` renders as findings (factored, not copied, so the check
/// and the gate cannot drift):
/// * a half-done replay (`REBASE_IN_FLIGHT`'s observation), and
/// * tracked modifications in wine after the `build_artifacts` allowlist
///   (`SUBMODULE_DIRTY`'s observation).
///
/// Undeclared UNTRACKED files deliberately do not refuse -- they are Hygiene
/// in the check for the same reason: a gate that fires on every warm build
/// tree gets `--force`d within a day, and then the gate protecting real work
/// is gone too.
///
/// Errors: a downcastable `Refusal` means "precondition failed, nothing was
/// changed" (CLI exit 1); any other error means the probe itself could not
/// answer (exit 2).
pub fn preflight(m: &Manifest, ws: &Workspace) -> anyhow::Result<()> {
    let gd = ws.wine.git_dir()?;
    if let Some(fl) = treestate::in_flight(&gd) {
        return Err(anyhow::Error::new(Refusal::InFlight {
            markers: fl.markers,
            narrative: fl.narrative,
        }));
    }
    let split = treestate::split_dirty(ws.wine.status(true)?, &m.build_artifacts);
    if !split.tracked.is_empty() {
        return Err(anyhow::Error::new(Refusal::DirtyTracked { paths: split.tracked }));
    }
    Ok(())
}

// ---- the journal -----------------------------------------------------------

/// What `.git/sodactl/journal/<plan-id>.json` holds. Written BEFORE the first
/// mutation and rewritten after every completed step, so a failure at any
/// point leaves a record of exactly how far the plan got. `resume` and a
/// human both read it; the backup refs it names outlive any reflog expiry.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Journal {
    pub verb: String,
    pub plan: Plan,
    pub started_at_utc: String,
    pub backups: Vec<(String, String)>,
    /// How many of `plan.steps` ran to completion.
    pub completed_steps: usize,
}

// ---- apply -----------------------------------------------------------------

/// The handles `apply` needs: a mutating and a reading handle per repo side.
/// The read handles serve pre-verification only -- every write goes through
/// the `GitMut`s.
pub struct ApplyCtx<'a> {
    pub wine: &'a GitMut,
    pub superproject: &'a GitMut,
    pub wine_read: &'a dyn GitRead,
    pub superproject_read: &'a dyn GitRead,
    /// The wine submodule path as the superproject records it, e.g. `wine` --
    /// used only to render `git -C wine ...` guidance, never to spawn.
    pub wine_path: String,
}

impl ApplyCtx<'_> {
    fn read(&self, side: RepoSide) -> &dyn GitRead {
        match side {
            RepoSide::Wine => self.wine_read,
            RepoSide::Superproject => self.superproject_read,
        }
    }
    fn mutate(&self, side: RepoSide) -> &GitMut {
        match side {
            RepoSide::Wine => self.wine,
            RepoSide::Superproject => self.superproject,
        }
    }
}

#[derive(Debug)]
pub struct ApplyOutcome {
    pub journal: PathBuf,
    pub backups: Vec<(String, String)>,
    pub steps_run: usize,
}

/// How `apply` fails. The variants are ordered by when they can occur; only
/// `Step` (and a mid-run `Journal`) can leave the repo different from how it
/// was found, and those name the journal and the backups in their Display.
#[derive(Debug)]
pub enum ApplyError {
    /// Pre-verification found a ref somewhere other than the plan's
    /// `expected_before`. Nothing was changed. `refused: ` prefix -- exit 1.
    RefusedExpectedMoved { refname: String, expected: String, live: String },
    /// A read (pre-verify, gitdir resolution) could not answer.
    Probe(GitError),
    /// The journal could not be written or serialized.
    Journal { path: PathBuf, detail: String },
    /// A backup ref could not be created. Steps had not started; the create-
    /// only CAS makes a collision with an existing backup an error, because
    /// overwriting a backup destroys exactly what it exists to preserve.
    Backup { refname: String, journal: PathBuf, detail: String },
    /// The caller's filesystem prelude (`apply_with_prelude`) failed. No git
    /// step has run, but the prelude may have partially done its filesystem
    /// work -- the detail says what; the journal records the attempt.
    Prelude { journal: PathBuf, detail: String },
    /// A step failed (or stopped on a conflict -- `GitMut` surfaces both as
    /// `Err`; the caller distinguishes them structurally via the gitdir).
    Step {
        index: usize,
        total: usize,
        description: String,
        detail: String,
        journal: PathBuf,
        backups: Vec<(String, String)>,
        guidance: String,
    },
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApplyError::RefusedExpectedMoved { refname, expected, live } => write!(
                f,
                "refused: {refname} moved under us -- the plan expected {expected}, live is {live}; nothing was changed. Re-run to derive a fresh plan"
            ),
            ApplyError::Probe(e) => write!(f, "could not verify live refs before applying: {e}"),
            ApplyError::Journal { path, detail } => write!(
                f,
                "the journal at {} could not be written: {detail} -- verify repo state by hand before re-running",
                path.display()
            ),
            ApplyError::Backup { refname, journal, detail } => write!(
                f,
                "could not create backup ref {refname}: {detail} -- no step has run; journal: {}",
                journal.display()
            ),
            ApplyError::Prelude { journal, detail } => write!(
                f,
                "the plan's filesystem work failed before any git step ran: {detail} -- journal: {}",
                journal.display()
            ),
            ApplyError::Step { index, total, description, detail, journal, backups, guidance } => {
                write!(
                    f,
                    "apply stopped at step {}/{total} ({description}): {detail}\n  journal: {}",
                    index + 1,
                    journal.display()
                )?;
                for (name, tip) in backups {
                    write!(f, "\n  backup: {name} -> {tip}")?;
                }
                write!(f, "\n  next: {guidance}")
            }
        }
    }
}

impl std::error::Error for ApplyError {}

/// What to do after a stopped step, keyed on what the step could leave in
/// flight. Only the replaying actions park sequencer state; everything else
/// fails atomically and the recovery is the backups.
fn guidance(action: &Action, wine_path: &str) -> String {
    match action {
        Action::RebaseOnto { .. } => format!(
            "resolve the conflicts, then `git -C {wine_path} add <paths>` and `git -C {wine_path} rebase --continue` (or `sodactl resume`); to abandon, `git -C {wine_path} rebase --abort`"
        ),
        Action::CherryPick { .. } => format!(
            "resolve the conflicts, then `git -C {wine_path} add <paths>` and `git -C {wine_path} cherry-pick --continue`; to abandon, `git -C {wine_path} cherry-pick --abort`"
        ),
        _ => "this command leaves nothing in flight; the cause is in the message above. `sodactl undo` restores the backed-up branches".to_string(),
    }
}

/// Run a plan: pre-verify, journal, back up, execute.
pub fn apply(plan: &Plan, ctx: &ApplyCtx) -> Result<ApplyOutcome, ApplyError> {
    apply_with_prelude(plan, ctx, || Ok(()))
}

/// `apply`, with a filesystem prelude that runs AFTER pre-verification, the
/// journal and the backups but BEFORE the first step. For verbs whose work
/// includes non-git filesystem changes a later step must observe (`rr gc`'s
/// cache prunes, which its `add` step stages): the prelude is irreversible,
/// so everything that can still REFUSE -- above all the `expected_before`
/// pre-verify, whose refusal promises "nothing was changed" -- must have
/// passed before it runs, or the refusal message lies about deleted files.
///
/// Order is the safety argument:
/// 1. every `expected_before` is checked against the live refs -- a mismatch
///    refuses with nothing changed (and for `update-ref` steps git re-checks
///    under its own ref lock at execution time, so the window between here and
///    there is closed by CAS, not by hope);
/// 2. the journal is written BEFORE the first mutation, so there is no
///    failure mode in which refs moved and no record says which plan moved
///    them;
/// 3. backup refs are created create-only in the WINE repo -- history is only
///    ever rewritten there; the superproject only gains commits, which git's
///    own reflog covers;
/// 4. the prelude runs (a no-op for plain `apply`);
/// 5. steps run in order, the journal's `completed_steps` advancing after
///    each, and the first failure stops everything with the journal, the
///    backups and the continue/abort commands in the error. A `checkout -B`
///    step -- the one branch move git gives no CAS for -- gets its target's
///    `expected_before` re-verified immediately before it runs, because the
///    window since step 1 can be wide (regen's fetch step sits on the network
///    first) and silently clobbering a concurrently-moved branch preserves
///    its new tip nowhere.
pub fn apply_with_prelude(
    plan: &Plan,
    ctx: &ApplyCtx,
    prelude: impl FnOnce() -> anyhow::Result<()>,
) -> Result<ApplyOutcome, ApplyError> {
    // 1. Pre-verify. `rev_parse_raw`, not `rev_parse`: a branch ref must match
    // the recorded oid AS STORED, and peeling could only blur that.
    for mu in &plan.mutations {
        let live = ctx.read(mu.repo).rev_parse_raw(&mu.refname).map_err(ApplyError::Probe)?;
        let ok = match (&mu.expected_before, &live) {
            (Some(e), Some(l)) => e == l,
            (None, None) => true,
            _ => false,
        };
        if !ok {
            let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "<absent>".to_string());
            return Err(ApplyError::RefusedExpectedMoved {
                refname: mu.refname.clone(),
                expected: show(&mu.expected_before),
                live: show(&live),
            });
        }
    }

    // 2. Journal, before anything moves.
    let gd = ctx.wine_read.git_dir().map_err(ApplyError::Probe)?;
    let dir = gd.join("sodactl").join("journal");
    let path = dir.join(format!("{}.json", plan.id));
    std::fs::create_dir_all(&dir)
        .map_err(|e| ApplyError::Journal { path: path.clone(), detail: e.to_string() })?;
    let mut journal = Journal {
        verb: plan.verb.clone(),
        plan: plan.clone(),
        started_at_utc: utc_stamp(),
        backups: plan.backups.clone(),
        completed_steps: 0,
    };
    write_journal(&path, &journal)?;

    // 3. Backups, create-only.
    for (refname, tip) in &plan.backups {
        ctx.wine.update_ref_cas(refname, tip, None).map_err(|e| ApplyError::Backup {
            refname: refname.clone(),
            journal: path.clone(),
            detail: e.to_string(),
        })?;
    }

    // 4. The prelude (see `apply_with_prelude`'s doc for why it sits exactly
    // here: after everything that can refuse, before anything a step stages).
    prelude().map_err(|e| ApplyError::Prelude { journal: path.clone(), detail: format!("{e:#}") })?;

    // 5. Steps.
    for (i, step) in plan.steps.iter().enumerate() {
        // The `checkout -B` re-verify (doc point 5). Skipped when an earlier
        // step already legitimately rewrote the same ref -- its recorded
        // `expected_before` is stale by the plan's own doing at that point.
        if let Action::CheckoutBranchReset { branch, .. } = &step.action {
            let refname = format!("refs/heads/{branch}");
            let touched_earlier = plan.steps[..i].iter().any(|s| match &s.action {
                Action::CheckoutBranchReset { branch: b, .. }
                | Action::RebaseOnto { branch: b, .. } => format!("refs/heads/{b}") == refname,
                Action::UpdateRefCas { refname: r, .. } => *r == refname,
                _ => false,
            });
            let expected = plan.mutations.iter().find(|mu| mu.refname == refname);
            if let (false, Some(mu)) = (touched_earlier, expected) {
                let live = ctx.read(step.repo).rev_parse_raw(&refname).map_err(ApplyError::Probe)?;
                if live != mu.expected_before {
                    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "<absent>".to_string());
                    return Err(ApplyError::Step {
                        index: i,
                        total: plan.steps.len(),
                        description: step.description.clone(),
                        detail: format!(
                            "{refname} moved while the earlier steps ran -- the plan expected {}, \
                             live is {}; the branch was NOT overwritten",
                            show(&mu.expected_before),
                            show(&live)
                        ),
                        journal: path.clone(),
                        backups: plan.backups.clone(),
                        guidance: "sort out who moved the branch, then re-run the verb to derive \
                                   a fresh plan (the completed steps -- fetch, config -- left \
                                   nothing to unwind)"
                            .to_string(),
                    });
                }
            }
        }
        step.action.run(ctx.mutate(step.repo)).map_err(|e| ApplyError::Step {
            index: i,
            total: plan.steps.len(),
            description: step.description.clone(),
            detail: e.to_string(),
            journal: path.clone(),
            backups: plan.backups.clone(),
            guidance: guidance(&step.action, &ctx.wine_path),
        })?;
        journal.completed_steps = i + 1;
        write_journal(&path, &journal)?;
    }

    Ok(ApplyOutcome { journal: path, backups: plan.backups.clone(), steps_run: plan.steps.len() })
}

fn write_journal(path: &Path, j: &Journal) -> Result<(), ApplyError> {
    let s = serde_json::to_string_pretty(j)
        .map_err(|e| ApplyError::Journal { path: path.to_path_buf(), detail: e.to_string() })?;
    std::fs::write(path, s)
        .map_err(|e| ApplyError::Journal { path: path.to_path_buf(), detail: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::Workspace;
    use sodactl_git::GitCli;
    use std::process::Command;

    // ---- fixtures ----------------------------------------------------------
    //
    // Real scratch repos, same reasoning as the sodactl-git suites: apply's
    // contract IS git's behaviour (ref locks, sequencer state), which a fake
    // would re-encode from the author's beliefs. Pid-scoped for the reason
    // documented on the lib.rs `scratch`: entry does `remove_dir_all`, so an
    // unscoped path lets a concurrent run delete this run's live repos.

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Run git for fixture setup, asserting success. Test-only spawning is
    /// fine here -- the audited-constructor discipline governs production
    /// code, and core has no production spawn sites at all.
    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Like `git`, for commands whose nonzero exit IS the fixture (a rebase
    /// stopping on a conflict).
    fn git_may_fail(dir: &Path, args: &[&str]) {
        let _ = Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
    }

    fn scratch(name: &str) -> (Scratch, GitCli) {
        let dir = std::env::temp_dir()
            .join(format!("sodactl-core-plan-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "one"]);
        let cli = GitCli::new(&dir);
        (Scratch(dir), cli)
    }

    fn commit(dir: &Path, file: &str, content: &str, msg: &str) {
        std::fs::write(dir.join(file), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", msg]);
    }

    fn head(g: &GitCli) -> String {
        g.rev_parse("HEAD").unwrap().unwrap()
    }

    /// A minimal parseable manifest; only `build_artifacts` varies per test.
    fn manifest(build_artifacts: &[&str]) -> Manifest {
        let arts =
            build_artifacts.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(", ");
        Manifest::parse(&format!(
            r#"
schema = 1
build_artifacts = [{arts}]
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "0000000"
describes = "none"
derivation = "none"
"#
        ))
        .unwrap()
    }

    /// Both roles point at the same scratch repo: preflight and apply only
    /// exercise the wine side, and the superproject handle merely has to
    /// exist.
    fn ctx<'a>(mutant: &'a GitMut, reader: &'a GitCli) -> ApplyCtx<'a> {
        ApplyCtx {
            wine: mutant,
            superproject: mutant,
            wine_read: reader,
            superproject_read: reader,
            wine_path: "wine".to_string(),
        }
    }

    fn journal_value(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    // ---- the id ------------------------------------------------------------

    fn sample_plan(expected: &str, notes: Vec<String>, backups: Vec<(String, String)>) -> Plan {
        Plan::new(
            "restack",
            vec![Step::new(
                "rebase soda-demo onto its parent",
                RepoSide::Wine,
                Action::RebaseOnto {
                    newbase: "abc".into(),
                    upstream: "def".into(),
                    branch: "soda-demo".into(),
                },
            )],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/soda-demo".into(),
                expected_before: Some(expected.into()),
                after: PlannedValue::Unknown { reason: "rebase result".into() },
            }],
            backups,
            notes,
        )
    }

    #[test]
    fn plan_id_is_stable_over_everything_that_is_not_identity() {
        let a = sample_plan("1111", vec![], vec![]);
        // Same verb, steps and mutations -- different notes and different
        // (timestamped) backups. The id must not move, or every re-derivation
        // would drift by construction.
        let b = sample_plan(
            "1111",
            vec!["reworded narration".into()],
            vec![(backup_refname("20260724T120000Z", "soda-demo"), "1111".into())],
        );
        assert_eq!(a.id, b.id);
        assert_eq!(a.id.len(), 16, "an fnv64 hex digest");
    }

    #[test]
    fn plan_id_drifts_when_state_or_commands_change() {
        let a = sample_plan("1111", vec![], vec![]);
        // The live state the plan was derived from changed underneath it.
        let moved = sample_plan("2222", vec![], vec![]);
        assert_ne!(a.id, moved.id, "expected_before is identity");
        // The commands changed.
        let mut other = sample_plan("1111", vec![], vec![]);
        other.steps[0] = Step::new(
            "rebase soda-demo onto its parent",
            RepoSide::Wine,
            Action::RebaseOnto {
                newbase: "zzz".into(),
                upstream: "def".into(),
                branch: "soda-demo".into(),
            },
        );
        let other = Plan::new("restack", other.steps, other.mutations, vec![], vec![]);
        assert_ne!(a.id, other.id, "argv is identity");
    }

    // ---- time --------------------------------------------------------------

    #[test]
    fn utc_stamp_formats_known_instants() {
        let at = |secs| utc_stamp_at(UNIX_EPOCH + std::time::Duration::from_secs(secs));
        assert_eq!(at(0), "19700101T000000Z");
        assert_eq!(at(1_000_000_000), "20010909T014640Z");
        // A leap day -- the civil_from_days conversion's classic edge.
        assert_eq!(at(951_782_400), "20000229T000000Z");
        // The ledger's extended twin renders the SAME instant, colons and all.
        let iso = |secs| utc_stamp_iso_at(UNIX_EPOCH + std::time::Duration::from_secs(secs));
        assert_eq!(iso(1_000_000_000), "2001-09-09T01:46:40Z");
    }

    // ---- preflight ---------------------------------------------------------

    #[test]
    fn preflight_refuses_a_half_done_replay() {
        let (s, g) = scratch("preflight-inflight");
        git(&s.0, &["checkout", "-q", "-b", "topic"]);
        commit(&s.0, "a.txt", "topic\n", "topic work");
        git(&s.0, &["checkout", "-q", "main"]);
        commit(&s.0, "a.txt", "main\n", "main work");
        // Both sides rewrote a.txt: the rebase stops in flight, which is the
        // fixture -- not a failure of the fixture.
        git_may_fail(&s.0, &["rebase", "main", "topic"]);

        let ws = Workspace {
            superproject: &g,
            wine: &g,
            superproject_path: s.0.clone(),
            wine_path: "wine".into(),
        };
        let err = preflight(&manifest(&[]), &ws).expect_err("mid-rebase must refuse");
        let r = err.downcast_ref::<Refusal>().expect("a typed, downcastable Refusal");
        assert!(matches!(r, Refusal::InFlight { .. }), "got {r:?}");
        assert!(r.to_string().starts_with("refused: "), "the greppable prefix: {r}");

        git_may_fail(&s.0, &["rebase", "--abort"]);
    }

    #[test]
    fn preflight_refuses_tracked_dirt_but_honours_the_allowlist() {
        let (s, g) = scratch("preflight-dirty");
        std::fs::write(s.0.join("a.txt"), "modified\n").unwrap();
        let ws = Workspace {
            superproject: &g,
            wine: &g,
            superproject_path: s.0.clone(),
            wine_path: "wine".into(),
        };

        let err = preflight(&manifest(&[]), &ws).expect_err("tracked dirt must refuse");
        match err.downcast_ref::<Refusal>().expect("typed") {
            Refusal::DirtyTracked { paths } => assert_eq!(paths, &vec!["a.txt".to_string()]),
            other => panic!("wrong refusal: {other:?}"),
        }

        // The same modification, declared as a build artifact: silent, the
        // `configure` story from treestate::split_dirty.
        preflight(&manifest(&["a.txt"]), &ws).expect("an allowlisted path must not gate");

        // Undeclared UNTRACKED files are Hygiene, not a gate.
        git(&s.0, &["checkout", "-q", "--", "a.txt"]);
        std::fs::write(s.0.join("stray.txt"), "x\n").unwrap();
        preflight(&manifest(&[]), &ws).expect("untracked-only dirt must not gate");
    }

    // ---- apply -------------------------------------------------------------

    #[test]
    fn apply_writes_the_journal_creates_backups_and_runs_the_steps() {
        let (s, g) = scratch("apply-happy");
        let m = GitMut::new(&s.0);
        let c1 = head(&g);
        let stamp = utc_stamp();
        let backup = backup_refname(&stamp, "main");

        let plan = Plan::new(
            "test",
            vec![Step::new(
                "create the spare branch",
                RepoSide::Wine,
                Action::UpdateRefCas {
                    refname: "refs/heads/spare".into(),
                    new: c1.clone(),
                    expected_old: None,
                },
            )],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/spare".into(),
                expected_before: None,
                after: PlannedValue::Oid { oid: c1.clone() },
            }],
            vec![(backup.clone(), c1.clone())],
            vec![],
        );

        let out = apply(&plan, &ctx(&m, &g)).unwrap();
        assert_eq!(out.steps_run, 1);
        assert_eq!(g.rev_parse_raw(&backup).unwrap().unwrap(), c1, "the backup ref exists");
        assert_eq!(g.rev_parse("refs/heads/spare").unwrap().unwrap(), c1, "the step ran");

        // The journal: at <gitdir>/sodactl/journal/<plan-id>.json, and shaped
        // as documented.
        assert_eq!(out.journal, g.git_dir().unwrap().join("sodactl/journal").join(format!("{}.json", plan.id)));
        let j = journal_value(&out.journal);
        assert_eq!(j["verb"], "test");
        assert_eq!(j["plan"]["id"], plan.id);
        assert_eq!(j["completed_steps"], 1);
        assert_eq!(j["backups"][0][0], backup);
        let ts = j["started_at_utc"].as_str().unwrap();
        assert_eq!(ts.len(), 16, "YYYYMMDDTHHMMSSZ: {ts}");
        assert!(ts.ends_with('Z') && ts.as_bytes()[8] == b'T', "{ts}");
    }

    #[test]
    fn apply_refuses_before_anything_when_a_ref_moved() {
        let (s, g) = scratch("apply-moved");
        let m = GitMut::new(&s.0);
        let c1 = head(&g);
        commit(&s.0, "b.txt", "b\n", "two");

        let plan = Plan::new(
            "test",
            vec![Step::new(
                "would move main",
                RepoSide::Wine,
                Action::UpdateRefCas {
                    refname: "refs/heads/main".into(),
                    new: c1.clone(),
                    expected_old: Some(c1.clone()),
                },
            )],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/main".into(),
                expected_before: Some(c1.clone()), // stale: main is at c2 now
                after: PlannedValue::Oid { oid: c1.clone() },
            }],
            vec![],
            vec![],
        );

        let err = apply(&plan, &ctx(&m, &g)).unwrap_err();
        match &err {
            ApplyError::RefusedExpectedMoved { refname, .. } => {
                assert_eq!(refname, "refs/heads/main")
            }
            other => panic!("wrong error: {other:?}"),
        }
        assert!(err.to_string().starts_with("refused: "), "{err}");
        // Refused means NOTHING happened -- including no journal.
        assert!(
            !g.git_dir().unwrap().join("sodactl/journal").exists(),
            "a refusal must not leave a journal claiming a run started"
        );
    }

    #[test]
    fn apply_refuses_to_clobber_an_existing_backup() {
        let (s, g) = scratch("apply-backup-collision");
        let m = GitMut::new(&s.0);
        let c1 = head(&g);
        let backup = backup_refname("20260724T000000Z", "main");

        // Steps and mutations empty on purpose: the collision under test is
        // the backup creation itself, not something pre-verify would catch.
        let plan =
            Plan::new("test", vec![], vec![], vec![(backup.clone(), c1.clone())], vec![]);
        apply(&plan, &ctx(&m, &g)).unwrap();

        let err = apply(&plan, &ctx(&m, &g)).unwrap_err();
        match &err {
            ApplyError::Backup { refname, .. } => assert_eq!(refname, &backup),
            other => panic!("wrong error: {other:?}"),
        }
        assert_eq!(
            g.rev_parse_raw(&backup).unwrap().unwrap(),
            c1,
            "the original backup must survive the collision untouched"
        );
    }

    #[test]
    fn apply_stops_on_a_cas_mismatch_and_names_the_recovery() {
        let (s, g) = scratch("apply-cas-stop");
        let m = GitMut::new(&s.0);
        let c1 = head(&g);
        git(&s.0, &["branch", "work", &c1]);
        commit(&s.0, "b.txt", "b\n", "two");
        let c2 = head(&g);
        let backup = backup_refname("20260724T000001Z", "work");

        // Step 1 legitimately moves `work` to c2; step 2 then presents a
        // STALE expected_old -- pre-verify cannot catch it (it was true when
        // the plan was derived), so git's own ref-lock CAS is the layer that
        // must stop the run.
        let plan = Plan::new(
            "test",
            vec![
                Step::new(
                    "advance work to c2",
                    RepoSide::Wine,
                    Action::UpdateRefCas {
                        refname: "refs/heads/work".into(),
                        new: c2.clone(),
                        expected_old: Some(c1.clone()),
                    },
                ),
                Step::new(
                    "stale move of work",
                    RepoSide::Wine,
                    Action::UpdateRefCas {
                        refname: "refs/heads/work".into(),
                        new: c1.clone(),
                        expected_old: Some(c1.clone()),
                    },
                ),
            ],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/work".into(),
                expected_before: Some(c1.clone()),
                after: PlannedValue::Oid { oid: c2.clone() },
            }],
            vec![(backup.clone(), c1.clone())],
            vec![],
        );

        let err = apply(&plan, &ctx(&m, &g)).unwrap_err();
        let (journal, msg) = match &err {
            ApplyError::Step { index, journal, .. } => {
                assert_eq!(*index, 1, "the FIRST step must have run");
                (journal.clone(), err.to_string())
            }
            other => panic!("wrong error: {other:?}"),
        };
        // The error names everything recovery needs.
        assert!(msg.contains(&journal.display().to_string()), "{msg}");
        assert!(msg.contains(&backup), "{msg}");
        assert!(msg.contains("sodactl undo"), "{msg}");

        // The repo shows exactly one step's worth of change...
        assert_eq!(g.rev_parse_raw("refs/heads/work").unwrap().unwrap(), c2);
        // ...and the journal records exactly how far the run got.
        assert_eq!(journal_value(&journal)["completed_steps"], 1);
    }

    #[test]
    fn a_checkout_b_reverifies_its_target_immediately_before_running() {
        let (s, g) = scratch("apply-checkout-reverify");
        let m = GitMut::new(&s.0);
        let c1 = head(&g);

        // `main` is checked out; the plan's middle steps advance it via a
        // commit -- a movement pre-verify could not see (it was true at step
        // 0) and no ref-lock CAS guards, because `checkout -B` has none. The
        // re-verify immediately before the overwrite is the only layer that
        // can stop it, and the moved tip must survive.
        std::fs::write(s.0.join("moved.txt"), "m\n").unwrap();
        let plan = Plan::new(
            "test",
            vec![
                Step::new(
                    "stage the stranger's change",
                    RepoSide::Wine,
                    Action::Add { paths: vec!["moved.txt".into()] },
                ),
                Step::new(
                    "the concurrent writer, played by a commit",
                    RepoSide::Wine,
                    Action::Commit { message: "someone else's work".into() },
                ),
                Step::new(
                    "reset main back to c1",
                    RepoSide::Wine,
                    Action::CheckoutBranchReset { branch: "main".into(), start: c1.clone() },
                ),
            ],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/main".into(),
                expected_before: Some(c1.clone()),
                after: PlannedValue::Oid { oid: c1.clone() },
            }],
            vec![],
            vec![],
        );

        let err = apply(&plan, &ctx(&m, &g)).unwrap_err();
        match &err {
            ApplyError::Step { index, detail, .. } => {
                assert_eq!(*index, 2, "the checkout step is where it stops");
                assert!(detail.contains("NOT overwritten"), "{detail}");
            }
            other => panic!("wrong error: {other:?}"),
        }
        let live = g.rev_parse_raw("refs/heads/main").unwrap().unwrap();
        assert_ne!(live, c1, "the concurrent tip survived -- the -B never ran");
        assert!(g.rev_parse_raw("HEAD:moved.txt").unwrap().is_some(), "the commit is intact");
    }

    #[test]
    fn the_prelude_runs_after_preverify_and_before_the_steps() {
        let (s, g) = scratch("apply-prelude");
        let m = GitMut::new(&s.0);
        let c1 = head(&g);

        // A refusing pre-verify must leave the prelude UNRUN -- that ordering
        // is the whole point of the hook (rr gc's "refused ... nothing was
        // changed" must be true of the cache files too).
        let stale = Plan::new(
            "test",
            vec![],
            vec![RefMutation {
                repo: RepoSide::Wine,
                refname: "refs/heads/main".into(),
                expected_before: Some("1".repeat(40)),
                after: PlannedValue::Oid { oid: c1.clone() },
            }],
            vec![],
            vec![],
        );
        let mut ran = false;
        let err = apply_with_prelude(&stale, &ctx(&m, &g), || {
            ran = true;
            Ok(())
        })
        .unwrap_err();
        assert!(matches!(err, ApplyError::RefusedExpectedMoved { .. }), "{err:?}");
        assert!(!ran, "a refused plan must not run its prelude");

        // On the happy path it runs before the first step: the step stages
        // the file the prelude writes.
        let ok = Plan::new(
            "test",
            vec![Step::new(
                "stage what the prelude wrote",
                RepoSide::Wine,
                Action::Add { paths: vec!["pre.txt".into()] },
            )],
            vec![],
            vec![],
            vec![],
        );
        let dir = s.0.clone();
        apply_with_prelude(&ok, &ctx(&m, &g), || {
            std::fs::write(dir.join("pre.txt"), "p\n")?;
            Ok(())
        })
        .unwrap();
        assert!(
            g.status(false).unwrap().iter().any(|e| e.path == "pre.txt" && e.xy.starts_with('A')),
            "the step saw and staged the prelude's file"
        );
    }

    #[test]
    fn backup_stamps_disambiguate_within_a_second_and_keep_sorting() {
        let a = backup_stamp();
        let b = backup_stamp();
        // Same wall-clock second in any sane run; the fraction still tells
        // them apart -- the collision class the create-only backup ref used
        // to abort on.
        assert_ne!(a, b, "two stamps in one second must differ");
        // Shape: <compact-utc>-<9-digit-nanos>, so the lexicographic
        // newest-first sort in `undo`'s listing stays correct across and
        // within seconds.
        let (utc, frac) = a.split_once('-').expect("a fraction suffix");
        assert_eq!(utc.len(), 16, "{utc}");
        assert_eq!(frac.len(), 9, "zero-padded: {frac}");
        assert!(frac.bytes().all(|b| b.is_ascii_digit()), "{frac}");
    }

    #[test]
    fn a_step_argv_is_the_gitmut_argv() {
        // The display twin of the create-only CAS must show the sentinel git
        // receives -- the reviewed plan and the executed argv are the same
        // bytes.
        let s = Step::new(
            "backup",
            RepoSide::Wine,
            Action::UpdateRefCas { refname: "refs/x".into(), new: "abc".into(), expected_old: None },
        );
        assert_eq!(s.argv, vec!["update-ref", "refs/x", "abc", ZERO_OID]);
        let t = Step::new(
            "tag",
            RepoSide::Wine,
            Action::TagAnnotated {
                name: "v1".into(),
                target: "abc".into(),
                message: "m".into(),
                force: true,
            },
        );
        assert_eq!(t.argv, vec!["tag", "-a", "-f", "-m", "m", "v1", "abc"]);
    }
}
