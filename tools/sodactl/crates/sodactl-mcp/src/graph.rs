//! The tier DAG, derived.
//!
//! WHY THIS USES ANCESTRY WHEN THE STITCH CHECK MUST NOT
//!
//! Tiers are *authored*: a child tier is a real descendant of its parent, so
//! `merge-base --is-ancestor(parent_tip, child)` is exactly the right question
//! and the SHAs are the same objects on both sides.
//!
//! Stitches and consumers are *replayed* -- cherry-pick and rebase respectively
//! -- so their commits are new objects with new SHAs and the originals are never
//! ancestors of the result. Ancestry there gives a confident wrong answer (it
//! did: four false positives), and patch-id equivalence via `git cherry` is the
//! only correct primitive. That is why this module answers about tiers only, and
//! why it says so here instead of leaving the next reader to generalise the
//! wrong half of the rule.
//!
//! Nothing here is declared. The manifest contributes exactly one thing -- the
//! parent edge. Tips, commit counts, staleness and restack order are all read
//! live, because a geometry constant in a config file is a lie waiting to happen
//! while a destructive operation depends on it.

use anyhow::Result;
use serde::Serialize;
use sodactl_core::Manifest;
use sodactl_git::GitRead;

#[derive(Debug, Serialize)]
pub struct Dag {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Tiers needing a restack, parents strictly before children.
    ///
    /// Order is the whole value: restacking a child before its parent rebases it
    /// onto a tip that is about to move, so the work is thrown away and done
    /// twice -- and the second pass starts from a child whose commit set no
    /// longer matches what the first pass computed.
    pub restack_order: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Node {
    pub name: String,
    pub tip: Option<String>,
    /// `$home_base` for a root tier.
    pub parent: String,
    /// The ref the parent edge actually resolved to, with `$home_base`
    /// substituted -- so a reader can see which commit the edge is measured
    /// against without re-deriving it.
    pub parent_ref: String,
    pub parent_tip: Option<String>,
    /// `merge-base(parent, child)..child` -- the tier's own work. DERIVED.
    pub own_commits: Option<u32>,
    /// `false` = the child predates its parent's tip. `None` = undeterminable.
    pub on_parent_tip: Option<bool>,
    /// True when this tier itself is stale, OR any ancestor tier is: a tier
    /// sitting correctly on a stale parent is just as unshippable, and reporting
    /// only the direct offender understates the blast radius.
    pub needs_restack: bool,
}

#[derive(Debug, Serialize)]
pub struct Edge {
    pub parent: String,
    pub child: String,
}

pub fn tier_graph<G: GitRead>(m: &Manifest, git: &G) -> Result<Dag> {
    let home = m.repo.home_base.commit.as_str();

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    for t in &m.tier {
        let parent_ref =
            if t.parent == "$home_base" { home.to_string() } else { t.parent.clone() };

        let tip = git.rev_parse(&t.name)?;
        let parent_tip = git.rev_parse(&parent_ref)?;

        let on_parent_tip = match (&tip, &parent_tip) {
            (Some(_), Some(_)) => Some(git.is_ancestor(&parent_ref, &t.name)?),
            _ => None,
        };

        // `.ok()` rather than `?`: a missing branch makes the range invalid, and
        // "unknown" is the honest answer to a count we could not take. Inventing
        // a 0 here would read as "this tier contributes nothing".
        let own_commits = git.count_commits(&format!("{parent_ref}..{}", t.name)).ok();

        if t.parent != "$home_base" {
            edges.push(Edge { parent: t.parent.clone(), child: t.name.clone() });
        }

        nodes.push(Node {
            name: t.name.clone(),
            tip,
            parent: t.parent.clone(),
            parent_ref,
            parent_tip,
            own_commits,
            on_parent_tip,
            // Filled in below; staleness inherits down the DAG and that needs
            // every node present first.
            needs_restack: on_parent_tip == Some(false),
        });
    }

    propagate_staleness(&mut nodes);

    let restack_order = nodes
        .iter()
        .filter(|n| n.needs_restack)
        .map(|n| n.name.clone())
        .collect::<Vec<_>>();
    // The manifest lists tiers parent-before-child, but nothing enforces that,
    // and a reordered manifest must not silently produce a destructive order.
    let restack_order = topo_sort(&restack_order, m);

    Ok(Dag { nodes, edges, restack_order })
}

/// A tier on a stale parent is stale, transitively. Iterating to a fixed point
/// rather than recursing keeps this correct for any manifest ordering and
/// terminates on a cycle instead of blowing the stack -- a hand-edited manifest
/// can declare one, and refusing to crash on bad policy input is cheap here.
fn propagate_staleness(nodes: &mut [Node]) {
    for _ in 0..nodes.len() {
        let mut changed = false;
        for i in 0..nodes.len() {
            if nodes[i].needs_restack {
                continue;
            }
            let parent = nodes[i].parent.clone();
            if nodes.iter().any(|p| p.name == parent && p.needs_restack) {
                nodes[i].needs_restack = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Depth from the root, which for a tree is a valid topological order and is all
/// the ordering `restack` needs. Depth is capped by the node count so a cyclic
/// manifest yields a stable arbitrary order rather than looping.
fn topo_sort(names: &[String], m: &Manifest) -> Vec<String> {
    // Walks by owned name rather than reborrowing `t.parent`: the closure's
    // parameter lifetime is inferred independently of `m`'s, so reassigning the
    // parameter from a borrow of `m` demands `'static`. One `clone` per hop over
    // a seven-element manifest is not the place to fight the borrow checker.
    let depth = |name: &str| -> usize {
        let mut cur = name.to_string();
        let mut d = 0;
        while let Some(t) = m.tier(&cur) {
            if t.parent == "$home_base" || d > m.tier.len() {
                break;
            }
            cur = t.parent.clone();
            d += 1;
        }
        d
    };
    let mut out = names.to_vec();
    out.sort_by_key(|n| depth(n));
    out
}
