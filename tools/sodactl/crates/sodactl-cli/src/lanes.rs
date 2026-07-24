//! The build-lane exec: the ONLY subprocess spawn in this crate.
//!
//! Core plans the lane (`sodactl_core::verbs::buildwrap` -- script, args,
//! env, preconditions); this module runs it. The split is a guardrail, not
//! taste: core is spawn-free (the mcp server links it and its stdout is the
//! protocol stream), and the guardrail test in `verbs.rs` enumerates every
//! `Command::new` in this crate against exactly this site.
//!
//! Stdio is INHERITED -- `Command::status()` leaves stdin/stdout/stderr on
//! the terminal -- because the lanes are long-running, user-facing scripts
//! (a Wine build's compile scroll, smoke's prefix boot). Capturing them to
//! re-print would buffer a half-hour build behind a silent cursor.
//!
//! The exit code passes through verbatim, deliberately OUTSIDE the CLI's
//! 0/1/2 contract: the lane's own exit code is the lane's own contract
//! (smoke.sh's failure taxonomy, make's 2), and collapsing it into "gate"
//! would erase what the script just said. `sodactl`'s own codes apply only
//! until the moment the script runs.

use anyhow::{Context as _, Result};
use sodactl_core::verbs::buildwrap::LanePlan;
use std::path::Path;
use std::process::Command;

/// Spawn the lane from the workspace root and wait. Returns the child's exit
/// code for the caller to pass through; a spawn failure or signal death is an
/// error (exit 2 -- the tool could not answer, distinct from the script
/// answering "failed").
pub fn exec_lane(plan: &LanePlan, root: &Path) -> Result<i32> {
    // AUDITED BUILD-LANE CONSTRUCTOR: the only commands this crate runs are
    // the superproject's own lane scripts (build.sh / packaging/package.sh /
    // ci/smoke.sh), resolved against the workspace root core derived, with
    // core's planned env on top of inheritance. The guardrail test greps
    // every `Command::new` in this crate against this site. Deliberately no
    // `LC_ALL=C`: nothing is captured or parsed -- the terminal is the
    // script's, and pinning the locale would change what the human sees.
    let script = root.join(&plan.script);
    let mut cmd = Command::new(&script);
    cmd.current_dir(root).args(&plan.args);
    for (k, v) in &plan.env {
        cmd.env(k, v);
    }
    let status = cmd
        .status()
        .with_context(|| format!("spawning {} in {}", script.display(), root.display()))?;
    status.code().ok_or_else(|| {
        anyhow::anyhow!(
            "`{}` was killed by a signal before exiting -- no exit code to pass through",
            plan.script
        )
    })
}
