//! The refresh worker: one std::thread, jobs in, outcomes out.
//!
//! The threading model in one paragraph: the UI thread owns the terminal and
//! never blocks on git; this thread owns git and never touches the terminal.
//! `mpsc` in both directions, a generation counter stamped on every job and
//! echoed on its outcome. The counter is the ONLY stale-detection there is --
//! the app applies an outcome iff it carries the newest generation issued
//! (`App::apply_outcome`), so a slow reconcile overtaken by a fresh `r` can
//! never paint older state over newer.

use crate::gitlog::{self, Context, LogView};
use anyhow::{Context as _, Result};
use sodactl_core::checks::{Policy, Workspace};
use sodactl_core::graph::{self, Dag};
use sodactl_core::{reconcile_workspace, Manifest, Report};
use sodactl_git::GitHybrid;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};

/// Everything one refresh derived. Moved into the app, never cloned --
/// `Report` is a pile of strings with exactly one consumer.
pub struct Snapshot {
    pub report: Report,
    pub dag: Dag,
    /// The manifest this snapshot was derived FROM, moved in whole: the graph
    /// pane needs the parts the report does not restate (stitches, the home
    /// base's prose), and the app needs it to classify a finding's subject as
    /// a modelled ref on Enter. Carrying it beats re-reading the file on the
    /// UI thread, which could disagree with what the worker just derived.
    pub manifest: Manifest,
    /// The commit chain for this snapshot's `Job::log_ctx`. `Err` is
    /// pane-local by design: a broken log range (deleted branch, bad
    /// manifest edit) must not take the findings and the graph down with it
    /// -- the pane renders the error, the rest of the dashboard stays live.
    pub log: std::result::Result<LogView, String>,
}

/// One refresh request, app -> worker.
pub struct Job {
    /// Monotonically increasing, issued by the app, echoed on the `Outcome`.
    pub gen: u64,
    /// Which commit-chain context to derive beside the report. Rides on the
    /// job so a context switch is a refresh like any other: same generation
    /// discipline, same coalescing, no second channel to race.
    pub log_ctx: Context,
}

/// One refresh answer, worker -> app.
pub struct Outcome {
    pub gen: u64,
    pub result: Result<Snapshot>,
}

/// Spawn the worker. Generic over the runner so the tests can drive the
/// channel and generation contract with a closure and no repository on disk;
/// production passes `run_once`.
pub fn spawn<F>(mut runner: F) -> (Sender<Job>, Receiver<Outcome>)
where
    F: FnMut(&Job) -> Result<Snapshot> + Send + 'static,
{
    let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Outcome>();
    std::thread::spawn(move || {
        while let Ok(mut job) = job_rx.recv() {
            // Coalesce a backlog to the newest request. The app would discard
            // the superseded outcomes anyway (stale by generation), so running
            // them is pure wasted git time: mashing `r` during a slow
            // reconcile must cost one reconcile, not N.
            while let Ok(newer) = job_rx.try_recv() {
                job = newer;
            }
            let result = runner(&job);
            if out_tx.send(Outcome { gen: job.gen, result }).is_err() {
                // The app hung up; so did the reason to work.
                break;
            }
        }
        // job_rx closed: the app dropped its Sender on quit. Fall off the
        // thread; any reconcile that was mid-flight was read-only anyway.
    });
    (job_tx, out_rx)
}

/// One full derivation from disk: manifest, reconcile, tier graph, commit
/// chain.
///
/// Re-reads the manifest every time. The startup parse in `lib::run` is only
/// the readable-failure gate; git is re-derived per refresh because it is the
/// live authority, and a hand-edited sodactl.toml deserves the same rule --
/// `r` must show the edited topology, not the one from startup.
///
/// Builds its OWN `GitHybrid` instances rather than receiving them:
/// `Workspace` (checks/mod.rs:53) borrows its git handles, so handles made on
/// the UI thread would have to be borrowed across the spawn -- and the borrow
/// checker is right to refuse, since this thread outlives any stack frame that
/// could own them. Construction is a path copy; nothing is lost by repeating
/// it.
pub fn run_once(manifest_path: &Path, log_ctx: &Context) -> Result<Snapshot> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("reading manifest {}", manifest_path.display()))?;
    let m = Manifest::parse(&text).context("parsing manifest")?;
    let root = sodactl_core::workspace_root(manifest_path);
    let wine = root.join(&m.repo.wine);
    let policy = Policy::from_manifest(&m);

    let git = GitHybrid::new(&wine);
    let super_git = GitHybrid::new(&root);
    let ws = Workspace {
        superproject: &super_git,
        wine: &git,
        superproject_path: root.clone(),
        wine_path: m.repo.wine.clone(),
    };
    let report = reconcile_workspace(&m, &git, &ws, &policy)?;
    let dag = graph::tier_graph(&m, &git)?;
    // After the reconcile, from the same manifest read -- the log describes
    // the same instant the report does, or as close as two git walks get.
    let log = gitlog::run(&m, &wine, log_ctx);
    Ok(Snapshot { report, dag, manifest: m, log })
}

/// A minimal, structurally-valid snapshot for tests across the crate: empty
/// report, empty dag, a manifest modelling one of each ref kind (the app's
/// Enter wiring and the graph pane's groups need them), and a log that says
/// out loud it was never derived (`Err`, the honest arm -- fabricating an
/// empty `Ok` view would render as "no commits in this range", which is a
/// claim about a range nobody walked).
#[cfg(test)]
pub(crate) fn test_snapshot() -> Snapshot {
    let manifest = Manifest::parse(
        r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "1bb0da91a5e1234567890abcdef1234567890abc"
describes = "test fixture home base"
derivation = "fixture"

[[tier]]
name = "soda-build"
parent = "$home_base"

[[tier]]
name = "soda-core"
parent = "soda-build"

[[tier]]
name = "soda-ntdll"
parent = "soda-core"

[[stitch]]
name = "soda-all"
base_tier = "soda-core"
order = ["soda-ntdll"]

[[consumer]]
name = "spritzwine"
base = "wine-11.13"
soda_tip = "soda-all"
"#,
    )
    .expect("the fixture manifest must parse");
    Snapshot {
        report: Report::default(),
        dag: Dag { nodes: vec![], edges: vec![], restack_order: vec![] },
        manifest,
        log: Err("log not derived in this fixture".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The channel contract, driven by a closure runner -- no repo, no
    /// terminal: the generation echoes back untouched, and an Err crosses the
    /// channel as an Err instead of killing the thread.
    #[test]
    fn outcomes_echo_generation_and_carry_errors() {
        let (jobs, outcomes) = spawn(|job: &Job| {
            if job.gen == 1 {
                anyhow::bail!("no repository")
            }
            Ok(test_snapshot())
        });
        jobs.send(Job { gen: 1, log_ctx: Context::Overview }).unwrap();
        let o = outcomes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(o.gen, 1);
        assert!(o.result.is_err(), "the runner's Err must survive the channel");

        jobs.send(Job { gen: 2, log_ctx: Context::Overview }).unwrap();
        let o = outcomes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(o.gen, 2);
        assert!(o.result.is_ok(), "the worker must outlive a failed job");
    }
}
