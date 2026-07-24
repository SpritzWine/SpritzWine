//! `build` / `package` / `smoke` -- the build-lane wrappers over the
//! superproject's own scripts (`./build.sh`, `packaging/package.sh`,
//! `ci/smoke.sh`).
//!
//! Core plans, the CLI execs. This module derives WHAT would run -- the
//! script, its arguments, its environment, the workspace root it runs from --
//! and refuses on the lane's own preconditions; the actual spawn lives in
//! `sodactl-cli` (its one audited build-lane site), because core is spawn-free
//! by guardrail (`no_stdout_writes` covers core, and the mcp server links it)
//! and because the lanes are long-running, user-facing processes that must
//! inherit the terminal -- capture would be a regression, not a feature.
//!
//! Preconditions, per lane:
//!
//! * All three refuse mid-replay. `build.sh:44` runs
//!   `git submodule update --init --recursive --force`, which would fight the
//!   sequencer over the wine tree; `package.sh` runs `make -C wine install`
//!   and `smoke.sh` gates on the packaged tree -- both would faithfully
//!   process a half-replayed checkout as if it were a release.
//! * `build` additionally refuses when the wine HEAD is not the
//!   superproject's effective pin (staged wins over committed -- the same
//!   precedence `git submodule update` itself applies). This is the
//!   `build.sh:44` gotcha: the forced submodule update checks out the
//!   RECORDED pointer and silently builds it, so with checkout != pin, what
//!   you see is not what builds. `--anyway` overrides with the consequence
//!   named (the PIN builds, not the checkout).
//!
//! `package.sh` has no `--version` flag: it reads `VERSION` from the
//! environment (verified against the script; `VERSION="${VERSION:-$(git
//! describe ...)}"`). The CLI's `--version <tag>` therefore lands in the
//! lane's `env`, and the rendering shows the exact `VERSION=x script`
//! invocation a human would type.

use std::fmt;

use crate::checks::Workspace;
use crate::plan::Refusal;
use crate::treestate;
use anyhow::Context as _;

/// The three lane scripts, as compiled facts. Every `plan_*` below constructs
/// its `LanePlan.script` from these constants and nowhere else, so the CLI's
/// no-push guardrail test can assert against what core actually plans instead
/// of a local copy that would keep passing while core drifted.
pub const BUILD_SCRIPT: &str = "build.sh";
pub const PACKAGE_SCRIPT: &str = "packaging/package.sh";
pub const SMOKE_SCRIPT: &str = "ci/smoke.sh";

/// A reviewable lane: what the CLI will exec, from where, with what
/// environment. Not a `plan::Plan` -- steps there are git actions with a CAS
/// story; a lane is one subprocess whose exit code passes through.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LanePlan {
    pub verb: &'static str,
    /// Script path relative to the workspace root; the CLI resolves it
    /// against the root before spawning.
    pub script: String,
    /// Extra argv after the script. Empty today for all three lanes; carried
    /// so the rendering and the exec site never grow a second shape.
    pub args: Vec<String>,
    /// Environment the lane needs beyond inheritance (`VERSION` for package).
    pub env: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// A lane-time precondition failure. Typed, `refused: `-prefixed,
/// downcastable -- exit 1, nothing ran.
#[derive(Debug)]
pub enum LaneRefusal {
    /// The wine checkout and the effective superproject pin disagree.
    HeadOffPin { head: String, pin: String, pin_source: &'static str },
}

impl fmt::Display for LaneRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneRefusal::HeadOffPin { head, pin, pin_source } => write!(
                f,
                "refused: the wine checkout is at {} but the {pin_source} pin is {} -- build.sh:44 \
                 (`git submodule update --init --recursive --force`) checks out the RECORDED pin \
                 and silently builds it, so what you see is not what builds; `git -C wine checkout` \
                 the pin (or stage the pin bump) first, or re-run with --anyway to build the pin \
                 knowingly",
                short(head),
                short(pin)
            ),
        }
    }
}

impl std::error::Error for LaneRefusal {}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// The shared lane preflight: a half-done replay in wine refuses every lane
/// (module header has the per-script consequence), and a script that does not
/// exist is "the tool cannot answer" (exit 2), not a gate -- this workspace
/// simply is not a SpritzWine superproject.
fn lane_preflight(ws: &Workspace, script: &str) -> anyhow::Result<()> {
    let gd = ws.wine.git_dir().context("resolving the wine gitdir")?;
    if let Some(fl) = treestate::in_flight(&gd) {
        return Err(anyhow::Error::new(Refusal::InFlight {
            markers: fl.markers,
            narrative: fl.narrative,
        }));
    }
    let p = ws.superproject_path.join(script);
    if !p.exists() {
        anyhow::bail!(
            "`{script}` not found at {} -- the workspace root does not look like the SpritzWine \
             superproject (the manifest's location derives the root)",
            p.display()
        );
    }
    Ok(())
}

