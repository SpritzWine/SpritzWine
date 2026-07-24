//! The mutating verbs (restack/undo/stitch/tag/pin) and the read-only
//! release-path verbs (push --print, release) against a real fixture
//! repository.
//!
//! Unlike `reconcile_real.rs`, which reads the SHARED committed fixture, every
//! test here MUTATES its repo -- so each builds its own private copy via
//! `tests/mkfixture.sh` into a pid-scoped tempdir (the committed fixture is
//! shared state for the read-only tests and must never be touched). The
//! fixture already carries exactly the state restack exists for: `soda-core`
//! moved and `soda-glorious` was authored on the old tip -- a real moved
//! parent, a real stale child, real fresh oids.
//!
//! Test-local topology extensions (the ambiguous branch, the grandchild tier)
//! are made on the COPY plus an in-code `m.tier.push(..)`, not by growing
//! `mkfixture.sh`: the generator also feeds `reconcile_real.rs`'s exact
//! finding-set assertion, and a new branch there would change what that suite
//! is pinned to.

use sodactl_core::checks::{Policy, Workspace};
use sodactl_core::plan::{self, ApplyCtx};
use sodactl_core::verbs::{pin, pushprint, regen, release, restack, stitch, tagverb, undo};
use sodactl_core::Manifest;
use sodactl_git::{GitCli, GitMut, GitRead};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    dir: PathBuf,
    m: Manifest,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn wine_dir(&self) -> PathBuf {
        self.dir.join(&self.m.repo.wine)
    }
    fn wine(&self) -> GitCli {
        GitCli::new(self.wine_dir())
    }
    fn superproject(&self) -> GitCli {
        GitCli::new(&self.dir)
    }
}

/// Run git for fixture surgery, asserting success. Spawning here is fine --
/// the audited-constructor discipline governs production code, and these are
/// tests against a throwaway copy.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
}

/// A private fixture in a pid-scoped tempdir, or `None` to skip loudly (same
/// contract as `reconcile_real.rs`: a fresh checkout without bash/git stays
/// green, it does not lie about having tested anything).
fn fixture(name: &str) -> Option<Fixture> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    let script = root.join("tests/mkfixture.sh");
    if !script.is_file() {
        return None;
    }
    // Pid-scoped for the reason documented on the sodactl-git `scratch`: the
    // build wipes the directory on entry, so an unscoped path lets a
    // concurrent run delete this run's live repos mid-test.
    let dir =
        std::env::temp_dir().join(format!("sodactl-verbs-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = Command::new("bash").arg(&script).arg(&dir).output().ok()?;
    if !out.status.success() {
        eprintln!("mkfixture.sh failed, skipping:\n{}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    // The generator relies on GIT_* env for identity; the rebases and
    // cherry-picks THESE tests run need repo-local config or they would
    // depend on whatever global config the machine has.
    for repo in [dir.join("wine"), dir.clone()] {
        git(&repo, &["config", "user.email", "verbs@test"]);
        git(&repo, &["config", "user.name", "verbs"]);
    }
    let m = Manifest::parse(&std::fs::read_to_string(dir.join("tools/sodactl.toml")).ok()?)
        .expect("fixture manifest must parse");
    Some(Fixture { dir, m })
}

macro_rules! fixture_test {
    ($name:ident, |$f:pat_param| $body:block) => {
        #[test]
        fn $name() {
            let Some($f) = fixture(stringify!($name)) else {
                eprintln!("skipping {}: no fixture (need bash+git)", stringify!($name));
                return;
            };
            $body
        }
    };
}

/// Both `GitMut`s plus the read handles, wired the way the CLI wires them.
fn apply_ctx<'a>(wine_mut: &'a GitMut, super_mut: &'a GitMut, wine: &'a GitCli, sup: &'a GitCli, f: &Fixture) -> ApplyCtx<'a> {
    ApplyCtx {
        wine: wine_mut,
        superproject: super_mut,
        wine_read: wine,
        superproject_read: sup,
        wine_path: f.m.repo.wine.clone(),
    }
}

fixture_test!(plan_restack_derives_the_own_set_and_the_old_base, |f| {
    let wine = f.wine();
    let plans = restack::plan_restack(&f.m, &wine, "soda-glorious", false).unwrap();
    assert_eq!(plans.len(), 1);
    let rp = &plans[0];

    // The fixture's geometry: soda-glorious carries exactly one own commit,
    // authored on the OLD soda-core tip (`soda-glorious~1`).
    let tip = wine.rev_parse("soda-glorious").unwrap().unwrap();
    let old_core = wine.rev_parse("soda-glorious~1").unwrap().unwrap();
    let new_core = wine.rev_parse("soda-core").unwrap().unwrap();
    assert_eq!(rp.own, vec![tip.clone()], "the own set is the one glorious commit");
    assert_eq!(rp.old_base, old_core, "the old base is COMPUTED, and it is the old core tip");
    assert_eq!(rp.parent_tip, new_core);

    // The reviewable surface: literal argv, CAS expectation, backup pair.
    assert_eq!(
        rp.plan.steps[0].argv,
        vec!["rebase", "--onto", &new_core, &old_core, "soda-glorious"]
    );
    let mu = &rp.plan.mutations[0];
    assert_eq!(mu.refname, "refs/heads/soda-glorious");
    assert_eq!(mu.expected_before.as_deref(), Some(tip.as_str()));
    assert!(rp.backup.0.starts_with("refs/sodactl/backup/") && rp.backup.0.ends_with("/soda-glorious"));
    assert_eq!(rp.backup.1, tip);
    assert!(!rp.provisional);

    // A tier already on its parent's tip plans NOTHING -- the idempotence the
    // design table promises.
    assert!(restack::plan_restack(&f.m, &wine, "soda-ntdll", false).unwrap().is_empty());
});

fixture_test!(apply_restack_moves_the_child_and_undo_restores_it, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(f.wine_dir());
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);

    // Preflight passes on the pristine fixture -- the same gate the CLI runs.
    let ws = Workspace {
        superproject: &sup,
        wine: &wine,
        superproject_path: f.dir.clone(),
        wine_path: f.m.repo.wine.clone(),
    };
    plan::preflight(&f.m, &ws).unwrap();

    let old_tip = wine.rev_parse("soda-glorious").unwrap().unwrap();
    let plans = restack::plan_restack(&f.m, &wine, "soda-glorious", false).unwrap();
    let out = restack::apply_restack(&plans[0], &ctx).unwrap();

    // End to end: the child moved, the postcondition's facts hold.
    assert!(wine.is_ancestor("soda-core", "soda-glorious").unwrap(), "now on the parent's tip");
    assert_eq!(wine.count_commits("soda-core..soda-glorious").unwrap(), 1);
    assert_ne!(out.new_tip, old_tip);
    assert!(out.outcome.journal.is_file(), "the journal was written");

    // The backup ref exists at the pre-rebase tip...
    let (backup_ref, backup_tip) = &plans[0].backup;
    assert_eq!(wine.rev_parse_raw(backup_ref).unwrap().unwrap(), old_tip);
    assert_eq!(backup_tip, &old_tip);

    // ...the listing shows it as newest-first with the branch moved...
    let l = undo::list_backups(&wine).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].branch, "soda-glorious");
    assert!(l[0].moved);

    // ...and undo restores via CAS against the live (rebased) tip. The rebase
    // left the branch checked out, so the plan must carry the worktree note.
    let up = undo::plan_undo(&wine, &format!("{}/{}", l[0].utc, l[0].branch)).unwrap();
    assert_eq!(up.mutations[0].expected_before.as_deref(), Some(out.new_tip.as_str()));
    assert!(up.notes.iter().any(|n| n.contains("currently checked out")), "{:?}", up.notes);
    plan::apply(&up, &ctx).unwrap();
    assert_eq!(wine.rev_parse_raw("refs/heads/soda-glorious").unwrap().unwrap(), old_tip);
});

