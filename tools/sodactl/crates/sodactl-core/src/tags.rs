//! Tag-name parsing: `<component>-<winebase>-<version>`.
//!
//! The wine repo carries **6600** tags, and all but a handful are upstream's or
//! Valve's: `wine-*`, `experimental-wine-10.0-2025…`, `proton-*`, `steamos*`,
//! `v6.3-rc`. So the one hard requirement on this parser is that it be **total
//! and non-panicking**, and that everything it does not recognise become
//! `Foreign` and produce *no finding whatsoever*. A tool that emits four
//! hundred hygiene rows about Valve's tags gets turned off on day one, and then
//! none of the real checks run either.
//!
//! Hand-rolled rather than regex: this is a host tool that must never enter the
//! Wine build's dependency graph, and the grammar is small enough that a
//! dependency would cost more than it explains.

/// The components whose tags are ours. `prater` lives in a THIRD repository
/// (the Prater fork), which v0 cannot see -- `tagcheck` says so out loud rather
/// than reporting a clean family check that examined two thirds of the family.
pub const OURS: [&str; 4] = ["soda", "spritzwine", "spritzvalve", "prater"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagName {
    /// `soda-11.12-3` -- the current scheme, the only one that participates in
    /// skew and collision arithmetic.
    Family { component: String, wine_base: String, version: u32 },
    /// `spritzwine-11.0-0.2.0` -- the pre-scheme SemVer tail. Recognised so it
    /// is not mistaken for Foreign, then excluded from every comparison: its
    /// trailing field is not a version in the same ordering.
    Legacy { component: String, wine_base: String, version: String },
    /// `soda-all`, `spritzwine-archive`. Must be MANIFEST-DECLARED. Guessing at
    /// a base-free pattern would swallow arbitrary upstream names.
    Alias { name: String },
    /// Not ours. Silent, always.
    Foreign,
}

impl TagName {
    /// `aliases` is the declared list; anything else base-free is Foreign.
    pub fn parse(tag: &str, aliases: &[String]) -> TagName {
        if aliases.iter().any(|a| a == tag) {
            return TagName::Alias { name: tag.to_string() };
        }

        // Split from the RIGHT: the component is a known literal, the base
        // contains a dot, the version is the tail. Splitting from the left
        // would mis-handle a component that is a prefix of another.
        let Some((head, tail)) = tag.rsplit_once('-') else { return TagName::Foreign };
        let Some((component, wine_base)) = head.split_once('-') else { return TagName::Foreign };

        if !OURS.contains(&component) {
            return TagName::Foreign;
        }
        if !is_wine_base(wine_base) {
            return TagName::Foreign;
        }

        if let Ok(version) = tail.parse::<u32>() {
            // Reject `007`-style padding rather than silently normalising it:
            // two tags that render differently but compare equal is exactly the
            // kind of thing that makes a collision check lie.
            if tail.len() > 1 && tail.starts_with('0') {
                return TagName::Foreign;
            }
            return TagName::Family {
                component: component.to_string(),
                wine_base: wine_base.to_string(),
                version,
            };
        }
        if is_semver(tail) {
            return TagName::Legacy {
                component: component.to_string(),
                wine_base: wine_base.to_string(),
                version: tail.to_string(),
            };
        }
        TagName::Foreign
    }

    /// The component, for tags that have one.
    pub fn component(&self) -> Option<&str> {
        match self {
            TagName::Family { component, .. } | TagName::Legacy { component, .. } => Some(component),
            _ => None,
        }
    }

    /// Sort key for "the newest tag in this family": `(major, minor, version)`.
    ///
    /// The wine base is part of the key, and omitting it was a live latent bug.
    /// `max_by_key(version)` parses the base and then discards it, so the moment
    /// `soda-11.13-1` is cut alongside `soda-11.12-3` the "newest" tag resolves
    /// to the OLDER base's tag (3 > 1). `TAG_ON_STALE_TARGET` would then keep
    /// measuring the superseded release and stay silent about the tag someone is
    /// actually about to ship -- an inversion at exactly the release boundary the
    /// check exists to guard. Invisible today only because every family in the
    /// repo currently has exactly one base.
    pub fn release_key(&self) -> Option<(u32, u32, u32)> {
        let TagName::Family { wine_base, version, .. } = self else { return None };
        let (a, b) = wine_base.split_once('.')?;
        Some((a.parse().ok()?, b.parse().ok()?, *version))
    }
}

/// `11.12` -- exactly two dot-separated runs of digits.
fn is_wine_base(s: &str) -> bool {
    let mut parts = s.split('.');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !a.is_empty() && !b.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit())
}

