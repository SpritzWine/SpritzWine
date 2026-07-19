//! App state + the event loop.
//!
//! Everything that decides is headless: `apply_outcome`, `footer_status` and
//! the key handlers never touch a terminal, because the properties worth
//! guarding -- stale outcomes are discarded, a failed refresh keeps the last
//! good snapshot, Enter jumps the log to a modelled ref -- are exactly the
//! ones a rendering test would bury under buffer diffs. The draw code only
//! shows what those decisions produced.
//!
//! Pane dispatch is a plain `enum Focus` + `match`, per the design record:
//! honest at N=3 interactive panes. Keys resolve to `keys::Action` through
//! the one static table the help overlay renders from -- the match arms here
//! dispatch actions, never raw key combinations, so a binding lives in
//! exactly one place.

use crate::gitlog::Context;
use crate::inspect;
use crate::keys::{self, Action};
use crate::panes::detail;
use crate::panes::findings::FindingsPane;
use crate::panes::graph::{GraphPane, GraphSel};
use crate::panes::help;
use crate::panes::log::LogPane;
use crate::terminal::{self, Term};
use crate::theme;
use crate::worker::{self, Job, Outcome, Snapshot};
use anyhow::Result;
use crokey::KeyCombination;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use sodactl_core::finding::Severity;
use sodactl_core::Manifest;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Braille throbber, advanced once per event-loop pass (~100 ms via the poll
/// timeout below) -- visible motion is the difference between "reconciling"
/// and "hung" from the chair.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Which pane the keys go to. `Tab` cycles in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Findings,
    Graph,
    Log,
}

/// What the right-top area shows. Distinct from `Focus`: focusing the
/// findings pane must not yank the log off screen mid-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RightTab {
    Graph,
    Log,
}

pub struct App {
    manifest: PathBuf,
    /// The last good derivation. NEVER cleared by a failed refresh: known-stale
    /// data beats a blank screen, and the footer says "stale" out loud rather
    /// than implying freshness. Blanking on error would trade evidence for
    /// nothing exactly when the user needs it most.
    snapshot: Option<Snapshot>,
    /// Newest generation issued. An outcome for anything older is stale BY
    /// DEFINITION -- a newer job exists -- regardless of arrival order.
    gen: u64,
    /// A job newer than the applied state is in flight.
    pending: bool,
    /// The last refresh failure, verbatim. Cleared only by a refresh that
    /// succeeds, so an old banner can never outlive its truth.
    error: Option<String>,
    /// The last shell-out failure, exact command line included. Deliberately
    /// NOT `error`: that field means "this data is stale", this one means "the
    /// inspection you just asked for did not happen" -- folding them together
    /// would let a failed `git show` read as a failed reconcile. Cleared by
    /// the next keypress (the acknowledgement), not by a refresh: a refresh
    /// says nothing about whether the user saw the banner.
    inspect_error: Option<String>,
    refreshed_at: Option<Instant>,
    spinner: usize,
    focus: Focus,
    tab: RightTab,
    /// The commit-chain context the NEXT refresh derives. Rides on every job
    /// (`Job::log_ctx`), so a context switch and a data refresh are one
    /// mechanism with one staleness rule.
    log_ctx: Context,
    /// Pane session state (selection, ticks, scroll). Lives on the app, not
    /// the snapshot: it must survive every refresh, while the snapshot is
    /// replaced wholesale.
    findings: FindingsPane,
    graph: GraphPane,
    log: LogPane,
    /// Whether `tig` was on PATH at startup -- probed once (see
    /// `inspect::tig_on_path` for why not per-keypress), decides which tool
    /// Enter-on-a-commit suspends into.
    tig: bool,
    /// The help overlay is visible. While it is, keys dismiss instead of act
    /// (Quit excepted): acting on a pane the reader cannot see is the
    /// vanishing-input failure the focus rules already forbid.
    show_help: bool,
}

/// A finding subject that names a modelled ref maps to its log context.
/// Anything else -- a path, an rr-cache id, a tag -- has no commit chain of
/// its own, and Enter deliberately does nothing rather than guessing a range.
/// The three namespaces share one repo's refs, so first-match order cannot
/// change the answer for a parseable manifest.
fn modelled_context(m: &Manifest, subject: &str) -> Option<Context> {
    if m.tier.iter().any(|t| t.name == subject) {
        Some(Context::Tier(subject.to_string()))
    } else if m.stitch.iter().any(|s| s.name == subject) {
        Some(Context::Stitch(subject.to_string()))
    } else if m.consumer.iter().any(|c| c.name == subject) {
        Some(Context::Consumer(subject.to_string()))
    } else {
        None
    }
}

