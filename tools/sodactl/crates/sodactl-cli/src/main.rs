//! `sodactl` -- read-only v0: `status` and `check`.
//!
//! Rendering only. Every fact comes from `sodactl-core`, so `--json` and a
//! future MCP surface return the identical values with no second code path.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sodactl_core::checks::{Policy, Workspace};
use sodactl_core::{finding::Severity, reconcile_workspace, Manifest, RrSurvey};
use sodactl_git::{Differential, GitCli, GitGix, GitHybrid, GitRead as _};
use std::path::PathBuf;

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
    let root = manifest
        .parent()
        .and_then(|p| p.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
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
    sodactl_mcp::graph::tier_graph(&m, &d_wine).context("tier graph through the differential")?;

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

    // The manifest sits in the superproject; wine is relative to it.
    //
    // The `.filter(|p| !p.as_os_str().is_empty())` is not defensive noise, it is
    // the whole reason the DEFAULT invocation works. `Path::new("tools/sodactl.toml")
    // .parent()` is `"tools"` and *its* parent is `Some("")` -- not `None`. So the
    // `unwrap_or(".")` never fired, `root` became the empty path, and every
    // `Command::current_dir("")` failed with a bare ENOENT. `sodactl status` from
    // the repo root -- the one invocation the docs give -- crashed, and only an
    // absolute `--manifest` ever worked.
    let root = cli
        .manifest
        .parent()
        .and_then(|p| p.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
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
        Cmd::RrHealth | Cmd::Mcp | Cmd::Selfcheck { .. } => unreachable!(),
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

fn short(o: &Option<String>) -> String {
    o.as_deref().map(|s| s[..s.len().min(11)].to_string()).unwrap_or_else(|| "-".into())
}

fn render_status(r: &sodactl_core::Report) {
    println!("TIERS");
    println!(
        "  {:<16} {:<12} {:<16} {:>7}  {:<9} {}",
        "branch", "commit", "parent", "commits", "vs remote", "on parent tip"
    );
    for t in &r.tiers {
        // Ahead AND behind is a restack, and its remedy is a FORCE push -- a
        // column showing only `ahead` renders a diverged tier as merely
        // unpushed and sends the reader into a rejected `git push`.
        let remote = match (t.ahead_of_remote, t.behind_remote) {
            (Some(0), Some(0)) => "=".to_string(),
            (Some(a), Some(b)) => format!("+{a}/-{b}"),
            (Some(a), None) => format!("+{a}/?"),
            _ => "-".to_string(),
        };
        println!(
            "  {:<16} {:<12} {:<16} {:>7}  {:<9} {}",
            t.name,
            short(&t.oid),
            t.parent,
            t.commits_on_parent.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
            remote,
            match t.contains_parent_tip {
                Some(true) => "yes",
                Some(false) => "NO -- stale",
                None => "?",
            }
        );
    }

    println!("\nCONSUMERS");
    println!(
        "  {:<16} {:<12} {:<28} {:<12} {:<10} {}",
        "branch", "commit", "base", "soda tip", "current", "pin (gitlink)"
    );
    for c in &r.consumers {
        // `current` is deliberately not a boolean with a default. A consumer is
        // a CROSS-BASE replay, so patch-id equivalence against the soda tip is
        // not evidence either way -- conflict resolution and binary_resolution
        // legitimately change the diff. Without a generation ledger the honest
        // answer is "unknown", and printing "no" would be a fabrication.
        println!(
            "  {:<16} {:<12} {:<28} {:<12} {:<10} {}",
            c.name,
            short(&c.oid),
            c.base,
            c.soda_tip,
            match c.current {
                Some(true) => "yes",
                Some(false) => "STALE",
                None => "unknown",
            },
            render_pin(c)
        );
    }

    println!();
    render_findings(r);
}

/// The gitlink, shown next to the branch tip because they are different commits
/// BY DESIGN -- consumers are force-pushed and a local tip routinely runs ahead
/// of its tag. Any reader eyeballing a pin finding needs to see both or they
/// cannot tell a true positive from the tool reading the wrong subject.
fn render_pin(c: &sodactl_core::finding::ConsumerState) -> String {
    use sodactl_git::Pin;
    match &c.pin {
        None => "-".into(),
        Some(Pin::Unknown { reason }) => format!("UNKNOWN ({reason})"),
        Some(Pin::Known { committed, staged }) => {
            let head = committed.as_deref().map(shorten).unwrap_or_else(|| "none".into());
            match staged {
                Some(s) if Some(s) != committed.as_ref() => {
                    format!("{head} (STAGED {})", shorten(s))
                }
                _ => head,
            }
        }
    }
}

fn shorten(s: &str) -> String {
    s[..s.len().min(11)].to_string()
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
