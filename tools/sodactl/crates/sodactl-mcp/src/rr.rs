//! Health of the tracked rerere cache.
//!
//! `tools/rr-cache` is the committed memory of every conflict resolution the
//! replay has ever needed. It is maintained by `cp -a`, which merges and never
//! prunes, and `git rerere gc` has never been run against it -- so it rots in
//! two specific, detectable ways:
//!
//!   * an entry with a `preimage` and no `postimage` is a conflict that was
//!     RECORDED but never RESOLVED. rerere replays nothing for it, so it stops
//!     every future replay forever, while the cache's existence implies the
//!     opposite.
//!   * a `thisimage` is transient in-merge state. Its presence in a committed
//!     cache means the directory was copied out of a tree mid-conflict.
//!
//! Pure filesystem, no git. Deliberately reads the SUPERPROJECT's tracked copy
//! rather than `wine/.git/rr-cache`: the latter is one developer's scratch state,
//! and reporting it would describe this machine instead of the project.

use anyhow::Result;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct RrHealth {
    pub path: String,
    pub present: bool,
    pub total: usize,
    /// Recorded-but-unresolved: guaranteed re-prompts.
    pub without_postimage: usize,
    pub with_stray_thisimage: usize,
    pub entries: Vec<RrEntry>,
    /// Emitted in the same vocabulary as `sodactl check`, so a caller can merge
    /// these into a findings list without a second shape to handle.
    pub findings: Vec<RrFinding>,
}

#[derive(Debug, Serialize)]
pub struct RrEntry {
    /// The conflict-content hash rerere keys on. Not a commit id.
    pub id: String,
    pub has_preimage: bool,
    pub has_postimage: bool,
    pub has_thisimage: bool,
    /// True when this entry will stop a replay: recorded, never resolved.
    pub will_reprompt: bool,
}

#[derive(Debug, Serialize)]
pub struct RrFinding {
    pub code: &'static str,
    pub severity: &'static str,
    pub subject: String,
    pub summary: String,
    pub remedy: String,
}

pub fn rr_health(dir: &Path) -> Result<RrHealth> {
    let path = dir.display().to_string();

    if !dir.is_dir() {
        return Ok(RrHealth {
            present: false,
            total: 0,
            without_postimage: 0,
            with_stray_thisimage: 0,
            entries: vec![],
            findings: vec![RrFinding {
                code: "RR_CACHE_MISSING",
                severity: "drift",
                subject: path.clone(),
                summary: "no tracked rerere cache -- every replay re-asks every conflict"
                    .to_string(),
                remedy: "restore tools/rr-cache from git history before regenerating a consumer"
                    .to_string(),
            }],
            path,
        });
    }

    let mut entries = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if !e.file_type()?.is_dir() {
            continue;
        }
        let p = e.path();
        let has_preimage = p.join("preimage").exists();
        let has_postimage = p.join("postimage").exists();
        let has_thisimage = p.join("thisimage").exists();
        entries.push(RrEntry {
            id: e.file_name().to_string_lossy().into_owned(),
            has_preimage,
            has_postimage,
            has_thisimage,
            will_reprompt: has_preimage && !has_postimage,
        });
    }
    // Directory order is filesystem-dependent; sorting makes two runs on two
    // machines diffable, which matters the moment anyone snapshots this output.
    entries.sort_by(|a, b| a.id.cmp(&b.id));

    let without_postimage = entries.iter().filter(|e| e.will_reprompt).count();
    let with_stray_thisimage = entries.iter().filter(|e| e.has_thisimage).count();

    let mut findings = Vec::new();
    // One finding per entry rather than one aggregate: each needs an independent
    // resolve-or-prune decision, and a single "5 entries are bad" line is a thing
    // you read and do nothing about.
    for e in entries.iter().filter(|e| e.will_reprompt) {
        findings.push(RrFinding {
            code: "RR_NO_POSTIMAGE",
            severity: "hygiene",
            subject: e.id.clone(),
            summary: "conflict recorded but never resolved -- rerere has nothing to replay, so \
this stops every future replay"
                .to_string(),
            remedy: "reproduce and resolve the conflict so a postimage is recorded, or prune the \
entry".to_string(),
        });
    }
    for e in entries.iter().filter(|e| e.has_thisimage) {
        findings.push(RrFinding {
            code: "RR_STRAY_THISIMAGE",
            severity: "hygiene",
            subject: e.id.clone(),
            summary: "transient in-merge state was committed -- the cache was captured from a \
tree that was mid-conflict"
                .to_string(),
            remedy: "delete the thisimage file and commit".to_string(),
        });
    }

    Ok(RrHealth {
        path,
        present: true,
        total: entries.len(),
        without_postimage,
        with_stray_thisimage,
        entries,
        findings,
    })
}
