//! The whole authored-history cycle, end to end, through the PUBLIC verb APIs
//! -- one fixture, one story: the parent tier gains a commit, its children are
//! restacked, the stitch is rebuilt from the moved tiers, the consumer is
//! re-poured (through a real text conflict, resolved once by hand and then
//! replayed by rerere), the release is tagged, pinned into the superproject,
//! push-printed, and finally the restack is undone from its backup ref.
//!
//! `verbs_real.rs` proves each verb's edges in isolation; this file proves the
//! HANDOFFS -- that what one verb leaves behind is exactly what the next one
//! derives from. Every intermediate assertion is a `GitRead` query (patch-id
//! sets, ancestry, ref values, pin state), never a comparison on rendered
//! porcelain.
//!
//! The fixture/skip helpers are duplicated from `verbs_real.rs` rather than
//! shared: integration-test binaries cannot share a module without a
//! `#[path]` include, and including that file would re-run its whole suite
//! inside this binary.

use sodactl_core::checks::{Policy, Workspace};
use sodactl_core::plan::{self, ApplyCtx, RepoSide};
use sodactl_core::verbs::{pin, pushprint, regen, restack, stitch, tagverb, undo};
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
    let dir = std::env::temp_dir().join(format!("sodactl-e2e-test-{name}-{}", std::process::id()));
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
        git(&repo, &["config", "user.email", "e2e@test"]);
        git(&repo, &["config", "user.name", "e2e"]);
    }
    let m = Manifest::parse(&std::fs::read_to_string(dir.join("tools/sodactl.toml")).ok()?)
        .expect("fixture manifest must parse");
    Some(Fixture { dir, m })
}

/// Every ref that matters, read through `GitRead` -- the mutates-nothing
/// assertions compare these snapshots, not rendered ref listings.
fn ref_snapshot(g: &GitCli) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for b in g.local_branches().unwrap() {
        let r = format!("refs/heads/{b}");
        let oid = g.rev_parse_raw(&r).unwrap().expect("listed branch must resolve");
        v.push((r, oid));
    }
    for (t, oid) in g.tags_with_targets().unwrap() {
        v.push((format!("refs/tags/{t}"), oid));
    }
    v.sort();
    v
}

