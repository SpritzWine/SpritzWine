//! Deploy a relocatable SpritzWine tree to a target, and initialise prefixes.
//!
//! The "payload" is the relocatable tree (`bin/wine`, `lib/wine/...`, the `spritzwine`
//! launcher). Each install copies the whole payload to the destination -- copies, not
//! symlinks, so each target is independently relocatable and survives the payload being
//! moved or deleted. Copying is symlink- and permission-preserving and reports progress so
//! a GUI can show a bar without blocking its event loop.

use std::fs;
use std::io::{self, BufRead};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::steam;

/// Progress snapshot handed to the caller's callback during a copy.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
}

impl Progress {
    /// Completion in `[0.0, 1.0]` (1.0 when there is nothing to copy).
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            1.0
        } else {
            (self.done_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
        }
    }
}

// ---------------------------------------------------------------------------------------
// Payload discovery / validation
// ---------------------------------------------------------------------------------------

/// Locate the relocatable tree we are installing. When the installer is shipped *inside*
/// the tarball it sits next to `bin/`, so the directory of the running executable is the
/// payload. Returns an error (telling the caller to pass `--payload`) if that directory is
/// not a SpritzWine tree.
pub fn detect_payload() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the running executable")?;
    let dir = exe
        .parent()
        .context("executable has no parent directory")?
        .to_path_buf();
    validate_payload(&dir)
        .with_context(|| format!("{} is not a SpritzWine tree", dir.display()))?;
    Ok(dir)
}

