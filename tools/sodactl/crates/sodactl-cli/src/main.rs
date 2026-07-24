//! `sodactl` -- the read surface (`status`/`check`/`rr-health`) plus the
//! plan-first write verbs (`restack`/`stitch`/`regen`/`tag`/`pin`/`rr-gc`/
//! `resume`/`undo`, dispatched in `verbs.rs`) and the build lanes.
//!
//! Rendering only in this file. Every fact comes from `sodactl-core`, so
//! `--json` and the MCP surface return the identical values with no second
//! code path. The write verbs default to printing a plan and changing
//! nothing; `--apply` executes it. There is no push verb -- `push` prints.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sodactl_core::checks::{Policy, Workspace};
use sodactl_core::{finding::Severity, present, reconcile_workspace, Manifest, RrSurvey};
use sodactl_git::{Differential, GitCli, GitGix, GitHybrid, GitMut, GitRead as _};
use std::path::PathBuf;

mod lanes;
mod verbs;

#[derive(Parser)]
#[command(
    name = "sodactl",
    about = "SpritzWine tier/consumer release state",
    version
)]
struct Cli {
    /// Path to the manifest.
    #[arg(long, default_value = "tools/sodactl.toml", global = true)]
    manifest: PathBuf,
    /// Emit JSON instead of a table.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// The graph as it stands: tiers, stitches, consumers.
    Status,
    /// Only what is wrong. Exits non-zero at or above --severity-min.
    Check {
        /// blocker | drift | hygiene
        #[arg(long, default_value = "drift")]
        severity_min: String,
    },
    /// The rerere cache: which entries can actually help, and which re-prompt forever.
    RrHealth,
    /// Rebase a stale tier's own commits onto its parent's current tip.
    ///
    /// `<old>` is never an argument: the old base is computed from the child's
    /// patch-id own-set, a backup ref is taken first, and the result is
    /// patch-id-verified (restored from the backup on drift). Default prints
    /// the plan and changes nothing.
    Restack {
        /// The tier branch to restack.
        tier: String,
        /// Also restack every descendant tier needing it, parents first.
        #[arg(long)]
        all_descendants: bool,
        /// Execute the plan (re-derived and id-compared against the review).
        #[arg(long)]
        apply: bool,
        /// Skip the interactive y/N confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// List history-rewrite backups, or restore one (CAS against the live tip).
    Undo {
        /// Backup to restore, as listed: `<utc>/<branch>` (or the full ref).
        /// Omit to list what exists.
        backup: Option<String>,
        /// Execute the restore (re-derived and id-compared against the review).
        #[arg(long)]
        apply: bool,
        /// Skip the interactive y/N confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Continue a conflicted replay, after verifying nothing else moved.
    Resume {
        /// Actually run `git rebase --continue` (default only reports).
        #[arg(long)]
        apply: bool,
    },
    /// Rebuild a declared stitch from scratch: -B from the base tier, then the
    /// manifest's cherry-pick order, patch-id-verified afterwards.
    Stitch {
        /// The [[stitch]] name from the manifest.
        name: String,
        /// Execute the plan (re-derived and id-compared against the review).
        #[arg(long)]
        apply: bool,
    },
    /// Cut `<family>-<winebase>-<n>` on the family's component tip.
    ///
    /// The target and the wine base are DERIVED (consumer branch or the soda
    /// stitch; the base from the target's own base ref) -- only the family and
    /// the version are arguments.
    Tag {
        /// A consumer name, or `soda`.
        family: String,
        /// The family version (the trailing integer).
        n: u32,
        /// Re-point an existing name / overrule the tag-time refusals.
        #[arg(long)]
        force: bool,
        /// Execute the plan (re-derived and id-compared against the review).
        #[arg(long)]
        apply: bool,
        /// Skip the interactive y/N confirmation (asked only with --force).
        #[arg(long)]
        yes: bool,
    },
    /// Move the superproject's wine pointer to a TAG: checkout, .gitmodules
    /// branch=, gitlink -- one commit.
    Pin {
        /// The tag to pin (never a SHA, never a branch).
        tag: String,
        /// Skip the ls-remote probe of the .gitmodules URL.
        #[arg(long)]
        offline: bool,
        /// Execute the plan (re-derived and id-compared against the review).
        #[arg(long)]
        apply: bool,
        /// Skip the interactive y/N confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Re-pour a consumer: fetch its base, replay the soda tiers onto it
    /// (rerere-seeded, `[[binary_resolution]]` rules applied per conflict),
    /// then record the pour -- rr-cache persisted + committed, a
    /// `[ledger.<consumer>]` entry written into the manifest, and an annotated
    /// `gen/<consumer>/<utc>` provenance tag cut on the result.
    Regen {
        /// The [[consumer]] name from the manifest.
        consumer: String,
        /// Execute the plan (re-derived and id-compared against the review).
        #[arg(long)]
        apply: bool,
    },
    /// Prune the tracked rerere cache: postimage-less entries older than
    /// --older-than, entries shadowed by an unscoped [[binary_resolution]]
    /// rule (any age), stray thisimage files; then `git rerere gc` in wine and
    /// one scoped superproject commit for the pruned cache.
    #[command(name = "rr-gc")]
    RrGc {
        /// Only prune postimage-less entries at least this many days old.
        #[arg(long, default_value_t = 30)]
        older_than: u64,
        /// Execute the plan (re-derived and compared against the review).
        #[arg(long)]
        apply: bool,
        /// Skip the interactive y/N confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Print the ordered push commands (wine first). NEVER executes -- there
    /// is no push verb and there will not be one; Florian pushes.
    Push,
    /// Run the superproject's `./build.sh` from the workspace root, terminal
    /// inherited, the script's exit code passed through.
    ///
    /// Refuses when the wine checkout is not the effective superproject pin:
    /// build.sh:44 (`git submodule update --force`) checks out the RECORDED
    /// pin and silently builds it, so what you see would not be what builds.
    Build {
        /// Build despite checkout != pin (the PIN builds, not the checkout).
        #[arg(long)]
        anyway: bool,
    },
    /// Package the already-built tree via `packaging/package.sh` (terminal
    /// inherited, exit code passed through). `--version` becomes the script's
    /// `VERSION` environment -- it has no flag of its own.
    Package {
        /// The tarball's version label -- the release tag, normally.
        #[arg(long)]
        version: String,
    },
    /// Gate the built + packaged tree via `ci/smoke.sh` (terminal inherited,
    /// exit code passed through) -- what Wine DECIDED, then the tarball.
    Smoke,
    /// The release checklist for one family version: tag -> pin -> build ->
    /// package -> smoke -> push, each with live-derived status. Plan-only by
    /// construction; nothing here executes anything.
    Release {
        /// A consumer name, or `soda`.
        family: String,
        /// The family version (the trailing integer).
        n: u32,
    },
    /// The failure-path dashboard: findings, tier graph, commit chain.
    Tui,
    /// Serve the read-only MCP surface over stdio (for Claude Code et al).
    ///
    /// STDOUT IS THE PROTOCOL from here on. Nothing else may write to it -- a
    /// stray `println!` corrupts the JSON-RPC stream and the client's failure
    /// mode is an unhelpful parse error, not a clue about the culprit. Every
    /// diagnostic in this process goes to stderr.
    Mcp,
    /// Hidden: run the git-backend differential over the live repositories.
    ///
    /// Drives the FULL production pipeline through `Differential` (so gix is
    /// exercised with production's argument distribution), then the directed
    /// probes a healthy repo never reaches. Exit codes are its own contract,
    /// deliberately NOT `check`'s: 0 = backends agree, 2 = disagreements,
    /// 1 = the differential could not run at all -- so a `&&` chain can tell
    /// "gix diverged" from "the harness is broken", and neither reads as
    /// `check`'s "gate tripped".
    #[command(hide = true)]
    Selfcheck {
        /// Compare the git binary against the gix backend on every read.
        #[arg(long)]
        differential: bool,
    },
}

/// Exit 1 means "the gate tripped"; exit 2 means "the tool could not answer".
///
/// They were both 1, and that is not a cosmetic collision: a CI job wiring
/// `sodactl check` as a gate cannot tell "four blockers" from "I could not read
/// a single ref". The failure mode is the bad direction -- a totally broken
/// invocation looks exactly like a working tool reporting real problems, so the
/// gate stays red, someone "fixes" it by loosening `--severity-min`, and the
/// tool is now silently not running at all.
const EXIT_GATE: i32 = 1;
const EXIT_ERROR: i32 = 2;

fn main() {
    // `selfcheck` owns its exit codes (see the subcommand doc) and therefore
    // must not fall through to the generic `EXIT_ERROR` mapping below --
    // there, 2 means "could not run", while selfcheck's 2 means "the backends
    // disagreed", and collapsing them would make a broken harness look like a
    // gix divergence.
    let cli = Cli::parse();
    if let Cmd::Selfcheck { differential } = &cli.cmd {
        if !*differential {
            eprintln!("sodactl selfcheck: nothing selected (try --differential)");
            std::process::exit(0);
        }
        match selfcheck_differential(&cli.manifest) {
            Ok(true) => std::process::exit(0),
            Ok(false) => std::process::exit(2),
            Err(e) => {
                eprintln!("sodactl selfcheck: {e:?}");
                std::process::exit(1);
            }
        }
    }
    if let Err(e) = run(cli) {
        eprintln!("sodactl: {e:?}");
        std::process::exit(EXIT_ERROR);
    }
}

/// The differential engine: both repos, the production pipeline, then the
/// directed probes -- the arms a healthy repo never exercises, where the
/// `Ok(None)`/`Err` discipline actually lives. Returns `Ok(clean)`.
fn selfcheck_differential(manifest: &std::path::Path) -> Result<bool> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading manifest {}", manifest.display()))?;
    let m = Manifest::parse(&text).context("parsing manifest")?;
    let root = sodactl_core::workspace_root(manifest);
    let wine = root.join(&m.repo.wine);
    let policy = Policy::from_manifest(&m);

    let wine_cli = GitCli::new(&wine);
    let wine_gix = GitGix::new(&wine);
    let root_cli = GitCli::new(&root);
    let root_gix = GitGix::new(&root);
    let d_wine = Differential::new(&wine_cli, &wine_gix);
    let d_root = Differential::new(&root_cli, &root_gix);

    // The production pipeline, unmodified, through both harnesses: the
    // reference's answers flow onward, so this run IS the golden run while
    // gix is scored on the side.
    let ws = Workspace {
        superproject: &d_root,
        wine: &d_wine,
        superproject_path: root.clone(),
        wine_path: m.repo.wine.clone(),
    };
    reconcile_workspace(&m, &d_wine, &ws, &policy).context("reconcile through the differential")?;
    sodactl_core::graph::tier_graph(&m, &d_wine).context("tier graph through the differential")?;

    // Directed probes, wine side. The absent-ref name is chosen to be
    // unambiguous in a repo with 6697 real refs.
    let _ = d_wine.rev_parse("sodactl-selfcheck-no-such-ref");
    let _ = d_wine.rev_parse_raw("HEAD:no/such/path");
    // The full-hex echo pair: raw answers the hex WITHOUT an odb lookup even
    // for an object the repo never contained (git 2.55's --verify --quiet
    // behaviour); the peeled form stays None. The quiet-direction bug of
    // 2026-07-19 lived exactly here, invisible to every organic call.
    let _ = d_wine.rev_parse_raw("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let _ = d_wine.rev_parse("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let _ = d_wine.ahead_behind("sodactl-selfcheck-no-such-ref", "HEAD");
    let _ = d_wine.submodule_index_oid("no/such/path");
    if let Ok(Some(parent)) = d_wine.rev_parse("HEAD^") {
        let _ = d_wine.tags_pointing_at(&parent);
    }
    // `branches_containing(HEAD, true)` on BOTH repos: the remote-HEAD symref
    // (`refs/remotes/<r>/HEAD`) only enters the comparison through this probe
    // -- the production pipeline reaches branches_containing solely via the
    // pin, which sat on no remote branch while the symref mis-naming bug was
    // live, so selfcheck exited 0 while the live test was red. Never again:
    // the two "live" gates must ask the same questions.
    if let Ok(Some(h)) = d_wine.rev_parse("HEAD") {
        let _ = d_wine.branches_containing(&h, true);
    }
    if let Ok(Some(h)) = d_root.rev_parse("HEAD") {
        let _ = d_root.branches_containing(&h, true);
    }
    // Superproject side: the gitlink read and the config discipline.
    let _ = d_root.rev_parse_raw(&format!("HEAD:{}", m.repo.wine));
    let _ = d_root.submodule_index_oid(&m.repo.wine);
    let _ = d_root.config_file_get(".gitmodules", "submodule.nope.branch");
    let _ = d_root.head_branch();
    let _ = d_wine.superproject_pin();

    let wine_report = d_wine.finish();
    let root_report = d_root.finish();
    println!("== wine ({}) ==\n{wine_report}", wine.display());
    println!("== superproject ({}) ==\n{root_report}", root.display());
    Ok(wine_report.is_clean() && root_report.is_clean())
}

fn run(cli: Cli) -> Result<()> {
    let text = std::fs::read_to_string(&cli.manifest)
        .with_context(|| format!("reading manifest {}", cli.manifest.display()))?;
    let m = Manifest::parse(&text).context("parsing manifest")?;

    // The manifest sits in the superproject; wine is relative to it. The
    // derivation lives in core (`workspace_root`) with the story of the
    // empty-`parent()` bug that makes its filter load-bearing.
    let root = sodactl_core::workspace_root(&cli.manifest);
    let wine = root.join(&m.repo.wine);
    // The hybrid backend: gix answers 18 methods, the binary keeps `status`
    // and `missing_by_patch_id` -- see `sodactl_git::gix_backend` for the seam
    // and `sodactl selfcheck --differential` for the proof it stays aligned.
    let git = GitHybrid::new(&wine);

    // Policy is declared, not compiled in: `[policy]` plus the per-consumer
    // `exclude` keys. A manifest without the section answers exactly as it did
    // before the section existed -- see `Policy::from_manifest`.
    let policy = Policy::from_manifest(&m);

    // rr-health needs the filesystem and the gitdir indirection, not the tier
    // DAG, so it answers before the (multi-second) reconcile work is spent.
    if matches!(cli.cmd, Cmd::RrHealth) {
        return rr_health(&root, &wine, &policy, cli.json);
    }

    // Likewise `mcp`, for a stronger reason: it must not run the reconcile here
    // at all. The server re-derives state per request, because a long-lived
    // session that answered from a snapshot taken at startup would confidently
    // describe a repository that has since moved -- and the whole point of this
    // tool is that git is the live authority.
    if matches!(cli.cmd, Cmd::Mcp) {
        let manifest = cli.manifest.clone();
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async move { sodactl_mcp::serve_stdio(manifest).await });
    }

    // And `tui`, for the same reason as `mcp`: no reconcile here. Its worker
    // thread re-derives state on every refresh (startup, `r`, terminal focus),
    // because a dashboard rendering a startup snapshot forever would
    // confidently describe a repository that has since moved -- and git is the
    // live authority.
    if matches!(cli.cmd, Cmd::Tui) {
        return sodactl_tui::run(cli.manifest.clone());
    }

    // Two repositories, because the pin is a fact about the superproject and
    // every ref it names is a fact about the submodule. `reconcile()` sees only
    // one of them, which is precisely why its pin check had to read the gitlink
    // through a back door.
    // One handle per repository. There used to be two for wine -- a `GitCli`
    // for `reconcile()` and a `GitExec` for `Workspace`, built from the same
    // path four lines apart -- which was the trait drift made visible.
    let super_git = GitHybrid::new(&root);
    let ws = Workspace {
        superproject: &super_git,
        wine: &git,
        superproject_path: root.clone(),
        wine_path: m.repo.wine.clone(),
    };

    // The mutating verbs. They never consult the reconcile report -- the gate
    // lives on push, and a verb refuses only on its OWN preconditions -- so
    // they dispatch before the (multi-second) reconcile is spent, like the
    // early-outs above. They sit after the Workspace exists because preflight
    // needs both repositories. Exit codes are decided per error by
    // `verbs::exit_code_for` (refusal/terminal state = the gate, everything
    // else = "could not answer"), so they cannot fall through to the generic
    // EXIT_ERROR mapping in `main`.
    if matches!(
        cli.cmd,
        Cmd::Restack { .. }
            | Cmd::Undo { .. }
            | Cmd::Resume { .. }
            | Cmd::Stitch { .. }
            | Cmd::Tag { .. }
            | Cmd::Pin { .. }
            | Cmd::Regen { .. }
            | Cmd::RrGc { .. }
            | Cmd::Push
            | Cmd::Release { .. }
            | Cmd::Build { .. }
            | Cmd::Package { .. }
            | Cmd::Smoke
    ) {
        let wine_cli = GitCli::new(&wine);
        let super_cli = GitCli::new(&root);
        let wine_mut = GitMut::new(&wine);
        let super_mut = GitMut::new(&root);
        let actx = sodactl_core::plan::ApplyCtx {
            wine: &wine_mut,
            superproject: &super_mut,
            wine_read: &git,
            superproject_read: &super_git,
            wine_path: m.repo.wine.clone(),
        };
        if let Err(e) = verbs::dispatch(
            &cli.cmd,
            &m,
            &ws,
            &git,
            &wine_cli,
            &super_cli,
            &policy,
            &actx,
            &cli.manifest,
            cli.json,
        ) {
            eprintln!("sodactl: {e:#}");
            std::process::exit(verbs::exit_code_for(&e));
        }
        return Ok(());
    }

    let report = reconcile_workspace(&m, &git, &ws, &policy)?;

    match cli.cmd {
        Cmd::Status => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_status(&report);
            }
        }
        Cmd::Check { severity_min } => {
            let min = parse_severity(&severity_min)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report.findings)?);
            } else {
                render_findings(&report);
            }
            // Exit non-zero only at or above the gate, so this can go into CI
            // advisory-first and be tightened later without changing the tool.
            if report.findings.iter().any(|f| f.severity <= min) {
                std::process::exit(EXIT_GATE);
            }
        }
        // Handled above, before the reconcile cost (Selfcheck in `main`).
        Cmd::RrHealth
        | Cmd::Mcp
        | Cmd::Tui
        | Cmd::Selfcheck { .. }
        | Cmd::Restack { .. }
        | Cmd::Undo { .. }
        | Cmd::Resume { .. }
        | Cmd::Stitch { .. }
        | Cmd::Tag { .. }
        | Cmd::Pin { .. }
        | Cmd::Regen { .. }
        | Cmd::RrGc { .. }
        | Cmd::Push
        | Cmd::Release { .. }
        | Cmd::Build { .. }
        | Cmd::Package { .. }
        | Cmd::Smoke => unreachable!(),
    }
    Ok(())
}

