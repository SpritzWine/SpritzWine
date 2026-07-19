//! `[policy]` -- the declared judgement calls.
//!
//! These assert the two halves of one contract. Declaring the section must
//! actually override the compiled-in values (otherwise the manifest is
//! decorative and the decisions still live in the binary), and OMITTING it must
//! reproduce them exactly (otherwise moving policy out of code was a behaviour
//! change wearing a refactor's clothes, and every finding this tool has ever
//! emitted is silently up for renegotiation).
//!
//! No git anywhere: `Policy::from_manifest` is a pure function of the manifest,
//! and a test that needed a repo to check a parse would be testing the wrong
//! thing.

use sodactl_core::{Manifest, Policy, SkewPolicy};

/// The minimum a manifest needs to parse. Every test below appends to it, so
/// the thing under test is the only thing that varies.
const SPINE: &str = r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "abc"
describes = "d"
derivation = "x"
"#;

fn policy_of(extra: &str) -> Policy {
    let m = Manifest::parse(&format!("{SPINE}{extra}")).expect("manifest must parse");
    Policy::from_manifest(&m)
}

/// The fallback, stated as an assertion rather than as a comment.
///
/// If someone edits `Policy::default`, this test is what tells them they have
/// changed the behaviour of every manifest that predates `[policy]` -- which is
/// a decision, not a cleanup.
#[test]
fn no_policy_section_reproduces_the_compiled_in_defaults() {
    let p = policy_of("");
    let d = Policy::default();
    assert_eq!(p.tag_aliases, d.tag_aliases);
    assert_eq!(p.tag_equivalences, d.tag_equivalences);
    assert_eq!(p.ignore_branches, d.ignore_branches);
    assert_eq!(p.family_skew, d.family_skew);
    assert_eq!(p.rr_cache, d.rr_cache);
    // The pre-`[policy]` values, spelled out. Comparing only against
    // `Default::default()` would pass just as happily if both sides were
    // emptied, which is the one regression this test exists to catch.
    assert_eq!(p.tag_aliases, ["soda-all", "spritzwine-archive"]);
    assert_eq!(p.ignore_branches, ["spritzvalve-full"]);
    assert_eq!(p.family_skew, SkewPolicy::Drift);
    assert_eq!(p.rr_cache, "tools/rr-cache");
    assert_eq!(
        p.tag_equivalences,
        vec![
            vec!["spritzvalve-11.0-1".to_string(), "spritzvalve-11.0-3".to_string()],
            vec!["soda-all".to_string(), "soda-11.12-1".to_string()],
        ]
    );
}

/// Round-trip: every declared key reaches the checks, and none of it is quietly
/// merged with the defaults.
#[test]
fn a_declared_policy_overrides_every_default() {
    let p = policy_of(
        r#"
[policy]
tag_aliases = ["only-this"]
ignore_branches = ["scratch/experiment"]
family_skew = "blocker"
rr_cache = "elsewhere/rr"

[[policy.tag_equivalence]]
tags = ["a-1.0-1", "a-1.0-2"]
reason = "the family rolled and nothing needed re-pouring"
"#,
    );
    assert_eq!(p.tag_aliases, ["only-this"]);
    assert_eq!(p.ignore_branches, ["scratch/experiment"]);
    assert_eq!(p.family_skew, SkewPolicy::Blocker);
    assert_eq!(p.rr_cache, "elsewhere/rr");
    assert_eq!(p.tag_equivalences, vec![vec!["a-1.0-1".to_string(), "a-1.0-2".to_string()]]);
}

/// The section is the switch, not each key.
///
/// A present `[policy]` that omits a list means EMPTY, not "and also whatever
/// the binary believes". Per-key fallback would make "we recognise no tag
/// aliases" inexpressible, and would put policy back in the binary for exactly
/// the keys nobody thought to write down -- the failure this whole section
/// exists to end.
#[test]
fn an_omitted_list_inside_a_present_section_is_empty_not_defaulted() {
    let p = policy_of("[policy]\nrr_cache = \"tools/rr-cache\"\n");
    assert!(p.tag_aliases.is_empty(), "{:?}", p.tag_aliases);
    assert!(p.ignore_branches.is_empty(), "{:?}", p.ignore_branches);
    assert!(p.tag_equivalences.is_empty(), "{:?}", p.tag_equivalences);
    // ...while the two scalars, which have no meaningful empty value, keep
    // theirs.
    assert_eq!(p.family_skew, SkewPolicy::Drift);
    assert_eq!(p.rr_cache, "tools/rr-cache");
}

/// An unrecognised severity is a parse ERROR naming the file, never a silent
/// fallback. Collapsing an unknown into a definite value is the one thing
/// `checks/` forbids everywhere else, and a typo'd `family_skew` that quietly
/// downgraded to Drift would be that mistake in the config layer.
#[test]
fn an_unrecognised_family_skew_is_rejected() {
    let e = Manifest::parse(&format!("{SPINE}[policy]\nfamily_skew = \"warn\"\n"))
        .expect_err("a bogus severity must not parse");
    assert!(e.to_string().contains("family_skew"), "error must name the key: {e}");
}

/// Excludes are a property of the consumer, not a global list keyed by its name.
#[test]
fn a_per_consumer_exclude_is_honoured() {
    let p = policy_of(
        r#"
[[consumer]]
name = "spritzvalve"
base = "valve-wine/proton_11.0"
soda_tip = "soda-branding"
exclude = ["soda-fsync"]

[[consumer]]
name = "spritzwine"
base = "wine-11.13"
soda_tip = "soda-full"
"#,
    );
    // Exactly one entry: a consumer that declares nothing excludes nothing, and
    // must not appear carrying an empty list that later reads as "checked".
    assert_eq!(p.consumer_excludes, vec![("spritzvalve".to_string(), vec!["soda-fsync".to_string()])]);
}

/// The reason a keyed list had to go, pinned as a test.
///
/// Excludes ride on the consumer entry, so they cannot be inherited by a
/// same-named consumer in a different manifest -- which is precisely what the
/// test fixture's coincidentally-named `spritzvalve` used to do, silently
/// passing on the live repo's policy.
#[test]
fn a_consumer_that_declares_nothing_excludes_nothing() {
    let p = policy_of(
        r#"
[[consumer]]
name = "spritzvalve"
base = "proton_11.0"
soda_tip = "soda-full"
"#,
    );
    assert!(p.consumer_excludes.is_empty(), "{:?}", p.consumer_excludes);
}