impl App {
    pub fn new(manifest: PathBuf) -> Self {
        Self {
            manifest,
            snapshot: None,
            gen: 0,
            pending: false,
            error: None,
            inspect_error: None,
            refreshed_at: None,
            spinner: 0,
            focus: Focus::Findings,
            tab: RightTab::Graph,
            log_ctx: Context::Overview,
            findings: FindingsPane::new(),
            graph: GraphPane::new(),
            log: LogPane::new(),
            tig: inspect::tig_on_path(),
            show_help: false,
        }
    }

    /// Issue a new generation and hand it to the worker.
    fn request_refresh(&mut self, jobs: &Sender<Job>) {
        self.gen += 1;
        self.pending = true;
        // A dead worker (the runner panicked) must not read as an eternal
        // spinner: clear error over silent breakdown applies to our own
        // plumbing too.
        if jobs.send(Job { gen: self.gen, log_ctx: self.log_ctx.clone() }).is_err() {
            self.pending = false;
            self.error = Some("refresh worker is gone -- restart the TUI".into());
        }
    }

    /// Stale-discard. Strict equality, not `<`-tolerance: ONLY the newest
    /// issued generation may land. A late outcome from an overtaken reconcile
    /// would otherwise paint old state over new -- and would do so only under
    /// refresh races, which is the least debuggable way to be wrong.
    fn apply_outcome(&mut self, o: Outcome) {
        if o.gen != self.gen {
            return;
        }
        self.pending = false;
        match o.result {
            Ok(s) => {
                // Pane state reconciles BEFORE the snapshot lands: tick GC and
                // selection re-anchoring are defined against the incoming
                // report, and a draw between the two would show new findings
                // under old session state.
                self.findings.apply_snapshot(&s.report);
                self.graph.apply_snapshot(&s);
                self.log.apply_snapshot(&s.log);
                self.snapshot = Some(s);
                self.error = None;
                self.refreshed_at = Some(Instant::now());
            }
            // The old snapshot stays; see the field doc. `:#` flattens the
            // anyhow chain into one line -- the cause ("reading manifest ...:
            // No such file") is the actionable half.
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    /// The footer's refresh cell. Pure, so the refresh-semantics tests can
    /// assert on the words: stale is STATED, never just coloured -- red alone
    /// is invisible to tests, pipes and some eyes.
    fn footer_status(&self) -> String {
        if self.pending {
            format!("{} reconciling", SPINNER[self.spinner % SPINNER.len()])
        } else if let Some(e) = &self.inspect_error {
            // `e` already carries the exact command line (inspect::run_show
            // builds it so) -- the banner's whole job.
            format!("INSPECT FAILED -- {e}")
        } else if let Some(e) = &self.error {
            format!("REFRESH FAILED -- showing last good state: {e}")
        } else if let Some(t) = self.refreshed_at {
            format!("refreshed {}s ago", t.elapsed().as_secs())
        } else {
            // Pre-first-outcome with nothing pending: only reachable when the
            // startup request itself could not be sent.
            "no data yet".into()
        }
    }

    /// Tab: cycle focus. Focusing the graph or the log also surfaces its tab
    /// -- keys going to a pane that is not on screen would be the least
    /// visible way for input to vanish.
    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Findings => Focus::Graph,
            Focus::Graph => Focus::Log,
            Focus::Log => Focus::Findings,
        };
        match self.focus {
            Focus::Graph => self.tab = RightTab::Graph,
            Focus::Log => self.tab = RightTab::Log,
            Focus::Findings => {}
        }
    }

    /// Point the log at a context and refresh. One path for both entrances
    /// (graph Enter, finding Enter), so the tab switch and the job issue can
    /// never come apart. Focus moves WITH the tab: leaving focus on the pane
    /// the jump just hid would send the next j/k to an off-screen widget --
    /// the least visible way for input to vanish (the cycle_focus rule).
    fn jump_to_log(&mut self, ctx: Context, jobs: &Sender<Job>) {
        self.log_ctx = ctx;
        self.tab = RightTab::Log;
        self.focus = Focus::Log;
        // The pane keeps showing the previous context's text -- captioned
        // with ITS title, so nothing misreads -- until the outcome lands;
        // the footer spinner covers the gap.
        self.request_refresh(jobs);
    }

