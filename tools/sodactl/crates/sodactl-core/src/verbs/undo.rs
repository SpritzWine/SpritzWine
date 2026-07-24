//! `undo` -- list the backup refs every history rewrite left behind, and
//! restore one, CAS-guarded.
//!
//! The backup namespace (`refs/sodactl/backup/<utc>/<branch>`) is created by
//! `plan::apply` before any rewrite and never auto-pruned -- this verb is the
//! read-and-restore half of that promise. The restore is a single
//! `update-ref` CAS: the expected old value is the branch's LIVE tip, read at
//! plan time and shown in the plan, so a branch that moves between review and
//! apply refuses under git's own ref lock instead of clobbering someone's
//! work.

use crate::plan::{Action, Plan, PlannedValue, RefMutation, RepoSide, Step};
use anyhow::Context as _;
use sodactl_git::{GitCli, GitRead};

/// The namespace as `mutate.rs` writes it. One constant, both sides.
pub const BACKUP_NS: &str = "refs/sodactl/backup/";

/// A backup's subject ref. The `<branch>` path segment names a branch --
/// except under the `tags/` prefix, which `tag --force` uses to preserve a
/// re-pointed tag's old TAG OBJECT (`.../tags/<name>` -> `refs/tags/<name>`).
/// Without this mapping a tag restore would mint a `refs/heads/tags/<name>`
/// branch and call it a restore.
fn subject_ref(branch: &str) -> String {
    match branch.strip_prefix("tags/") {
        Some(t) => format!("refs/tags/{t}"),
        None => format!("refs/heads/{branch}"),
    }
}

/// One backup ref, decorated with where its subject branch is NOW -- the
/// listing's whole value is showing whether restoring would change anything.
#[derive(Debug, serde::Serialize)]
pub struct BackupListing {
    pub refname: String,
    /// The `<utc>` path segment; empty when the ref does not match the
    /// namespace's `<utc>/<branch>` shape (listed anyway -- a foreign ref in
    /// our namespace is a fact worth seeing, not one to silently drop).
    pub utc: String,
    pub branch: String,
    /// The backed-up tip.
    pub tip: String,
    /// The branch's live tip; `None` = the branch no longer exists.
    pub branch_tip: Option<String>,
    /// True when restoring would actually move something.
    pub moved: bool,
}

/// All backups, newest first (the UTC stamp in the refname sorts
/// lexicographically, which is why the stamp format exists).
pub fn list_backups(git: &GitCli) -> anyhow::Result<Vec<BackupListing>> {
    let mut out = Vec::new();
    for (refname, tip) in git.refs_under(BACKUP_NS)? {
        let rest = refname.strip_prefix(BACKUP_NS).unwrap_or(&refname);
        let (utc, branch) = match rest.split_once('/') {
            Some((u, b)) => (u.to_string(), b.to_string()),
            None => (String::new(), rest.to_string()),
        };
        let branch_tip = git.rev_parse_raw(&subject_ref(&branch))?;
        let moved = branch_tip.as_deref() != Some(tip.as_str());
        out.push(BackupListing { refname, utc, branch, tip, branch_tip, moved });
    }
    out.sort_by(|a, b| (&b.utc, &b.refname).cmp(&(&a.utc, &a.refname)));
    Ok(out)
}

/// Plan the restore of one backup. `backup` is the full refname or the
/// `<utc>/<branch>` tail the listing shows.
///
/// A branch already at the backed-up tip yields a plan with NO steps and a
/// note saying so -- the honest no-op, distinguishable from both an error and
/// a silent success.
pub fn plan_undo(git: &GitCli, backup: &str) -> anyhow::Result<Plan> {
    let refname = if backup.starts_with(BACKUP_NS) {
        backup.to_string()
    } else {
        format!("{BACKUP_NS}{backup}")
    };
    let tip = git.rev_parse_raw(&refname)?.with_context(|| {
        format!("no such backup ref {refname} -- run `sodactl undo` to list what exists")
    })?;
    let rest = refname.strip_prefix(BACKUP_NS).expect("normalised above");
    let (_utc, branch) = rest.split_once('/').with_context(|| {
        format!("{refname} does not match {BACKUP_NS}<utc>/<branch> -- not something this tool wrote; restore it by hand if you must")
    })?;

    let target = subject_ref(branch);
    // Raw on purpose, same as the mutation pre-verify: the CAS compares the
    // ref AS STORED.
    let live = git.rev_parse_raw(&target)?;

    if live.as_deref() == Some(tip.as_str()) {
        return Ok(Plan::new(
            "undo",
            vec![],
            vec![],
            vec![],
            vec![format!(
                "`{branch}` is already at the backed-up tip {} -- nothing to restore",
                short(&tip)
            )],
        ));
    }

    let mut notes = vec![format!("restores from {refname}")];
    if live.is_none() {
        notes.push(format!("`{branch}` no longer exists -- the restore recreates it"));
    }
    if git.head_branch()?.as_deref() == Some(branch) {
        // update-ref moves the ref, not the files. Saying so here is the
        // difference between a surprising dirty tree and an expected one.
        notes.push(format!(
            "`{branch}` is currently checked out: the ref moves but the worktree does not -- \
             run `git -C wine reset --hard HEAD` afterwards to sync it (discards local edits)"
        ));
    }

    let steps = vec![Step::new(
        format!(
            "restore `{branch}` to its backed-up tip {} (CAS against the live tip {})",
            short(&tip),
            live.as_deref().map(short).unwrap_or_else(|| "<absent>".into())
        ),
        RepoSide::Wine,
        Action::UpdateRefCas { refname: target.clone(), new: tip.clone(), expected_old: live.clone() },
    )];
    let mutations = vec![RefMutation {
        repo: RepoSide::Wine,
        refname: target,
        expected_before: live,
        after: PlannedValue::Oid { oid: tip },
    }];

    Ok(Plan::new("undo", steps, mutations, vec![], notes))
}

