//! `reconcile` against a real git repository.
//!
//! The unit tests in `reconcile_fake.rs` prove the reconciler does what its
//! author thinks. These prove the author's model of git is right -- which is a
//! different claim, and the one that actually failed. A cherry-picked commit's
//! patch id matching its original is a property of `git patch-id`'s
//! whitespace/context normalisation, not of anything we control; a fake asserts
//! it, this observes it.
//!
//! Everything here runs against `tests/fixtures/micro`, built by
//! `tests/mkfixture.sh` in under a second. If it is absent the tests skip
//! loudly rather than fail: `cargo test` on a fresh checkout should be green.
//!
//! It must never run against the real repo. `mkfixture.sh` builds a throwaway
//! and nothing here writes.

#[path = "fake.rs"]
mod fake;

use fake::{all_codes, codes_for};
use sodactl_core::{reconcile, Manifest, Report, Severity};
use sodactl_git::{GitCli, GitRead};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixture root, building it if it is missing. Returns `None` when it
/// cannot be had -- no bash, no git, a script that failed -- and every test
/// then skips.
fn fixture() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SODACTL_FIXTURE") {
        let p = PathBuf::from(p);
        return p.join("wine").is_dir().then_some(p);
    }
    // crates/sodactl-core -> tools/sodactl
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    let dir = root.join("tests/fixtures/micro");

    if !dir.join("wine").is_dir() {
        let script = root.join("tests/mkfixture.sh");
        if !script.is_file() {
            return None;
        }
        // Cheap and hermetic: it only ever writes inside tests/fixtures.
        let out = Command::new("bash").arg(&script).arg(&dir).output().ok()?;
        if !out.status.success() {
            eprintln!(
                "mkfixture.sh failed, skipping real-repo tests:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return None;
        }
    }
    dir.join("wine").is_dir().then_some(dir)
}

/// `Some((manifest, git, report))`, or `None` to skip.
fn load() -> Option<(Manifest, GitCli, Report)> {
    let dir = fixture()?;
    let text = std::fs::read_to_string(dir.join("tools/sodactl.toml")).ok()?;
    let m = Manifest::parse(&text).expect("fixture manifest must parse");
    let git = GitCli::new(dir.join(&m.repo.wine));
    let r = reconcile(&m, &git).expect("reconcile must not error on a well-formed repo");
    Some((m, git, r))
}

macro_rules! fixture_test {
    ($name:ident, |$m:pat_param, $git:pat_param, $r:pat_param| $body:block) => {
        #[test]
        fn $name() {
            let Some(($m, $git, $r)) = load() else {
                eprintln!("skipping {}: no fixture (run tests/mkfixture.sh)", stringify!($name));
                return;
            };
            $body
        }
    };
}

// ---- the property the whole strategy rests on -----------------------------

// Before trusting any verdict below, establish that this fixture actually
// exhibits the trap: the stitch carries both tiers, yet neither tier is an
// ancestor of it and no oid is shared. If a future change to `mkfixture.sh`
// makes the stitch a descendant, every stitch test below silently stops
// testing anything -- so this asserts it directly.
fixture_test!(the_fixture_really_is_a_replay, |_m, git, _r| {
    assert!(!git.is_ancestor("soda-glorious", "soda-full").unwrap());
    assert!(!git.is_ancestor("soda-ntdll", "soda-full").unwrap());
    assert!(!git.is_ancestor("soda-full", "spritzwine").unwrap());
    assert_ne!(git.rev_parse("soda-ntdll").unwrap(), git.rev_parse("soda-full").unwrap());

    // Same commit count, no shared commits: every count- or sha-based
    // "does it carry it?" answer is wrong here, and only patch-id is right.
    assert_eq!(
        git.count_commits("soda-core..soda-full").unwrap(),
        git.count_commits("soda-core..soda-glorious").unwrap()
            + git.count_commits("soda-core..soda-ntdll").unwrap()
    );
});

// ---- stitches -------------------------------------------------------------

// THE regression test, against real cherry-picks. `soda-full` was stitched from
// the current tiers; `merge-base --is-ancestor` says it carries neither, and
// the original implementation believed it.
fixture_test!(a_current_stitch_does_not_fire_stale, |_m, git, r| {
    for tier in ["soda-glorious", "soda-ntdll"] {
        assert!(
            git.missing_by_patch_id("soda-full", tier, "soda-core").unwrap().is_empty(),
            "{tier} should be patch-id present in soda-full"
        );
    }
    assert!(
        !codes_for(&r, "soda-full").contains("STITCH_STALE"),
        "ancestry-based staleness is back: {:?}",
        r.findings
    );
    assert_eq!(codes_for(&r, "soda-full"), set(["STITCH_LOCAL_ONLY"]));
});

// ...and the tool must still be able to see a real one. `soda-stale-full` was
// stitched without soda-ntdll. It is structurally indistinguishable from
// `soda-full` by ancestry, count or sha -- both descend from soda-core, both
// share nothing with the tiers.
fixture_test!(a_stitch_missing_a_tier_is_stale, |_m, git, r| {
    assert!(git.missing_by_patch_id("soda-stale-full", "soda-glorious", "soda-core").unwrap().is_empty());
    assert_eq!(git.missing_by_patch_id("soda-stale-full", "soda-ntdll", "soda-core").unwrap().len(), 1);

    assert_eq!(codes_for(&r, "soda-stale-full"), set(["STITCH_STALE", "STITCH_LOCAL_ONLY"]));
    assert_eq!(r.findings.iter().filter(|f| f.code == "STITCH_STALE").count(), 1);
});

