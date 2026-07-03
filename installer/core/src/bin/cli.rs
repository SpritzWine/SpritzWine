//! `spritzwine-cli-install` -- headless front-end to the installer core.
//!
//! Same deploy logic as the GUI (`spritzwine-setup`), no display required -- for servers,
//! scripts and CI. By default the payload is the directory this binary lives in (i.e. when
//! shipped inside the relocatable tarball); override with `--payload`.
//!
//!   spritzwine-cli-install --list
//!   spritzwine-cli-install --steam --lutris
//!   spritzwine-cli-install --payload dist/spritzwine-v0.1.0 --custom ~/sw --prefix ~/.sw-test
//!   spritzwine-cli-install --steam-dir /path/to/compatibilitytools.d   # explicit target

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{bail, Context, Result};
use spritzwine_install_core as core;

const HELP: &str = "\
spritzwine-cli-install -- deploy a relocatable SpritzWine build

USAGE:
    spritzwine-cli-install [TARGETS] [OPTIONS]

TARGETS (choose one or more):
    --steam                 Register as a Steam Play tool in the first detected Steam
    --steam-dir <DIR>       ...or into an explicit compatibilitytools.d directory
    --lutris                Register as a Wine runner in the first detected Lutris
    --lutris-dir <DIR>      ...or into an explicit runners/wine directory
    --custom <DIR>          Install the tree into an arbitrary directory
    --prefix <DIR>          Initialise (wineboot) a WINEPREFIX with this build

OPTIONS:
    --payload <DIR>         The SpritzWine tree to install (default: this binary's dir)
    --name <NAME>           Steam tool / Lutris runner directory name (default: SpritzWine)
    --no-beisl              Don't install the bundled Beisl perf tracer (installed by default)
    --list                  Print detected Steam/Lutris targets and exit
    -h, --help              Show this help
";

#[derive(Default)]
struct Args {
    payload: Option<PathBuf>,
    name: String,
    custom: Option<PathBuf>,
    prefix: Option<PathBuf>,
    lutris_auto: bool,
    lutris_dir: Option<PathBuf>,
    steam_auto: bool,
    steam_dir: Option<PathBuf>,
    no_beisl: bool,
    list: bool,
}

fn parse() -> Result<Args> {
    let mut a = Args {
        name: core::DEFAULT_NAME.to_string(),
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut next = |flag: &str| -> Result<PathBuf> {
            it.next()
                .map(PathBuf::from)
                .with_context(|| format!("{flag} requires a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--list" => a.list = true,
            "--no-beisl" => a.no_beisl = true,
            "--steam" => a.steam_auto = true,
            "--lutris" => a.lutris_auto = true,
            "--payload" => a.payload = Some(next("--payload")?),
            "--custom" => a.custom = Some(next("--custom")?),
            "--prefix" => a.prefix = Some(next("--prefix")?),
            "--steam-dir" => a.steam_dir = Some(next("--steam-dir")?),
            "--lutris-dir" => a.lutris_dir = Some(next("--lutris-dir")?),
            "--name" => {
                a.name = it.next().context("--name requires a value")?;
            }
            other => bail!("unknown argument: {other} (try --help)"),
        }
    }
    Ok(a)
}

fn print_targets() {
    let steam = core::detect_steam_roots();
    let lutris = core::detect_lutris_roots();
    println!("Steam targets:");
    if steam.is_empty() {
        println!("  (none detected)");
    }
    for r in &steam {
        println!("  {} -> {}", r.label, r.compat_dir.display());
    }
    println!("Lutris targets:");
    if lutris.is_empty() {
        println!("  (none detected)");
    }
    for r in &lutris {
        println!("  {} -> {}", r.label, r.runners_wine_dir.display());
    }
}

/// Progress callback: an overwriting bar on a TTY, sparse lines otherwise. Only redraws
/// when the integer percentage changes, so a multi-thousand-file copy doesn't flood a
/// non-terminal (log/CI) stderr.
fn progress(p: core::Progress) {
    static LAST_PCT: AtomicU32 = AtomicU32::new(u32::MAX);
    let pct = (p.fraction() * 100.0).round() as u32;
    if LAST_PCT.swap(pct, Ordering::Relaxed) == pct {
        return;
    }
    if std::io::stderr().is_terminal() {
        let filled = (pct / 4) as usize; // 25-wide bar
        eprint!("\r  [{}{}] {pct:3}%", "#".repeat(filled), " ".repeat(25 - filled));
        let _ = std::io::stderr().flush();
    } else if pct % 10 == 0 {
        eprintln!("  {pct}%");
    }
}

fn run() -> Result<()> {
    let a = parse()?;
    if a.list {
        print_targets();
        return Ok(());
    }

    let want_install =
        a.custom.is_some() || a.lutris_auto || a.lutris_dir.is_some() || a.steam_auto || a.steam_dir.is_some();
    if !want_install && a.prefix.is_none() {
        print!("{HELP}");
        bail!("no target selected");
    }

    let payload = match a.payload {
        Some(p) => {
            core::validate_payload(&p)
                .with_context(|| format!("--payload {} is not a SpritzWine tree", p.display()))?;
            p
        }
        None => core::detect_payload()?,
    };
    println!("Payload: {}", payload.display());

    if let Some(dir) = &a.custom {
        println!("==> Custom: {}", dir.display());
        let dest = core::install_custom(&payload, dir, &mut progress)?;
        eprintln!();
        println!("    installed to {}", dest.display());
    }

    // --lutris / --steam deploy to EVERY detected root (e.g. native + Flatpak Lutris), so
    // the build is found whichever one the user runs; --*-dir targets a single explicit dir.
    if a.lutris_auto || a.lutris_dir.is_some() {
        let dirs: Vec<PathBuf> = match &a.lutris_dir {
            Some(d) => vec![d.clone()],
            None => {
                let roots = core::detect_lutris_roots();
                if roots.is_empty() {
                    bail!("no Lutris installation detected (use --lutris-dir)");
                }
                roots.into_iter().map(|r| r.runners_wine_dir).collect()
            }
        };
        for runners in dirs {
            println!("==> Lutris: {}/{}", runners.display(), a.name);
            let dest = core::install_lutris(&payload, &runners, &a.name, &mut progress)?;
            eprintln!();
            println!("    installed runner to {}", dest.display());
        }
    }

    if a.steam_auto || a.steam_dir.is_some() {
        let dirs: Vec<PathBuf> = match &a.steam_dir {
            Some(d) => vec![d.clone()],
            None => {
                let roots = core::detect_steam_roots();
                if roots.is_empty() {
                    bail!("no Steam installation detected (use --steam-dir)");
                }
                roots.into_iter().map(|r| r.compat_dir).collect()
            }
        };
        for compat in dirs {
            println!("==> Steam: {}/{}", compat.display(), a.name);
            let dest = core::install_steam(&payload, &compat, &a.name, &mut progress)?;
            eprintln!();
            println!(
                "    installed tool to {} (select it in Steam under 'Force compatibility tool')",
                dest.display()
            );
        }
    }

    if let Some(prefix) = &a.prefix {
        println!("==> WINEPREFIX: {}", prefix.display());
        core::init_prefix(&payload, prefix, &mut |line| println!("    {line}"))?;
    }

    // Beisl (the bundled perf tracer) installs onto PATH by default when present.
    if core::has_beisl(&payload) && !a.no_beisl {
        println!("==> Beisl (perf tracer)");
        core::install_beisl(&payload, &mut |line| println!("    {line}"))?;
    }

    println!("Done.");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