fn short(oid: &str) -> String {
    oid[..oid.len().min(11)].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{self, ApplyCtx};
    use sodactl_git::GitMut;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // Real scratch repos, pid-scoped -- same reasoning and pattern as the
    // plan.rs suite one module over.

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out =
            Command::new("git").current_dir(dir).env("LC_ALL", "C").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn scratch(name: &str) -> (Scratch, GitCli, GitMut) {
        let dir = std::env::temp_dir()
            .join(format!("sodactl-core-undo-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@t"]);
        git(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "one"]);
        (Scratch(dir.clone()), GitCli::new(&dir), GitMut::new(&dir))
    }

    fn commit(dir: &Path, file: &str, content: &str, msg: &str) {
        std::fs::write(dir.join(file), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", msg]);
    }

    #[test]
    fn listing_is_newest_first_and_knows_whether_the_branch_moved() {
        let (s, r, m) = scratch("list");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();
        git(&s.0, &["branch", "steady", &c1]);
        m.update_ref_cas("refs/sodactl/backup/20260724T000000Z/steady", &c1, None).unwrap();
        m.update_ref_cas("refs/sodactl/backup/20260724T000001Z/main", &c1, None).unwrap();
        commit(&s.0, "b.txt", "b\n", "two"); // main moves past its backup

        let l = list_backups(&r).unwrap();
        assert_eq!(
            l.iter().map(|b| b.utc.as_str()).collect::<Vec<_>>(),
            vec!["20260724T000001Z", "20260724T000000Z"],
            "newest first"
        );
        assert!(l[0].moved, "main moved past its backup");
        assert_eq!(l[0].branch, "main");
        assert!(!l[1].moved, "steady still sits on the backed-up tip");
    }

    /// The `tags/` subject prefix (`tag --force`'s backup) restores the TAG,
    /// not a `refs/heads/tags/...` branch -- the whole point of `subject_ref`.
    #[test]
    fn a_tag_backup_restores_the_tag_ref() {
        let (s, r, m) = scratch("tag-restore");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();
        git(&s.0, &["tag", "-a", "-m", "v1", "demo-1", &c1]);
        let old_obj = r.rev_parse_raw("refs/tags/demo-1").unwrap().unwrap();
        m.update_ref_cas("refs/sodactl/backup/20260724T000000Z/tags/demo-1", &old_obj, None)
            .unwrap();
        // The re-point the backup exists to undo.
        commit(&s.0, "b.txt", "b\n", "two");
        git(&s.0, &["tag", "-a", "-f", "-m", "v1 again", "demo-1", "HEAD"]);

        let plan = plan_undo(&r, "20260724T000000Z/tags/demo-1").unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.mutations[0].refname, "refs/tags/demo-1", "the TAG ref, not a branch");
        let ctx = ApplyCtx {
            wine: &m,
            superproject: &m,
            wine_read: &r,
            superproject_read: &r,
            wine_path: "wine".into(),
        };
        plan::apply(&plan, &ctx).unwrap();
        assert_eq!(r.rev_parse_raw("refs/tags/demo-1").unwrap().unwrap(), old_obj);
        assert!(r.rev_parse_raw("refs/heads/tags/demo-1").unwrap().is_none(), "no phantom branch");
        // The listing knows the subject moved back into place.
        let l = list_backups(&r).unwrap();
        assert!(!l[0].moved, "restored: the live tag sits on the backed-up object again");
    }

    #[test]
    fn plan_undo_is_a_cas_against_the_live_tip_and_apply_restores() {
        let (s, r, m) = scratch("restore");
        let c1 = r.rev_parse("HEAD").unwrap().unwrap();
        m.update_ref_cas("refs/sodactl/backup/20260724T000000Z/main", &c1, None).unwrap();
        commit(&s.0, "b.txt", "b\n", "two");
        let c2 = r.rev_parse("HEAD").unwrap().unwrap();

        // The shorthand the listing shows is accepted, and the CAS names the
        // LIVE tip -- the plan the user reviews states what it will refuse on.
        let plan = plan_undo(&r, "20260724T000000Z/main").unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].argv, vec!["update-ref", "refs/heads/main", &c1, &c2]);
        assert_eq!(plan.mutations[0].expected_before.as_deref(), Some(c2.as_str()));
        // main is checked out, so the worktree note must be present.
        assert!(plan.notes.iter().any(|n| n.contains("currently checked out")), "{:?}", plan.notes);

        let ctx = ApplyCtx {
            wine: &m,
            superproject: &m,
            wine_read: &r,
            superproject_read: &r,
            wine_path: "wine".into(),
        };
        plan::apply(&plan, &ctx).unwrap();
        assert_eq!(r.rev_parse_raw("refs/heads/main").unwrap().unwrap(), c1);

        // Re-planning after the restore is the honest no-op: no steps, and a
        // note that says why.
        let again = plan_undo(&r, "20260724T000000Z/main").unwrap();
        assert!(again.steps.is_empty() && again.mutations.is_empty());
        assert!(again.notes.iter().any(|n| n.contains("nothing to restore")), "{:?}", again.notes);
    }
}