fixture_test!(an_ambiguous_old_parent_refuses_with_the_evidence, |mut f| {
    let wd = f.wine_dir();
    let wine = f.wine();

    // A branch whose own-set is NOT its first-parent tail: `amb` carries A
    // then B on the old core tip, and B's patch is cherry-picked into the new
    // parent. The cherry own-set is then {A} alone, but the last first-parent
    // commit is B -- deriving `old_base = amb~1` would silently drop A's
    // position, so the verb must refuse rather than guess.
    git(&wd, &["checkout", "-q", "-b", "amb", "soda-glorious~1"]);
    std::fs::write(wd.join("amb-a.c"), "a\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "amb: A"]);
    let a = wine.rev_parse("amb").unwrap().unwrap();
    std::fs::write(wd.join("amb-b.c"), "b\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "amb: B"]);
    let b = wine.rev_parse("amb").unwrap().unwrap();
    git(&wd, &["checkout", "-q", "soda-core"]);
    // `cherry-pick` has no `-q` (the mkfixture.sh lesson); chatty is fine here.
    git(&wd, &["cherry-pick", "--no-edit", &b]);

    f.m.tier.push(sodactl_core::manifest::Tier {
        name: "amb".into(),
        parent: "soda-core".into(),
        remote: None,
    });

    let err = restack::plan_restack(&f.m, &wine, "amb", false).unwrap_err();
    match err.downcast_ref::<restack::RestackRefusal>().expect("a typed, downcastable refusal") {
        restack::RestackRefusal::AmbiguousOldParent { tier, own, first_parent, mismatch_at } => {
            assert_eq!(tier, "amb");
            assert_eq!(own, &vec![a.clone()], "only A is missing upstream");
            assert_eq!(*mismatch_at, 0, "amb~0 (B) is not in the own set");
            assert_eq!(first_parent, &vec![b.clone()], "the evidence names the mismatching commit");
        }
        other => panic!("wrong refusal: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.starts_with("refused: "), "the greppable prefix: {msg}");
    assert!(msg.contains(&a[..11]) && msg.contains(&b[..11]), "evidence in the message: {msg}");
});

fixture_test!(all_descendants_walks_parents_before_children_and_converges, |mut f| {
    let wd = f.wine_dir();
    let wine = f.wine();

    // A grandchild tier on the stale child: `deep` sits correctly on
    // soda-glorious's CURRENT tip, so it is only inherited-stale -- until the
    // batch restacks glorious, at which point it becomes directly stale. The
    // batch must order glorious first and converge in two applies.
    git(&wd, &["checkout", "-q", "-b", "deep", "soda-glorious"]);
    std::fs::write(wd.join("deep.c"), "d\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "deep: only commit"]);
    git(&wd, &["checkout", "-q", "master"]);
    f.m.tier.push(sodactl_core::manifest::Tier {
        name: "deep".into(),
        parent: "soda-glorious".into(),
        remote: None,
    });

    let plans = restack::plan_restack(&f.m, &wine, "soda-glorious", true).unwrap();
    assert_eq!(
        plans.iter().map(|p| p.tier.as_str()).collect::<Vec<_>>(),
        vec!["soda-glorious", "deep"],
        "parents strictly before children"
    );
    assert!(!plans[0].provisional);
    assert!(
        plans[1].provisional,
        "deep's preview is derived against a parent tip the batch is about to move"
    );

    // The apply loop as the CLI drives it: apply the first, re-derive, repeat.
    let sup = f.superproject();
    let wine_mut = GitMut::new(&wd);
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);
    let mut applied = Vec::new();
    loop {
        let cur = restack::plan_restack(&f.m, &wine, "soda-glorious", true).unwrap();
        let Some(rp) = cur.first() else { break };
        restack::apply_restack(rp, &ctx).unwrap();
        applied.push(rp.tier.clone());
        assert!(applied.len() <= 2, "must converge");
    }
    assert_eq!(applied, vec!["soda-glorious", "deep"]);
    assert!(wine.is_ancestor("soda-core", "soda-glorious").unwrap());
    assert!(wine.is_ancestor("soda-glorious", "deep").unwrap(), "deep followed its parent");
    assert_eq!(wine.count_commits("soda-glorious..deep").unwrap(), 1);
});

/// Run git for fixture INSPECTION, returning stdout. The assert-success `git`
/// above is for surgery; this one is for reading evidence back.
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---- stitch -----------------------------------------------------------------

fixture_test!(stitch_rebuilds_a_stale_stitch_and_verifies_by_patch_id, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(f.wine_dir());
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);

    // The fixture's soda-stale-full was stitched before soda-ntdll existed:
    // the real stale-stitch state, with real cherry-picked (non-ancestor)
    // commits.
    let old_tip = wine.rev_parse("soda-stale-full").unwrap().unwrap();
    let base_tip = wine.rev_parse("soda-core").unwrap().unwrap();

    let sp = stitch::plan_stitch(&f.m, &wine, "soda-stale-full").unwrap();
    assert_eq!(sp.base_tip, base_tip);
    assert_eq!(
        sp.picks.iter().map(|p| p.tier.as_str()).collect::<Vec<_>>(),
        vec!["soda-glorious", "soda-ntdll"],
        "the manifest's declared order, verbatim"
    );
    // soda-glorious is the fixture's stale child: the stitch still plans it,
    // but the review must say the inputs are stale.
    assert!(sp.picks[0].stale && !sp.picks[1].stale);
    assert!(sp.plan.notes.iter().any(|n| n.contains("linearise stale inputs")), "{:?}", sp.plan.notes);
    // Reviewable surface: -B from the base tip, then oid-pinned pick ranges.
    assert_eq!(sp.plan.steps[0].argv, vec!["checkout", "-B", "soda-stale-full", &base_tip]);
    assert_eq!(
        sp.plan.steps[1].argv,
        vec!["cherry-pick", &format!("{}..{}", sp.picks[0].old_base, sp.picks[0].tip)]
    );
    let (backup_ref, backup_tip) = sp.backup.as_ref().expect("an existing stitch gets a backup");
    assert_eq!(backup_tip, &old_tip);

    let out = stitch::apply_stitch(&sp, &ctx).unwrap();
    assert_ne!(out.new_tip, old_tip, "the stitch was rebuilt");
    assert!(wine.is_ancestor("soda-core", "soda-stale-full").unwrap());
    assert_eq!(wine.count_commits("soda-core..soda-stale-full").unwrap(), 2);
    // The postcondition's own facts, re-derived here: every tier's own
    // commits are patch-present in the fresh stitch.
    for p in &sp.picks {
        assert_eq!(
            wine.missing_by_patch_id("soda-stale-full", &p.tier, &p.old_base).unwrap(),
            Vec::<String>::new(),
            "`{}` must be fully carried",
            p.tier
        );
    }
    assert_eq!(wine.rev_parse_raw(backup_ref).unwrap().unwrap(), old_tip, "backup at the old tip");
    assert!(out.outcome.journal.is_file());

    // An unknown stitch name is an error naming the manifest, not a guess.
    assert!(stitch::plan_stitch(&f.m, &wine, "soda-nope").is_err());
});

// ---- tag --------------------------------------------------------------------

fixture_test!(tag_refuses_collisions_honours_equivalence_and_cuts_annotated, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(f.wine_dir());
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);
    let policy = Policy::from_manifest(&f.m);
    let tip = wine.rev_parse("spritzvalve").unwrap().unwrap();

    // The family/base derivation: spritzvalve's base `proton_11.0` carries
    // 11.0; spritzwine's base `master` carries nothing, so no honest name
    // exists and the verb says so instead of inventing one.
    let err = tagverb::plan_tag(&f.m, &wine, &policy, "spritzwine", 1, false).unwrap_err();
    assert!(
        matches!(err.downcast_ref::<tagverb::TagRefusal>(), Some(tagverb::TagRefusal::UnderivableBase { .. })),
        "{err}"
    );

    // Collision: spritzvalve-11.0-1 already names the tip; cutting -2 there
    // without a declared equivalence would ship version 1's bits as 2.
    let err = tagverb::plan_tag(&f.m, &wine, &policy, "spritzvalve", 2, false).unwrap_err();
    match err.downcast_ref::<tagverb::TagRefusal>().expect("typed refusal") {
        tagverb::TagRefusal::Collision { existing, .. } => assert_eq!(existing, "spritzvalve-11.0-1"),
        other => panic!("wrong refusal: {other:?}"),
    }
    assert!(err.to_string().starts_with("refused: "), "{err}");

    // Exists: the same NAME again is a different refusal with a different fix.
    let err = tagverb::plan_tag(&f.m, &wine, &policy, "spritzvalve", 1, false).unwrap_err();
    assert!(matches!(err.downcast_ref::<tagverb::TagRefusal>(), Some(tagverb::TagRefusal::Exists { .. })), "{err}");

    // A declared equivalence is BRANCHING.md's intended same-commit roll: the
    // collision arm goes quiet and the plan records the declaration.
    let mut eq = Policy::from_manifest(&f.m);
    eq.tag_equivalences.push(vec!["spritzvalve-11.0-1".into(), "spritzvalve-11.0-2".into()]);
    let tp = tagverb::plan_tag(&f.m, &wine, &eq, "spritzvalve", 2, false).unwrap();
    assert_eq!(tp.tag, "spritzvalve-11.0-2");
    assert_eq!(tp.commit, tip);
    assert!(tp.plan.notes.iter().any(|n| n.contains("declared equivalent")), "{:?}", tp.plan.notes);
    assert!(tp.previous.is_none(), "create-only: expected_before is None");

    // End to end: annotated, on the commit, journaled.
    tagverb::apply_tag(&tp, &ctx).unwrap();
    assert_eq!(wine.rev_parse("spritzvalve-11.0-2").unwrap().unwrap(), tip);
    assert_ne!(
        wine.rev_parse_raw("refs/tags/spritzvalve-11.0-2").unwrap().unwrap(),
        tip,
        "the ref stores a TAG OBJECT -- the cut is annotated, not lightweight"
    );

    // --force on an existing name: the plan re-points, pre-verifying the OLD
    // tag object -- and BACKS IT UP under the `tags/` subject, because the
    // journal's recorded oid alone stops naming anything once gc prunes the
    // dangling object. A tag move is a ref rewrite; it gets a rewrite's backup.
    let tp = tagverb::plan_tag(&f.m, &wine, &eq, "spritzvalve", 1, true).unwrap();
    assert_eq!(tp.previous.as_deref(), Some(tip.as_str()), "the lightweight -1 stores the commit itself");
    assert!(tp.plan.steps[0].argv.contains(&"-f".to_string()));
    let (bref, btip) = tp.plan.backups.first().expect("a re-point gets a backup ref");
    assert!(bref.contains("/tags/spritzvalve-11.0-1"), "{bref}");
    assert_eq!(btip, tip.as_str(), "the backup preserves the OLD tag ref's object");
});