/// The rerere cache, both copies of it.
///
/// There are two directories and they drift: `regenerate.sh` seeds the
/// submodule's live cache from the superproject's tracked one. The live copy is
/// NOT `wine/.git/rr-cache` -- `wine/.git` is a *file* holding
/// `gitdir: ../.git/modules/wine`, so a check that guesses that path inspects a
/// directory that does not exist, reports clean, and is a no-op forever. Ask git
/// where the gitdir is instead of assuming.
fn rr_health(
    root: &std::path::Path,
    wine: &std::path::Path,
    policy: &Policy,
    json: bool,
) -> Result<()> {
    let tracked = RrSurvey::read(root.join(&policy.rr_cache))?;
    let live = GitHybrid::new(wine)
        .git_dir()
        .ok()
        .map(|d| RrSurvey::read(d.join("rr-cache")))
        .transpose()?;

    if json {
        // Hand-built rather than derived: `RrSurvey` is a filesystem survey, not
        // a wire type, and giving it a `Serialize` impl would freeze its shape
        // as an API the moment anything consumed it.
        let entry = |e: &sodactl_core::RrEntry| {
            serde_json::json!({
                "id": e.id,
                "has_preimage": e.has_preimage,
                "has_postimage": e.has_postimage,
                "has_thisimage": e.has_thisimage,
                "unresolved": e.unresolved(),
                "preimage_head": e.preimage_head,
            })
        };
        let survey = |s: &RrSurvey| {
            serde_json::json!({
                "root": s.root,
                "missing": s.missing,
                "entries": s.entries.iter().map(entry).collect::<Vec<_>>(),
            })
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tracked": survey(&tracked),
                "live": live.as_ref().map(survey),
            }))?
        );
        return Ok(());
    }

    render_rr("TRACKED", &tracked);
    match &live {
        Some(l) => render_rr("LIVE (submodule gitdir)", l),
        // Not "0 entries": the gitdir could not be resolved, so the live cache
        // was never looked at. Printing an empty table would read as "seeded and
        // empty", which is a different and much more alarming fact.
        None => println!("\nLIVE (submodule gitdir)\n  could not resolve the wine gitdir -- not surveyed"),
    }
    Ok(())
}

