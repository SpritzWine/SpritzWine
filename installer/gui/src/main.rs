//! `spritzwine-setup` -- a small libcosmic installer for SpritzWine.
//!
//! A four-page wizard (Welcome -> Targets -> Progress -> Done) over the GUI-free
//! `spritzwine_install_core` crate, which does all the actual work. When shipped inside the
//! relocatable tarball this binary sits next to `bin/`, so it installs the surrounding
//! tree; pass `--payload <dir>` to point it elsewhere (e.g. running from a dev checkout).
//!
//! Installs run on a background thread that publishes progress into an `Arc<Mutex<_>>`;
//! the UI polls it on a timer. That keeps the event loop responsive without leaning on the
//! most version-volatile parts of libcosmic's async/Task API.

#![windows_subsystem = "windows"] // (no-op on Linux; keeps intent clear)

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Length, Subscription};
use cosmic::widget;
use cosmic::{Application, Element};

use spritzwine_install_core as core;
use spritzwine_install_core::{LutrisRoot, SteamRoot};

/// SpritzWine logo shown on the Welcome page (PNG, white background already cut out).
const LOGO: &[u8] = include_bytes!("../assets/spritzwine.png");

// ---------------------------------------------------------------------------------------
// Background install worker
// ---------------------------------------------------------------------------------------

/// One deploy step queued from the user's selections.
enum Action {
    Custom(PathBuf),
    Lutris { dir: PathBuf, name: String },
    Steam { dir: PathBuf, name: String },
    Prefix(PathBuf),
}

impl Action {
    fn label(&self) -> String {
        match self {
            Action::Custom(d) => format!("Custom location: {}", d.display()),
            Action::Lutris { dir, name } => format!("Lutris runner: {}/{name}", dir.display()),
            Action::Steam { dir, name } => format!("Steam tool: {}/{name}", dir.display()),
            Action::Prefix(p) => format!("Initialise WINEPREFIX: {}", p.display()),
        }
    }

