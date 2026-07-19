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