fn render_rr(label: &str, s: &RrSurvey) {
    println!("\n{label}  {}", s.root.display());
    if s.missing {
        println!("  directory does not exist");
        return;
    }
    let unresolved = s.entries.iter().filter(|e| e.unresolved()).count();
    let stray = s.entries.iter().filter(|e| e.has_thisimage).count();
    println!("  {} entries, {unresolved} without a postimage, {stray} stray thisimage", s.entries.len());
    for e in s.entries.iter().filter(|e| e.unresolved() || e.has_thisimage) {
        // The preimage head is the only identity available: rerere records the
        // conflicted CONTENT, never the path. Offered as evidence to recognise,
        // not as a derived fact.
        let hint = e.preimage_head.first().map(String::as_str).unwrap_or("(empty preimage)");
        println!(
            "  {}  {}  {hint}",
            &e.id[..e.id.len().min(12)],
            if e.has_thisimage { "thisimage" } else { "no postimage" }
        );
    }
}

fn parse_severity(s: &str) -> Result<Severity> {
    Ok(match s {
        "blocker" => Severity::Blocker,
        "drift" => Severity::Drift,
        "hygiene" => Severity::Hygiene,
        other => anyhow::bail!("unknown severity `{other}` (blocker|drift|hygiene)"),
    })
}