// ---- tiers ----------------------------------------------------------------

fixture_test!(the_stale_child_is_reported_and_its_siblings_are_not, |_m, _git, r| {
    assert_eq!(codes_for(&r, "soda-glorious"), set(["TIER_CHILD_STALE"]));
    assert!(codes_for(&r, "soda-core").is_empty());
    assert!(!codes_for(&r, "soda-ntdll").contains("TIER_CHILD_STALE"));
});

fixture_test!(tier_geometry_is_derived_from_the_repo, |_m, _git, r| {
    let t = |n: &str| r.tiers.iter().find(|t| t.name == n).unwrap();
    // Measured from the CURRENT parent tip, so the stale child still reports
    // its own single commit rather than inheriting the parent's drift.
    assert_eq!(t("soda-core").commits_on_parent, Some(2));
    assert_eq!(t("soda-glorious").commits_on_parent, Some(1));
    assert_eq!(t("soda-glorious").contains_parent_tip, Some(false));
    assert_eq!(t("soda-ntdll").contains_parent_tip, Some(true));
});

fixture_test!(unpushed_and_absent_tracking_refs_are_distinguished, |_m, _git, r| {
    assert_eq!(codes_for(&r, "soda-demo"), set(["TIER_UNPUSHED"]));
    let demo = r.tiers.iter().find(|t| t.name == "soda-demo").unwrap();
    assert_eq!(demo.ahead_of_remote, Some(1));

    assert_eq!(codes_for(&r, "soda-ntdll"), set(["TIER_NO_REMOTE"]));
    let ntdll = r.tiers.iter().find(|t| t.name == "soda-ntdll").unwrap();
    assert_eq!(ntdll.ahead_of_remote, None, "never-pushed must not read as in-sync");
});

fixture_test!(a_declared_tier_with_no_branch_does_not_crash_the_walk, |_m, _git, r| {
    assert_eq!(codes_for(&r, "soda-missing"), set(["TIER_MISSING"]));
    // The rest of the graph was still walked.
    assert!(r.tiers.len() >= 6);
    assert!(!codes_for(&r, "soda-glorious").is_empty());
});

// ---- consumers ------------------------------------------------------------

// The failure this prevents is `Fetched in submodule path 'wine', but it did
// not contain <sha>`: consumers are force-pushed, so an untagged pinned commit
// stops being fetchable the moment the branch is regenerated.
fixture_test!(a_pinned_consumer_needs_a_tag_on_its_exact_tip, |_m, _git, r| {
    assert_eq!(codes_for(&r, "spritzwine"), set(["PINNED_CONSUMER_UNTAGGED"]));
    assert_eq!(
        r.findings.iter().find(|f| f.code == "PINNED_CONSUMER_UNTAGGED").unwrap().severity,
        Severity::Blocker
    );
    // There is only ever ONE gitlink, so only one consumer can be pinned.
    // spritzvalve is the unpinned control: nothing pins it, so there is nothing
    // to be untagged about, and the check must stay silent rather than fall back
    // to judging its branch tip -- which is the wrong-subject bug this pair of
    // assertions exists to hold down.
    assert!(codes_for(&r, "spritzvalve").is_empty());
});

fixture_test!(consumer_currency_stays_unknown_without_a_ledger, |_m, _git, r| {
    // A consumer shares no oid with its soda tip and is nobody's descendant.
    // Until the v3 generation ledger exists, `None` is the only honest answer;
    // anything else here would be the ancestry bug wearing a different hat.
    for c in &r.consumers {
        assert_eq!(c.current, None, "{} claimed a currency it cannot know", c.name);
    }
});

// ---- everything else ------------------------------------------------------

fixture_test!(unmodelled_branches_surface_and_vendor_lines_do_not, |_m, _git, r| {
    assert_eq!(codes_for(&r, "spritzwine-backup-20260703"), set(["UNMODELLED_BRANCH"]));
    for skipped in ["master", "proton_11.0"] {
        assert!(codes_for(&r, skipped).is_empty(), "{skipped} should not be modelled by us");
    }
});

// The whole-report shape, as one assertion. Deliberately an exact set: a new
// finding code that fires on this fixture is either a real improvement worth
// recording here or a false positive, and both should stop the build.
fixture_test!(the_fixture_produces_exactly_this_finding_set, |_m, _git, r| {
    assert_eq!(
        all_codes(&r),
        set([
            "PINNED_CONSUMER_UNTAGGED",
            "STITCH_LOCAL_ONLY",
            "STITCH_STALE",
            "TIER_CHILD_STALE",
            "TIER_MISSING",
            "TIER_NO_REMOTE",
            "TIER_UNPUSHED",
            "UNMODELLED_BRANCH",
        ]),
        "{:#?}",
        r.findings
    );
    assert_eq!(r.count(Severity::Blocker), 2); // TIER_MISSING + the untagged pin
    assert_eq!(r.worst(), Some(Severity::Blocker));
});

// ---- helpers --------------------------------------------------------------

fn set<const N: usize>(xs: [&str; N]) -> std::collections::BTreeSet<String> {
    xs.iter().map(|s| s.to_string()).collect()
}
