//! `reconcile` against a synthetic DAG.
//!
//! These are the fast tests: no disk, no `git` process, one contrived graph per
//! behaviour, asserting an exact finding set. They are the right place to pin
//! down *which* finding fires and with what severity.
//!
//! They are NOT sufficient on their own, and `reconcile_real.rs` exists for that
//! reason: a fake can encode a wrong mental model just as easily as the code
//! can. What the fake buys is that each test states one story in five lines.

#[path = "fake.rs"]
mod fake;

use fake::{all_codes, codes_for, manifest, FakeRepo};
use sodactl_core::{reconcile, Severity};
use sodactl_git::Reapply;

/// The standard shape: fork base -> soda-build -> soda-core, everything pushed.
/// Returns the repo with `soda-core` at its tip, ready for a test to hang the
/// interesting part off.
fn spine() -> FakeRepo {
    let mut g = FakeRepo::new();
    let base = g.root("wine-base"); // c01 -- the manifest's $home_base
    g.tier("soda-build", &base, &["build1"]);
    g.tier("soda-core", "soda-build", &["core1"]);
    g.remote_branch("codeberg/soda-build", "soda-build");
    g.remote_branch("codeberg/soda-core", "soda-core");
    g
}

const SPINE_TIERS: &str = r#"
[[tier]]
name = "soda-build"
parent = "$home_base"
remote = "codeberg"

[[tier]]
name = "soda-core"
parent = "soda-build"
remote = "codeberg"
"#;

// ---- tiers ----------------------------------------------------------------

#[test]
fn tier_child_authored_on_an_old_parent_tip_is_stale() {
    let mut g = spine();
    let old_core = g.at("soda-core");
    g.extend("soda-core", &["core2"]); // core moves on...
    g.tier("soda-glorious", &old_core, &["glor1"]); // ...the child does not
    g.remote_branch("codeberg/soda-glorious", "soda-glorious");

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    assert_eq!(codes_for(&r, "soda-glorious"), set(["TIER_CHILD_STALE"]));
    // Derived geometry, not declared: one own commit, measured from the CURRENT
    // parent tip. If this ever comes from the manifest, the whole model is lost.
    let t = tier(&r, "soda-glorious");
    assert_eq!(t.commits_on_parent, Some(1));
    assert_eq!(t.contains_parent_tip, Some(false));
}

#[test]
fn a_restacked_child_is_silent() {
    let mut g = spine();
    g.extend("soda-core", &["core2"]);
    g.tier("soda-glorious", "soda-core", &["glor1"]); // authored on the new tip
    g.remote_branch("codeberg/soda-glorious", "soda-glorious");

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    assert!(codes_for(&r, "soda-glorious").is_empty());
    assert_eq!(tier(&r, "soda-glorious").contains_parent_tip, Some(true));
}

#[test]
fn a_declared_tier_with_no_branch_is_a_blocker() {
    let g = spine();
    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-ghost"
parent = "soda-core"
remote = "codeberg"
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    assert_eq!(codes_for(&r, "soda-ghost"), set(["TIER_MISSING"]));
    assert_eq!(severity(&r, "TIER_MISSING"), Severity::Blocker);
    // It still appears in the state table -- a missing tier is a fact to report,
    // not a row to drop on the floor.
    assert!(tier(&r, "soda-ghost").oid.is_none());
}

#[test]
fn unpushed_and_never_pushed_are_different_findings() {
    let mut g = spine();
    g.extend("soda-core", &["core2"]); // committed, not pushed
    g.tier("soda-local", "soda-core", &["l1"]); // no tracking ref at all

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-local"
parent = "soda-core"
remote = "codeberg"
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    assert_eq!(codes_for(&r, "soda-core"), set(["TIER_UNPUSHED"]));
    assert_eq!(tier(&r, "soda-core").ahead_of_remote, Some(1));
    assert_eq!(codes_for(&r, "soda-local"), set(["TIER_NO_REMOTE"]));
    // "never pushed" must not masquerade as "in sync".
    assert_eq!(tier(&r, "soda-local").ahead_of_remote, None);
}