/// Plan `build`. The pin gate compares the wine HEAD to the EFFECTIVE pin --
/// staged over committed, mirroring both `git submodule update`'s own
/// precedence and `checks/pin.rs`'s "the staged pin is what the next commit
/// will publish" rule.
pub fn plan_build(ws: &Workspace, anyway: bool) -> anyhow::Result<LanePlan> {
    lane_preflight(ws, BUILD_SCRIPT)?;
    let mut notes = Vec::new();

    let committed = ws.superproject.rev_parse_raw(&format!("HEAD:{}", ws.wine_path))?;
    let staged = ws.superproject.submodule_index_oid(&ws.wine_path)?;
    let head = ws.wine.rev_parse("HEAD")?;
    let (pin, pin_source) = match (&staged, &committed) {
        (Some(s), _) => (Some(s.clone()), "index"),
        (None, Some(c)) => (Some(c.clone()), "HEAD"),
        (None, None) => (None, "HEAD"),
    };

    match (&head, &pin) {
        (Some(h), Some(p)) if h == p => notes.push(format!(
            "the wine checkout matches the {pin_source} pin ({}) -- build.sh's forced submodule \
             update is a no-op",
            short(p)
        )),
        (Some(h), Some(p)) => {
            if !anyway {
                return Err(anyhow::Error::new(LaneRefusal::HeadOffPin {
                    head: h.clone(),
                    pin: p.clone(),
                    pin_source,
                }));
            }
            // The override records the CONSEQUENCE, not just the flag: the
            // pin builds, and the checkout the human is looking at does not.
            notes.push(format!(
                "--anyway: the checkout ({}) does NOT match the {pin_source} pin ({}); build.sh:44 \
                 will yank the checkout back and build the PIN",
                short(h),
                short(p)
            ));
        }
        // Unknown is a real answer: an unreadable pin or HEAD cannot trip a
        // gate that exists to catch a KNOWN mismatch, but silence would imply
        // the check passed. Say what could not be verified and proceed.
        (h, p) => notes.push(format!(
            "could not verify checkout-vs-pin (checkout {}, {pin_source} pin {}) -- build.sh \
             builds the recorded pin regardless",
            h.as_deref().map(short).unwrap_or_else(|| "unreadable".into()),
            p.as_deref().map(short).unwrap_or_else(|| "unreadable".into()),
        )),
    }

    Ok(LanePlan {
        verb: "build",
        script: BUILD_SCRIPT.into(),
        args: vec![],
        env: vec![],
        notes,
    })
}

/// Plan `package`. `version` becomes the lane's `VERSION` (the script's only
/// version input -- there is no flag to pass instead).
pub fn plan_package(ws: &Workspace, version: &str) -> anyhow::Result<LanePlan> {
    lane_preflight(ws, PACKAGE_SCRIPT)?;
    let mut notes =
        vec!["package.sh reads VERSION from the environment; it has no --version flag".to_string()];
    // Advisory, not a gate: VERSION is free-form in the script (its default is
    // `git describe`), but a tag-shaped argument that resolves to nothing is
    // usually a typo worth seeing before a multi-minute staging install.
    if ws.wine.rev_parse(version)?.is_none() {
        notes.push(format!(
            "`{version}` does not resolve in the wine repo -- packaging anyway (VERSION is only \
             a label), but check the spelling if a tag was meant"
        ));
    }
    Ok(LanePlan {
        verb: "package",
        script: PACKAGE_SCRIPT.into(),
        args: vec![],
        env: vec![("VERSION".into(), version.to_string())],
        notes,
    })
}

/// Plan `smoke`. Nothing to derive: the script gates on what the built +
/// packaged tree DECIDED, which is exactly why it must run unmodified.
pub fn plan_smoke(ws: &Workspace) -> anyhow::Result<LanePlan> {
    lane_preflight(ws, SMOKE_SCRIPT)?;
    Ok(LanePlan {
        verb: "smoke",
        script: SMOKE_SCRIPT.into(),
        args: vec![],
        env: vec![],
        notes: vec![],
    })
}
