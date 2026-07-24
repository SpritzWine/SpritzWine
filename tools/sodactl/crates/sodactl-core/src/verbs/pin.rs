//! `pin` -- move the superproject's wine pointer to a TAG, atomically.
//!
//! The verb encodes the three disciplines the pin checks exist to enforce,
//! so a pin bump done through it cannot produce the findings:
//!
//! * A TAG, never a SHA: consumers are force-pushed, so a raw pinned SHA
//!   falls out of the branch on the next pour and every clone dies with
//!   "Fetched in submodule path 'wine', but it did not contain <sha>".
//! * The tag must deref to a PINNED consumer's tip -- `git add wine` while
//!   the checkout sits on a tier is the catastrophic-and-silent
//!   `GITLINK_NOT_A_CONSUMER` mistake, pre-refused here.
//! * The gitlink and `.gitmodules branch=` move in the SAME commit
//!   (`GITMODULES_BRANCH_MISMATCH`'s requirement), and `branch=` gets the TAG
//!   name -- the live superproject's own convention (`branch =
//!   spritzwine-11.13-3`), and the only value that keeps resolving to the pin
//!   after the consumer branch moves on.
//!
//! The remote probe (`ls-remote` against the URL `.gitmodules` records --
//! literally what CI and every fresh clone will fetch) runs at PLAN time:
//! catching an unpushed tag before the commit exists beats CI catching it
//! after. `--offline` skips it, recorded in the plan notes, because the probe
//! is the one part of the plan that needs a network.

use std::fmt;

use crate::manifest::Manifest;
use crate::plan::{Action, Plan, PlannedValue, RefMutation, RepoSide, Step};
use anyhow::Context as _;
use sodactl_git::{GitCli, GitRead, StatusKind};

/// What the plan-time remote probe concluded.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinProbe {
    /// The remote advertises the tag at the same commit.
    Verified { url: String },
    /// `--offline`: nothing was asked; the plan notes say so.
    SkippedOffline,
}

/// The reviewable pin: the `Plan` plus the derivation facts.
#[derive(Debug, serde::Serialize)]
pub struct PinPlan {
    pub plan: Plan,
    pub tag: String,
    /// The commit the tag derefs to -- what the gitlink will record.
    pub commit: String,
    /// The pinned consumer whose tip the tag names.
    pub consumer: String,
    pub probe: PinProbe,
}

/// The pin-time preconditions. Typed, `refused: `-prefixed, downcastable --
/// exit 1.
#[derive(Debug)]
pub enum PinRefusal {
    /// No `refs/tags/<tag>` in the wine repo. A branch name resolving is not
    /// good enough -- that is the raw-SHA-with-extra-steps failure.
    NoSuchTag { tag: String },
    /// The tag derefs to a commit that is no pinned consumer's tip.
    NotAPinnedConsumerTip { tag: String, commit: String, detail: String },
    /// The remote `.gitmodules` names does not advertise the tag: CI's
    /// `submodule update` would fail on exactly this. `push_hint` is derived
    /// at refusal time (`pushprint::tiers_single_remote` + the real wine
    /// path) -- never an invented remote name.
    RemoteMissingTag { tag: String, url: String, push_hint: String },
    /// The remote has the tag at a DIFFERENT commit: someone's tag moved, and
    /// pinning either side without sorting that out ships ambiguity.
    RemoteTagDiffers { tag: String, url: String, remote: String, local: String },
    /// Other paths are staged in the superproject: `git commit` commits the
    /// index, so they would be swept invisibly into the pin commit.
    SuperprojectStaged { paths: Vec<String> },
}

impl fmt::Display for PinRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PinRefusal::NoSuchTag { tag } => write!(
                f,
                "refused: no tag `{tag}` in the wine repo -- pin takes a TAG, never a SHA or a \
                 branch (consumers are force-pushed; anything else stops being fetchable on the \
                 next pour). Cut it first (`sodactl tag <family> <n> --apply`) and push it -- \
                 pin verifies the remote serves the tag before committing the pointer"
            ),
            PinRefusal::NotAPinnedConsumerTip { tag, commit, detail } => write!(
                f,
                "refused: `{tag}` derefs to {commit}, which is no pinned consumer's tip{detail} \
                 -- pinning it would commit a tree no release is built from"
            ),
            PinRefusal::RemoteMissingTag { tag, url, push_hint } => write!(
                f,
                "refused: {url} does not advertise `{tag}` -- CI and every fresh clone fetch \
                 exactly that remote, so the pin would fail with \"did not contain\"; push the \
                 tag first{push_hint} (or re-run with --offline to pin anyway and push before \
                 the superproject)"
            ),
            PinRefusal::RemoteTagDiffers { tag, url, remote, local } => write!(
                f,
                "refused: {url} advertises `{tag}` at {remote} but it is {local} here -- a \
                 published tag moved somewhere; sort out which side is right before pinning \
                 either"
            ),
            PinRefusal::SuperprojectStaged { paths } => write!(
                f,
                "refused: {} other path(s) staged in the superproject ({}{}) -- `git commit` \
                 commits the index, so they would be swept into the pin commit; commit or \
                 unstage them first",
                paths.len(),
                paths.iter().take(4).cloned().collect::<Vec<_>>().join(", "),
                if paths.len() > 4 { ", …" } else { "" }
            ),
        }
    }
}