    /// Enter on a finding: jump the log to its subject, when the subject IS a
    /// modelled ref.
    fn enter_on_finding(&mut self, jobs: &Sender<Job>) {
        let Some(ctx) = self
            .snapshot
            .as_ref()
            .and_then(|s| {
                self.findings
                    .selected_finding(&s.report)
                    .and_then(|fd| modelled_context(&s.manifest, &fd.subject))
            })
        else {
            return;
        };
        self.jump_to_log(ctx, jobs);
    }

    /// Per-focus action dispatch. Global actions (quit, refresh, suspend,
    /// focus, help) and the log's Enter (it needs the terminal) are handled in
    /// `run` before this is reached.
    fn handle_action(&mut self, action: Action, jobs: &Sender<Job>) {
        match self.focus {
            Focus::Findings => match action {
                Action::Up => {
                    self.findings.state.key_up();
                }
                Action::Down => {
                    self.findings.state.key_down();
                }
                Action::Close => {
                    self.findings.state.key_left();
                }
                Action::Open => {
                    self.findings.state.key_right();
                }
                Action::Tick => self.findings.toggle_tick(),
                Action::Enter => self.enter_on_finding(jobs),
                _ => {}
            },
            Focus::Graph => match action {
                Action::Up => {
                    self.graph.state.key_up();
                }
                Action::Down => {
                    self.graph.state.key_down();
                }
                Action::Close => {
                    self.graph.state.key_left();
                }
                Action::Open => {
                    self.graph.state.key_right();
                }
                Action::Enter => self.jump_to_log(self.graph.log_context(), jobs),
                _ => {}
            },
            Focus::Log => match action {
                Action::Up => self.log.key_up(),
                Action::Down => {
                    if let Some(s) = &self.snapshot {
                        self.log.key_down(&s.log);
                    }
                }
                // Enter is run's business (terminal handover); the tree keys
                // are inert here on purpose -- one nav vocabulary, not three.
                _ => {}
            },
        }
    }

    /// The inspection decision, headless: the exact argv Enter-on-a-commit
    /// would run, or None when there is nothing sound to inspect -- no
    /// snapshot, a failed log, or no commit vouched for under the cursor
    /// (between commits, or the mapping is down; `selected_oid` refuses both).
    fn inspect_argv(&self) -> Option<Vec<String>> {
        let s = self.snapshot.as_ref()?;
        let view = s.log.as_ref().ok()?;
        let oid = self.log.selected_oid(view)?;
        // The same wine-dir derivation the worker uses (workspace_root carries
        // the load-bearing empty-parent filter) -- against the manifest the
        // snapshot was derived FROM, so the argv names the repo the oid came
        // out of even if sodactl.toml was edited since.
        let wine = sodactl_core::workspace_root(&self.manifest).join(&s.manifest.repo.wine);
        Some(inspect::show_argv(self.tig, &wine, oid))
    }

    /// Enter in the log: suspend into `tig show`/`git show`, come back, and
    /// say so if it failed. The terminal is ALWAYS re-entered before the
    /// failure is looked at -- there must be a screen to print the banner on.
    fn inspect_selected(&mut self, term: &mut Term) -> Result<()> {
        let Some(argv) = self.inspect_argv() else { return Ok(()) };
        if let Err(e) = terminal::with_suspended(term, || inspect::run_show(&argv))? {
            self.inspect_error = Some(e);
        }
        Ok(())
    }

