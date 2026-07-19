//! The superproject pin cluster.
//!
//! This is where the sharpest failure in the model lives, and it is a
//! **wrong-subject** error rather than a wrong-primitive one: `reconcile()`
//! asks its tag question about the consumer's *branch tip*, but the thing that
//! breaks is the superproject's *gitlink*. BRANCHING.md states the operational
//! fact that guarantees they differ -- "your local `spritzvalve` and
//! `spritzwine` branch tips will routinely sit ahead of their tags, a pour that
//! has not passed the gate yet". Reading the tip therefore produces a false
//! Blocker whose remedy is "tag this untested pour and pin it", which is worse
//! than no finding at all.
//!
//! There are THREE pins, not one, and they are three separate facts:
//!   * `HEAD:wine`        -- what is committed
//!   * the index gitlink  -- what is STAGED, one `git commit` from being real
//!   * `wine`'s own HEAD  -- what the checkout is actually on
//! Every check below says which one it fired on.

use super::{Oid, Workspace};
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;

pub fn check(m: &Manifest, ws: &Workspace) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();

    // Raw, not peeled: the gitlink names a commit that lives in the submodule.
    let committed = ws.superproject.rev_parse_raw(&format!("HEAD:{}", ws.wine_path))?;
    let staged = ws.superproject.submodule_index_oid(&ws.wine_path)?;
    let checkout = ws.wine.rev_parse("HEAD")?;

    // The staged pin is what the next commit will publish, so it is the one to
    // judge; fall back to the committed one when nothing is staged.
    let (pin, source) = match (&staged, &committed) {
        (Some(s), _) => (Some(s.clone()), "index"),
        (None, Some(c)) => (Some(c.clone()), "HEAD"),
        (None, None) => (None, "HEAD"),
    };

    // When the judged pin is STAGED and differs from HEAD's, every finding below
    // describes a state that does not exist yet.
    //
    // This is a severity-model fix, not a cosmetic one. On the live repo the
    // committed gitlink is fully clean -- tagged, pushed, matching `.gitmodules`
    // -- and only the index carries the new pin. Judging the index
    // unconditionally therefore raised three BLOCKERs at a developer midway
    // through a perfectly correct pin bump, describing a state no CI runner can
    // ever observe (CI checks out HEAD, where index == HEAD). Three blockers for
    // "you have not finished typing yet" is how a tool gets its severity column
    // ignored. `PIN_STAGED_UNCOMMITTED` already states the real fact at the
    // honest severity; these become conditional statements about what
    // committing WOULD do.
    let uncommitted = matches!((&staged, &committed), (Some(s), Some(c)) if s != c);
    let sev = |blocking: Severity| if uncommitted { Severity::Drift } else { blocking };
    let mood = if uncommitted { "committing would leave " } else { "" };

    // A4 -- nothing is broken yet, but a `git commit` away from the pin
    // findings below landing for real. Its remedy is "unstage", which no other
    // finding suggests, so it earns its own code.
    if let (Some(s), Some(c)) = (&staged, &committed) {
        if s != c {
            f.push(Finding::new(
                "PIN_STAGED_UNCOMMITTED",
                Severity::Drift,
                &ws.wine_path,
                format!("the `{}` gitlink is staged at {} but HEAD still records {} -- the pin bump is uncommitted", ws.wine_path, short(s), short(c)),
                format!("git restore --staged {}   # or tag the new tip and update .gitmodules branch= in the SAME commit", ws.wine_path),
            ));
        }
    }

    let Some(pin) = pin else {
        return Ok(f);
    };

    // A1 -- Blocker. Consumers are force-pushed: an untagged pinned SHA falls
    // out of the branch on the next pour, the remote stops advertising it, and
    // `git submodule update` dies with "Fetched in submodule path 'wine', but
    // it did not contain <sha>". Silent until a fresh clone or CI, which is
    // precisely the worst moment to discover it.
    let tags_here = ws.wine.tags_pointing_at(&pin)?;
    if tags_here.is_empty() {
        f.push(Finding::new(
            "PIN_UNTAGGED",
            sev(Severity::Blocker),
            &ws.wine_path,
            format!(
                "{mood}the {source} gitlink {} carrying no tag -- consumers are force-pushed, so this SHA stops being fetchable on the next pour",
                short(&pin)
            ),
            format!(
                "git -C {w} tag <component>-<base>-<n> {p} && git -C {w} push codeberg <component>-<base>-<n>   # then pin the TAG, and update .gitmodules branch= in the SAME commit",
                w = ws.wine_path,
                p = short(&pin)
            ),
        ));
    }

    // A2 -- the same failure caught one step earlier and without network. CI
    // probes Codeberg's API after the commit; this catches it before.
    //
    // Offline, so a stale tracking ref can produce a false negative. Say so in
    // the message rather than claiming more than the data supports.
    let remote_refs = ws.wine.branches_containing(&pin, true)?;
    if remote_refs.is_empty() {
        f.push(Finding::new(
            "PIN_NOT_ON_REMOTE",
            sev(Severity::Blocker),
            &ws.wine_path,
            format!(
                "{mood}the {source} gitlink {} on no remote-tracking branch -- it exists only on this disk (offline check: a stale tracking ref would show the same)",
                short(&pin)
            ),
            // Pushing the BRANCH does not durably fix this, and saying so was
            // the old remedy's defect. This finding's own mechanism is that
            // consumers are force-pushed, so a branch push makes the SHA
            // reachable only until the next pour, which then reintroduces the
            // identical failure. The tag is what makes it permanent -- same
            // remedy `PIN_UNTAGGED` gives, because it is the same fix.
            format!(
                "git -C {w} tag <component>-<base>-<n> {p} && git -C {w} push codeberg <component>-<base>-<n>   # push the TAG, not just the branch: a branch push is undone by the next pour. Florian pushes; submodule first",
                w = ws.wine_path,
                p = short(&pin)
            ),
        ));
    }

    // A3 -- `.gitmodules branch =` and the gitlink must name the same commit,
    // and BRANCHING.md requires the `branch=` edit to land in the SAME commit
    // as the pin bump. A mismatch means `submodule update --remote` pulls a
    // different tree than the one that was built and tested.
    //
    // Parsed by `git config -f`, not by regex: `.gitmodules` is git-config
    // format and hand-parsing gets continuations and quoting wrong.
    let key = format!("submodule.{}.branch", ws.wine_path);
    match ws.superproject.config_file_get(".gitmodules", &key)? {
        None => f.push(Finding::new(
            "GITMODULES_BRANCH_MISSING",
            Severity::Drift,
            ".gitmodules",
            format!("`.gitmodules` declares no `{key}` -- nothing records which consumer ref the pin is supposed to track"),
            format!("git config -f .gitmodules {key} <tag-naming-the-gitlink> && git add .gitmodules"),
        )),
        Some(branch) => match ws.wine.rev_parse(&branch)? {
            // A typo and a drift want different remedies, so they are different
            // codes even though both stem from one line of config.
            None => f.push(Finding::new(
                "GITMODULES_BRANCH_UNRESOLVABLE",
                Severity::Blocker,
                ".gitmodules",
                format!("`.gitmodules` names `{branch}` but no such ref exists in the wine repo"),
                format!("git config -f .gitmodules {key} <an-existing-tag> && git add .gitmodules"),
            )),
            Some(oid) if oid != pin => f.push(Finding::new(
                "GITMODULES_BRANCH_MISMATCH",
                sev(Severity::Blocker),
                ".gitmodules",
                format!(
                    "`.gitmodules` names `{branch}` ({}) but the {source} gitlink is {} -- `submodule update --remote` would fetch a different tree than the pin",
                    short(&oid),
                    short(&pin)
                ),
                format!("git config -f .gitmodules {key} <tag-naming-{}> && git add .gitmodules {w}   # one commit", short(&pin), w = ws.wine_path),
            )),
            Some(_) => {}
        },
    }

    // A5 -- the catastrophic-and-silent one: `git add wine` performed while the
    // checkout sat on a TIER tip commits soda-without-wine as the build pin. It
    // compiles nothing and fails weirdly.
    //
    // Ancestry is the wrong primitive here (consumers are force-pushed), so the
    // question is CONTAINMENT: is the pin carried by a declared consumer branch,
    // or named by a consumer-family tag?
    let local_refs = ws.wine.branches_containing(&pin, false)?;
    let consumer_names: Vec<&str> = m.consumer.iter().map(|c| c.name.as_str()).collect();
    let in_consumer = local_refs.iter().any(|b| consumer_names.contains(&b.as_str()))
        || remote_refs
            .iter()
            .any(|b| b.rsplit('/').next().is_some_and(|n| consumer_names.contains(&n)))
        || tags_here
            .iter()
            .any(|t| consumer_names.iter().any(|c| t.starts_with(&format!("{c}-"))));

    if !in_consumer {
        let on_tier = local_refs.iter().find(|b| m.tier.iter().any(|t| &t.name == *b));
        let detail = match on_tier {
            // Naming the specific mistake is the point: "it is a tier" is
            // actionable, "it is not a consumer" is not.
            Some(t) => format!(" -- it is contained in the TIER `{t}`, which is soda without wine"),
            None => String::new(),
        };
        f.push(Finding::new(
            "GITLINK_NOT_A_CONSUMER",
            sev(Severity::Blocker),
            &ws.wine_path,
            format!(
                "the {source} gitlink {} is in no declared consumer branch and carries no consumer tag{detail}",
                short(&pin)
            ),
            format!("git -C {w} checkout <consumer-tag> && git add {w}   # pin a consumer, never a tier", w = ws.wine_path),
        ));
    }

    // A6 -- the build.sh gotcha inverted. `submodule update --force` yanks the
    // checkout back to the COMMITTED pin, so a checkout sitting elsewhere means
    // the next build compiles something other than what is checked out.
    // Judged against the pin that is about to be PUBLISHED, not the one being
    // replaced. Comparing to `committed` unconditionally -- while every other
    // check in this module prefers `staged` -- meant that a developer who had
    // staged the checkout's own commit (the normal, correct pin bump) was told
    // "build.sh will yank it back", which stops being true the instant they do
    // the obvious next thing and commit. Warning about a state the user is
    // already one keystroke from leaving is noise; `PIN_STAGED_UNCOMMITTED`
    // covers the uncommitted-ness itself.
    if let Some(head) = &checkout {
        if head != &pin {
            f.push(Finding::new(
                "WINE_HEAD_OFF_PIN",
                Severity::Hygiene,
                &ws.wine_path,
                format!(
                    "the wine checkout is at {} but the {source} pin is {} -- build.sh's `submodule update --force` will yank it back and build the pin",
                    short(head),
                    short(&pin)
                ),
                format!("git -C {w} checkout <the-pin>, or stage/commit the pin bump", w = ws.wine_path),
            ));
        }
    }

    Ok(f)
}