#[test]
fn the_full_release_cycle_hands_off_between_every_verb() {
    let Some(mut f) = fixture("cycle") else {
        eprintln!(
            "skipping the_full_release_cycle_hands_off_between_every_verb: no fixture (need bash+git)"
        );
        return;
    };
    let wd = f.wine_dir();
    let wine = f.wine();
    let sup = f.superproject();
    let wine_mut = GitMut::new(&wd);
    let super_mut = GitMut::new(&f.dir);
    let ctx = ApplyCtx {
        wine: &wine_mut,
        superproject: &super_mut,
        wine_read: &wine,
        superproject_read: &sup,
        wine_path: f.m.repo.wine.clone(),
    };
    let manifest_path = f.dir.join("tools/sodactl.toml");
    let home = f.m.repo.home_base.commit.clone();

    // ---- act 1: history is authored ----------------------------------------
    // The parent tier gains a commit that ALSO touches wine.c -- the same file
    // the vendor base is about to touch, so the eventual pour meets a real
    // text conflict (the live repo's everyday collision class), not just the
    // clean-replay best case.
    git(&wd, &["checkout", "-q", "soda-core"]);
    std::fs::write(wd.join("core-e2e.c"), "authored\n").unwrap();
    std::fs::write(wd.join("wine.c"), "wine: fork point\nsoda: authored line\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "soda-core: e2e authored work"]);
    let core_tip = wine.rev_parse("soda-core").unwrap().unwrap();

    git(&wd, &["checkout", "-q", "proton_11.0"]);
    std::fs::write(wd.join("wine.c"), "wine: fork point\nvalve: vendored line\n").unwrap();
    git(&wd, &["add", "-A"]);
    git(&wd, &["commit", "-qm", "valve: touch wine.c"]);
    git(&wd, &["checkout", "-q", "master"]);

    let glorious_before = wine.rev_parse("soda-glorious").unwrap().unwrap();
    let ntdll_before = wine.rev_parse("soda-ntdll").unwrap().unwrap();

    // The same gate the CLI runs before any mutating verb.
    let ws = Workspace {
        superproject: &sup,
        wine: &wine,
        superproject_path: f.dir.clone(),
        wine_path: f.m.repo.wine.clone(),
    };
    plan::preflight(&f.m, &ws).unwrap();

    // ---- act 2: restack the subtree under the moved parent ------------------
    // Both children went stale the moment the parent moved; the batch driver
    // is the CLI's loop -- re-derive, apply the first, repeat until quiet.
    let first = restack::plan_restack(&f.m, &wine, "soda-core", true).unwrap();
    assert_eq!(
        first.iter().map(|p| p.tier.as_str()).collect::<Vec<_>>(),
        vec!["soda-glorious", "soda-ntdll"],
        "the parent's move stales exactly its children, parents-before-children"
    );
    let mut restacked: Vec<(String, String, String)> = Vec::new(); // (tier, backup ref, old tip)
    loop {
        let plans = restack::plan_restack(&f.m, &wine, "soda-core", true).unwrap();
        let Some(rp) = plans.first() else { break };
        let out = restack::apply_restack(rp, &ctx).unwrap();

        // The postcondition's facts, re-derived HERE through GitRead alone.
        assert!(wine.is_ancestor("soda-core", &rp.tier).unwrap(), "`{}` moved onto the new parent", rp.tier);
        assert_eq!(
            wine.count_commits(&format!("soda-core..{}", rp.tier)).unwrap() as usize,
            rp.own.len(),
            "`{}` carries exactly its own commits",
            rp.tier
        );
        // Patch-id set equality as its two subset directions: nothing the old
        // tip carried was dropped, nothing on the new tip was fabricated. The
        // backup ref IS the old tip, so it anchors both queries.
        assert_eq!(
            wine.missing_by_patch_id(&rp.tier, &rp.backup.0, &rp.old_base).unwrap(),
            Vec::<String>::new(),
            "every pre-restack patch of `{}` is present after the rebase",
            rp.tier
        );
        assert_eq!(
            wine.missing_by_patch_id(&rp.backup.0, &rp.tier, &rp.parent_tip).unwrap(),
            Vec::<String>::new(),
            "the rebased `{}` fabricated no patch",
            rp.tier
        );
        assert_eq!(wine.rev_parse_raw(&rp.backup.0).unwrap().as_deref(), Some(rp.backup.1.as_str()));
        assert!(out.outcome.journal.is_file(), "the journal was written before the rewrite");
        restacked.push((rp.tier.clone(), rp.backup.0.clone(), rp.backup.1.clone()));
        assert!(restacked.len() <= 2, "the batch must converge");
    }
    assert_eq!(
        restacked.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        vec!["soda-glorious", "soda-ntdll"]
    );
    assert_eq!(restacked[0].2, glorious_before, "the backup preserved the pre-restack tip");
    assert_eq!(restacked[1].2, ntdll_before);

    // ---- act 3: rebuild the stitch from the restacked tiers -----------------
    let full_before = wine.rev_parse("soda-full").unwrap().unwrap();
    let sp = stitch::plan_stitch(&f.m, &wine, "soda-full").unwrap();
    assert_eq!(sp.base_tip, core_tip, "the stitch starts from the moved parent's tip");
    assert!(
        sp.picks.iter().all(|p| !p.stale),
        "the restack left no stale inputs to linearise: {:?}",
        sp.picks
    );
    let full_tip = stitch::apply_stitch(&sp, &ctx).unwrap().new_tip;
    assert_ne!(full_tip, full_before, "the stitch was rebuilt");
    assert!(wine.is_ancestor("soda-core", "soda-full").unwrap());
    assert_eq!(
        wine.count_commits("soda-core..soda-full").unwrap() as usize,
        sp.picks.iter().map(|p| p.own.len()).sum::<usize>()
    );
    for p in &sp.picks {
        assert_eq!(
            wine.missing_by_patch_id("soda-full", &p.tier, &p.old_base).unwrap(),
            Vec::<String>::new(),
            "`{}`'s own patches are all carried by the fresh stitch",
            p.tier
        );
    }
    let (backup_ref, backup_tip) = sp.backup.as_ref().expect("an existing stitch gets a backup");
    assert_eq!(backup_tip, &full_before);
    assert_eq!(wine.rev_parse_raw(backup_ref).unwrap().as_deref(), Some(full_before.as_str()));

    // ---- act 4: re-pour the consumer through the conflict -------------------
    let valve_before = wine.rev_parse("spritzvalve").unwrap().unwrap();
    let deps = regen::RegenDeps {
        wine_cli: &wine,
        manifest_path: &manifest_path,
        tracked_cache: f.dir.join("tools/rr-cache"),
        rr_cache_rel: "tools/rr-cache",
    };

    // First pour: the authored wine.c line meets the vendored one -- the
    // typed terminal Text stop, replay left in flight for the human.
    let rp = regen::plan_regen(&f.m, &wine, "spritzvalve").unwrap();
    assert_eq!(rp.soda_tip, full_tip, "the plan pins the FRESH stitch tip act 3 produced");
    let err = regen::apply_regen(&rp, &ctx, &deps).unwrap_err();
    match err.downcast_ref::<regen::RegenStop>().expect("a typed terminal stop") {
        regen::RegenStop::Text { paths, .. } => assert_eq!(paths, &vec!["wine.c".to_string()]),
        other => panic!("wrong stop: {other:?}"),
    }

    // The human's one move, exactly as the stop message instructs: merge both
    // lines and continue. rerere (enabled by the plan's config steps) records
    // the postimage on the way through.
    std::fs::write(wd.join("wine.c"), "wine: fork point\nvalve: vendored line\nsoda: authored line\n")
        .unwrap();
    git(&wd, &["add", "wine.c"]);
    git(&wd, &["rebase", "--continue"]);

    // No sleep between the pours, deliberately: backup stamps carry
    // sub-second precision (`plan::backup_stamp`), so a same-second re-pour
    // of the same branch mints a distinct create-only refname. This used to
    // need a 1100ms sleep -- its absence is the regression test.

    // Second pour, per the guidance: rerere replays the resolution, the pour
    // drives through, and the cycle's bookkeeping lands.
    let super_head = sup.rev_parse("HEAD").unwrap().unwrap();
    let rp = regen::plan_regen(&f.m, &wine, "spritzvalve").unwrap();
    let out = regen::apply_regen(&rp, &ctx, &deps).unwrap();
    let valve_tip = out.new_tip.clone();
    assert_ne!(valve_tip, valve_before, "a re-pour mints new commits");
    assert!(wine.is_ancestor("proton_11.0", "spritzvalve").unwrap());
    assert_eq!(
        std::fs::read_to_string(wd.join("wine.c")).unwrap(),
        "wine: fork point\nvalve: vendored line\nsoda: authored line\n",
        "rerere replayed the recorded resolution"
    );
    // The pour carries the whole soda delta by patch-id EXCEPT the conflicted
    // commit, whose patch legitimately changed when the resolution merged the
    // vendored line in -- the missing set names exactly that commit.
    assert_eq!(
        wine.missing_by_patch_id("spritzvalve", "soda-full", &home).unwrap(),
        vec![core_tip.clone()],
        "only the resolved conflict's patch-id differs from the stitch"
    );

    // The ledger: written into the manifest FILE, comments intact, parseable,
    // recording exactly the inputs the pour consumed.
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(text.contains("# GENERATED by tests/mkfixture.sh"), "the generator banner survived");
    let m2 = Manifest::parse(&text).unwrap();
    let g = m2.ledger.get("spritzvalve").expect("a ledger entry exists");
    assert_eq!(g.result, valve_tip);
    assert_eq!(g.soda_commit, full_tip);
    assert_eq!(g.home_base, home);
    assert_eq!(g.base_commit, wine.rev_parse("proton_11.0").unwrap().unwrap());

    // The provenance tag: annotated, on the result.
    assert!(out.gen_tag.starts_with("gen/spritzvalve/"), "{}", out.gen_tag);
    assert_eq!(wine.rev_parse(&out.gen_tag).unwrap().unwrap(), valve_tip);
    assert_ne!(
        wine.rev_parse_raw(&format!("refs/tags/{}", out.gen_tag)).unwrap().unwrap(),
        valve_tip,
        "the ref stores a TAG OBJECT -- annotated, not lightweight"
    );

    // The learned resolution was captured: one superproject commit, and the
    // tracked cache now carries a complete (pre+post) entry.
    let rr_commit = out.rr_commit.expect("the resolved conflict must be committed to the cache");
    assert_eq!(sup.rev_parse("HEAD").unwrap().unwrap(), rr_commit);
    assert_eq!(sup.count_commits(&format!("{super_head}..HEAD")).unwrap(), 1);
    let survey = sodactl_core::RrSurvey::read(f.dir.join("tools/rr-cache")).unwrap();
    assert!(!survey.missing && survey.entries.iter().any(|e| e.has_postimage), "{survey:?}");

    // ---- act 5: cut the release tag on the fresh pour -----------------------
    let policy = Policy::from_manifest(&f.m);
    let tp = tagverb::plan_tag(&f.m, &wine, &policy, "spritzvalve", 2, false).unwrap();
    assert_eq!(tp.tag, "spritzvalve-11.0-2");
    assert_eq!(tp.commit, valve_tip, "the tag lands on what act 4 poured");
    tagverb::apply_tag(&tp, &ctx).unwrap();
    assert_eq!(wine.rev_parse("spritzvalve-11.0-2").unwrap().unwrap(), valve_tip);
    assert_ne!(
        wine.rev_parse_raw("refs/tags/spritzvalve-11.0-2").unwrap().unwrap(),
        valve_tip,
        "annotated, not lightweight"
    );

    // ---- act 6: the push list -- derived, printed, never executed -----------
    // Run at the moment the human would run it: the release is tagged but
    // nothing has been pushed yet.
    let snap_wine = ref_snapshot(&wine);
    let snap_sup = ref_snapshot(&sup);
    let pr = pushprint::plan_push(&f.m, &policy, &wine, &sup, "wine").unwrap();
    assert_eq!(ref_snapshot(&wine), snap_wine, "wine refs untouched by the derivation");
    assert_eq!(ref_snapshot(&sup), snap_sup, "superproject refs untouched by the derivation");
    let commands: Vec<&str> = pr.items.iter().map(|i| i.command.as_str()).collect();
    // History rewrites travel with a lease: the restacked tiers and the
    // re-poured consumers all moved off their remote.
    assert!(
        commands.iter().any(|c| c.contains("--force-with-lease")
            && c.contains("soda-glorious")
            && c.contains("soda-ntdll")),
        "{commands:?}"
    );
    assert!(
        commands.iter().any(|c| c.contains("--force-with-lease")
            && c.split_whitespace().any(|w| w == "spritzvalve")),
        "{commands:?}"
    );
    // The fresh release tag names the consumer tip and is absent from the
    // remote: a plain (no-lease) push. The superseded -1 tag no longer names
    // any release surface and must NOT be listed -- a published tag is never
    // moved from here.
    assert!(
        commands.iter().any(|c| !c.contains("--force-with-lease") && c.contains("spritzvalve-11.0-2")),
        "{commands:?}"
    );
    assert!(!commands.iter().any(|c| c.contains("spritzvalve-11.0-1")), "{commands:?}");
    // Wine before superproject, structurally.
    let last_wine = pr.items.iter().rposition(|i| matches!(i.repo, RepoSide::Wine));
    let first_sup = pr.items.iter().position(|i| matches!(i.repo, RepoSide::Superproject));
    if let (Some(w), Some(s)) = (last_wine, first_sup) {
        assert!(w < s, "wine first, then the superproject");
    }

    // ---- act 7: bump the superproject pin to the tagged pour ----------------
    // The fixture's gitlink pins spritzwine; this cycle released spritzvalve.
    // One gitlink means one pinned consumer (mkfixture.sh's own comment), so
    // the in-code surgery MOVES the flag rather than pretending two pointers
    // exist -- the same pattern verbs_real.rs uses for topology extensions.
    for c in &mut f.m.consumer {
        c.pinned_by_superproject = c.name == "spritzvalve";
    }
    // The remote probe is CI's "did not contain" failure pre-detected at plan
    // time; the tag must reach the declared remote first -- the human running
    // act 6's printed command, played here against the throwaway bare remote.
    git(&wd, &["push", "-q", "codeberg", "spritzvalve-11.0-2"]);
    let head_before = sup.rev_parse("HEAD").unwrap().unwrap();
    let pp = pin::plan_pin(&f.m, &wine, &sup, "wine", "spritzvalve-11.0-2", false).unwrap();
    assert_eq!(pp.consumer, "spritzvalve");
    assert!(matches!(pp.probe, pin::PinProbe::Verified { .. }));
    pin::apply_pin(&pp, &ctx, "wine").unwrap();
    assert_eq!(sup.count_commits(&format!("{head_before}..HEAD")).unwrap(), 1, "one commit, both halves");
    assert_eq!(sup.rev_parse_raw("HEAD:wine").unwrap().unwrap(), valve_tip);
    assert_eq!(
        sup.config_file_get(".gitmodules", "submodule.wine.branch").unwrap().as_deref(),
        Some("spritzvalve-11.0-2")
    );
    // Pin state read back the way the checks read it: through the wine repo's
    // own superproject probe, committed and not merely staged.
    let pin_state = wine.superproject_pin().unwrap();
    assert_eq!(pin_state.effective(), Some(valve_tip.as_str()));
    assert!(!pin_state.is_staged(), "the pin is committed, not staged: {pin_state:?}");
    assert_eq!(wine.head_branch().unwrap(), None, "wine sits detached at the tag, as build.sh will find it");

    // ---- act 8: undo restores the pre-restack tip ----------------------------
    let listing = undo::list_backups(&wine).unwrap();
    let b = listing
        .iter()
        .find(|b| b.branch == "soda-glorious")
        .expect("act 2's restack backup is still listed");
    assert_eq!(b.tip, glorious_before);
    assert!(b.moved, "the listing knows the branch has moved since");
    let up = undo::plan_undo(&wine, &format!("{}/{}", b.utc, b.branch)).unwrap();
    // CAS against the LIVE (rebased) tip, read at plan time.
    let rebased = wine.rev_parse_raw("refs/heads/soda-glorious").unwrap().unwrap();
    assert_eq!(up.mutations[0].expected_before.as_deref(), Some(rebased.as_str()));
    plan::apply(&up, &ctx).unwrap();
    assert_eq!(
        wine.rev_parse_raw("refs/heads/soda-glorious").unwrap().unwrap(),
        glorious_before,
        "the branch is byte-identically back at its pre-restack tip"
    );
    // The loop is closed: the cycle's own first verb sees the restored state
    // and would derive the same repair again.
    let replan = restack::plan_restack(&f.m, &wine, "soda-core", true).unwrap();
    assert_eq!(replan.len(), 1, "{replan:?}");
    assert_eq!(replan[0].tier, "soda-glorious");
    assert_eq!(replan[0].own.len(), 1);
}