    /// Run the step. `on(Some(frac), _)` updates the copy fraction; `on(_, Some(line))`
    /// appends a log line.
    fn run(&self, payload: &Path, on: &mut dyn FnMut(Option<f32>, Option<String>)) -> Result<(), String> {
        match self {
            Action::Custom(dir) => {
                let dest = core::install_custom(payload, dir, &mut |p| on(Some(p.fraction()), None))
                    .map_err(|e| format!("{e:#}"))?;
                on(None, Some(format!("Installed to {}", dest.display())));
            }
            Action::Lutris { dir, name } => {
                let dest = core::install_lutris(payload, dir, name, &mut |p| on(Some(p.fraction()), None))
                    .map_err(|e| format!("{e:#}"))?;
                on(None, Some(format!("Runner installed: {}", dest.display())));
            }
            Action::Steam { dir, name } => {
                let dest = core::install_steam(payload, dir, name, &mut |p| on(Some(p.fraction()), None))
                    .map_err(|e| format!("{e:#}"))?;
                on(None, Some(format!("Steam tool installed: {}", dest.display())));
            }
            Action::Prefix(prefix) => {
                core::init_prefix(payload, prefix, &mut |l| on(None, Some(l.to_string())))
                    .map_err(|e| format!("{e:#}"))?;
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Shared {
    log: Vec<String>,
    fraction: f32,
    current: String,
    finished: bool,
    error: Option<String>,
}

fn spawn_worker(payload: PathBuf, actions: Vec<Action>) -> Arc<Mutex<Shared>> {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let s2 = shared.clone();
    std::thread::spawn(move || {
        let total = actions.len().max(1) as f32;
        for (i, action) in actions.iter().enumerate() {
            {
                let mut s = s2.lock().unwrap();
                s.current = action.label();
                s.log.push(format!("==> {}", action.label()));
            }
            let base = i as f32 / total;
            let span = 1.0 / total;
            let res = action.run(&payload, &mut |frac, line| {
                let mut s = s2.lock().unwrap();
                if let Some(f) = frac {
                    s.fraction = base + span * f;
                }
                if let Some(l) = line {
                    s.log.push(l);
                }
            });
            if let Err(e) = res {
                let mut s = s2.lock().unwrap();
                s.log.push(format!("error: {e}"));
                s.error = Some(e);
                s.finished = true;
                return;
            }
        }
        let mut s = s2.lock().unwrap();
        s.fraction = 1.0;
        s.finished = true;
    });
    shared
}

// ---------------------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Targets,
    Progress,
    Done,
}

#[derive(Debug, Clone)]
enum Message {
    Next,
    Back,
    Cancel,
    ToggleSteam(bool),
    ToggleSteamRoot(usize, bool),
    ToggleLutris(bool),
    ToggleLutrisRoot(usize, bool),
    ToggleCustom(bool),
    CustomPath(String),
    TogglePrefix(bool),
    PrefixPath(String),
    Name(String),
    StartInstall,
    Tick,
}

struct Flags {
    payload: Result<PathBuf, String>,
}

struct AppModel {
    core: Core,
    page: Page,

    payload: Result<PathBuf, String>,
    steam_roots: Vec<SteamRoot>,
    lutris_roots: Vec<LutrisRoot>,

    name: String,
    steam_on: bool,
    steam_sel: Vec<bool>,
    lutris_on: bool,
    lutris_sel: Vec<bool>,
    custom_on: bool,
    custom_path: String,
    prefix_on: bool,
    prefix_path: String,

    worker: Option<Arc<Mutex<Shared>>>,
    fraction: f32,
    current: String,
    log: Vec<String>,
    finished: bool,
    error: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Short tag for a detected root's variant, for the compact per-root checkbox labels.
fn short_variant(label: &str) -> &'static str {
    if label.contains("Flatpak") {
        "Flatpak"
    } else {
        "native"
    }
}

impl AppModel {
    fn any_target_selected(&self) -> bool {
        (self.steam_on && self.steam_sel.iter().any(|&b| b))
            || (self.lutris_on && self.lutris_sel.iter().any(|&b| b))
            || (self.custom_on && !self.custom_path.trim().is_empty())
            || (self.prefix_on && !self.prefix_path.trim().is_empty())
    }

    fn build_actions(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        let name = if self.name.trim().is_empty() {
            core::DEFAULT_NAME.to_string()
        } else {
            self.name.trim().to_string()
        };
        if self.custom_on && !self.custom_path.trim().is_empty() {
            actions.push(Action::Custom(PathBuf::from(self.custom_path.trim())));
        }
        // Deploy to each root the user ticked (e.g. native and/or Flatpak Lutris).
        if self.lutris_on {
            for (i, r) in self.lutris_roots.iter().enumerate() {
                if self.lutris_sel.get(i).copied().unwrap_or(false) {
                    actions.push(Action::Lutris {
                        dir: r.runners_wine_dir.clone(),
                        name: name.clone(),
                    });
                }
            }
        }
        if self.steam_on {
            for (i, r) in self.steam_roots.iter().enumerate() {
                if self.steam_sel.get(i).copied().unwrap_or(false) {
                    actions.push(Action::Steam {
                        dir: r.compat_dir.clone(),
                        name: name.clone(),
                    });
                }
            }
        }
        if self.prefix_on && !self.prefix_path.trim().is_empty() {
            actions.push(Action::Prefix(PathBuf::from(self.prefix_path.trim())));
        }
        actions
    }
}

impl Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;

    const APP_ID: &'static str = "org.spritzwine.Installer";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Self::Flags) -> (Self, Task<Self::Message>) {
        let steam_roots = core::detect_steam_roots();
        let lutris_roots = core::detect_lutris_roots();
        let steam_sel = vec![true; steam_roots.len()];
        let lutris_sel = vec![true; lutris_roots.len()];

        let app = AppModel {
            core,
            page: Page::Welcome,
            payload: flags.payload,
            steam_on: !steam_roots.is_empty(),
            steam_sel,
            lutris_on: false,
            lutris_sel,
            steam_roots,
            lutris_roots,
            name: core::DEFAULT_NAME.to_string(),
            custom_on: false,
            custom_path: home().join("SpritzWine").display().to_string(),
            prefix_on: false,
            prefix_path: home().join(".spritzwine").display().to_string(),
            worker: None,
            fraction: 0.0,
            current: String::new(),
            log: Vec::new(),
            finished: false,
            error: None,
        };
        (app, Task::none())
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::Next => {
                if self.page == Page::Welcome && self.payload.is_ok() {
                    self.page = Page::Targets;
                }
            }
            Message::Back => {
                if self.page == Page::Targets {
                    self.page = Page::Welcome;
                }
            }
            Message::Cancel => std::process::exit(0),
            Message::ToggleSteam(v) => self.steam_on = v,
            Message::ToggleSteamRoot(i, v) => {
                if let Some(s) = self.steam_sel.get_mut(i) {
                    *s = v;
                }
            }
            Message::ToggleLutris(v) => self.lutris_on = v,
            Message::ToggleLutrisRoot(i, v) => {
                if let Some(s) = self.lutris_sel.get_mut(i) {
                    *s = v;
                }
            }
            Message::ToggleCustom(v) => self.custom_on = v,
            Message::CustomPath(p) => self.custom_path = p,
            Message::TogglePrefix(v) => self.prefix_on = v,
            Message::PrefixPath(p) => self.prefix_path = p,
            Message::Name(n) => self.name = n,
            Message::StartInstall => {
                if let Ok(payload) = self.payload.clone() {
                    if self.any_target_selected() {
                        let actions = self.build_actions();
                        self.worker = Some(spawn_worker(payload, actions));
                        self.page = Page::Progress;
                        self.fraction = 0.0;
                        self.log.clear();
                    }
                }
            }
            Message::Tick => {
                if let Some(w) = &self.worker {
                    let s = w.lock().unwrap();
                    self.fraction = s.fraction;
                    self.current = s.current.clone();
                    self.log = s.log.clone();
                    self.finished = s.finished;
                    self.error = s.error.clone();
                    if self.finished {
                        drop(s);
                        self.page = Page::Done;
                    }
                }
            }
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        if self.page == Page::Progress && !self.finished {
            cosmic::iced::time::every(Duration::from_millis(120)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let content = match self.page {
            Page::Welcome => self.view_welcome(),
            Page::Targets => self.view_targets(),
            Page::Progress => self.view_progress(),
            Page::Done => self.view_done(),
        };
        widget::container(content)
            .padding(28)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

// ---------------------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------------------

impl AppModel {
    fn view_welcome(&self) -> Element<'_, Message> {
        let logo = widget::image(widget::image::Handle::from_bytes(LOGO))
            .width(Length::Fixed(78.0))
            .height(Length::Fixed(128.0));
        let header = widget::Row::new()
            .spacing(16)
            .push(logo)
            .push(widget::text::title1("SpritzWine"));
        let mut col = widget::Column::new()
            .spacing(16)
            .push(header)
            .push(widget::text::body(
                "Install this SpritzWine build so you can pick it from Steam or Lutris, drop \
                 it in a folder of your choice, or set up a Wine prefix with it.",
            ));

        match &self.payload {
            Ok(p) => {
                col = col.push(widget::text::caption(format!("Build: {}", p.display())));
            }
            Err(e) => {
                col = col
                    .push(widget::text::body(
                        "Could not find a SpritzWine build to install.",
                    ))
                    .push(widget::text::caption(e.clone()))
                    .push(widget::text::caption(
                        "Run this from inside an extracted SpritzWine tree, or pass --payload <dir>.",
                    ));
            }
        }

        let mut next = widget::button::suggested("Next");
        if self.payload.is_ok() {
            next = next.on_press(Message::Next);
        }
        let buttons = widget::Row::new()
            .spacing(8)
            .push(widget::button::standard("Cancel").on_press(Message::Cancel))
            .push(widget::Space::new().width(Length::Fill))
            .push(next);

        col.push(widget::Space::new().height(Length::Fill)).push(buttons).into()
    }

    fn view_targets(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new()
            .spacing(14)
            .push(widget::text::title3("Where should it go?"))
            .push(widget::text::caption(
                "Choose one or more. Steam/Lutris let you pick this build from their menus.",
            ));

        // Name (used for the Steam tool / Lutris runner directory).
        col = col.push(
            widget::Row::new()
                .spacing(8)
                .push(widget::text::body("Name"))
                .push(widget::text_input("SpritzWine", &self.name).on_input(Message::Name)),
        );

        // Steam: master toggle + a per-root checkbox (native / Flatpak) in the same row.
        if self.steam_roots.is_empty() {
            col = col.push(widget::text::body("Steam — not detected"));
        } else {
            let mut srow = widget::Row::new().spacing(16).push(
                widget::checkbox(self.steam_on)
                    .label("Steam (Steam Play compatibility tool)")
                    .on_toggle(Message::ToggleSteam),
            );
            for (i, r) in self.steam_roots.iter().enumerate() {
                let checked = self.steam_sel.get(i).copied().unwrap_or(false);
                srow = srow.push(
                    widget::checkbox(checked)
                        .label(short_variant(&r.label))
                        .on_toggle(move |v| Message::ToggleSteamRoot(i, v)),
                );
            }
            col = col.push(srow);
        }

        // Lutris: master toggle + a per-root checkbox (native / Flatpak) in the same row.
        if self.lutris_roots.is_empty() {
            col = col.push(widget::text::body("Lutris — not detected"));
        } else {
            let mut lrow = widget::Row::new().spacing(16).push(
                widget::checkbox(self.lutris_on)
                    .label("Lutris (Wine runner)")
                    .on_toggle(Message::ToggleLutris),
            );
            for (i, r) in self.lutris_roots.iter().enumerate() {
                let checked = self.lutris_sel.get(i).copied().unwrap_or(false);
                lrow = lrow.push(
                    widget::checkbox(checked)
                        .label(short_variant(&r.label))
                        .on_toggle(move |v| Message::ToggleLutrisRoot(i, v)),
                );
            }
            col = col.push(lrow);
        }

        // Custom location
        col = col.push(
            widget::checkbox(self.custom_on)
                .label("Custom location")
                .on_toggle(Message::ToggleCustom),
        );
        if self.custom_on {
            col = col.push(
                widget::text_input("/path/to/install", &self.custom_path)
                    .on_input(Message::CustomPath),
            );
        }

        // Initialise a prefix
        col = col.push(
            widget::checkbox(self.prefix_on)
                .label("Initialise a Wine prefix with this build")
                .on_toggle(Message::TogglePrefix),
        );
        if self.prefix_on {
            col = col.push(
                widget::text_input("~/.spritzwine", &self.prefix_path).on_input(Message::PrefixPath),
            );
        }

        let mut install = widget::button::suggested("Install");
        if self.any_target_selected() {
            install = install.on_press(Message::StartInstall);
        }
        let buttons = widget::Row::new()
            .spacing(8)
            .push(widget::button::standard("Back").on_press(Message::Back))
            .push(widget::Space::new().width(Length::Fill))
            .push(install);

        col.push(widget::Space::new().height(Length::Fill)).push(buttons).into()
    }

    fn view_progress(&self) -> Element<'_, Message> {
        let mut logcol = widget::Column::new().spacing(2);
        for line in self.log.iter().rev().take(120).rev() {
            logcol = logcol.push(widget::text::monotext(line.clone()));
        }
        widget::Column::new()
            .spacing(14)
            .push(widget::text::title3("Installing…"))
            .push(widget::progress_bar::determinate_linear(self.fraction))
            .push(widget::text::caption(self.current.clone()))
            .push(
                widget::scrollable(logcol)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .into()
    }

    fn view_done(&self) -> Element<'_, Message> {
        let mut col = widget::Column::new().spacing(14);
        match &self.error {
            None => {
                col = col
                    .push(widget::text::title3("Done"))
                    .push(widget::text::body("SpritzWine was installed."));
                if self.steam_on && !self.steam_roots.is_empty() {
                    col = col.push(widget::text::caption(
                        "Steam: a game's Properties → Compatibility → Force the use of a specific \
                         Steam Play tool → select your build (restart Steam if it isn't listed).",
                    ));
                }
                if self.lutris_on && !self.lutris_roots.is_empty() {
                    col = col.push(widget::text::caption(
                        "Lutris: in a game's runner options, pick the Wine version by this name.",
                    ));
                }
            }
            Some(e) => {
                col = col
                    .push(widget::text::title3("Something went wrong"))
                    .push(widget::text::body(e.clone()));
            }
        }
        col.push(widget::Space::new().height(Length::Fill))
            .push(
                widget::Row::new()
                    .push(widget::Space::new().width(Length::Fill))
                    .push(widget::button::suggested("Close").on_press(Message::Cancel)),
            )
            .into()
    }
}

// ---------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------

/// Resolve the payload: `--payload <dir>` if given, else the directory this binary lives in
/// (the relocatable tree, when shipped inside the tarball).
fn resolve_payload() -> Result<PathBuf, String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--payload" {
            let dir = args.next().ok_or_else(|| "--payload requires a value".to_string())?;
            let dir = PathBuf::from(dir);
            return core::validate_payload(&dir)
                .map(|_| dir.clone())
                .map_err(|e| format!("{e:#}"));
        }
    }
    core::detect_payload().map_err(|e| format!("{e:#}"))
}

fn main() -> cosmic::iced::Result {
    let flags = Flags {
        payload: resolve_payload(),
    };
    let settings = Settings::default().size(cosmic::iced::Size::new(660.0, 600.0));
    cosmic::app::run::<AppModel>(settings, flags)
}