fixture_test!(tag_refuses_a_stale_stitch_target_unless_forced, |mut f| {
    let wine = f.wine();
    // Make the soda family derivable and point it at the STALE stitch: the
    // in-code manifest surgery pattern (mkfixture.sh stays untouched, see the
    // module header).
    f.m.repo.home_base.describes = "wine-11.12-fixture".into();
    for c in &mut f.m.consumer {
        c.soda_tip = "soda-stale-full".into();
    }
    let policy = Policy::from_manifest(&f.m);

    let err = tagverb::plan_tag(&f.m, &wine, &policy, "soda", 1, false).unwrap_err();
    match err.downcast_ref::<tagverb::TagRefusal>().expect("typed refusal") {
        tagverb::TagRefusal::StaleTarget { tag, target_ref, detail } => {
            assert_eq!(tag, "soda-11.12-1");
            assert_eq!(target_ref, "soda-stale-full");
            assert!(detail.contains("soda-ntdll"), "names the tier that moved: {detail}");
        }
        other => panic!("wrong refusal: {other:?}"),
    }

    // The operation protecting itself is overridable by the human who knows
    // better -- and the overruling is recorded in the reviewed plan.
    let tp = tagverb::plan_tag(&f.m, &wine, &policy, "soda", 1, true).unwrap();
    assert!(tp.plan.notes.iter().any(|n| n.contains("OVERRULED by --force")), "{:?}", tp.plan.notes);
});