impl std::error::Error for PinRefusal {}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

/// Plan the pin of `tag`.
///
/// `wine` is the binary-backed handle because the remote probe
/// (`ls_remote_tag`) is inherent on `GitCli` -- see its doc for why a network
/// probe stays off the `GitRead` trait.
pub fn plan_pin(
    m: &Manifest,
    wine: &GitCli,
    superproject: &dyn GitRead,
    wine_path: &str,
    tag: &str,
    offline: bool,
) -> anyhow::Result<PinPlan> {
    // The full ref, raw: `rev_parse(tag)` alone would happily resolve a
    // branch of the same name, which is the exact mistake this verb refuses.
    if wine.rev_parse_raw(&format!("refs/tags/{tag}"))?.is_none() {
        return Err(anyhow::Error::new(PinRefusal::NoSuchTag { tag: tag.to_string() }));
    }
    let commit = wine
        .rev_parse(&format!("refs/tags/{tag}"))?
        .with_context(|| format!("`refs/tags/{tag}` does not peel to a commit"))?;

    let consumer = {
        let mut found = None;
        for c in m.consumer.iter().filter(|c| c.pinned_by_superproject) {
            if wine.rev_parse(&c.name)?.as_deref() == Some(commit.as_str()) {
                found = Some(c.name.clone());
                break;
            }
        }
        match found {
            Some(c) => c,
            None => {
                // Name the specific mistake where possible: an unpinned
                // consumer and a tier want different next moves.
                let mut detail = String::new();
                for c in m.consumer.iter().filter(|c| !c.pinned_by_superproject) {
                    if wine.rev_parse(&c.name)?.as_deref() == Some(commit.as_str()) {
                        detail = format!(
                            " (it is `{}`'s tip, but that consumer is not pinned_by_superproject)",
                            c.name
                        );
                    }
                }
                if detail.is_empty() {
                    for t in &m.tier {
                        if wine.rev_parse(&t.name)?.as_deref() == Some(commit.as_str()) {
                            detail = format!(
                                " (it is the TIER `{}`'s tip -- soda without wine)",
                                t.name
                            );
                        }
                    }
                }
                return Err(anyhow::Error::new(PinRefusal::NotAPinnedConsumerTip {
                    tag: tag.to_string(),
                    commit: short(&commit),
                    detail,
                }));
            }
        }
    };

    // The probe target is the URL `.gitmodules` records -- what `git
    // submodule update` in CI and in every fresh clone will actually fetch,
    // which makes it the exact failure being pre-detected (not whichever
    // remote name this checkout happens to configure).
    let url_key = format!("submodule.{wine_path}.url");
    let probe = if offline {
        PinProbe::SkippedOffline
    } else {
        let url = superproject
            .config_file_get(".gitmodules", &url_key)?
            .with_context(|| format!("`.gitmodules` records no `{url_key}` -- nothing says where the submodule is fetched from"))?;
        match wine.ls_remote_tag(&url, tag)? {
            None => {
                // Derived, never invented (pushprint's rule): a single tier
                // remote yields the exact command; otherwise words.
                let push_hint = match crate::verbs::pushprint::tiers_single_remote(m) {
                    Some(r) => format!(": `git -C {wine_path} push {r} {tag}`"),
                    None => " (the tiers name no single remote -- `sodactl push` derives the \
                             command once the tag exists locally)"
                        .to_string(),
                };
                return Err(anyhow::Error::new(PinRefusal::RemoteMissingTag {
                    tag: tag.to_string(),
                    url,
                    push_hint,
                }))
            }
            Some(remote) if remote != commit => {
                return Err(anyhow::Error::new(PinRefusal::RemoteTagDiffers {
                    tag: tag.to_string(),
                    url,
                    remote: short(&remote),
                    local: short(&commit),
                }))
            }
            Some(_) => PinProbe::Verified { url },
        }
    };

    // Staged-elsewhere refusal: `commit -m` commits the whole index. The pin's
    // own two paths are exempt; unmerged entries refuse unconditionally (a
    // half-resolved superproject is nothing to commit on top of). UNSTAGED
    // modifications are deliberately fine -- they stay out of the commit.
    let staged: Vec<String> = superproject
        .status(false)?
        .into_iter()
        .filter(|e| {
            e.kind == StatusKind::Unmerged
                || (matches!(e.kind, StatusKind::Tracked | StatusKind::Renamed)
                    && !e.xy.starts_with('.')
                    && e.path != wine_path
                    && e.path != ".gitmodules")
        })
        .map(|e| e.path)
        .collect();
    if !staged.is_empty() {
        return Err(anyhow::Error::new(PinRefusal::SuperprojectStaged { paths: staged }));
    }

    let head_branch = superproject
        .head_branch()?
        .context("the superproject HEAD is detached -- a pin commit made here would dangle; check a branch out first")?;
    let head_oid = superproject
        .rev_parse_raw(&format!("refs/heads/{head_branch}"))?
        .with_context(|| format!("refs/heads/{head_branch} does not resolve"))?;

    // The honest no-op: already pinned, in all three places a pin lives
    // (committed gitlink, `.gitmodules branch=`, and nothing DIFFERENT staged
    // -- a stale staged gitlink means the index still needs the repair).
    let committed = superproject.rev_parse_raw(&format!("HEAD:{wine_path}"))?;
    let staged = superproject.submodule_index_oid(wine_path)?;
    let branch_key = format!("submodule.{wine_path}.branch");
    let recorded_branch = superproject.config_file_get(".gitmodules", &branch_key)?;
    if committed.as_deref() == Some(commit.as_str())
        && recorded_branch.as_deref() == Some(tag)
        && staged.as_deref().is_none_or(|s| s == commit)
    {
        return Ok(PinPlan {
            plan: Plan::new(
                "pin",
                vec![],
                vec![],
                vec![],
                vec![format!(
                    "the superproject already pins `{tag}` ({}) with `.gitmodules {branch_key}` \
                     matching -- nothing to commit",
                    short(&commit)
                )],
            ),
            tag: tag.to_string(),
            commit,
            consumer,
            probe,
        });
    }

    let mut notes = Vec::new();
    match &probe {
        PinProbe::Verified { url } => {
            notes.push(format!("`{tag}` verified fetchable at {} on {url}", short(&commit)))
        }
        PinProbe::SkippedOffline => notes.push(
            "--offline: the remote was NOT asked whether it advertises the tag -- push it before \
             the superproject or CI fails with \"did not contain\""
                .to_string(),
        ),
    }
    notes.push(format!("the wine checkout detaches at `{tag}` (that is what `git add {wine_path}` records)"));
    if let Some(c) = &committed {
        if c != &commit {
            notes.push(format!("gitlink moves {} -> {}", short(c), short(&commit)));
        }
    }

    let steps = vec![
        Step::new(
            format!("check the wine submodule out at `{tag}` (detached)"),
            RepoSide::Wine,
            Action::CheckoutDetach { rev: tag.to_string() },
        ),
        Step::new(
            format!("record `{tag}` as the tracked ref in .gitmodules"),
            RepoSide::Superproject,
            Action::ConfigFileSet {
                file: ".gitmodules".into(),
                key: branch_key,
                value: tag.to_string(),
            },
        ),
        Step::new(
            "stage the gitlink and .gitmodules together",
            RepoSide::Superproject,
            Action::Add { paths: vec![wine_path.to_string(), ".gitmodules".into()] },
        ),
        Step::new(
            "commit the pin (one commit, both halves)",
            RepoSide::Superproject,
            Action::Commit { message: format!("{consumer}: pin the regenerated consumer ({tag})") },
        ),
    ];
    let mutations = vec![RefMutation {
        repo: RepoSide::Superproject,
        refname: format!("refs/heads/{head_branch}"),
        expected_before: Some(head_oid),
        after: PlannedValue::Unknown {
            reason: "the pin commit mints a new oid; unknowable until it exists".into(),
        },
    }];

    let plan = Plan::new("pin", steps, mutations, vec![], notes);
    Ok(PinPlan { plan, tag: tag.to_string(), commit, consumer, probe })
}

