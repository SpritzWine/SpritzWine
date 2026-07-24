//! `release <family> <n>` -- the composite checklist. Plan-only BY
//! CONSTRUCTION: there is no apply path, no confirm, no `GitMut` anywhere in
//! sight; the verb's whole output is the ordered list tag -> pin -> build ->
//! package -> smoke -> push with each item's status derived live.
//!
//! Status honesty: `Done`/`Pending` only where git can actually answer (tags,
//! the pin, the push list). The build/package/smoke lanes have no evidence
//! ledger yet (state digest / receipts are explicitly out of scope this
//! round), so they are `Unknown` with the exact command -- claiming Done
//! because a binary happens to exist on disk would be the counts-as-evidence
//! mistake with a fresh coat of paint.

use crate::checks::Policy;
use crate::manifest::Manifest;
use crate::verbs::{pushprint, tagverb};
use sodactl_git::{GitCli, GitRead};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Done,
    Pending,
    /// Not derivable from what this tool can see -- a real answer, not a gap.
    Unknown,
}

#[derive(Debug, serde::Serialize)]
pub struct ReleaseItem {
    pub name: &'static str,
    pub status: ItemStatus,
    pub detail: String,
    /// The command(s) that clear the item. Copy-pasteable; never executed.
    pub commands: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ReleaseReport {
    pub family: String,
    pub version: u32,
    /// The derived tag name, when the family's base is derivable.
    pub tag: Option<String>,
    pub items: Vec<ReleaseItem>,
}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Derive the checklist. Read-only; errors mean the tool could not answer
/// (an unknown family, an unreadable repo), never a gate.
pub fn plan_release(
    m: &Manifest,
    p: &Policy,
    wine: &GitCli,
    superproject: &GitCli,
    wine_path: &str,
    family: &str,
    n: u32,
) -> anyhow::Result<ReleaseReport> {
    let ft = tagverb::family_target(m, family)?;
    let tag = ft.wine_base.as_ref().map(|b| format!("{family}-{b}-{n}"));
    let target_tip = wine.rev_parse(&ft.target_ref)?;
    let mut items = Vec::new();

    // ---- 1. tag -------------------------------------------------------------
    let tag_commit = match &tag {
        None => {
            items.push(ReleaseItem {
                name: "tag",
                status: ItemStatus::Unknown,
                detail: format!(
                    "no wine base is derivable from the {} -- no `{family}-<base>-{n}` name can \
                     be constructed; tag by hand if intended",
                    ft.base_source
                ),
                commands: vec![],
            });
            None
        }
        Some(tag) => match wine.rev_parse(tag)? {
            Some(at) => {
                let moved = target_tip.as_deref().is_some_and(|t| t != at);
                items.push(ReleaseItem {
                    name: "tag",
                    status: ItemStatus::Done,
                    detail: format!(
                        "`{tag}` exists at {}{}",
                        short(&at),
                        if moved {
                            format!(
                                " -- but `{}` has since moved on; a re-release needs version {}",
                                ft.target_ref,
                                n + 1
                            )
                        } else {
                            String::new()
                        }
                    ),
                    commands: vec![],
                });
                Some(at)
            }
            None => {
                items.push(ReleaseItem {
                    name: "tag",
                    status: ItemStatus::Pending,
                    detail: format!("`{tag}` does not exist yet (target `{}`)", ft.target_ref),
                    commands: vec![format!("sodactl tag {family} {n} --apply")],
                });
                None
            }
        },
    };

    // ---- 2. pin -------------------------------------------------------------
    // Only a pinned consumer has a pin step; the soda family (and an unpinned
    // consumer) simply has none, and inventing one would put a permanent
    // pending row on every checklist.
    if m.consumer.iter().any(|c| c.name == family && c.pinned_by_superproject) {
        let committed = superproject.rev_parse_raw(&format!("HEAD:{wine_path}"))?;
        let (status, detail, commands) = match (&tag, &tag_commit) {
            (Some(tag), Some(at)) if committed.as_deref() == Some(at.as_str()) => (
                ItemStatus::Done,
                format!("the superproject gitlink records `{tag}`'s commit {}", short(at)),
                vec![],
            ),
            (Some(tag), _) => (
                ItemStatus::Pending,
                format!(
                    "the gitlink records {}, not `{tag}`{}",
                    committed.as_deref().map(short).unwrap_or_else(|| "nothing".into()),
                    if tag_commit.is_none() { " (blocked on the tag step)" } else { "" }
                ),
                // The tag push comes FIRST: `pin` probes the `.gitmodules`
                // URL and refuses a tag the remote does not serve, so the
                // checklist's canonical order must not walk straight into
                // pin's own gate. Remote derived, never invented
                // (pushprint's rule); without one, words instead.
                match pushprint::tiers_single_remote(m) {
                    Some(r) => vec![
                        format!(
                            "git -C {wine_path} push {r} {tag}   # pin refuses a tag the remote \
                             does not serve"
                        ),
                        format!("sodactl pin {tag} --apply"),
                    ],
                    None => vec![format!(
                        "sodactl pin {tag} --apply   # push the tag first (no single tier \
                         remote is derivable -- see `sodactl push`), or --offline"
                    )],
                },
            ),
            (None, _) => (
                ItemStatus::Unknown,
                "blocked on the tag step (no derivable tag name)".to_string(),
                vec![],
            ),
        };
        items.push(ReleaseItem { name: "pin", status, detail, commands });
    }

    // ---- 3-5. build / package / smoke --------------------------------------
    let head = wine.rev_parse("HEAD")?;
    let committed = superproject.rev_parse_raw(&format!("HEAD:{wine_path}"))?;
    let on_pin = match (&head, &committed) {
        (Some(h), Some(c)) => Some(h == c),
        _ => None,
    };
    items.push(ReleaseItem {
        name: "build",
        status: ItemStatus::Unknown,
        detail: format!(
            "no build evidence exists (receipts are v3); note: build.sh checks out the RECORDED \
             pin and silently builds it{}",
            match on_pin {
                Some(true) => " -- the wine checkout currently matches the pin".to_string(),
                Some(false) => format!(
                    " -- the wine checkout ({}) does NOT match the pin ({}); what you see is not \
                     what builds",
                    head.as_deref().map(short).unwrap_or_default(),
                    committed.as_deref().map(short).unwrap_or_default()
                ),
                None => String::new(),
            }
        ),
        // The wrapper, not the raw script: `sodactl build` carries the
        // checkout-vs-pin gate this very detail line warns about.
        commands: vec!["sodactl build".to_string()],
    });
    items.push(ReleaseItem {
        name: "package",
        status: ItemStatus::Unknown,
        detail: "no packaging evidence exists (receipts are v3)".to_string(),
        // The wrapper again, and not by taste: the raw script has NO
        // `--version` flag (it reads `VERSION` from the environment), so the
        // obvious raw invocation silently packages a `git describe` label
        // instead of the tag. `sodactl package --version` routes the flag
        // into the env the script actually reads.
        commands: vec![match &tag {
            Some(tag) => format!("sodactl package --version {tag}"),
            None => "sodactl package --version <tag>".to_string(),
        }],
    });
    items.push(ReleaseItem {
        name: "smoke",
        status: ItemStatus::Unknown,
        detail: "no smoke evidence exists (receipts are v3)".to_string(),
        commands: vec!["sodactl smoke".to_string()],
    });

    // ---- 6. push ------------------------------------------------------------
    let pr = pushprint::plan_push(m, p, wine, superproject, wine_path)?;
    if pr.items.is_empty() {
        items.push(ReleaseItem {
            name: "push",
            status: ItemStatus::Done,
            detail: format!(
                "every derived ref matches its remote{}",
                if pr.notes.is_empty() {
                    String::new()
                } else {
                    format!(" ({} note(s) -- see `sodactl push`)", pr.notes.len())
                }
            ),
            commands: vec![],
        });
    } else {
        items.push(ReleaseItem {
            name: "push",
            status: ItemStatus::Pending,
            detail: format!(
                "{} command(s) to run, wine first{}",
                pr.items.len(),
                if pr.notes.is_empty() {
                    String::new()
                } else {
                    format!("; {} note(s) -- see `sodactl push`", pr.notes.len())
                }
            ),
            commands: pr.items.into_iter().map(|i| i.command).collect(),
        });
    }

    Ok(ReleaseReport { family: family.to_string(), version: n, tag, items })
}