// ---- pin --------------------------------------------------------------------

fixture_test!(pin_moves_gitlink_and_gitmodules_in_one_commit, |f| {
    let wd = f.wine_dir();
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(&wd);
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);

    // A fresh pour on the pinned consumer, tagged and pushed -- the state a
    // pin bump actually happens from.
    git(&wd, &["checkout", "-q", "spritzwine"]);
    std::fs::write(wd.join("pour.c"), "fresh\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "fresh pour"]);
    let new_tip = wine.rev_parse("spritzwine").unwrap().unwrap();
    git(&wd, &["tag", "spritzwine-11.13-1", "spritzwine"]);

    // Refusal: not a tag. `spritzwine` resolves fine as a branch, which is
    // exactly the raw-SHA-with-extra-steps mistake the verb exists to refuse.
    let err = pin::plan_pin(&f.m, &wine, &sup, "wine", "spritzwine", true).unwrap_err();
    assert!(matches!(err.downcast_ref::<pin::PinRefusal>(), Some(pin::PinRefusal::NoSuchTag { .. })), "{err}");

    // Refusal: a tag on a TIER tip -- soda without wine, named as such.
    git(&wd, &["tag", "oops-tier-tag", "soda-core"]);
    let err = pin::plan_pin(&f.m, &wine, &sup, "wine", "oops-tier-tag", true).unwrap_err();
    match err.downcast_ref::<pin::PinRefusal>().expect("typed refusal") {
        pin::PinRefusal::NotAPinnedConsumerTip { detail, .. } => {
            assert!(detail.contains("TIER `soda-core`"), "{detail}")
        }
        other => panic!("wrong refusal: {other:?}"),
    }

    // Refusal: the tag is not on the remote `.gitmodules` names -- CI's
    // "did not contain" failure, pre-detected at plan time.
    let err = pin::plan_pin(&f.m, &wine, &sup, "wine", "spritzwine-11.13-1", false).unwrap_err();
    assert!(
        matches!(err.downcast_ref::<pin::PinRefusal>(), Some(pin::PinRefusal::RemoteMissingTag { .. })),
        "{err}"
    );
    git(&wd, &["push", "-q", "codeberg", "spritzwine-11.13-1"]);

    // Refusal: another staged path would be swept into the pin commit.
    std::fs::write(f.dir.join("stray.txt"), "x\n").unwrap();
    git(&f.dir, &["add", "stray.txt"]);
    let err = pin::plan_pin(&f.m, &wine, &sup, "wine", "spritzwine-11.13-1", false).unwrap_err();
    match err.downcast_ref::<pin::PinRefusal>().expect("typed refusal") {
        pin::PinRefusal::SuperprojectStaged { paths } => assert_eq!(paths, &vec!["stray.txt".to_string()]),
        other => panic!("wrong refusal: {other:?}"),
    }
    git(&f.dir, &["restore", "--staged", "stray.txt"]);
    std::fs::remove_file(f.dir.join("stray.txt")).unwrap();

    // The clean plan: probe verified, four steps, superproject branch CAS'd.
    let head_before = sup.rev_parse("HEAD").unwrap().unwrap();
    let pp = pin::plan_pin(&f.m, &wine, &sup, "wine", "spritzwine-11.13-1", false).unwrap();
    assert_eq!(pp.consumer, "spritzwine");
    assert_eq!(pp.commit, new_tip);
    assert!(matches!(pp.probe, pin::PinProbe::Verified { .. }));
    assert_eq!(pp.plan.steps.len(), 4);
    assert_eq!(pp.plan.mutations[0].refname, "refs/heads/main");
    assert_eq!(pp.plan.mutations[0].expected_before.as_deref(), Some(head_before.as_str()));

    pin::apply_pin(&pp, &ctx, "wine").unwrap();
    // One commit, both halves.
    assert_eq!(sup.count_commits(&format!("{head_before}..HEAD")).unwrap(), 1);
    let touched = git_out(&f.dir, &["show", "--pretty=format:", "--name-only", "HEAD"]);
    assert!(touched.contains(".gitmodules") && touched.contains("wine"), "{touched}");
    assert_eq!(sup.rev_parse_raw("HEAD:wine").unwrap().unwrap(), new_tip);
    assert_eq!(
        sup.config_file_get(".gitmodules", "submodule.wine.branch").unwrap().as_deref(),
        Some("spritzwine-11.13-1")
    );
    assert!(
        git_out(&f.dir, &["log", "-1", "--format=%s"]).trim().contains("spritzwine: pin"),
        "the existing superproject message style"
    );
    // The wine checkout sits detached at the tag -- what `git add wine` read.
    assert_eq!(wine.head_branch().unwrap(), None);
    assert_eq!(wine.rev_parse("HEAD").unwrap().unwrap(), new_tip);

    // Re-planning is the honest no-op.
    let again = pin::plan_pin(&f.m, &wine, &sup, "wine", "spritzwine-11.13-1", true).unwrap();
    assert!(again.plan.steps.is_empty());
    assert!(again.plan.notes.iter().any(|n| n.contains("already pins")), "{:?}", again.plan.notes);
});