// ---- stitches -------------------------------------------------------------

/// THE regression test. A stitch is built by cherry-pick, so no tier commit is
/// ever an ancestor of it and no oid is ever shared. The original check asked
/// `merge-base --is-ancestor` and produced four confident false positives.
///
/// If STITCH_STALE fires here, the ancestry bug is back.
#[test]
fn a_current_stitch_does_not_fire_stale() {
    let mut g = spine();
    g.tier("soda-glorious", "soda-core", &["glor1"]);
    g.tier("soda-ntdll", "soda-core", &["ntdll1"]);
    g.replay("soda-full", "soda-core", &["glor1", "ntdll1"]);
    for b in ["soda-glorious", "soda-ntdll"] {
        g.remote_branch(&format!("codeberg/{b}"), b);
    }

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-ntdll"
parent = "soda-core"
remote = "codeberg"

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious", "soda-ntdll"]
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    // Prove the fixture actually exhibits the trap before trusting the verdict.
    // Without these two lines a fake that quietly made the stitch a descendant
    // would pass this test while testing nothing.
    use sodactl_git::GitRead;
    assert!(!g.is_ancestor("soda-glorious", "soda-full").unwrap());
    assert_ne!(g.rev_parse("soda-glorious").unwrap(), g.rev_parse("soda-full").unwrap());

    assert!(
        !all_codes(&r).contains("STITCH_STALE"),
        "ancestry-based staleness is back: {:?}",
        r.findings
    );
}

#[test]
fn a_stitch_missing_a_tier_commit_is_stale() {
    let mut g = spine();
    g.tier("soda-glorious", "soda-core", &["glor1"]);
    g.tier("soda-ntdll", "soda-core", &["ntdll1"]);
    g.replay("soda-full", "soda-core", &["glor1", "ntdll1"]);
    // ...and then a tier moves. The stitch is now genuinely behind.
    g.extend("soda-ntdll", &["ntdll2"]);
    for b in ["soda-glorious", "soda-ntdll"] {
        g.remote_branch(&format!("codeberg/{b}"), b);
    }

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[tier]]
name = "soda-ntdll"
parent = "soda-core"
remote = "codeberg"

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious", "soda-ntdll"]
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    assert_eq!(codes_for(&r, "soda-full"), set(["STITCH_STALE"]));
    // Exactly one: soda-glorious is still fully carried, and a check that fired
    // per-stitch rather than per-tier would report two.
    assert_eq!(r.findings.iter().filter(|f| f.code == "STITCH_STALE").count(), 1);
}

#[test]
fn a_declared_stitch_with_no_branch_is_reported() {
    let g = spine();
    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = []
"#
    ));
    let r = reconcile(&m, &g).unwrap();
    assert_eq!(codes_for(&r, "soda-full"), set(["STITCH_MISSING"]));
}

#[test]
fn a_local_only_stitch_says_so_once_and_only_as_hygiene() {
    let mut g = spine();
    g.replay("soda-full", "soda-core", &[]);
    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = []
local_only = true
"#
    ));
    let r = reconcile(&m, &g).unwrap();
    assert_eq!(codes_for(&r, "soda-full"), set(["STITCH_LOCAL_ONLY"]));
    assert_eq!(severity(&r, "STITCH_LOCAL_ONLY"), Severity::Hygiene);
}

// ---- consumers ------------------------------------------------------------