/// `0.2.0` -- exactly three digit runs.
fn is_semver(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases() -> Vec<String> {
        vec!["soda-all".into(), "spritzwine-archive".into()]
    }

    #[test]
    fn parses_the_current_scheme() {
        assert_eq!(
            TagName::parse("soda-11.12-3", &aliases()),
            TagName::Family { component: "soda".into(), wine_base: "11.12".into(), version: 3 }
        );
        assert_eq!(
            TagName::parse("spritzwine-11.13-3", &aliases()),
            TagName::Family { component: "spritzwine".into(), wine_base: "11.13".into(), version: 3 }
        );
    }

    #[test]
    fn legacy_semver_is_recognised_but_not_a_family() {
        let t = TagName::parse("spritzwine-11.0-0.2.0", &aliases());
        assert!(matches!(t, TagName::Legacy { .. }));
        assert_eq!(t.component(), Some("spritzwine"));
    }

    #[test]
    fn aliases_must_be_declared() {
        assert!(matches!(TagName::parse("soda-all", &aliases()), TagName::Alias { .. }));
        // Same SHAPE, not declared -> Foreign, not a guessed alias.
        assert_eq!(TagName::parse("soda-whatever", &aliases()), TagName::Foreign);
    }

    /// The ordering regression. `max_by_key(version)` alone picks `-11.12-3`
    /// over `-11.13-1`, so on the day a new wine base is cut the "newest tag"
    /// becomes the SUPERSEDED one and every newest-only check silently starts
    /// measuring the wrong release.
    #[test]
    fn a_newer_base_outranks_a_higher_version() {
        let newer = TagName::parse("soda-11.13-1", &aliases()).release_key().unwrap();
        let older = TagName::parse("soda-11.12-3", &aliases()).release_key().unwrap();
        assert!(newer > older, "{newer:?} should outrank {older:?}");
    }

    /// Within one base the trailing integer still orders, and it orders
    /// NUMERICALLY -- a string compare would put `-10` before `-9`.
    #[test]
    fn within_a_base_the_version_orders_numerically() {
        let k = |s: &str| TagName::parse(s, &aliases()).release_key().unwrap();
        assert!(k("soda-11.12-10") > k("soda-11.12-9"));
    }

    /// Only `Family` tags have a release key. A Legacy SemVer tail is not a
    /// version in the same ordering, and inventing one for it would silently
    /// re-admit the tags `TagName` exists to keep out of the arithmetic.
    #[test]
    fn non_family_tags_have_no_release_key() {
        assert!(TagName::parse("spritzwine-11.0-0.2.0", &aliases()).release_key().is_none());
        assert!(TagName::parse("soda-all", &aliases()).release_key().is_none());
        assert!(TagName::parse("wine-11.13", &aliases()).release_key().is_none());
    }

    /// The load-bearing test: six thousand foreign tags must stay silent.
    #[test]
    fn upstream_and_valve_tags_are_foreign() {
        for t in [
            "wine-11.13",
            "experimental-wine-10.0-20250101",
            "proton-9.0-4",
            "steamos_3.5",
            "v6.3-rc",
            "hotfix-wine-8.0.1",
            "v0.1.0",
            "",
            "-",
            "--",
            "soda-",
            "soda-11.12-",
            "soda-11.12-007",
            "soda-11.12.1-3",
            "soda-abc-3",
        ] {
            assert_eq!(TagName::parse(t, &aliases()), TagName::Foreign, "{t} should be Foreign");
        }
    }
}