fn render_status(r: &sodactl_core::Report) {
    println!("TIERS");
    println!(
        "  {:<16} {:<12} {:<16} {:>7}  {:<9} {}",
        "branch", "commit", "parent", "commits", "vs remote", "on parent tip"
    );
    for t in &r.tiers {
        // The cells live in `sodactl_core::present` so the TUI shows the exact
        // same strings -- including `remote_cell`'s diverged form, whose
        // rationale (force push, not `git push`) is documented there.
        println!(
            "  {:<16} {:<12} {:<16} {:>7}  {:<9} {}",
            t.name,
            present::short_opt(&t.oid),
            t.parent,
            t.commits_on_parent.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
            present::remote_cell(t.ahead_of_remote, t.behind_remote),
            present::parent_tip_cell(t.contains_parent_tip)
        );
    }

    println!("\nCONSUMERS");
    println!(
        "  {:<16} {:<12} {:<28} {:<12} {:<10} {}",
        "branch", "commit", "base", "soda tip", "current", "pin (gitlink)"
    );
    for c in &r.consumers {
        // `current_cell`'s "unknown" and `pin_cell`'s STAGED form carry the
        // honest-tri-state and gitlink-beside-tip reasoning; see `present`.
        println!(
            "  {:<16} {:<12} {:<28} {:<12} {:<10} {}",
            c.name,
            present::short_opt(&c.oid),
            c.base,
            c.soda_tip,
            present::current_cell(c.current),
            present::pin_cell(&c.pin)
        );
    }

    println!();
    render_findings(r);
}

fn render_findings(r: &sodactl_core::Report) {
    if r.findings.is_empty() {
        println!("no findings.");
        return;
    }
    println!(
        "FINDINGS  {} blocker, {} drift, {} hygiene",
        r.count(Severity::Blocker),
        r.count(Severity::Drift),
        r.count(Severity::Hygiene)
    );
    for f in &r.findings {
        println!("\n  [{}] {}  {}", f.severity.label(), f.code, f.subject);
        println!("      {}", f.summary);
        println!("      -> {}", f.remedy);
    }
}