/// Run one pin plan through the shared engine, then verify both halves landed
/// in the new HEAD. No auto-restore -- the superproject only gained a commit,
/// and the error names the one command that removes it.
pub fn apply_pin(
    pp: &PinPlan,
    ctx: &crate::plan::ApplyCtx,
    wine_path: &str,
) -> anyhow::Result<crate::plan::ApplyOutcome> {
    let outcome = crate::plan::apply(&pp.plan, ctx).map_err(anyhow::Error::new)?;
    if pp.plan.steps.is_empty() {
        return Ok(outcome);
    }
    let gitlink = ctx.superproject_read.rev_parse_raw(&format!("HEAD:{wine_path}"))?;
    let branch = ctx
        .superproject_read
        .config_file_get(".gitmodules", &format!("submodule.{wine_path}.branch"))?;
    if gitlink.as_deref() != Some(pp.commit.as_str()) || branch.as_deref() != Some(pp.tag.as_str()) {
        anyhow::bail!(
            "pin postcondition FAILED: HEAD records gitlink {} / branch= {:?}, expected {} / \
             `{}` -- inspect the commit (`git show HEAD`) and drop it with `git reset --hard \
             HEAD~1` if it is wrong",
            gitlink.as_deref().map(short).unwrap_or_else(|| "<absent>".into()),
            branch,
            short(&pp.commit),
            pp.tag
        );
    }
    Ok(outcome)
}