fn short(o: &Oid) -> String {
    o.chars().take(11).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::FakeGit;
    use std::collections::BTreeSet;

    const MANIFEST: &str = r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "home"
describes = "d"
derivation = "x"
[[consumer]]
name = "spritzwine"
base = "winehq/wine-11.13"
soda_tip = "soda-full"
pinned_by_superproject = true
"#;

    fn codes(f: &[Finding]) -> BTreeSet<&str> {
        f.iter().map(|x| x.code).collect()
    }

    fn run(pin: &str, tip: &str, tag_at: &str, gitmodules_at: &str) -> Vec<Finding> {
        let m = Manifest::parse(MANIFEST).unwrap();
        let sp = FakeGit { head: None,
            refs: vec![("HEAD:wine".into(), pin.into())],
            tags: vec![],
            index: vec![("wine".into(), pin.into())],
            config: vec![("submodule.wine.branch".into(), "spritzwine-11.13-3".into())],
            branches: vec![],
        };
        let wine = FakeGit { head: None,
            refs: vec![
                ("HEAD".into(), tip.into()),
                ("spritzwine".into(), tip.into()),
                ("spritzwine-11.13-3".into(), gitmodules_at.into()),
            ],
            tags: vec![("spritzwine-11.13-3".into(), tag_at.into())],
            index: vec![],
            config: vec![],
            branches: vec![
                ("spritzwine".into(), tip.into()),
                ("codeberg/spritzwine".into(), tip.into()),
                // The pin is an older commit still carried by the branch.
                ("spritzwine".into(), pin.into()),
                ("codeberg/spritzwine".into(), pin.into()),
            ],
        };
        let ws = Workspace {
            superproject: &sp,
            wine: &wine,
            superproject_path: ".".into(),
            wine_path: "wine".into(),
        };
        check(&m, &ws).unwrap()
    }

    /// Same as `run`, but the index and HEAD disagree: a pin bump midway
    /// through, which is the state a developer is actually in while doing the
    /// right thing.
    fn run_staged(committed: &str, staged: &str, tip: &str) -> Vec<Finding> {
        let m = Manifest::parse(MANIFEST).unwrap();
        let sp = FakeGit { head: None,
            refs: vec![("HEAD:wine".into(), committed.into())],
            tags: vec![],
            index: vec![("wine".into(), staged.into())],
            config: vec![("submodule.wine.branch".into(), "spritzwine-11.13-3".into())],
            branches: vec![],
        };
        let wine = FakeGit { head: None,
            refs: vec![
                ("HEAD".into(), tip.into()),
                ("spritzwine".into(), tip.into()),
                ("spritzwine-11.13-3".into(), committed.into()),
            ],
            tags: vec![("spritzwine-11.13-3".into(), committed.into())],
            index: vec![],
            config: vec![],
            branches: vec![
                ("spritzwine".into(), staged.into()),
                ("spritzwine".into(), committed.into()),
                ("codeberg/spritzwine".into(), committed.into()),
            ],
        };
        let ws = Workspace {
            superproject: &sp,
            wine: &wine,
            superproject_path: ".".into(),
            wine_path: "wine".into(),
        };
        check(&m, &ws).unwrap()
    }

    /// The severity regression. The COMMITTED pin here is perfect -- tagged,
    /// pushed, matching `.gitmodules` -- and only the index carries a fresh,
    /// not-yet-tagged commit. Judging the index unconditionally raised three
    /// Blockers at a developer who has done nothing wrong, describing a state
    /// no CI runner can observe (CI checks out HEAD, where index == HEAD).
    /// The facts are still reported; the severity now says "not yet".
    #[test]
    fn a_staged_pin_bump_does_not_block() {
        let f = run_staged("good_pin", "fresh_pour", "fresh_pour");
        let blockers: Vec<&str> =
            f.iter().filter(|x| x.severity == Severity::Blocker).map(|x| x.code).collect();
        assert!(blockers.is_empty(), "a staged bump must not block: {blockers:?}");
        // ...but it must still SAY so. Demotion, not suppression.
        assert!(codes(&f).contains("PIN_STAGED_UNCOMMITTED"), "{f:#?}");
        assert!(codes(&f).contains("PIN_UNTAGGED"), "{f:#?}");
    }

    /// The converse, so the demotion cannot become a blanket mute: once the
    /// same bad pin is COMMITTED, it blocks.
    #[test]
    fn the_same_pin_blocks_once_committed() {
        let f = run("untagged_pin", "untagged_pin", "elsewhere", "elsewhere");
        assert!(
            f.iter().any(|x| x.code == "PIN_UNTAGGED" && x.severity == Severity::Blocker),
            "{f:#?}"
        );
    }

    /// `WINE_HEAD_OFF_PIN` judges against the pin about to be PUBLISHED. Here
    /// the checkout and the staged gitlink agree -- the normal shape of a
    /// correct pin bump -- so the old "build.sh will yank it back" warning was
    /// false the instant the user committed.
    #[test]
    fn a_checkout_matching_the_staged_pin_is_not_off_pin() {
        let f = run_staged("good_pin", "fresh_pour", "fresh_pour");
        assert!(!codes(&f).contains("WINE_HEAD_OFF_PIN"), "{f:#?}");
    }

    /// ...and it still fires when the checkout genuinely sits somewhere else.
    #[test]
    fn a_checkout_off_every_pin_is_reported() {
        let f = run_staged("good_pin", "fresh_pour", "some_tier");
        assert!(codes(&f).contains("WINE_HEAD_OFF_PIN"), "{f:#?}");
    }

    /// THE regression this module exists for.
    ///
    /// `reconcile()` asks its tag question about the consumer's branch TIP.
    /// Here the tip is an untagged pour sitting ahead of a correctly tagged,
    /// correctly pinned commit -- BRANCHING.md's documented normal state. The
    /// tip-based check calls that a Blocker and tells you to tag an untested
    /// pour. Reading the gitlink, there is nothing wrong and nothing is said.
    #[test]
    fn a_tip_ahead_of_a_correctly_tagged_pin_is_silent() {
        let f = run("pinned", "newer_pour", "pinned", "pinned");
        // WINE_HEAD_OFF_PIN is expected and true here -- the checkout really is
        // sitting on the pour rather than the pin. What must NOT appear is any
        // claim that the PIN is wrong.
        let bad: Vec<&str> =
            f.iter().filter(|x| x.severity != Severity::Hygiene).map(|x| x.code).collect();
        assert!(bad.is_empty(), "a correctly tagged pin produced {bad:?}: {f:#?}");
    }

    /// And the converse: the tip carries a tag while the GITLINK does not.
    /// Same repo, opposite verdict -- a tip-based check reports clean here and
    /// the pin breaks on the next pour.
    #[test]
    fn an_untagged_gitlink_blocks_even_when_the_tip_is_tagged() {
        let f = run("stale_pin", "tipoid", "tipoid", "tipoid");
        assert!(codes(&f).contains("PIN_UNTAGGED"), "{f:#?}");
        // ...and the .gitmodules branch names a different commit than the pin.
        assert!(codes(&f).contains("GITMODULES_BRANCH_MISMATCH"), "{f:#?}");
    }
}
