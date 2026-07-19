//! What every tool call needs: the manifest, and a git handle rooted at wine.
//!
//! The manifest is read once at startup; git is read live on every call. That
//! asymmetry is the domain model, not an optimisation -- git is the only
//! authority on where refs are, so no SHA may be cached across a call boundary.
//! The manifest is policy, reviewed like code, and changing it means restarting
//! the server, which is the same ceremony as changing any other config.
//!
//! This is also why no tool takes a `refresh` argument. The plan sketched one;
//! it would be a no-op that implies a cache exists, and a knob that lies about
//! the design is worse than no knob.

use anyhow::{Context as _, Result};
use sodactl_core::{Manifest, Policy};
use sodactl_git::GitHybrid;
use std::path::{Path, PathBuf};

pub struct Ctx {
    pub manifest: Manifest,
    /// Resolved once, beside the manifest it is declared in. Policy has exactly
    /// the manifest's lifetime and exactly its restart ceremony; re-deriving it
    /// per call would invite someone to make it reloadable and quietly break
    /// the asymmetry the module header states.
    pub policy: Policy,
    /// Rooted at the wine submodule -- where every ref in the tier DAG lives.
    /// The hybrid backend, same as the CLI: server and CLI must never answer
    /// one question through two different backends.
    pub git: GitHybrid,
    /// The superproject root. The rr-cache is tracked here, not in wine.
    pub root: PathBuf,
    pub manifest_path: PathBuf,
}

impl Ctx {
    /// Resolve the same way the CLI does: the manifest sits at
    /// `<root>/tools/sodactl.toml`, and `repo.wine` is relative to `<root>`.
    /// Keeping the derivation identical is what stops `sodactl status` and
    /// `sodactl_status` from ever describing two different repositories.
    pub fn load(manifest_path: impl AsRef<Path>) -> Result<Self> {
        let manifest_path = manifest_path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading manifest {}", manifest_path.display()))?;
        let manifest = Manifest::parse(&text).context("parsing manifest")?;

        let root = manifest_path
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let git = GitHybrid::new(root.join(&manifest.repo.wine));
        let policy = Policy::from_manifest(&manifest);

        Ok(Self { manifest, policy, git, root, manifest_path })
    }

    /// The tracked rerere cache. It lives in the SUPERPROJECT and is seeded into
    /// `wine/.git/rr-cache` by `regenerate.sh` -- reading the live `.git` copy
    /// instead would report one developer's scratch state as if it were the
    /// committed policy.
    ///
    /// The path comes from policy, not from a literal here. It was a literal,
    /// duplicating the CLI's `policy.rr_cache`, which meant `sodactl rr-health`
    /// and `sodactl_rr_health` would silently survey two different directories
    /// the moment anyone relocated the cache.
    pub fn rr_cache_dir(&self) -> PathBuf {
        self.root.join(&self.policy.rr_cache)
    }
}