// ---- push --print -----------------------------------------------------------

fixture_test!(pushprint_lists_only_what_differs_and_mutates_nothing, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let policy = Policy::from_manifest(&f.m);

    // The read-only promise, checked against the repos themselves: every ref
    // in both repositories is byte-identical across the derivation.
    let before_wine = git_out(&f.wine_dir(), &["for-each-ref"]);
    let before_sup = git_out(&f.dir, &["for-each-ref"]);
    let pr = pushprint::plan_push(&f.m, &policy, &wine, &sup, "wine").unwrap();
    assert_eq!(git_out(&f.wine_dir(), &["for-each-ref"]), before_wine, "wine refs untouched");
    assert_eq!(git_out(&f.dir, &["for-each-ref"]), before_sup, "superproject refs untouched");

    let commands: Vec<&str> = pr.items.iter().map(|i| i.command.as_str()).collect();
    // Tiers: soda-ntdll's tracking ref was deleted (first push) and soda-demo
    // gained an unpushed commit; the in-sync tiers must NOT appear.
    assert!(
        commands.contains(&"git -C wine push --force-with-lease codeberg soda-ntdll soda-demo"),
        "{commands:?}"
    );
    // Consumers: never pushed anywhere -- derived onto the tiers' remote.
    assert!(
        commands.contains(&"git -C wine push --force-with-lease codeberg spritzwine spritzvalve"),
        "{commands:?}"
    );
    // The consumer tag is absent from the fixture remote: a plain push.
    assert!(commands.contains(&"git -C wine push codeberg spritzvalve-11.0-1"), "{commands:?}");
    assert!(
        !commands.iter().any(|c| c.contains("soda-core") || c.contains("soda-glorious") || c.contains("soda-build")),
        "in-sync tiers must not be listed: {commands:?}"
    );
    // Everything underivable is a NOTE, never a silent omission.
    assert!(pr.notes.iter().any(|n| n.contains("soda-missing")), "{:?}", pr.notes);
    assert!(
        pr.notes.iter().any(|n| n.contains("`main` has no tracking ref")),
        "the superproject branch cannot be derived here and must say so: {:?}",
        pr.notes
    );
    // Wine before superproject, structurally.
    let last_wine = pr.items.iter().rposition(|i| matches!(i.repo, plan::RepoSide::Wine));
    let first_sup = pr.items.iter().position(|i| matches!(i.repo, plan::RepoSide::Superproject));
    if let (Some(w), Some(s)) = (last_wine, first_sup) {
        assert!(w < s, "wine first, then the superproject");
    }
});

// ---- regen ------------------------------------------------------------------

/// The regen deps as the CLI wires them (fixture manifests keep the default
/// `tools/rr-cache`).
fn regen_deps<'a>(f: &'a Fixture, wine_cli: &'a GitCli, manifest: &'a PathBuf) -> regen::RegenDeps<'a> {
    regen::RegenDeps {
        wine_cli,
        manifest_path: manifest,
        tracked_cache: f.dir.join("tools/rr-cache"),
        rr_cache_rel: "tools/rr-cache",
    }
}