    /// The detail pane follows the FOCUSED pane's selection: three sources,
    /// every arm words -- the pane is never left blank or stale.
    fn detail_lines(&self, s: &Snapshot) -> Vec<Line<'static>> {
        match self.focus {
            Focus::Findings => {
                if let Some(fd) = self.findings.selected_finding(&s.report) {
                    detail::finding_lines(fd)
                } else if let Some(sev) = self.findings.selected_group() {
                    detail::group_lines(sev, s.report.count(sev))
                } else {
                    detail::nothing_lines(s.report.findings.is_empty())
                }
            }
            Focus::Graph => match self.graph.selection(s) {
                Some(GraphSel::Home) => {
                    detail::home_lines(&s.manifest.repo.home_base, &s.dag.restack_order)
                }
                Some(GraphSel::Tier(n)) => detail::tier_lines(n, &s.dag.restack_order),
                Some(GraphSel::Stitch(st)) => detail::stitch_lines(st),
                Some(GraphSel::Consumer(c)) => detail::consumer_lines(c),
                Some(GraphSel::StitchGroup) => {
                    detail::graph_group_lines("stitches", s.manifest.stitch.len())
                }
                Some(GraphSel::ConsumerGroup) => {
                    detail::graph_group_lines("consumers", s.report.consumers.len())
                }
                // Transient: the next apply_snapshot re-anchors. Still words.
                None => vec![Line::from("nothing selected in the graph pane")],
            },
            Focus::Log => match &s.log {
                Ok(v) => detail::commit_lines(
                    self.log.selected_oid(v),
                    v.mapping_error.is_some(),
                    self.tig,
                ),
                Err(_) => vec![Line::from("the log failed -- the log pane has the cause")],
            },
        }
    }

    /// The frame: findings checklist (left), graph/log tabs over
    /// context-following detail (right), lifecycle footer. `&mut` because the
    /// tree widgets mutate their scroll/selection state on render.
    fn draw(&mut self, f: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());

        match &self.snapshot {
            Some(s) => {
                // 35% findings per the approved layout: the checklist is a
                // navigation column; the prose that explains it needs the
                // width.
                let [left, right] =
                    Layout::horizontal([Constraint::Percentage(35), Constraint::Fill(1)])
                        .areas(body);
                let [tabs, det] =
                    Layout::vertical([Constraint::Percentage(40), Constraint::Fill(1)])
                        .areas(right);
                self.findings.render(f, left, &s.report, self.focus == Focus::Findings);
                match self.tab {
                    RightTab::Graph => {
                        self.graph.render(f, tabs, s, self.focus == Focus::Graph);
                    }
                    RightTab::Log => {
                        self.log.render(f, tabs, &s.log, self.focus == Focus::Log);
                    }
                }
                let lines = self.detail_lines(s);
                detail::render(f, det, lines);
            }
            None => f.render_widget(
                Paragraph::new(match &self.error {
                    // Startup failed and there is nothing older to show: the
                    // error IS the screen, not a blank one.
                    Some(e) => format!("no data: {e}"),
                    None => "reconciling...".to_string(),
                }),
                body,
            ),
        }

        f.render_widget(Paragraph::new(self.footer_line()), footer);

        if self.show_help {
            // Over the whole frame, footer included: while help is up, the
            // bindings are the subject.
            help::render(f, f.area());
        }
    }

    /// The footer, as one styled line -- pulled out of `draw` so the three
    /// lifecycle states (spinner, refreshed, stale-error) are snapshot-testable
    /// without a full app frame.
    fn footer_line(&self) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        match &self.snapshot {
            Some(s) => {
                // Same words as ever ("0 blocker, 14 drift, 15 hygiene"),
                // now in the theme's severity colours -- reinforcement, the
                // words stay the carrier. A ZERO count renders quiet instead:
                // a permanently red "0 blocker" would train the eye to ignore
                // red, which is the one lesson this footer must never teach.
                for (i, (sev, word)) in [
                    (Severity::Blocker, "blocker"),
                    (Severity::Drift, "drift"),
                    (Severity::Hygiene, "hygiene"),
                ]
                .into_iter()
                .enumerate()
                {
                    if i > 0 {
                        spans.push(Span::raw(", "));
                    }
                    let n = s.report.count(sev);
                    let style = if n > 0 {
                        Style::default().fg(theme::severity_color(sev))
                    } else {
                        theme::ticked_style()
                    };
                    spans.push(Span::styled(format!("{n} {word}"), style));
                }
            }
            None => spans.push(Span::raw("-")),
        }
        spans.push(Span::raw("  |  "));
        // Red only when the status IS an error and no spinner is covering it:
        // the words say which error (footer_status), the colour just points.
        let status_style = if (self.error.is_some() || self.inspect_error.is_some())
            && !self.pending
        {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        spans.push(Span::styled(self.footer_status(), status_style));
        // The one discoverability hint: without it, `?` is a binding only
        // people who read keys.rs know about.
        spans.push(Span::styled("  |  ? help", theme::ticked_style()));
        Line::from(spans)
    }

    pub fn run(mut self, term: &mut Term) -> Result<()> {
        let manifest = self.manifest.clone();
        let (jobs, outcomes) =
            worker::spawn(move |job: &Job| worker::run_once(&manifest, &job.log_ctx));
        // Startup trigger: the first frame must already say "reconciling", not
        // wait for a keypress to begin.
        self.request_refresh(&jobs);
        loop {
            while let Ok(o) = outcomes.try_recv() {
                self.apply_outcome(o);
            }
            term.draw(|f| self.draw(f))?;
            // 100 ms poll: the tick that animates the spinner, re-renders
            // "refreshed Ns ago", and bounds how stale the outcome drain above
            // can get. A blocking `read()` would freeze all three.
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => {
                        // Any keypress acknowledges a shell-out failure
                        // banner -- including the one that just caused it
                        // (cleared here, re-set below if Enter fails again).
                        self.inspect_error = None;
                        let action = keys::action_for(KeyCombination::from(k));
                        if self.show_help {
                            match action {
                                // Quit still quits from inside help; anything
                                // else -- bound or not -- only dismisses. A
                                // key that ALSO acted would mutate a pane the
                                // overlay is covering.
                                Some(Action::Quit) => return Ok(()),
                                _ => self.show_help = false,
                            }
                            continue;
                        }
                        match action {
                            // Quit drops `jobs`; the worker winds down after
                            // its current read-only job or dies with the
                            // process -- either is safe. (ctrl-c is bound in
                            // the table too: raw mode swallowed SIGINT.)
                            Some(Action::Quit) => return Ok(()),
                            Some(Action::Refresh) => self.request_refresh(&jobs),
                            Some(Action::Suspend) => terminal::suspend(term)?,
                            Some(Action::CycleFocus) => self.cycle_focus(),
                            Some(Action::Help) => self.show_help = true,
                            // The log's Enter needs the terminal (suspend into
                            // tig/git show), which headless handle_action must
                            // never touch -- so it is dispatched here.
                            Some(Action::Enter) if self.focus == Focus::Log => {
                                self.inspect_selected(term)?;
                            }
                            Some(a) => self.handle_action(a, &jobs),
                            None => {}
                        }
                    }
                    // The second refresh trigger (spec: manual `r` + focus).
                    // Coming back to the terminal after doing git work
                    // elsewhere is exactly when the screen is most likely
                    // stale.
                    Event::FocusGained => self.request_refresh(&jobs),
                    _ => {}
                }
            }
            self.spinner = self.spinner.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use sodactl_core::finding::Finding;
    use std::sync::mpsc;

    /// A snapshot recognisable by which delivery produced it: the marker rides
    /// in a finding code.
    fn snap(code: &'static str) -> Snapshot {
        let mut s = worker::test_snapshot();
        s.report.findings.push(Finding::new(code, Severity::Hygiene, "s", "sum", "rem"));
        s
    }

    fn shown(app: &App) -> &'static str {
        app.snapshot.as_ref().expect("a snapshot should be applied").report.findings[0].code
    }

    #[test]
    fn stale_outcomes_lose_to_the_newest_generation_in_either_order() {
        let (tx, _keep_alive) = mpsc::channel();
        // Newest-first delivery: gen 1 limps in after gen 2 already landed.
        let mut app = App::new(PathBuf::from("unused"));
        app.request_refresh(&tx); // gen 1
        app.request_refresh(&tx); // gen 2 -- the only one allowed to land
        app.apply_outcome(Outcome { gen: 2, result: Ok(snap("GEN_TWO")) });
        app.apply_outcome(Outcome { gen: 1, result: Ok(snap("GEN_ONE")) });
        assert_eq!(shown(&app), "GEN_TWO");
        assert!(!app.pending);

        // Issue-order delivery: gen 1 arrives while gen 2 is already issued.
        // It must not render even briefly, and the newest job stays owed.
        let mut app = App::new(PathBuf::from("unused"));
        app.request_refresh(&tx);
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 1, result: Ok(snap("GEN_ONE")) });
        assert!(app.snapshot.is_none(), "a superseded outcome must not render");
        assert!(app.pending, "the newest generation is still in flight");
        app.apply_outcome(Outcome { gen: 2, result: Ok(snap("GEN_TWO")) });
        assert_eq!(shown(&app), "GEN_TWO");
        assert!(!app.pending);
    }

    #[test]
    fn failed_refresh_keeps_the_old_snapshot_and_says_so() {
        let (tx, _keep_alive) = mpsc::channel();
        let mut app = App::new(PathBuf::from("unused"));
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 1, result: Ok(snap("GOOD")) });
        assert!(app.error.is_none());

        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 2, result: Err(anyhow!("wine gitdir vanished")) });
        // The screen keeps the last good state...
        assert_eq!(shown(&app), "GOOD");
        // ...and the footer says stale in words, cause included -- not just in
        // red, which no test (and no pipe) can see.
        let status = app.footer_status();
        assert!(status.contains("last good state"), "{status}");
        assert!(status.contains("wine gitdir vanished"), "{status}");

        // A later success clears the banner: the error never outlives its
        // truth.
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 3, result: Ok(snap("NEWER")) });
        assert!(app.error.is_none());
        assert_eq!(shown(&app), "NEWER");
    }

    /// The Stage-5 wiring claim, headless: Enter on a finding whose subject
    /// is a modelled ref switches the context, surfaces the Log tab and
    /// issues a refresh; an unmodelled subject changes nothing.
    #[test]
    fn enter_on_a_finding_jumps_the_log_iff_the_subject_is_modelled() {
        let (tx, _keep_alive) = mpsc::channel();
        let mut app = App::new(PathBuf::from("unused"));
        app.request_refresh(&tx);
        // The fixture manifest models soda-ntdll as a tier; rr/cafe is a
        // cache path -- present as a finding subject, absent from the graph.
        let mut s = worker::test_snapshot();
        s.report.findings.push(Finding::new(
            "TIER_CHILD_STALE",
            Severity::Drift,
            "soda-ntdll",
            "sum",
            "rem",
        ));
        s.report.findings.push(Finding::new(
            "RR_NO_POSTIMAGE",
            Severity::Hygiene,
            "rr/cafe",
            "sum",
            "rem",
        ));
        s.report.sort();
        app.apply_outcome(Outcome { gen: 1, result: Ok(s) });
        assert!(!app.pending);
        // apply_snapshot pre-selected the worst finding: the stale tier.

        app.enter_on_finding(&tx);
        assert_eq!(app.log_ctx, Context::Tier("soda-ntdll".into()));
        assert_eq!(app.tab, RightTab::Log);
        assert_eq!(app.focus, Focus::Log, "keys must follow the pane the jump surfaced");
        assert!(app.pending, "a context switch IS a refresh");

        // The unmodelled subject: select it, Enter must be a no-op.
        app.focus = Focus::Findings;
        use crate::panes::findings::NodeId;
        app.findings.state.select(vec![
            NodeId::Sev(Severity::Hygiene),
            NodeId::F { code: "RR_NO_POSTIMAGE", subject: "rr/cafe".into() },
        ]);
        let gen_before = app.gen;
        app.enter_on_finding(&tx);
        assert_eq!(app.log_ctx, Context::Tier("soda-ntdll".into()), "context must not move");
        assert_eq!(app.gen, gen_before, "no refresh for an unmodelled subject");
    }

    // ---- Stage 6/7: inspection decision + footer states ----

    use crate::gitlog::LogView;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const OID: &str = "aaaaaaaa94d5eb2f423a1d194ce67dae0f5db878";

    fn app_with_log(line_oids: Vec<Option<String>>) -> App {
        let (tx, _keep_alive) = mpsc::channel();
        // Relative manifest path on purpose: workspace_root's paid-for filter
        // makes this "." (see sodactl_core::workspace_root), so the derived
        // wine dir is deterministic in the argv assertions below.
        let mut app = App::new(PathBuf::from("tools/sodactl.toml"));
        app.request_refresh(&tx);
        let mut s = worker::test_snapshot();
        s.log = Ok(LogView {
            title: "t".into(),
            text: line_oids.iter().map(|_| "line").collect::<Vec<_>>().join("\n"),
            line_oids,
            mapping_error: None,
        });
        app.apply_outcome(Outcome { gen: 1, result: Ok(s) });
        app
    }

    /// The headless half of Enter-on-a-commit: the exact argv, tool chosen by
    /// the startup probe, wine dir derived like the worker derives it.
    #[test]
    fn inspect_argv_names_the_probed_tool_and_the_exact_commit() {
        let mut app = app_with_log(vec![Some(OID.into())]);
        app.tig = false;
        assert_eq!(
            app.inspect_argv().expect("a vouched-for commit must be inspectable"),
            ["git", "-C", "./wine", "show", OID].map(String::from)
        );
        app.tig = true;
        assert_eq!(app.inspect_argv().unwrap()[0], "tig");
    }

    /// No sound target, no argv: an unvouched line (mapping down or between
    /// commits) and a failed log must both refuse -- this is the input `git
    /// show` would otherwise be fed.
    #[test]
    fn inspect_refuses_without_a_vouched_commit() {
        let app = app_with_log(vec![None]);
        assert!(app.inspect_argv().is_none(), "an unvouched line must not be inspectable");

        let (tx, _keep_alive) = mpsc::channel();
        let mut app = App::new(PathBuf::from("tools/sodactl.toml"));
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 1, result: Ok(worker::test_snapshot()) }); // log: Err
        assert!(app.inspect_argv().is_none(), "a failed log has no commits to inspect");
    }

    /// A failed shell-out is its own footer state, distinct from a stale
    /// refresh -- the words must not claim the DATA is bad when only the
    /// inspection was.
    #[test]
    fn a_failed_inspect_has_its_own_footer_words() {
        let mut app = app_with_log(vec![Some(OID.into())]);
        app.inspect_error =
            Some("exit status: 128 -- `git -C ./wine show aaaaaaaa`".into());
        let s = app.footer_status();
        assert!(s.contains("INSPECT FAILED"), "{s}");
        assert!(s.contains("`git -C ./wine show aaaaaaaa`"), "the exact command line: {s}");
        assert!(!s.contains("last good state"), "must not read as a refresh failure: {s}");
    }

    fn footer_frame(app: &App, width: u16) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
        term.draw(|f| f.render_widget(Paragraph::new(app.footer_line()), f.area())).unwrap();
        term
    }

    /// The three footer lifecycle states, pinned as snapshots: spinner while
    /// pending, "refreshed Ns ago" after success, the stale banner carrying
    /// the error after failure. Counts stay in words throughout.
    #[test]
    fn footer_states_spinner_refreshed_and_stale() {
        let (tx, _keep_alive) = mpsc::channel();
        let mut app = App::new(PathBuf::from("unused"));
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 1, result: Ok(snap("SOME_FINDING")) });

        // Pending: the spinner covers the cell (index 0 -> the first glyph).
        app.request_refresh(&tx);
        insta::assert_snapshot!("footer_pending", footer_frame(&app, 60).backend());

        // Refreshed: 0s, deterministically -- the Instant was set this test.
        app.apply_outcome(Outcome { gen: 2, result: Ok(snap("SOME_FINDING")) });
        insta::assert_snapshot!("footer_refreshed", footer_frame(&app, 60).backend());

        // Stale: the words carry the cause; the colour is red -- asserted on
        // a cell, since the snapshot text cannot see styles.
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 3, result: Err(anyhow!("wine gitdir vanished")) });
        let term = footer_frame(&app, 110);
        insta::assert_snapshot!("footer_stale", term.backend());
        let buf = term.backend().buffer();
        let red = (0..buf.area.width)
            .any(|x| buf.cell((x, 0)).unwrap().fg == Color::Red);
        assert!(red, "the stale banner must also point in red");
    }

    /// Focus cycling surfaces the pane it lands on; findings focus leaves the
    /// tab alone (yanking the log off screen to read a finding would lose the
    /// reader's place).
    #[test]
    fn focus_cycle_surfaces_graph_and_log_tabs() {
        let mut app = App::new(PathBuf::from("unused"));
        assert_eq!(app.focus, Focus::Findings);
        app.cycle_focus();
        assert_eq!((app.focus, app.tab), (Focus::Graph, RightTab::Graph));
        app.cycle_focus();
        assert_eq!((app.focus, app.tab), (Focus::Log, RightTab::Log));
        app.cycle_focus();
        assert_eq!((app.focus, app.tab), (Focus::Findings, RightTab::Log));
    }

    // ---- adversarial verification: the real event path, end to end ----

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn buffer_text(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf.cell((x, y)).unwrap().symbol()).collect())
            .collect()
    }

    /// Drive a key through the REAL resolution the event loop uses:
    /// KeyEvent -> KeyCombination -> keys::action_for -> handle_action.
    fn press(app: &mut App, jobs: &Sender<Job>, code: KeyCode) {
        if let Some(a) =
            keys::action_for(KeyCombination::from(KeyEvent::new(code, KeyModifiers::NONE)))
        {
            app.handle_action(a, jobs);
        }
    }

    /// Cold start through the real path: outcome applied, first-EVER draw --
    /// the frame must already show the worst finding highlighted and the
    /// detail pane explaining IT (the select_first-is-a-no-op class), and the
    /// table-resolved keys must then move/tick against that same tree.
    #[test]
    fn cold_start_first_frame_selects_and_explains_the_worst_finding() {
        let (tx, _keep_alive) = mpsc::channel();
        let mut app = App::new(PathBuf::from("unused"));
        app.request_refresh(&tx);
        let mut s = worker::test_snapshot();
        s.report.findings.push(Finding::new(
            "RR_NO_POSTIMAGE",
            Severity::Hygiene,
            "rr/cafe",
            "an rr summary",
            "rr remedy",
        ));
        s.report.findings.push(Finding::new(
            "PINNED_CONSUMER_UNTAGGED",
            Severity::Blocker,
            "spritzwine",
            "the worst summary",
            "tag it now",
        ));
        s.report.sort();
        app.apply_outcome(Outcome { gen: 1, result: Ok(s) });

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let all = buffer_text(&term).join("\n");
        assert!(all.contains("PINNED_CONSUMER_UNTAGGED"), "{all}");
        assert!(all.contains("the worst summary"), "detail must follow the selection:\n{all}");
        assert!(all.contains("tag it now"), "the remedy must reach the detail pane:\n{all}");
        // The highlight exists on frame one -- x-bounded to the findings pane
        // (35% of 120), so a right-pane style cannot vouch for it.
        use ratatui::style::Modifier;
        let buf = term.backend().buffer();
        let highlighted = (0..buf.area.height).any(|y| {
            (0..42).any(|x| buf.cell((x, y)).unwrap().modifier.contains(Modifier::REVERSED))
        });
        assert!(highlighted, "the worst finding must render highlighted on frame one");

        // The real key path: j lands on the hygiene group header (Space is a
        // deliberate no-op there), a second j reaches its leaf, Space ticks
        // THAT leaf and only it.
        press(&mut app, &tx, KeyCode::Char('j'));
        press(&mut app, &tx, KeyCode::Char(' ')); // group header: no-op
        press(&mut app, &tx, KeyCode::Char('j'));
        press(&mut app, &tx, KeyCode::Char(' '));
        term.draw(|f| app.draw(f)).unwrap();
        let all = buffer_text(&term).join("\n");
        assert!(all.contains("[x] RR_NO_POSTIMAGE"), "j j space must tick the leaf:\n{all}");
        assert!(all.contains("[ ] PINNED_CONSUMER_UNTAGGED"), "{all}");
    }

    /// LIVE (read-only, skipped unless SODACTL_LIVE_MANIFEST is set): the
    /// rendered findings tree shows exactly the codes the derived Report
    /// carries, one leaf per finding -- the same Report `sodactl check
    /// --json` serialises, so the two surfaces cannot disagree on content.
    #[test]
    #[ignore]
    fn live_findings_tree_agrees_with_the_report() {
        // Silent skip without the env var (the crate-wide stdout guardrail
        // forbids raw prints, tests included): #[ignore] already keeps this
        // out of the default run.
        let Ok(path) = std::env::var("SODACTL_LIVE_MANIFEST") else { return };
        let manifest = PathBuf::from(path);
        let s = worker::run_once(&manifest, &Context::Overview).unwrap();
        let mut codes_in_report: Vec<&str> = s.report.findings.iter().map(|f| f.code).collect();
        codes_in_report.sort();
        codes_in_report.dedup();
        let n_findings = s.report.findings.len();

        let (tx, _keep_alive) = mpsc::channel();
        let mut app = App::new(manifest);
        app.request_refresh(&tx);
        app.apply_outcome(Outcome { gen: 1, result: Ok(s) });
        let mut term = Terminal::new(TestBackend::new(240, 60)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        // Every leaf row is "[ ]/[x] CODE subject"; collect the codes.
        let mut codes_on_screen: Vec<String> = buffer_text(&term)
            .iter()
            .filter_map(|r| {
                let rest = r.split("[ ] ").nth(1).or_else(|| r.split("[x] ").nth(1))?;
                Some(rest.split_whitespace().next()?.to_string())
            })
            .collect();
        let leaf_rows = codes_on_screen.len();
        assert_eq!(leaf_rows, n_findings, "one leaf per finding");
        codes_on_screen.sort();
        codes_on_screen.dedup();
        assert_eq!(
            codes_on_screen,
            codes_in_report.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "the tree must show exactly the report's codes"
        );
    }
}
