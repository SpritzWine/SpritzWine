//! Discover where Steam and Lutris look for, respectively, compatibility tools and Wine
//! runners. We only ever write under `$HOME`, so no candidate needs elevated privileges.
//!
//! Detection is intentionally forgiving: a root is offered whenever its *base* (the Steam
//! or Lutris data dir) exists, even if the leaf (`compatibilitytools.d` / `runners/wine`)
//! does not yet -- the installer creates the leaf on demand. Symlinked aliases (e.g.
//! `~/.steam/steam`, `~/.steam/root` and `~/.local/share/Steam` often point at one place)
//! are collapsed by canonicalising the longest existing ancestor.

use std::path::{Path, PathBuf};

/// A Steam installation we can register a compatibility tool into.
#[derive(Debug, Clone)]
pub struct SteamRoot {
    /// Human label, e.g. `"Steam"` or `"Steam (Flatpak)"`.
    pub label: String,
    /// The `compatibilitytools.d` directory (may not exist yet).
    pub compat_dir: PathBuf,
}

/// A Lutris installation we can register a Wine runner into.
#[derive(Debug, Clone)]
pub struct LutrisRoot {
    /// Human label, e.g. `"Lutris"` or `"Lutris (Flatpak)"`.
    pub label: String,
    /// The `runners/wine` directory (may not exist yet).
    pub runners_wine_dir: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Canonicalise the longest existing prefix of `p` and re-append the rest, so that
/// symlinked aliases that resolve to the same place share a key even when the leaf is
/// missing.
fn dedupe_key(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if existing.exists() {
            if let Ok(c) = existing.canonicalize() {
                let mut key = c;
                for r in rest.iter().rev() {
                    key.push(r);
                }
                return key;
            }
            return p.to_path_buf();
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Detect Steam compatibility-tool directories.
///
/// If `SPRITZWINE_STEAM_DIR` is set it overrides discovery entirely and is treated as the
/// `compatibilitytools.d` directory (used by tests/CI to avoid touching a real Steam).
pub fn detect_steam_roots() -> Vec<SteamRoot> {
    if let Some(dir) = std::env::var_os("SPRITZWINE_STEAM_DIR") {
        return vec![SteamRoot {
            label: "Steam (override)".into(),
            compat_dir: PathBuf::from(dir),
        }];
    }

    let h = home();
    // (label, Steam data dir). compat_dir = <base>/compatibilitytools.d.
    let candidates: [(&str, PathBuf); 4] = [
        ("Steam", h.join(".local/share/Steam")),
        ("Steam", h.join(".steam/steam")),
        ("Steam", h.join(".steam/root")),
        (
            "Steam (Flatpak)",
            h.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        ),
    ];

    let mut out: Vec<SteamRoot> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (label, base) in candidates {
        if !base.is_dir() {
            continue;
        }
        let compat_dir = base.join("compatibilitytools.d");
        let key = dedupe_key(&compat_dir);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(SteamRoot {
            label: label.to_string(),
            compat_dir,
        });
    }
    out
}

/// Detect Lutris Wine-runner directories.
///
/// `SPRITZWINE_LUTRIS_DIR`, if set, overrides discovery and is treated as the
/// `runners/wine` directory.
pub fn detect_lutris_roots() -> Vec<LutrisRoot> {
    if let Some(dir) = std::env::var_os("SPRITZWINE_LUTRIS_DIR") {
        return vec![LutrisRoot {
            label: "Lutris (override)".into(),
            runners_wine_dir: PathBuf::from(dir),
        }];
    }

    let h = home();
    let candidates: [(&str, PathBuf); 2] = [
        ("Lutris", h.join(".local/share/lutris")),
        (
            "Lutris (Flatpak)",
            h.join(".var/app/net.lutris.Lutris/data/lutris"),
        ),
    ];

    let mut out: Vec<LutrisRoot> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for (label, base) in candidates {
        if !base.is_dir() {
            continue;
        }
        let runners_wine_dir = base.join("runners/wine");
        let key = dedupe_key(&runners_wine_dir);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(LutrisRoot {
            label: label.to_string(),
            runners_wine_dir,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_short_circuits_steam() {
        // SAFETY: single-threaded test; restore afterwards.
        std::env::set_var("SPRITZWINE_STEAM_DIR", "/tmp/sw-steam-x");
        let roots = detect_steam_roots();
        std::env::remove_var("SPRITZWINE_STEAM_DIR");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].compat_dir, PathBuf::from("/tmp/sw-steam-x"));
    }

    #[test]
    fn env_override_short_circuits_lutris() {
        std::env::set_var("SPRITZWINE_LUTRIS_DIR", "/tmp/sw-lutris-x");
        let roots = detect_lutris_roots();
        std::env::remove_var("SPRITZWINE_LUTRIS_DIR");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].runners_wine_dir, PathBuf::from("/tmp/sw-lutris-x"));
    }

    #[test]
    fn dedupe_collapses_symlinked_aliases() {
        // a real dir and a symlink pointing at it should share a dedupe key.
        let base = std::env::temp_dir().join(format!("sw-dedupe-{}", std::process::id()));
        let real = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(&real).unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            dedupe_key(&link.join("compatibilitytools.d")),
            dedupe_key(&real.join("compatibilitytools.d"))
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