fixture_test!(regen_repours_writes_the_ledger_and_cuts_provenance, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(f.wine_dir());
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);
    let manifest_path = f.dir.join("tools/sodactl.toml");

    let home = f.m.repo.home_base.commit.clone();
    let soda_tip = wine.rev_parse("soda-full").unwrap().unwrap();
    let old_tip = wine.rev_parse("spritzvalve").unwrap().unwrap();

    // The reviewable surface: regenerate.sh's mechanics, verbatim. No fetch
    // step -- the fixture base `proton_11.0` names no declared remote, and
    // the plan must say so instead of guessing one.
    let rp = regen::plan_regen(&f.m, &wine, "spritzvalve").unwrap();
    assert_eq!(rp.plan.steps[0].argv, vec!["config", "rerere.enabled", "true"]);
    assert_eq!(rp.plan.steps[1].argv, vec!["config", "rerere.autoupdate", "true"]);
    assert_eq!(rp.plan.steps[2].argv, vec!["checkout", "-B", "spritzvalve", &soda_tip]);
    assert_eq!(
        rp.plan.steps[3].argv,
        vec!["rebase", "--onto", "proton_11.0", &home, "spritzvalve"],
        "the base by REF (a fetch may move it), the home base from the manifest"
    );
    assert_eq!(rp.replay_count, wine.count_commits(&format!("{home}..{soda_tip}")).unwrap());
    assert!(rp.plan.notes.iter().any(|n| n.contains("no fetch step")), "{:?}", rp.plan.notes);
    let (_, backup_tip) = rp.backup.as_ref().expect("an existing consumer gets a backup");
    assert_eq!(backup_tip, &old_tip);

    let super_head = sup.rev_parse("HEAD").unwrap().unwrap();
    let out = regen::apply_regen(&rp, &ctx, &regen_deps(&f, &wine, &manifest_path)).unwrap();

    // The pour: fresh oids on the declared base, carrying the replay.
    assert_ne!(out.new_tip, old_tip, "a re-pour mints new commits");
    assert!(wine.is_ancestor("proton_11.0", "spritzvalve").unwrap());
    assert!(out.auto_resolved.is_empty(), "disjoint files -- nothing for rules to do");
    assert_eq!(out.rr_commit, None, "no conflicts -> no resolutions -> no capture commit");
    assert_eq!(sup.rev_parse("HEAD").unwrap().unwrap(), super_head, "superproject untouched");

    // The ledger: written into the manifest FILE, comments intact, parseable.
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(text.contains("# GENERATED by tests/mkfixture.sh"), "the generator banner survived");
    let m2 = Manifest::parse(&text).unwrap();
    let g = m2.ledger.get("spritzvalve").expect("a ledger entry exists");
    assert_eq!(g.result, out.new_tip);
    assert_eq!(g.base_commit, wine.rev_parse("proton_11.0").unwrap().unwrap());
    assert_eq!(g.soda_commit, soda_tip);
    assert_eq!(g.home_base, home);

    // The provenance tag: annotated, on the result, mirroring the entry.
    assert!(out.gen_tag.starts_with("gen/spritzvalve/"), "{}", out.gen_tag);
    assert_eq!(wine.rev_parse(&out.gen_tag).unwrap().unwrap(), out.new_tip);
    assert_ne!(
        wine.rev_parse_raw(&format!("refs/tags/{}", out.gen_tag)).unwrap().unwrap(),
        out.new_tip,
        "the ref stores a TAG OBJECT -- annotated, not lightweight"
    );

    // Currency is now DECIDABLE: the ledgered consumer goes quiet while the
    // ledger-less one keeps its honest unknown.
    let policy = Policy::from_manifest(&m2);
    let codes = |m: &Manifest| -> Vec<(String, String)> {
        sodactl_core::checks::consumers::check(m, &wine, &policy)
            .unwrap()
            .into_iter()
            .map(|x| (x.code.to_string(), x.subject.clone()))
            .collect()
    };
    let c = codes(&m2);
    assert!(
        !c.contains(&("CONSUMER_CURRENCY_UNKNOWN".into(), "spritzvalve".into())),
        "{c:?}"
    );
    assert!(
        c.contains(&("CONSUMER_CURRENCY_UNKNOWN".into(), "spritzwine".into())),
        "the ledger-less consumer must stay unknown: {c:?}"
    );
    assert!(!c.iter().any(|(code, _)| code.starts_with("LEDGER_") || code == "CONSUMER_STALE_BY_LEDGER"), "{c:?}");

    // Move an input (the stitch): stale-by-ledger fires, naming it.
    git(&f.wine_dir(), &["checkout", "-q", "soda-full"]);
    std::fs::write(f.wine_dir().join("late.c"), "late\n").unwrap();
    git(&f.wine_dir(), &["add", "-A"]);
    git(&f.wine_dir(), &["commit", "-qm", "soda-full: late work"]);
    let c = codes(&m2);
    assert!(c.contains(&("CONSUMER_STALE_BY_LEDGER".into(), "spritzvalve".into())), "{c:?}");

    // Commit ON the consumer: the tip contradicts the recorded result.
    git(&f.wine_dir(), &["checkout", "-q", "spritzvalve"]);
    std::fs::write(f.wine_dir().join("stray.c"), "s\n").unwrap();
    git(&f.wine_dir(), &["add", "-A"]);
    git(&f.wine_dir(), &["commit", "-qm", "authored on a consumer (the mistake)"]);
    let c = codes(&m2);
    assert!(c.contains(&("LEDGER_CONTRADICTS_TIP".into(), "spritzvalve".into())), "{c:?}");

    // A NEWER provenance tag pointing elsewhere: trust neither.
    git(
        &f.wine_dir(),
        &["tag", "-a", "-m", "forged", "gen/spritzvalve/29991231T000000Z", "soda-core"],
    );
    let c = codes(&m2);
    assert!(c.contains(&("LEDGER_CONTRADICTS_TAG".into(), "spritzvalve".into())), "{c:?}");
});

