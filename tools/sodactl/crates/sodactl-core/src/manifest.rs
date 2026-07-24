//! The manifest: what git cannot know.
//!
//! The rule that governs this file: **git is the only authority on where refs
//! are.** The manifest declares policy, topology edges and the handful of magic
//! constants that are genuinely unrecoverable. It never declares geometry --
//! no commit counts, no "this tier has N commits". Those are observations, and
//! an observation written into a config file is a lie waiting to happen while a
//! destructive operation depends on it.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub schema: u32,
    pub repo: Repo,
    #[serde(default)]
    pub remotes: BTreeMap<String, String>,
    /// Build products that must not trip the dirty-tree guard.
    #[serde(default)]
    pub build_artifacts: Vec<String>,
    #[serde(default)]
    pub tier: Vec<Tier>,
    #[serde(default)]
    pub stitch: Vec<Stitch>,
    #[serde(default)]
    pub consumer: Vec<Consumer>,
    /// Conflicts rerere is structurally incapable of learning.
    #[serde(default)]
    pub binary_resolution: Vec<BinaryResolution>,
    /// Declared judgement calls. `None` -- the section absent entirely -- is the
    /// documented fallback to `checks::Policy::default()`; see `PolicyDecl`.
    pub policy: Option<PolicyDecl>,
    /// The generation ledger: `[ledger.<consumer>]`, written by `sodactl
    /// regen` on success, never hand-edited. Source of truth #3 -- a CLAIM to
    /// be verified against git, never a fact (`checks/consumers.rs` does the
    /// verifying, and emits LEDGER_CONTRADICTS_* when the claim fails).
    ///
    /// A top-level table keyed by consumer name, NOT a field under
    /// `[[consumer]]`: array-of-tables entries are positional, so a surgical
    /// `toml_edit` write would have to find "the [[consumer]] whose name is X"
    /// by scanning -- and a scan that miscounts writes the ledger onto the
    /// wrong consumer. `[ledger.<name>]` is addressable by the one key that
    /// identifies it.
    #[serde(default)]
    pub ledger: BTreeMap<String, Generation>,
}

/// One recorded generation: the inputs a consumer was last poured from and
/// what came out. Every field is an observation frozen at pour time; comparing
/// them to live values is what makes consumer currency decidable at all.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Generation {
    /// `2026-07-24T12:00:00Z` -- when the pour finished.
    pub at: String,
    /// What the declared base resolved to.
    pub base_commit: String,
    /// What the declared soda tip resolved to.
    pub soda_commit: String,
    /// The home base the replay excluded (the manifest constant at pour time).
    pub home_base: String,
    /// The consumer tip the pour produced.
    pub result: String,
    /// The sodactl version that wrote this entry.
    pub sodactl: String,
}

/// The `[policy]` section: the decisions that are not facts about the graph.
///
/// Everything here was previously a hardcoded Rust default, which contradicted
/// this file's own governing rule -- the manifest declares policy, git is the
/// authority on state -- by deciding policy in code where no reviewer sees it.
///
/// **Presence of the section is the switch, not presence of each key.** A
/// manifest with no `[policy]` at all gets `Policy::default()` wholesale, so
/// this move cannot change a single existing finding. A manifest that HAS the
/// section is authoritative for the lists in it: an omitted list means *empty*,
/// never "and also whatever the binary believes". Anything else makes it
/// impossible to declare "we recognise no tag aliases", and re-creates in a
/// subtler form the exact problem this section exists to remove. The two
/// scalars (`family_skew`, `rr_cache`) have no meaningful empty value and so
/// keep their default when omitted.
#[derive(Debug, Deserialize, Serialize)]
pub struct PolicyDecl {
    /// Base-free tag names that are OURS. Declared, never pattern-guessed: a
    /// guess that misses reads the tag as `Foreign`, and every alias silently
    /// vanishes from every comparison rather than failing loudly.
    #[serde(default)]
    pub tag_aliases: Vec<String>,
    /// Local branches that are documented artifacts rather than debris.
    #[serde(default)]
    pub ignore_branches: Vec<String>,
    /// Severity for `TAG_FAMILY_SKEW`. Parsed by serde rather than mapped by
    /// hand so an unrecognised value is a manifest parse error naming the key,
    /// not a silent fallback -- collapsing "unknown" into a definite value is
    /// the one thing `checks/` forbids everywhere else.
    #[serde(default = "default_family_skew")]
    pub family_skew: crate::checks::SkewPolicy,
    /// Where the tracked rr-cache lives, relative to the superproject.
    #[serde(default = "default_rr_cache")]
    pub rr_cache: String,
    #[serde(default)]
    pub tag_equivalence: Vec<TagEquivalence>,
}

