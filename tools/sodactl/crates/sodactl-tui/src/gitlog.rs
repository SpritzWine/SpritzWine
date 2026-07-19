//! The commit chain: `git log --graph` argv construction, execution, and the
//! line -> commit mapping the log pane selects with.
//!
//! git renders its own merge lanes (`--graph --color=always`), so there is no
//! lane-layout code here to drift from what git thinks the history is. The
//! price of borrowing git's renderer is that the coloured text carries no
//! machine-readable commit identity per line -- so a second, `--no-graph
//! --pretty=%H` pass runs over the SAME range with the SAME ordering flags
//! (serie's idiom), and a pure matcher stitches the two together. When the
//! stitching cannot be trusted (`map_lines` count mismatch) the pane keeps the
//! text and drops selection: degrade loudly, still usefully.
//!
//! Everything argv-shaped in here is PURE and unit-tested on exact strings,
//! because the failure mode of a wrong range is not an error -- it is a
//! plausible-looking log of the wrong commits.

use anyhow::Result;
use sodactl_core::checks::own_limit;
use sodactl_core::Manifest;
use std::path::Path;

/// Which slice of history the Log tab shows. Rides on every `worker::Job`, so
/// switching context is a refresh like any other -- the worker derives the log
/// beside the report and the pane never renders a range the snapshot did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    /// Every modelled ref since the home base: the whole family at a glance.
    Overview,
    Tier(String),
    Stitch(String),
    Consumer(String),
}

/// One derived log, ready to render. `line_oids` has exactly one entry per
/// line of `text`; a continuation line (merge lane, message body) carries the
/// commit ABOVE it, so any line the cursor lands on names a commit.
pub struct LogView {
    /// Names the context and its range in words -- the pane title. Kept on the
    /// view rather than recomputed so the pane can never caption one context's
    /// text with another context's name while a refresh is in flight.
    pub title: String,
    /// Raw ANSI from git, untouched. ansi-to-tui does the parsing at render.
    pub text: String,
    pub line_oids: Vec<Option<String>>,
    /// `map_lines`' count mismatch, when the two passes disagreed. The pane
    /// renders this AND the text: a broken mapping loses selection, not the
    /// evidence.
    pub mapping_error: Option<String>,
}

/// The range arguments for a context -- the part of the argv where being
/// subtly wrong is easiest, isolated so the exact-string tests pin each arm.
fn range_args(m: &Manifest, ctx: &Context) -> Result<Vec<String>> {
    let home = m.repo.home_base.commit.as_str();
    Ok(match ctx {
        // Everything ours, bounded below by the RESOLVED home-base commit.
        // The `$home_base` literal is a manifest-file convention, not a ref --
        // handed to git it would be "unknown revision", so it must never reach
        // an argv (unit-tested across every context below).
        Context::Overview => {
            let mut args: Vec<String> =
                m.modelled_refs().iter().map(|r| r.to_string()).collect();
            args.push(format!("^{home}"));
            args
        }
        // `own_limit(...)..tier`: the tier's OWN commits, limited at its
        // declared parent -- NOT ancestry from the home base. Limiting at the
        // home base is the single most repeated mistake in this codebase
        // (see `checks::own_limit`): it silently answers a question about one
        // tier with its whole ancestry's commits.
        Context::Tier(t) => {
            let limit = own_limit(m, t, home).ok_or_else(|| {
                anyhow::anyhow!("tier `{t}` is not in the manifest -- no range to show")
            })?;
            vec![format!("{limit}..{t}")]
        }
        // A stitch is cherry-picked from its base tier, so its own work is
        // `base_tier..name` -- and nothing smarter: patch-id reasoning belongs
        // to the checks, this is just "show me the replayed commits".
        Context::Stitch(name) => {
            let s = m.stitch.iter().find(|s| &s.name == name).ok_or_else(|| {
                anyhow::anyhow!("stitch `{name}` is not in the manifest -- no range to show")
            })?;
            vec![format!("{}..{}", s.base_tier, s.name)]
        }
        // A consumer is rebased onto its wine base: `base..name` is the replay.
        Context::Consumer(name) => {
            let c = m.consumer.iter().find(|c| &c.name == name).ok_or_else(|| {
                anyhow::anyhow!("consumer `{name}` is not in the manifest -- no range to show")
            })?;
            vec![format!("{}..{}", c.base, c.name)]
        }
    })
}