/// Consumers are force-pushed. An untagged pinned SHA falls out of the branch on
/// the next pour and `git submodule update` dies with "did not contain <sha>".
/// This is the one case where a tag is mandatory outside a release cut.
#[test]
fn a_pinned_untagged_consumer_is_a_blocker() {
    let mut g = spine();
    g.tier("master", "c01", &["w1"]); // upstream wine base
    g.replay("spritzwine", "master", &["build1", "core1"]);
    // The SUBJECT is the gitlink, never the branch tip. BRANCHING.md makes tips
    // running ahead of their tags normal operation, so a check that read the tip
    // would be right only on the days the two happen to coincide -- which is
    // exactly how the original version passed its own review. Stating the pin
    // explicitly is the point: the test now has to say which one it means.
    g.pin("spritzwine");

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[consumer]]
name = "spritzwine"
base = "winehq/wine-11.13"
soda_tip = "soda-core"
pinned_by_superproject = true
"#
    ));
    let r = reconcile(&m, &g).unwrap();

    assert_eq!(codes_for(&r, "spritzwine"), set(["PINNED_CONSUMER_UNTAGGED"]));
    assert_eq!(severity(&r, "PINNED_CONSUMER_UNTAGGED"), Severity::Blocker);
}

#[test]
fn a_pinned_tagged_consumer_is_silent() {
    let mut g = spine();
    g.tier("master", "c01", &["w1"]);
    g.replay("spritzwine", "master", &["build1", "core1"]);
    g.tag("spritzwine-11.13-3", "spritzwine");
    g.pin("spritzwine"); // tip and gitlink coincide here, and only here

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[consumer]]
name = "spritzwine"
base = "winehq/wine-11.13"
soda_tip = "soda-core"
pinned_by_superproject = true
"#
    ));
    let r = reconcile(&m, &g).unwrap();
    assert!(codes_for(&r, "spritzwine").is_empty(), "{:?}", r.findings);
}

/// A tag that names an ancestor of the tip is not a pin for the tip. The check
/// must compare oids, not reachability -- a tag one commit back is exactly the
/// "version 3 ships version-1 bits" defect.
#[test]
fn a_tag_on_an_earlier_commit_does_not_satisfy_the_pin() {
    let mut g = spine();
    g.tier("master", "c01", &["w1"]);
    g.replay("spritzwine", "master", &["build1"]);
    g.tag("spritzwine-11.13-1", "spritzwine");
    g.extend("spritzwine", &["core1"]); // regenerated, tag left behind
    g.pin("spritzwine"); // the pin followed the pour; the tag did not

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[consumer]]
name = "spritzwine"
base = "winehq/wine-11.13"
soda_tip = "soda-core"
pinned_by_superproject = true
"#
    ));
    let r = reconcile(&m, &g).unwrap();
    assert_eq!(codes_for(&r, "spritzwine"), set(["PINNED_CONSUMER_UNTAGGED"]));
}

#[test]
fn an_unpinned_consumer_needs_no_tag() {
    let mut g = spine();
    g.tier("proton_11.0", "c01", &["v1"]);
    g.replay("spritzvalve", "proton_11.0", &["build1", "core1"]);

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[consumer]]
name = "spritzvalve"
base = "valve-wine/proton_11.0"
soda_tip = "soda-core"
pinned_by_superproject = false
"#
    ));
    let r = reconcile(&m, &g).unwrap();
    assert!(codes_for(&r, "spritzvalve").is_empty());
}

/// Staleness of a consumer is not answerable without a generation ledger (v3).
/// `None` is the honest answer; a guess here would be the ancestry bug again,
/// since a consumer shares no oid with its soda tip either.
#[test]
fn consumer_currency_is_unknown_not_guessed() {
    let mut g = spine();
    g.tier("master", "c01", &["w1"]);
    g.replay("spritzwine", "master", &["build1", "core1"]);
    g.tag("t", "spritzwine");

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[consumer]]
name = "spritzwine"
base = "winehq/wine-11.13"
soda_tip = "soda-core"
pinned_by_superproject = true
"#
    ));
    let r = reconcile(&m, &g).unwrap();
    assert_eq!(r.consumers.iter().find(|c| c.name == "spritzwine").unwrap().current, None);
}

// ---- unmodelled branches --------------------------------------------------