fn default_family_skew() -> crate::checks::SkewPolicy {
    crate::checks::SkewPolicy::Drift
}

fn default_rr_cache() -> String {
    "tools/rr-cache".to_string()
}

/// Tags that are *intended* to name one commit.
///
/// A table with a mandatory `reason`, in the shape of `[[binary_resolution]]`
/// and for the same reason: an equivalence suppresses a `TAG_COLLISION`, and a
/// suppression with no recorded justification is indistinguishable from someone
/// having muted an inconvenient finding.
#[derive(Debug, Deserialize, Serialize)]
pub struct TagEquivalence {
    pub tags: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Repo {
    /// Superproject root, relative to the manifest.
    pub superproject: String,
    /// The wine submodule, relative to the superproject.
    pub wine: String,
    pub home_base: HomeBase,
}

/// The commit the soda tiers are authored on -- the exclusive start of every
/// replayed range. This is the one constant that is NOT derivable: it is
/// emphatically not `git merge-base <wine-base> <soda-tip>`, which for a
/// cross-base replay is the common wine ancestor down around 11.0 and would
/// replay thousands of upstream commits onto Valve's tree.
///
/// Stored with its `describes` and `derivation` so a reviewer can tell a
/// deliberate bump from a typo.
#[derive(Debug, Deserialize, Serialize)]
pub struct HomeBase {
    pub commit: String,
    pub describes: String,
    pub derivation: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Tier {
    pub name: String,
    /// The ONLY declared geometry. `$home_base` means the fork base.
    pub parent: String,
    #[serde(default)]
    pub remote: Option<String>,
}

/// A linearization of a tier subset. Order is load-bearing: `soda-ntdll` and
/// `soda-fsync` both live in `dlls/ntdll/unix/`.
#[derive(Debug, Deserialize, Serialize)]
pub struct Stitch {
    pub name: String,
    pub base_tier: String,
    pub order: Vec<String>,
    /// Local-only scratch branches are not expected to have a remote.
    #[serde(default)]
    pub local_only: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Consumer {
    pub name: String,
    /// The wine base this is replayed ONTO.
    pub base: String,
    /// The soda tip replayed onto it: a tier or a stitch.
    pub soda_tip: String,
    /// Set when the superproject pins this consumer. A pinned consumer MUST be
    /// tagged -- consumers are force-pushed, and an untagged pinned SHA falls
    /// out of the branch and stops being fetchable.
    #[serde(default)]
    pub pinned_by_superproject: bool,
    /// Tiers deliberately kept OUT of this consumer's replay.
    ///
    /// Per-consumer rather than a global list keyed by consumer name, which is
    /// what this was in code. A keyed list can silently fail to match -- and
    /// worse, silently match the wrong thing: the test fixture's own
    /// coincidentally-named `spritzvalve` inherited the live repo's exclusions
    /// from a hardcoded default it never declared. Here an exclusion cannot
    /// exist without a consumer to attach to, and cannot attach to a consumer
    /// the manifest does not define, because the parser would have to invent
    /// one.
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BinaryResolution {
    pub path: String,
    /// "ours" = the tier's blob wins; "base" = the wine base wins;
    /// "delete" = the path stays deleted.
    pub strategy: String,
    pub reason: String,
    /// Narrow to specific consumers when the rule is not universal.
    #[serde(default)]
    pub consumers: Vec<String>,
}

impl Manifest {
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    pub fn tier(&self, name: &str) -> Option<&Tier> {
        self.tier.iter().find(|t| t.name == name)
    }

    /// Every ref the manifest models, for unmodelled-branch detection.
    pub fn modelled_refs(&self) -> Vec<&str> {
        self.tier
            .iter()
            .map(|t| t.name.as_str())
            .chain(self.stitch.iter().map(|s| s.name.as_str()))
            .chain(self.consumer.iter().map(|c| c.name.as_str()))
            .collect()
    }
}

/// The superproject root, derived from where the manifest sits
/// (`<root>/tools/sodactl.toml` -- two `parent()` hops up).
///
/// This existed twice in `sodactl-cli` and the TUI would have been the third
/// copy; a third copy of a subtle path derivation is how one of them quietly
/// loses the filter below.
///
/// The `.filter(|p| !p.as_os_str().is_empty())` is not defensive noise, it is
/// the whole reason the DEFAULT invocation works. `Path::new("tools/sodactl.toml")
/// .parent()` is `"tools"` and *its* parent is `Some("")` -- not `None`. So the
/// `unwrap_or(".")` never fired, `root` became the empty path, and every
/// `Command::current_dir("")` failed with a bare ENOENT. `sodactl status` from
/// the repo root -- the one invocation the docs give -- crashed, and only an
/// absolute `--manifest` ever worked.
pub fn workspace_root(manifest: &std::path::Path) -> std::path::PathBuf {
    manifest
        .parent()
        .and_then(|p| p.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf()
}

/// Write (or overwrite) one `[ledger.<consumer>]` entry into the manifest
/// file, preserving everything else byte-for-byte.
///
/// `toml_edit`, never `toml`: the live manifest is ~70% comments, every one a
/// recorded decision, and a `toml::to_string` round-trip would erase them all
/// while still producing a file that parses -- the worst kind of data loss,
/// the invisible kind. This is the ONLY manifest write in the workspace; the
/// `[[binary_resolution]]` offer in regen's conflict guidance deliberately
/// prints a snippet instead of calling this, because a rule is a reviewed
/// policy decision and a ledger entry is a recorded observation.
///
/// The rewritten text is re-parsed as a `Manifest` BEFORE the file is
/// touched: a write that would leave the manifest unreadable to the next
/// invocation must fail here, with the old file intact.
pub fn write_ledger_entry(
    path: &std::path::Path,
    consumer: &str,
    g: &Generation,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut =
        text.parse().with_context(|| format!("parsing manifest {}", path.display()))?;

    let fresh = !doc.contains_key("ledger");
    let ledger = doc
        .entry("ledger")
        .or_insert_with(|| {
            // Implicit: the file gains exactly `[ledger.<name>]`, never a bare
            // `[ledger]` header with nothing under it.
            let mut t = toml_edit::Table::new();
            t.set_implicit(true);
            toml_edit::Item::Table(t)
        })
        .as_table_mut()
        .with_context(|| "`ledger` exists in the manifest but is not a table".to_string())?;

    // Whatever comment sits above the entry being replaced -- the section
    // banner below on a re-pour, or anything a human wrote there despite the
    // banner -- travels as the OLD table's prefix decor and would vanish with
    // it. Measured: the banner survived exactly one overwrite without this.
    let kept_prefix = ledger
        .get(consumer)
        .and_then(|it| it.as_table())
        .and_then(|t| t.decor().prefix())
        .and_then(|raw| raw.as_str())
        .map(str::to_string);

    let mut entry = toml_edit::Table::new();
    // Field order mirrors the `Generation` declaration / the plan doc schema.
    entry["at"] = toml_edit::value(&g.at);
    entry["base_commit"] = toml_edit::value(&g.base_commit);
    entry["soda_commit"] = toml_edit::value(&g.soda_commit);
    entry["home_base"] = toml_edit::value(&g.home_base);
    entry["result"] = toml_edit::value(&g.result);
    entry["sodactl"] = toml_edit::value(&g.sodactl);
    if fresh {
        // The banner the plan doc's example manifest carries; written once,
        // when the section first appears, then preserved by the carry above.
        entry.decor_mut().set_prefix(
            "\n# ---- the ledger. Written by `sodactl regen` on success. Never hand-edited. ----\n",
        );
    } else if let Some(p) = kept_prefix {
        entry.decor_mut().set_prefix(p);
    }
    ledger.insert(consumer, toml_edit::Item::Table(entry));

    let out = doc.to_string();
    Manifest::parse(&out)
        .with_context(|| "the ledger write would leave the manifest unparseable -- nothing was written".to_string())?;
    std::fs::write(path, out).with_context(|| format!("writing manifest {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod ledger_tests {
    use super::*;

    const COMMENTED: &str = r#"# top-of-file banner: a recorded decision
schema = 1
# build products, placed before the first table for TOML scoping
build_artifacts = ["configure"]   # inline: the autoreconf story

[repo]
superproject = "."
wine = "wine"   # the submodule path

# the one constant that is NOT derivable
[repo.home_base]
commit = "abc"
describes = "d"
derivation = "x"

[[consumer]]
name = "spritzwine"
base = "wine-11.13"
soda_tip = "soda-full"
"#;

    fn generation(result: &str) -> Generation {
        Generation {
            at: "2026-07-24T12:00:00Z".into(),
            base_commit: "base1111111".into(),
            soda_commit: "soda2222222".into(),
            home_base: "abc".into(),
            result: result.into(),
            sodactl: "0.1.0".into(),
        }
    }

    /// THE mandatory property: a ledger write must not cost a single comment.
    /// The live manifest is ~70% comments and every one is a recorded
    /// decision; the write path that erased them would still parse cleanly,
    /// which is why this is a test and not a hope.
    #[test]
    fn a_ledger_write_preserves_every_comment_and_parses_back() {
        let dir = std::env::temp_dir()
            .join(format!("sodactl-ledger-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sodactl.toml");
        std::fs::write(&path, COMMENTED).unwrap();

        write_ledger_entry(&path, "spritzwine", &generation("res3333333")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for comment in [
            "# top-of-file banner: a recorded decision",
            "# build products, placed before the first table for TOML scoping",
            "# inline: the autoreconf story",
            "# the submodule path",
            "# the one constant that is NOT derivable",
        ] {
            assert!(text.contains(comment), "comment lost by the ledger write: {comment}\n{text}");
        }
        assert!(text.contains("[ledger.spritzwine]"), "{text}");
        assert!(text.contains("Never hand-edited"), "the section banner: {text}");

        let m = Manifest::parse(&text).unwrap();
        assert_eq!(m.ledger.get("spritzwine").unwrap(), &generation("res3333333"));

        // Overwrite in place: a re-pour UPDATES the entry, it does not stack a
        // duplicate table (which would be a parse error) or a second banner.
        write_ledger_entry(&path, "spritzwine", &generation("res4444444")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("[ledger.spritzwine]").count(), 1);
        assert_eq!(text.matches("Never hand-edited").count(), 1);
        let m = Manifest::parse(&text).unwrap();
        assert_eq!(m.ledger["spritzwine"].result, "res4444444");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hard gate's parsing half: a manifest with no ledger yields an
    /// EMPTY map -- absence stays absence, so every no-ledger code path is
    /// reachable exactly as before the field existed.
    #[test]
    fn a_manifest_without_a_ledger_parses_to_an_empty_map() {
        let m = Manifest::parse(COMMENTED).unwrap();
        assert!(m.ledger.is_empty());
    }
}

#[cfg(test)]
mod workspace_root_tests {
    use super::workspace_root;
    use std::path::Path;

    /// The paid-for case: the documented relative default must yield `.`,
    /// never the empty path.
    #[test]
    fn relative_default_yields_dot() {
        assert_eq!(workspace_root(Path::new("tools/sodactl.toml")), Path::new("."));
    }

    #[test]
    fn absolute_yields_grandparent() {
        assert_eq!(
            workspace_root(Path::new("/repo/tools/sodactl.toml")),
            Path::new("/repo")
        );
    }
}