/// The pane title: context name plus its range, in words.
fn title(m: &Manifest, ctx: &Context) -> Result<String> {
    Ok(match ctx {
        Context::Overview => "overview -- all modelled refs since the home base".into(),
        _ => {
            let range = range_args(m, ctx)?.join(" ");
            let name = match ctx {
                Context::Tier(n) | Context::Stitch(n) | Context::Consumer(n) => n.as_str(),
                Context::Overview => unreachable!(),
            };
            format!("{name}  ({range})")
        }
    })
}

/// The coloured `--graph` pass. Pure: returns the exact argv, `git` first.
///
/// `-n 400` bounds the overview on a repo whose modelled refs span thousands
/// of commits; `--date-order` is load-bearing rather than cosmetic -- `--graph`
/// would otherwise imply `--topo-order`, and the `%H` pass below must walk the
/// SAME order or every line maps to the wrong commit.
pub fn argv(m: &Manifest, wine: &Path, ctx: &Context) -> Result<Vec<String>> {
    let mut v: Vec<String> = [
        "git",
        "-C",
        &wine.to_string_lossy(),
        "log",
        "--graph",
        "--color=always",
        "--decorate=short",
        "--date-order",
        "-n",
        "400",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.extend(range_args(m, ctx)?);
    Ok(v)
}

/// The parallel selection pass: same range, same ordering flags, no graph, one
/// full oid per line. This list is the ground truth `map_lines` matches the
/// coloured lines against.
pub fn oids_argv(m: &Manifest, wine: &Path, ctx: &Context) -> Result<Vec<String>> {
    let mut v: Vec<String> = [
        "git",
        "-C",
        &wine.to_string_lossy(),
        "log",
        "--no-graph",
        "--pretty=%H",
        "--date-order",
        "-n",
        "400",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.extend(range_args(m, ctx)?);
    Ok(v)
}

/// Drop CSI escape sequences (`ESC [ ... final-byte`), which is all
/// `git log --color` emits. Pure so the mapping tests run on canned fixtures.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: consume through the final byte (0x40-0x7e). A bare ESC not
            // followed by `[` is dropped alone -- git does not emit those, and
            // swallowing one char beats desynchronising the whole line.
            if chars.clone().next() == Some('[') {
                chars.next();
                for f in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&f) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Match the coloured `--graph` lines against the `%H` list: pure ANSI-strip +
/// hex-prefix matching, in order. A line "introduces" the next commit when any
/// of its hex tokens (>= 7 chars, so lane characters and short words cannot
/// collide) is a prefix of the next expected oid; every other line -- merge
/// lanes, decorations, message bodies -- belongs to the commit above it.
///
/// The prefix must match the NEXT oid specifically, not any oid: a commit
/// message quoting some sha therefore cannot advance the cursor unless it
/// quotes precisely the next commit in the walk, at which point the following
/// real commit line fails to match and the count check below catches it.
///
/// Returns one entry per line, plus the mismatch error when the passes
/// disagreed -- in which case every line maps to `None` (plain pager) rather
/// than to a half-right guess: a selection that names the wrong commit is
/// worse than no selection, because Stage 6 feeds it to `git show`.
pub fn map_lines(graph: &str, oids: &[String]) -> (Vec<Option<String>>, Option<String>) {
    let mut out = Vec::new();
    let mut next = 0usize;
    let mut headers = 0usize;
    let mut current: Option<String> = None;
    for line in graph.lines() {
        let plain = strip_ansi(line);
        // Count commit HEADERS (lane glyphs, then literally `commit <hex>`)
        // independently of the match below. The matched-count check alone is
        // one-sided: it catches a %H pass with MORE commits than matched, but
        // when the graph pass holds more (the two execs race a moving ref --
        // a lower bound advancing between them truncates the %H list to a
        // strict prefix), the tail commit lines would quietly map to the LAST
        // matched oid -- the wrong-commit selection Stage 6 feeds to `git
        // show`, which this function's whole contract is to refuse. A message
        // body line that happens to start with `commit <hex>` after its
        // indent over-counts and degrades to the plain pager: the direction
        // this pane is allowed to be wrong in.
        let lead = plain
            .trim_start_matches(|c: char| matches!(c, '|' | '\\' | '/' | '*' | '_' | ' '));
        if lead.strip_prefix("commit ").is_some_and(|rest| {
            rest.len() >= 7 && rest.chars().take(7).all(|c| c.is_ascii_hexdigit())
        }) {
            headers += 1;
        }
        let introduces = next < oids.len()
            && plain
                .split(|c: char| !c.is_ascii_hexdigit())
                .any(|tok| tok.len() >= 7 && oids[next].starts_with(tok));
        if introduces {
            current = Some(oids[next].clone());
            next += 1;
        }
        out.push(current.clone());
    }
    if next != oids.len() || headers != oids.len() {
        // Both counts, FIRST: the log pane renders this unwrapped (wrapping
        // would mangle git's lane art below it), so the numbers must survive
        // a narrow pane -- a truncated "mapping failed..." with the evidence
        // clipped off would be half an error.
        return (
            vec![None; out.len()],
            Some(format!(
                "mapping failed: {} vs {} commits (--graph pass vs %H pass) \
                 -- selection disabled, plain pager",
                // The header count, not the matched count: in the graph-has-
                // more direction every tail header matched nothing, and "3 vs
                // 3" from the matched cursor would deny the very disagreement
                // being reported.
                headers,
                oids.len()
            )),
        );
    }
    (out, None)
}

/// Run both passes and build the view. Errors come back as `String` rather
/// than failing the whole snapshot: a broken log range must not take the
/// findings and the graph down with it -- the pane renders the error, the rest
/// of the dashboard stays live.
///
/// AUDITED READ-ONLY CONSTRUCTOR: the only git this module runs is `log`
/// (both passes above). The Stage-6 guardrail test greps `Command::new` in
/// this crate against the audited sites; this is one of them.
pub fn run(m: &Manifest, wine: &Path, ctx: &Context) -> std::result::Result<LogView, String> {
    let title = title(m, ctx).map_err(|e| format!("{e:#}"))?;
    let graph_argv = argv(m, wine, ctx).map_err(|e| format!("{e:#}"))?;
    let oid_argv = oids_argv(m, wine, ctx).map_err(|e| format!("{e:#}"))?;
    let text = exec(&graph_argv)?;
    let oid_text = exec(&oid_argv)?;
    let oids: Vec<String> = oid_text.lines().map(str::to_string).collect();
    let (line_oids, mapping_error) = map_lines(&text, &oids);
    Ok(LogView { title, text, line_oids, mapping_error })
}

/// Execute one argv, stdout as UTF-8 (lossy: commit messages are arbitrary
/// bytes and a replacement char beats refusing to show the log). A failure
/// names the exact command line -- the reader's next move is to run it by hand.
fn exec(argv: &[String]) -> std::result::Result<String, String> {
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        // Same discipline as sodactl-git's exec: stable English stderr on a
        // de_AT host, so a failure message here is greppable evidence.
        .env("LC_ALL", "C")
        .output()
        .map_err(|e| format!("spawning `{}`: {e}", argv.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "`{}` failed ({}): {}",
            argv.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A manifest with all four context shapes: a root tier (the `$home_base`
    /// resolution arm), a child tier (the `own_limit` arm), a stitch, a
    /// consumer. The home commit is deliberately full-length: the argv must
    /// carry it verbatim.
    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
schema = 1
[repo]
superproject = "."
wine = "wine"
[repo.home_base]
commit = "1bb0da91a5e1234567890abcdef1234567890abc"
describes = "wine-11.12 fork point"
derivation = "test fixture"

[[tier]]
name = "soda-core"
parent = "$home_base"

[[tier]]
name = "soda-ntdll"
parent = "soda-core"

[[stitch]]
name = "soda-all"
base_tier = "soda-core"
order = ["soda-ntdll"]

[[consumer]]
name = "spritzwine"
base = "wine-11.13"
soda_tip = "soda-all"
"#,
        )
        .unwrap()
    }

    fn wine() -> PathBuf {
        PathBuf::from("/repo/wine")
    }

    const FLAGS: [&str; 10] = [
        "git",
        "-C",
        "/repo/wine",
        "log",
        "--graph",
        "--color=always",
        "--decorate=short",
        "--date-order",
        "-n",
        "400",
    ];

    fn with_ranges(ranges: &[&str]) -> Vec<String> {
        FLAGS.iter().chain(ranges).map(|s| s.to_string()).collect()
    }

    #[test]
    fn overview_argv_bounds_at_the_resolved_home_commit() {
        let m = manifest();
        assert_eq!(
            argv(&m, &wine(), &Context::Overview).unwrap(),
            with_ranges(&[
                "soda-core",
                "soda-ntdll",
                "soda-all",
                "spritzwine",
                "^1bb0da91a5e1234567890abcdef1234567890abc",
            ])
        );
    }

    #[test]
    fn tier_argv_is_own_commits_not_ancestry() {
        let m = manifest();
        // Root tier: the `$home_base` parent resolves to the commit.
        assert_eq!(
            argv(&m, &wine(), &Context::Tier("soda-core".into())).unwrap(),
            with_ranges(&["1bb0da91a5e1234567890abcdef1234567890abc..soda-core"])
        );
        // Child tier: limited at its DECLARED parent. `^home..soda-ntdll`
        // would silently include soda-core's commits -- the most repeated
        // mistake in this codebase (see `checks::own_limit`).
        assert_eq!(
            argv(&m, &wine(), &Context::Tier("soda-ntdll".into())).unwrap(),
            with_ranges(&["soda-core..soda-ntdll"])
        );
    }

    #[test]
    fn stitch_and_consumer_argv() {
        let m = manifest();
        assert_eq!(
            argv(&m, &wine(), &Context::Stitch("soda-all".into())).unwrap(),
            with_ranges(&["soda-core..soda-all"])
        );
        assert_eq!(
            argv(&m, &wine(), &Context::Consumer("spritzwine".into())).unwrap(),
            with_ranges(&["wine-11.13..spritzwine"])
        );
    }

    #[test]
    fn oids_argv_shares_range_and_ordering_flags() {
        let m = manifest();
        let v = oids_argv(&m, &wine(), &Context::Tier("soda-ntdll".into())).unwrap();
        assert_eq!(
            v,
            [
                "git",
                "-C",
                "/repo/wine",
                "log",
                "--no-graph",
                "--pretty=%H",
                "--date-order",
                "-n",
                "400",
                "soda-core..soda-ntdll",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        );
    }

    /// The `$home_base` literal is a manifest convention, not a ref. If it
    /// ever reaches git it becomes "unknown revision" at best and, worse, a
    /// silently-wrong range if a branch of that name ever exists. Sweep every
    /// context.
    #[test]
    fn the_home_base_literal_never_reaches_an_argv() {
        let m = manifest();
        for ctx in [
            Context::Overview,
            Context::Tier("soda-core".into()),
            Context::Tier("soda-ntdll".into()),
            Context::Stitch("soda-all".into()),
            Context::Consumer("spritzwine".into()),
        ] {
            for arg in argv(&m, &wine(), &ctx)
                .unwrap()
                .iter()
                .chain(oids_argv(&m, &wine(), &ctx).unwrap().iter())
            {
                assert!(!arg.contains("$home_base"), "literal leaked in {ctx:?}: {arg}");
            }
        }
    }

    #[test]
    fn undeclared_names_error_instead_of_guessing_a_range() {
        let m = manifest();
        for (ctx, what) in [
            (Context::Tier("soda-nope".into()), "tier"),
            (Context::Stitch("soda-nope".into()), "stitch"),
            (Context::Consumer("soda-nope".into()), "consumer"),
        ] {
            let e = argv(&m, &wine(), &ctx).unwrap_err().to_string();
            assert!(e.contains(what) && e.contains("soda-nope"), "{e}");
        }
    }

    // ---- mapping ----

    const A: &str = "aaaaaaaa94d5eb2f423a1d194ce67dae0f5db878";
    const B: &str = "bbbbbbbb0714b716e10742250037ce0797712d9c";
    const C: &str = "cccccccc86bd82f7b2e0eb14f8f77d95e3e2e8f5";
    const D: &str = "dddddddd0aa10eafb459a9928d9b47a583b1c000";

    fn oids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// A canned `git log --graph --color=always` shape: coloured commit
    /// lines, a merge with lane continuation lines, indented message bodies.
    fn fixture() -> String {
        [
            format!("*   \u{1b}[33mcommit {A}\u{1b}[m\u{1b}[33m (\u{1b}[m\u{1b}[1;36mHEAD\u{1b}[m\u{1b}[33m -> \u{1b}[m\u{1b}[1;32msoda-all\u{1b}[m\u{1b}[33m)\u{1b}[m"),
            "|\\  ".to_string(),
            format!("| * \u{1b}[33mcommit {B}\u{1b}[m"),
            "| | ".to_string(),
            "| |     stitched: the fsync port".to_string(),
            format!("* | \u{1b}[33mcommit {C}\u{1b}[m"),
            "|/  ".to_string(),
            format!("* \u{1b}[33mcommit {D}\u{1b}[m"),
            "  ".to_string(),
            "      the base commit message".to_string(),
        ]
        .join("\n")
    }

    #[test]
    fn mapping_walks_commit_lines_and_hands_continuations_to_the_commit_above() {
        let (map, err) = map_lines(&fixture(), &oids(&[A, B, C, D]));
        assert!(err.is_none(), "{err:?}");
        let want: Vec<Option<String>> = [A, A, B, B, B, C, C, D, D, D]
            .iter()
            .map(|s| Some(s.to_string()))
            .collect();
        assert_eq!(map, want);
    }

    #[test]
    fn mapping_mismatch_names_both_counts_and_disables_selection() {
        // One commit the graph text never shows: the counts disagree.
        let (map, err) = map_lines(&fixture(), &oids(&[A, B, C, D, "e".repeat(40).as_str()]));
        let err = err.expect("a count mismatch must be an error, not a shrug");
        assert!(err.contains("4 vs 5 commits"), "both counts must be named: {err}");
        // Plain-pager degradation: no line claims a commit it might not be.
        assert!(map.iter().all(Option::is_none));
        assert_eq!(map.len(), fixture().lines().count());
    }

    /// The OTHER mismatch direction (found by adversarial verification): the
    /// %H pass returns a strict PREFIX of the graph pass's commits -- the two
    /// execs race a moving ref, e.g. the range's lower bound advanced between
    /// them. The matched-count check alone cannot see it (every oid matched),
    /// and the tail commit lines would map to the LAST matched oid: the
    /// wrong-commit selection `git show` must never be fed.
    #[test]
    fn a_truncated_oid_list_disables_selection_instead_of_lying() {
        let graph = format!(
            "* \u{1b}[33mcommit {A}\u{1b}[m\n\
             * \u{1b}[33mcommit {B}\u{1b}[m\n\
             * \u{1b}[33mcommit {C}\u{1b}[m\n\
             * \u{1b}[33mcommit {D}\u{1b}[m"
        );
        let (map, err) = map_lines(&graph, &oids(&[A, B, C]));
        let err = err.expect("a graph pass with MORE commits must be an error, not a shrug");
        assert!(err.contains("4 vs 3 commits"), "both counts must be named: {err}");
        assert!(map.iter().all(Option::is_none), "no line may claim a commit it might not be");
    }

    /// A message body quoting a random sha must not advance the cursor: the
    /// token has to prefix the NEXT expected oid specifically.
    #[test]
    fn a_quoted_sha_in_a_message_body_cannot_derail_the_mapping() {
        let text = format!(
            "* \u{1b}[33mcommit {A}\u{1b}[m\n\
             |     reverts deadbeefdeadbeefdeadbeef\n\
             * \u{1b}[33mcommit {B}\u{1b}[m"
        );
        let (map, err) = map_lines(&text, &oids(&[A, B]));
        assert!(err.is_none(), "{err:?}");
        assert_eq!(map[1].as_deref(), Some(A), "the body line belongs to the commit above");
        assert_eq!(map[2].as_deref(), Some(B));
    }

    #[test]
    fn empty_range_maps_to_nothing_without_error() {
        let (map, err) = map_lines("", &oids(&[]));
        assert!(map.is_empty() && err.is_none());
    }
}