#[test]
fn a_branch_outside_the_manifest_is_reported_not_ignored() {
    let mut g = spine();
    g.branch("spritzwine-backup-20260703", "soda-core");

    let m = manifest(SPINE_TIERS);
    let r = reconcile(&m, &g).unwrap();

    assert_eq!(codes_for(&r, "spritzwine-backup-20260703"), set(["UNMODELLED_BRANCH"]));
    assert_eq!(severity(&r, "UNMODELLED_BRANCH"), Severity::Hygiene);
}

#[test]
fn upstream_and_vendor_lines_are_not_ours_to_model() {
    let mut g = spine();
    g.branch("master", "soda-core");
    g.branch("proton_11.0", "soda-core");
    g.branch("stable", "soda-core");

    let m = manifest(SPINE_TIERS);
    let r = reconcile(&m, &g).unwrap();
    assert!(!all_codes(&r).contains("UNMODELLED_BRANCH"), "{:?}", r.findings);
}

// ---- ordering -------------------------------------------------------------

#[test]
fn findings_sort_worst_first() {
    let mut g = spine();
    g.branch("scratch", "soda-core"); // hygiene
    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-ghost"
parent = "soda-core"
"#
    )); // blocker
    let r = reconcile(&m, &g).unwrap();

    let sevs: Vec<_> = r.findings.iter().map(|f| f.severity).collect();
    let mut sorted = sevs.clone();
    sorted.sort();
    assert_eq!(sevs, sorted);
}

// ---- helpers --------------------------------------------------------------

fn set<const N: usize>(xs: [&str; N]) -> std::collections::BTreeSet<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn tier<'a>(r: &'a sodactl_core::Report, name: &str) -> &'a sodactl_core::finding::TierState {
    r.tiers.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("no tier state for `{name}`"))
}

fn severity(r: &sodactl_core::Report, code: &str) -> Severity {
    r.findings
        .iter()
        .find(|f| f.code == code)
        .unwrap_or_else(|| panic!("no finding `{code}`"))
        .severity
}

/// A tier that is BEHIND its remote with nothing of its own must not be called
/// diverged.
///
/// `(0, behind)` failed both the `(0, 0)` and `(a, 0)` arms and fell into the
/// `(a, b)` catch-all, so one tier produced two contradictory findings: a Drift
/// `TIER_DIVERGED` whose remedy is `push --force-with-lease` (destroying commits
/// that are not yours) and a Blocker `TIER_BEHIND_REMOTE` saying "do NOT
/// force-push over this". `checks/tiers.rs` gated on `ahead > 0 && behind > 0`;
/// this copy of the logic did not.
#[test]
fn a_tier_behind_its_remote_is_not_diverged() {
    let mut g = spine();
    // The remote carries two commits this checkout has never seen -- someone
    // else's authored work.
    g.branch("codeberg/soda-core", "soda-core");
    g.extend("codeberg/soda-core", &["theirs1", "theirs2"]);

    let m = manifest(SPINE_TIERS);
    let r = reconcile(&m, &g).unwrap();
    let codes = codes_for(&r, "soda-core");

    assert!(codes.contains("TIER_BEHIND_REMOTE"), "the true finding must fire: {codes:?}");
    assert!(
        !codes.contains("TIER_DIVERGED"),
        "behind-only is not divergence, and its remedy would drop the remote's work: {codes:?}"
    );
    // The one that survives is the Blocker, not a Drift telling you to force-push.
    assert_eq!(
        r.findings
            .iter()
            .find(|f| f.code == "TIER_BEHIND_REMOTE")
            .map(|f| f.severity),
        Some(Severity::Blocker)
    );
}

