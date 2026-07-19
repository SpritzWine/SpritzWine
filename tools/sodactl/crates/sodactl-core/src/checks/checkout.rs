//! Where the wine checkout is *standing*.
//!
//! Distinct from every other check here: those ask whether the graph is
//! consistent, this asks whether the working tree is parked somewhere that
//! makes the next mistake easy. Nothing is wrong yet when it fires -- that is
//! the point. It is the guard rail, not the crash report.
//!
//! Consumers are GENERATED and force-pushed. A commit made while HEAD is on one
//! is not rejected by anything: it commits cleanly, it builds, and it disappears
//! without a trace on the next pour, because the branch is reset to the replay
//! result and the old tip stops being reachable. There is no conflict, no
//! warning and no reflog entry anywhere the next person will look. The failure
//! is silent and total, which is exactly the kind this tool exists to make loud.
//!
//! The safe places to stand are: detached at the commit you are building, or on
//! an authored `soda-*` tier. Detaching costs nothing -- the tree is identical,
//! so a warm build survives it untouched.

use super::Workspace;
use crate::finding::{Finding, Severity};
use crate::manifest::Manifest;

pub fn check(m: &Manifest, ws: &Workspace) -> anyhow::Result<Vec<Finding>> {
    let mut f = Vec::new();

    // `None` means detached, which is the recommended state -- nothing to say.
    let Some(branch) = ws.wine.head_branch()? else {
        return Ok(f);
    };

    if m.consumer.iter().any(|c| c.name == branch) {
        f.push(Finding::new(
            "CHECKOUT_ON_GENERATED_BRANCH",
            Severity::Drift,
            &m.repo.wine,
            format!(
                "the wine checkout is ON `{}`, a generated consumer -- a commit made here is force-pushed away by the next pour, silently and with nothing to recover from",
                branch
            ),
            format!(
                "git -C {} checkout --detach {}   # same commit, identical tree, a warm build survives it",
                m.repo.wine, branch
            ),
        ));
    }

    // A stitch is derived too: re-stitching does `checkout -B`, so anything
    // committed on top is discarded the same way.
    if let Some(s) = m.stitch.iter().find(|s| s.name == branch) {
        f.push(Finding::new(
            "CHECKOUT_ON_GENERATED_BRANCH",
            Severity::Hygiene,
            &m.repo.wine,
            format!(
                "the wine checkout is ON `{}`, a derived stitch -- re-stitching resets it and drops anything committed here",
                s.name
            ),
            format!(
                "git -C {} checkout --detach {}   # or move to the tier you actually mean to author on",
                m.repo.wine, branch
            ),
        ));
    }

    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::{FakeGit, Workspace};

    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "aaa"
describes = "wine-11.12"
derivation = "x^"
[[tier]]
name = "soda-core"
parent = "$home_base"
[[stitch]]
name = "soda-full"
base_tier = "soda-core"
order = ["soda-core"]
[[consumer]]
name = "spritzwine"
base = "winehq/wine-11.13"
soda_tip = "soda-full"
"#,
        )
        .unwrap()
    }

    fn ws_on(branch: Option<&str>) -> (FakeGit, FakeGit) {
        let mut wine = FakeGit::default();
        wine.head = branch.map(str::to_string);
        (FakeGit::default(), wine)
    }

    fn run(branch: Option<&str>) -> Vec<Finding> {
        let m = manifest();
        let (sup, wine) = ws_on(branch);
        let ws = Workspace {
            superproject: &sup,
            wine: &wine,
            superproject_path: ".".into(),
            wine_path: "wine".into(),
        };
        check(&m, &ws).unwrap()
    }

    #[test]
    fn detached_is_the_safe_state_and_says_nothing() {
        assert!(run(None).is_empty());
    }

    #[test]
    fn standing_on_an_authored_tier_is_fine() {
        assert!(run(Some("soda-core")).is_empty());
    }

    #[test]
    fn standing_on_a_consumer_is_drift() {
        let f = run(Some("spritzwine"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code, "CHECKOUT_ON_GENERATED_BRANCH");
        assert_eq!(f[0].severity, Severity::Drift);
    }

    #[test]
    fn standing_on_a_stitch_is_hygiene_not_drift() {
        let f = run(Some("soda-full"));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Hygiene);
    }
}