fixture_test!(regen_stops_on_uncovered_binary_then_declared_rules_pour, |mut f| {
    let wd = f.wine_dir();
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(&wd);
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);
    let manifest_path = f.dir.join("tools/sodactl.toml");

    // Both bases gain the same two paths with different content: a binary
    // icon (NUL bytes) and a generated text file -- the live repo's exact
    // conflict classes (oic_winlogo.ico / configure).
    git(&wd, &["checkout", "-q", "proton_11.0"]);
    std::fs::write(wd.join("logo.ico"), b"ICON\x00VALVE\x01").unwrap();
    std::fs::write(wd.join("configure"), "base-generated\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "valve: blobs"]);
    git(&wd, &["checkout", "-q", "-b", "soda-bin", "soda-full"]);
    std::fs::write(wd.join("logo.ico"), b"ICON\x00TIER\x02").unwrap();
    std::fs::write(wd.join("configure"), "tier-generated\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "soda-bin: blobs"]);
    git(&wd, &["checkout", "-q", "master"]);
    f.m.consumer.push(sodactl_core::manifest::Consumer {
        name: "spritzbin".into(),
        base: "proton_11.0".into(),
        soda_tip: "soda-bin".into(),
        pinned_by_superproject: false,
        exclude: vec![],
    });
    // A rule scoped to a DIFFERENT consumer must not enter this pour's scope.
    f.m.binary_resolution.push(sodactl_core::manifest::BinaryResolution {
        path: "configure".into(),
        strategy: "delete".into(),
        reason: "someone else's call".into(),
        consumers: vec!["spritzwine".into()],
    });

    let rp = regen::plan_regen(&f.m, &wine, "spritzbin").unwrap();
    assert!(rp.rules.is_empty(), "the foreign-scoped rule must not apply: {:?}", rp.rules);
    let err = regen::apply_regen(&rp, &ctx, &regen_deps(&f, &wine, &manifest_path)).unwrap_err();
    match err.downcast_ref::<regen::RegenStop>().expect("a typed terminal stop") {
        regen::RegenStop::UncoveredBinary { path, .. } => assert_eq!(path, "logo.ico"),
        other => panic!("wrong stop: {other:?}"),
    }
    // The guidance: blob extraction for both sides, the exact TOML offer, and
    // NO auto-edit of the manifest.
    let msg = err.to_string();
    assert!(msg.contains(":2:logo.ico") && msg.contains(":3:logo.ico"), "{msg}");
    assert!(msg.contains("[[binary_resolution]]") && msg.contains("MANUAL manifest edit"), "{msg}");
    assert!(
        sodactl_core::treestate::in_flight(&wine.git_dir().unwrap()).is_some(),
        "the stop leaves the replay in flight for inspection"
    );
    git(&wd, &["rebase", "--abort"]);

    // Declare the decisions (the offer, accepted) and re-run: the pour must
    // now drive straight through both conflicts.
    f.m.binary_resolution.push(sodactl_core::manifest::BinaryResolution {
        path: "logo.ico".into(),
        strategy: "ours".into(),
        reason: "the tier owns the icon".into(),
        consumers: vec![],
    });
    f.m.binary_resolution.push(sodactl_core::manifest::BinaryResolution {
        path: "configure".into(),
        strategy: "delete".into(),
        reason: "generated; the pour must not carry it".into(),
        consumers: vec!["spritzbin".into()],
    });
    let rp = regen::plan_regen(&f.m, &wine, "spritzbin").unwrap();
    assert_eq!(rp.rules.len(), 2);
    let out = regen::apply_regen(&rp, &ctx, &regen_deps(&f, &wine, &manifest_path)).unwrap();
    let mut resolved = out.auto_resolved.clone();
    resolved.sort();
    assert_eq!(
        resolved,
        vec![("configure".to_string(), "delete".to_string()), ("logo.ico".to_string(), "ours".to_string())]
    );
    // Strategy semantics, checked against CONTENT: "ours" = the TIER's blob
    // (git's --theirs during the replay -- the inversion the mapping comment
    // exists for), "delete" = the path is gone.
    assert_eq!(
        wine.rev_parse_raw("spritzbin:logo.ico").unwrap().unwrap(),
        wine.rev_parse_raw("soda-bin:logo.ico").unwrap().unwrap(),
        "the tier's icon bytes won"
    );
    assert!(wine.rev_parse_raw("spritzbin:configure").unwrap().is_none(), "configure stayed deleted");
    assert!(wine.is_ancestor("proton_11.0", "spritzbin").unwrap());
    assert!(Manifest::parse(&std::fs::read_to_string(&manifest_path).unwrap())
        .unwrap()
        .ledger
        .contains_key("spritzbin"));
});

fixture_test!(regen_stops_on_text_conflicts_then_rerere_captures_and_commits, |mut f| {
    let wd = f.wine_dir();
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(&wd);
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);
    let manifest_path = f.dir.join("tools/sodactl.toml");

    // Both sides append a different second line to wine.c: a real text
    // conflict WITH a common ancestor stage, which is what rerere records.
    git(&wd, &["checkout", "-q", "proton_11.0"]);
    std::fs::write(wd.join("wine.c"), "wine: fork point\nvalve: touch\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "valve: touch wine.c"]);
    git(&wd, &["checkout", "-q", "-b", "soda-txt", "soda-full"]);
    std::fs::write(wd.join("wine.c"), "wine: fork point\nsoda: touch\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "soda-txt: touch wine.c"]);
    git(&wd, &["checkout", "-q", "master"]);
    f.m.consumer.push(sodactl_core::manifest::Consumer {
        name: "spritztxt".into(),
        base: "proton_11.0".into(),
        soda_tip: "soda-txt".into(),
        pinned_by_superproject: false,
        exclude: vec![],
    });

    // First pour: the terminal Text stop, with the plan doc's exact next
    // moves in the message.
    let rp = regen::plan_regen(&f.m, &wine, "spritztxt").unwrap();
    let err = regen::apply_regen(&rp, &ctx, &regen_deps(&f, &wine, &manifest_path)).unwrap_err();
    match err.downcast_ref::<regen::RegenStop>().expect("a typed terminal stop") {
        regen::RegenStop::Text { paths, .. } => assert_eq!(paths, &vec!["wine.c".to_string()]),
        other => panic!("wrong stop: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("git -C wine add wine.c"), "{msg}");
    assert!(msg.contains("git -C wine rebase --continue"), "{msg}");
    assert!(msg.contains("sodactl regen spritztxt --apply"), "{msg}");

    // The human resolves and continues -- rerere (enabled by the plan's
    // config steps) records the postimage on the way through.
    std::fs::write(wd.join("wine.c"), "wine: fork point\nvalve: touch\nsoda: touch\n").unwrap();
    git(&wd, &["add", "wine.c"]);
    git(&wd, &["rebase", "--continue"]);

    // Second pour, per the guidance: rerere replays the resolution, the loop
    // continues through the known conflict, and the NEW resolution is
    // persisted AND committed -- rr-cache paths only.
    let super_head = sup.rev_parse("HEAD").unwrap().unwrap();
    let rp = regen::plan_regen(&f.m, &wine, "spritztxt").unwrap();
    let out = regen::apply_regen(&rp, &ctx, &regen_deps(&f, &wine, &manifest_path)).unwrap();
    assert!(wine.is_ancestor("proton_11.0", "spritztxt").unwrap());
    assert_eq!(
        std::fs::read_to_string(wd.join("wine.c")).unwrap(),
        "wine: fork point\nvalve: touch\nsoda: touch\n",
        "rerere replayed the recorded resolution"
    );

    let rr_commit = out.rr_commit.expect("the learned resolution must be committed");
    assert_eq!(sup.rev_parse("HEAD").unwrap().unwrap(), rr_commit);
    assert_eq!(sup.count_commits(&format!("{super_head}..HEAD")).unwrap(), 1);
    let subject = git_out(&f.dir, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), "sodactl: capture rerere resolutions from spritztxt regen");
    let touched = git_out(&f.dir, &["show", "--pretty=format:", "--name-only", "HEAD"]);
    assert!(
        touched.trim().lines().all(|p| p.starts_with("tools/rr-cache/")),
        "the capture commit is scoped to the cache: {touched}"
    );
    // The tracked cache now carries a complete (pre+post) entry.
    let survey = sodactl_core::RrSurvey::read(f.dir.join("tools/rr-cache")).unwrap();
    assert!(!survey.missing && !survey.entries.is_empty());
    assert!(survey.entries.iter().all(|e| e.has_postimage), "{survey:?}");
    // And the ledger landed for the second pour, leaving the manifest edit
    // uncommitted for review.
    assert!(Manifest::parse(&std::fs::read_to_string(&manifest_path).unwrap())
        .unwrap()
        .ledger
        .contains_key("spritztxt"));
});

// A detached superproject HEAD refuses BEFORE anything runs -- rr gc's
// dangling-commit refusal, applied to regen's own superproject commit (the
// rr-cache capture). Nothing may have moved: not the consumer, not the
// rr-cache seed, no journal.
fixture_test!(regen_refuses_a_detached_superproject, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(f.wine_dir());
    let super_mut = GitMut::new(&f.dir);
    let ctx = apply_ctx(&wine_mut, &super_mut, &wine, &sup, &f);
    let manifest_path = f.dir.join("tools/sodactl.toml");
    git(&f.dir, &["checkout", "-q", "--detach", "HEAD"]);

    let old_tip = wine.rev_parse("spritzvalve").unwrap().unwrap();
    let rp = regen::plan_regen(&f.m, &wine, "spritzvalve").unwrap();
    let err = regen::apply_regen(&rp, &ctx, &regen_deps(&f, &wine, &manifest_path)).unwrap_err();
    assert!(
        matches!(err.downcast_ref::<regen::RegenRefusal>(), Some(regen::RegenRefusal::DetachedSuperproject)),
        "{err}"
    );
    assert!(err.to_string().starts_with("refused: "), "{err}");
    assert_eq!(wine.rev_parse("spritzvalve").unwrap().unwrap(), old_tip, "nothing moved");
    assert!(
        !wine.git_dir().unwrap().join("sodactl/journal").exists(),
        "refused before the engine -- no journal claiming a started run"
    );
});

// ---- release ----------------------------------------------------------------

fixture_test!(release_checklist_derives_each_items_status_live, |f| {
    let wine = f.wine();
    let sup = f.superproject();
    let policy = Policy::from_manifest(&f.m);

    // spritzvalve 1 is already cut: tag Done; no pin item (the consumer is
    // not pinned_by_superproject); the lanes are honestly Unknown; push has
    // real commands pending.
    let rr = release::plan_release(&f.m, &policy, &wine, &sup, "wine", "spritzvalve", 1).unwrap();
    assert_eq!(rr.tag.as_deref(), Some("spritzvalve-11.0-1"));
    let names: Vec<&str> = rr.items.iter().map(|i| i.name).collect();
    assert_eq!(names, vec!["tag", "build", "package", "smoke", "push"], "no pin step for an unpinned consumer");
    let item = |n: &str| rr.items.iter().find(|i| i.name == n).unwrap();
    assert!(matches!(item("tag").status, release::ItemStatus::Done), "{:?}", item("tag"));
    assert!(matches!(item("build").status, release::ItemStatus::Unknown));
    // Stage 6 made the lane wrappers real, so the checklist recommends them --
    // and the old raw-script hint was wrong anyway: package.sh has no
    // `--version` flag (it reads VERSION from the environment), which is
    // exactly what `sodactl package --version` routes for you.
    assert_eq!(item("build").commands, vec!["sodactl build"]);
    assert_eq!(item("package").commands, vec!["sodactl package --version spritzvalve-11.0-1"]);
    assert!(matches!(item("push").status, release::ItemStatus::Pending));
    assert!(!item("push").commands.is_empty(), "the push commands travel with the checklist");

    // Version 2 does not exist: tag Pending with the exact verb to run.
    let rr = release::plan_release(&f.m, &policy, &wine, &sup, "wine", "spritzvalve", 2).unwrap();
    let tag = rr.items.iter().find(|i| i.name == "tag").unwrap();
    assert!(matches!(tag.status, release::ItemStatus::Pending));
    assert_eq!(tag.commands, vec!["sodactl tag spritzvalve 2 --apply"]);

    // spritzwine IS pinned but its base derives no wine version: the tag item
    // is honestly Unknown and the pin item exists, blocked on it.
    let rr = release::plan_release(&f.m, &policy, &wine, &sup, "wine", "spritzwine", 1).unwrap();
    assert_eq!(rr.tag, None);
    let names: Vec<&str> = rr.items.iter().map(|i| i.name).collect();
    assert_eq!(names, vec!["tag", "pin", "build", "package", "smoke", "push"]);
    assert!(matches!(rr.items[0].status, release::ItemStatus::Unknown));
    assert!(matches!(rr.items[1].status, release::ItemStatus::Unknown));
    assert!(rr.items[1].detail.contains("blocked on the tag step"), "{}", rr.items[1].detail);
});