/// The converse, so the new `(0, _)` arm cannot become a blanket mute: a tier
/// that is genuinely ahead AND behind is still diverged, and still says so.
#[test]
fn a_restacked_tier_is_still_diverged() {
    let mut g = spine();
    g.branch("codeberg/soda-core", "soda-core");
    g.extend("codeberg/soda-core", &["theirs1"]);
    g.extend("soda-core", &["mine1"]);

    let m = manifest(SPINE_TIERS);
    let r = reconcile(&m, &g).unwrap();
    let codes = codes_for(&r, "soda-core");
    assert!(codes.contains("TIER_DIVERGED"), "{codes:?}");
    assert!(!codes.contains("TIER_BEHIND_REMOTE"), "{codes:?}");
}

/// The rerere false-negative: patch-id says a commit is missing from the stitch,
/// but reapplying it is a no-op, so it IS there.
///
/// This is not hypothetical. `git patch-id` normalises whitespace but still
/// hashes the diff hunks, so a cherry-pick whose conflict git-rerere resolved --
/// this project's central mechanism -- hashes differently from its source while
/// being the same change. Before the reapply slow path, that fired STITCH_STALE
/// on precisely the commits the rerere pipeline worked hardest on, and the
/// remedy told you to re-stitch a stitch that was already correct.
#[test]
fn patch_id_crying_missing_is_overruled_by_a_clean_reapply() {
    let mut g = spine();
    g.tier("soda-glorious", "soda-core", &["glor1"]);
    // The stitch carries a REWRITTEN copy: same change, different patch hash,
    // exactly as rerere leaves it.
    g.replay("soda-full", "soda-core", &["glor1-rerere-resolved"]);
    g.remote_branch("codeberg/soda-glorious", "soda-glorious");
    // Reapplying the tier's commit onto the stitch changes nothing.
    g.reapply("soda-glorious", "soda-full", Reapply::Present);

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious"]
local_only = true
"#
    ));

    let r = sodactl_core::reconcile(&m, &g).unwrap();
    assert!(
        !all_codes(&r).contains("STITCH_STALE"),
        "a clean reapply must overrule patch-id; findings: {:?}",
        all_codes(&r)
    );
}

/// The converse, so the fix above is not a mute button: when the reapply agrees
/// the commit is absent, STITCH_STALE must still fire and must say it was
/// confirmed.
#[test]
fn a_genuinely_missing_commit_is_confirmed_by_reapply() {
    let mut g = spine();
    g.tier("soda-glorious", "soda-core", &["glor1"]);
    g.replay("soda-full", "soda-core", &[]);
    g.remote_branch("codeberg/soda-glorious", "soda-glorious");
    // default verdict is Missing -- the reapply agrees with patch-id

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious"]
local_only = true
"#
    ));

    let r = sodactl_core::reconcile(&m, &g).unwrap();
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "STITCH_STALE")
        .expect("a genuinely missing commit must still be reported");
    assert!(
        f.summary.contains("confirmed by reapply"),
        "summary must state the confirmation landed: {}",
        f.summary
    );
}

/// A conflicted reapply is NOT evidence of absence. The finding is kept (dropping
/// it would hide real staleness) but must admit it is unconfirmed rather than
/// dressing a guess up as a measurement.
#[test]
fn a_conflicted_reapply_reports_itself_as_unconfirmed() {
    let mut g = spine();
    g.tier("soda-glorious", "soda-core", &["glor1"]);
    g.replay("soda-full", "soda-core", &[]);
    g.remote_branch("codeberg/soda-glorious", "soda-glorious");
    g.reapply("soda-glorious", "soda-full", Reapply::Unknown);

    let m = manifest(&format!(
        r#"{SPINE_TIERS}
[[tier]]
name = "soda-glorious"
parent = "soda-core"
remote = "codeberg"

[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-glorious"]
local_only = true
"#
    ));

    let r = sodactl_core::reconcile(&m, &g).unwrap();
    let f = r.findings.iter().find(|f| f.code == "STITCH_STALE").expect("kept");
    assert!(
        f.summary.contains("unconfirmed"),
        "a conflicted reapply must be admitted, not hidden: {}",
        f.summary
    );
}