/// Check that `dir` looks like a relocatable SpritzWine tree.
pub fn validate_payload(dir: &Path) -> Result<()> {
    for needed in ["bin/wine", "spritzwine"] {
        let p = dir.join(needed);
        if !p.exists() {
            bail!("missing {} (expected a relocatable SpritzWine tree)", p.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Recursive, progress-reporting copy
// ---------------------------------------------------------------------------------------

fn tree_size(src: &Path) -> io::Result<u64> {
    let md = fs::symlink_metadata(src)?;
    let ft = md.file_type();
    if ft.is_symlink() {
        Ok(0) // links are recreated, their target bytes are not copied
    } else if ft.is_dir() {
        let mut total = 0;
        for entry in fs::read_dir(src)? {
            total += tree_size(&entry?.path())?;
        }
        Ok(total)
    } else {
        Ok(md.len())
    }
}

/// Copy the tree at `src` to `dst`, recreating symlinks and preserving permissions.
/// `on_progress` is called after each file with cumulative/total bytes.
pub fn copy_tree(src: &Path, dst: &Path, on_progress: &mut dyn FnMut(Progress)) -> Result<()> {
    let total = tree_size(src).with_context(|| format!("sizing {}", src.display()))?;
    let mut done = 0u64;
    copy_into(src, dst, total, &mut done, on_progress)?;
    on_progress(Progress { done_bytes: done, total_bytes: total });
    Ok(())
}

fn copy_into(
    src: &Path,
    dst: &Path,
    total: u64,
    done: &mut u64,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    let md = fs::symlink_metadata(src)?;
    let ft = md.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(src)?;
        let _ = fs::remove_file(dst);
        symlink(&target, dst).with_context(|| format!("symlink {}", dst.display()))?;
    } else if ft.is_dir() {
        fs::create_dir_all(dst).with_context(|| format!("mkdir {}", dst.display()))?;
        for entry in fs::read_dir(src)? {
            let name = entry?.file_name();
            copy_into(&src.join(&name), &dst.join(&name), total, done, on_progress)?;
        }
        fs::set_permissions(dst, md.permissions()).ok();
    } else {
        fs::copy(src, dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        *done += md.len();
        on_progress(Progress { done_bytes: *done, total_bytes: total });
    }
    Ok(())
}

/// Refuse to clobber a directory that is neither empty nor a prior SpritzWine install;
/// otherwise clear it so the copy lands cleanly (a reinstall replaces an old tree).
fn prepare_dest(dst: &Path) -> Result<()> {
    if !dst.exists() {
        return Ok(());
    }
    let empty = fs::read_dir(dst)
        .map(|mut it| it.next().is_none())
        .unwrap_or(false);
    let ours = dst.join("bin/wine").exists() || dst.join("spritzwine").exists();
    if empty || ours {
        fs::remove_dir_all(dst).with_context(|| format!("clearing {}", dst.display()))?;
        Ok(())
    } else {
        bail!(
            "{} already exists and is not a SpritzWine install -- choose an empty directory",
            dst.display()
        );
    }
}

// ---------------------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------------------

/// Install the payload to an arbitrary directory `dest` (which becomes the tree root).
pub fn install_custom(
    payload: &Path,
    dest: &Path,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<PathBuf> {
    validate_payload(payload)?;
    prepare_dest(dest)?;
    copy_tree(payload, dest, on_progress)?;
    Ok(dest.to_path_buf())
}

/// Install the payload as a Lutris Wine runner: `<runners_wine_dir>/<name>/bin/wine`.
pub fn install_lutris(
    payload: &Path,
    runners_wine_dir: &Path,
    name: &str,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<PathBuf> {
    validate_payload(payload)?;
    fs::create_dir_all(runners_wine_dir)
        .with_context(|| format!("creating {}", runners_wine_dir.display()))?;
    let dest = runners_wine_dir.join(name);
    prepare_dest(&dest)?;
    copy_tree(payload, &dest, on_progress)?;
    Ok(dest)
}

/// Install the payload as a Steam Play compatibility tool: copy the tree into
/// `<compat_dir>/<name>/` and lay the Proton-style descriptors on top.
pub fn install_steam(
    payload: &Path,
    compat_dir: &Path,
    name: &str,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<PathBuf> {
    validate_payload(payload)?;
    fs::create_dir_all(compat_dir)
        .with_context(|| format!("creating {}", compat_dir.display()))?;
    let dest = compat_dir.join(name);
    prepare_dest(&dest)?;
    copy_tree(payload, &dest, on_progress)?;

    fs::write(dest.join("compatibilitytool.vdf"), steam::compatibilitytool_vdf(name))
        .context("writing compatibilitytool.vdf")?;
    fs::write(dest.join("toolmanifest.vdf"), steam::toolmanifest_vdf())
        .context("writing toolmanifest.vdf")?;
    let proton = dest.join(steam::ENTRY_POINT);
    fs::write(&proton, steam::PROTON_WRAPPER).context("writing the Steam entry point")?;
    fs::set_permissions(&proton, fs::Permissions::from_mode(0o755))
        .context("chmod the Steam entry point")?;
    Ok(dest)
}

/// Environment that makes the bundled, relocatable Wine usable from any path -- the same
/// variables the `spritzwine` launcher exports (see `packaging/spritzwine`).
fn wine_env(payload: &Path) -> Vec<(String, String)> {
    let join_ld = {
        let mut p = format!(
            "{}:{}",
            payload.join("lib/wine/x86_64-unix").display(),
            payload.join("lib").display()
        );
        if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
            if !existing.is_empty() {
                p.push(':');
                p.push_str(&existing.to_string_lossy());
            }
        }
        p
    };
    vec![
        ("WINELOADER".into(), payload.join("bin/wine").display().to_string()),
        ("WINESERVER".into(), payload.join("bin/wineserver").display().to_string()),
        ("LD_LIBRARY_PATH".into(), join_ld),
    ]
}

/// Run a bundled Wine tool, streaming combined stdout+stderr to `log`, and return its status.
fn run_wine_tool(
    payload: &Path,
    tool: &str,
    args: &[&str],
    extra_env: &[(&str, &str)],
    prefix: &Path,
    log: &mut dyn FnMut(&str),
) -> Result<std::process::ExitStatus> {
    let bin = payload.join("bin").join(tool);
    let mut cmd = Command::new(&bin);
    cmd.args(args)
        .envs(wine_env(payload))
        .env("WINEPREFIX", prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;

    // Drain stdout on a thread so neither pipe can fill and deadlock; stream stderr (where
    // wine logs) to the caller's log.
    let stdout = child.stdout.take();
    let pump = std::thread::spawn(move || {
        if let Some(out) = stdout {
            for _ in io::BufReader::new(out).lines().map_while(Result::ok) {}
        }
    });
    if let Some(err) = child.stderr.take() {
        for line in io::BufReader::new(err).lines().map_while(Result::ok) {
            log(&line);
        }
    }
    let status = child.wait().context("waiting for wine tool")?;
    let _ = pump.join();
    Ok(status)
}

/// Initialise (or update) a `WINEPREFIX` with this build, then wait for the wineserver to
/// flush so the prefix is complete on disk when this returns. Output is streamed to `log`.
///
/// `mscoree,mshtml=d` suppresses the Mono/Gecko install prompt for the initial boot only
/// (it is not persisted to the prefix); later runs install them on demand as usual.
pub fn init_prefix(payload: &Path, prefix: &Path, log: &mut dyn FnMut(&str)) -> Result<()> {
    validate_payload(payload)?;
    fs::create_dir_all(prefix).with_context(|| format!("creating {}", prefix.display()))?;

    log(&format!("Initialising WINEPREFIX at {} ...", prefix.display()));
    let status = run_wine_tool(
        payload,
        "wine",
        &["wineboot", "-i"],
        &[("WINEDLLOVERRIDES", "mscoree,mshtml=d")],
        prefix,
        log,
    )?;
    if !status.success() {
        bail!("wineboot exited with {status}");
    }

    // The registry (system.reg/user.reg) is held in the wineserver and only flushed to disk
    // on its shutdown, so wait for it -- otherwise we'd return a half-written prefix.
    log("Finalising (waiting for wineserver to flush the registry) ...");
    let _ = run_wine_tool(payload, "wineserver", &["-w"], &[], prefix, log);

    log("WINEPREFIX ready.");
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Beisl (bundled perf tracer)
// ---------------------------------------------------------------------------------------

/// Whether this payload bundles Beisl, so the front-ends can offer to install it.
pub fn has_beisl(payload: &Path) -> bool {
    payload.join("beisl").is_file()
}

/// Install the bundled Beisl (the perf tracer) onto the user's PATH by running its own
/// `install` subcommand (`<payload>/beisl install`): that copies `beisl` (+ the bundled
/// `beisl-setup` GUI) into `~/.local/bin` and registers a desktop entry, so `beisl %command%`
/// works in launch options straight away. Output is streamed to `log`.
pub fn install_beisl(payload: &Path, log: &mut dyn FnMut(&str)) -> Result<()> {
    let bin = payload.join("beisl");
    if !bin.is_file() {
        bail!("this build does not bundle Beisl ({} missing)", bin.display());
    }
    let out = Command::new(&bin)
        .arg("install")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} install", bin.display()))?;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        log(line);
    }
    if !out.status.success() {
        for line in String::from_utf8_lossy(&out.stderr).lines() {
            log(line);
        }
        bail!("beisl install exited with {}", out.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal fake payload tree (no real binaries) to exercise copy/install logic.
    fn fake_payload(root: &Path) {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("lib/wine/x86_64-unix")).unwrap();
        fs::write(root.join("bin/wine"), b"#!/bin/sh\necho fake-wine\n").unwrap();
        fs::set_permissions(root.join("bin/wine"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("spritzwine"), b"#!/bin/sh\nexec \"$(dirname \"$0\")/bin/wine\" \"$@\"\n").unwrap();
        fs::set_permissions(root.join("spritzwine"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("lib/wine/x86_64-unix/libwine_ntdll.so"), b"x").unwrap();
        // a symlink to prove links are recreated, not dereferenced
        let _ = symlink("libwine_ntdll.so", root.join("lib/wine/x86_64-unix/libwine_ntdll.so.1"));
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sw-it-{tag}-{}", std::process::id()))
    }

    #[test]
    fn custom_install_copies_tree_and_preserves_symlink_and_mode() {
        let base = tmp("custom");
        let _ = fs::remove_dir_all(&base);
        let payload = base.join("payload");
        let dest = base.join("dest");
        fake_payload(&payload);

        let mut last = Progress { done_bytes: 0, total_bytes: 0 };
        install_custom(&payload, &dest, &mut |p| last = p).unwrap();

        assert!(dest.join("bin/wine").exists());
        let link = dest.join("lib/wine/x86_64-unix/libwine_ntdll.so.1");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        let mode = fs::metadata(dest.join("bin/wine")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "exec bits preserved");
        assert_eq!(last.done_bytes, last.total_bytes);
        assert!(last.total_bytes > 0);

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn steam_install_writes_descriptors_and_executable_proton() {
        let base = tmp("steam");
        let _ = fs::remove_dir_all(&base);
        let payload = base.join("payload");
        let compat = base.join("compatibilitytools.d");
        fake_payload(&payload);

        let dest = install_steam(&payload, &compat, "SpritzWine", &mut |_| {}).unwrap();
        assert_eq!(dest, compat.join("SpritzWine"));
        assert!(dest.join("compatibilitytool.vdf").exists());
        assert!(dest.join("toolmanifest.vdf").exists());
        let entry = dest.join("spritzwine-proton");
        assert!(entry.exists());
        assert_eq!(fs::metadata(&entry).unwrap().permissions().mode() & 0o111, 0o111);
        // Must NOT be named `proton`, or Lutris misclassifies it as a Proton build and
        // crashes looking for files/bin/wine.
        assert!(!dest.join("proton").exists(), "entry point must not be named 'proton'");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn lutris_install_lands_under_runner_name() {
        let base = tmp("lutris");
        let _ = fs::remove_dir_all(&base);
        let payload = base.join("payload");
        let runners = base.join("runners/wine");
        fake_payload(&payload);

        let dest = install_lutris(&payload, &runners, "SpritzWine-v0.1.0", &mut |_| {}).unwrap();
        assert_eq!(dest, runners.join("SpritzWine-v0.1.0"));
        assert!(dest.join("bin/wine").exists());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn prepare_dest_refuses_foreign_nonempty_dir() {
        let base = tmp("foreign");
        let _ = fs::remove_dir_all(&base);
        let payload = base.join("payload");
        let dest = base.join("important");
        fake_payload(&payload);
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("my-thesis.txt"), b"do not delete").unwrap();

        let err = install_custom(&payload, &dest, &mut |_| {}).unwrap_err();
        assert!(err.to_string().contains("not a SpritzWine install"));
        assert!(dest.join("my-thesis.txt").exists(), "foreign data left intact");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn reinstall_over_prior_install_succeeds() {
        let base = tmp("reinstall");
        let _ = fs::remove_dir_all(&base);
        let payload = base.join("payload");
        let dest = base.join("dest");
        fake_payload(&payload);
        install_custom(&payload, &dest, &mut |_| {}).unwrap();
        // second time: dest now looks like a SpritzWine install -> allowed to overwrite
        install_custom(&payload, &dest, &mut |_| {}).unwrap();
        assert!(dest.join("bin/wine").exists());
        fs::remove_dir_all(&base).ok();
    }
}
