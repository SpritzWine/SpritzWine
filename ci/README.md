# CI for SpritzWine

Three lanes, one rule: **the same scripts a human runs are the ones CI runs.**

| workflow | trigger | runner | does |
|---|---|---|---|
| `lint.yml` | push `rust` / PR | `spritzwine-local` | shellcheck, submodule-URL sanity, **wine pin sanity**, build-image stamp. Seconds. |
| `build.yml` | push `rust` / PR | `spritzwine-local` (self-hosted) | `build.sh` → `packaging/package.sh` → `ci/smoke.sh` → **ask Prater to integrate** |
| `release.yml` | tag `v*` | `spritzwine-local` | build, package, smoke, Codeberg release |

## What this replaced, and why

`.woodpecker.yml` **could never have gone green.** It built Wine for both PE arches on a
Codeberg *hosted* runner, where a job is capped at 10 minutes — the `-lazy` variants only
delay when a job *starts*, they do not extend its runtime. Its own comment admitted "~30 min".
Caching cannot rescue that: the cold build has to fit inside the cap at least once, and it
never can.

It was also on a different CI system from the rest of the org (Codeberg runs *both* Woodpecker
and Forgejo Actions). beisl and Prater are on Forgejo Actions, so SpritzWine now is too: one
system, **one org-scoped runner** (`beisl/ci/runner/`), three labels.

## The build image *is* the job container

`ci/build-env.Dockerfile` → `spritzwine-build:fedora44`, and `build.yml` runs **inside** it.
No `dnf install` at the top of every job, no docker-in-docker, no docker socket.

That is not just about speed. **Wine's `configure` silently disables any backend whose dev
headers are missing** — it does not warn and it does not fail; it just builds a Wine without
that driver. The old package list contained no `libX11-devel` at all, so the tarball that lane
would have published had **no X11 driver**, and nothing anywhere would have said so. Pinning
the dependency set in an image, and asserting the *result* in `smoke.sh`, is what turns that
from invisible into loud.

(A caution from this gate's own history: its first version grepped `config.h` for
`SONAME_LIBASOUND`/`SONAME_LIBWAYLAND_CLIENT` and "proved" a perfectly sound-capable Wine had
no audio. Wine defines `SONAME_*` **only for libraries it dlopens**; directly-linked backends
— wayland, alsa, pulse — never get one, in any build. Those are judged by the only truthful
artifact instead: the driver `.so` that wine builds exactly when configure found the backend.)

Build it on the runner host, and re-stamp whenever the Dockerfile changes:

```sh
./ci/rebuild.sh   # re-stamps FIRST, builds, verifies the baked stamp, prunes dangling layers
```

(The order matters — the stamp is baked into the image as its last layer — which is exactly
why it is a script and not two commands to remember.)

`force_pull: false` on the runner means the **locally built** tag is used as-is, never pulled.
Staleness is caught twice, at two different layers:

- **`lint.yml`** checks that `ci/build-env.sha256` matches the Dockerfile *in the same commit*
  (i.e. you edited one and forgot the other).
- **The build jobs assert the image itself.** The stamp is baked into the image at
  `/etc/spritzwine/build-env.sha256` (last layer of the Dockerfile); a job that wakes up inside
  an image whose stamp differs from the commit's fails in seconds with rebuild instructions,
  instead of hours later in whatever way the missing package dictates.

One consequence to know: **one local tag serves every ref**, so a PR that changes the image
inputs cannot be green in `build.yml` at the same time as the base branch. The operator flow
for a Dockerfile PR: fetch the PR branch on the runner host, run `./ci/rebuild.sh` from that
checkout, re-run the PR's build job, and merge promptly — pushes to `rust` assert red against
the new image until the merge lands the new stamp. A red "Assert the build image" step on a
Dockerfile PR is expected; only the runner-host operator can clear it.

And one layer deeper: **a changed image invalidates the warm tree's configure cache.**
Wine's `configure` gating is tree-internal — it cannot see that the *environment* changed, so
after an image change the warm tree keeps a `config.h` describing the old image (that is how
the winebus/SDL2 link error survived three runs). `build.sh` therefore records the env stamp
in the tree (`wine/.build-env-stamp`) and forces a reconfigure when it changes. A forced
configure is cheap when nothing effective changed: `config.status` leaves an identical
`config.h` untouched, so no rebuild cascades.

## The smoke gate (`ci/smoke.sh`)

A green Wine build proves almost nothing, so CI gates on behaviour:

1. **backends** — what `configure` *decided*, read out of `wine/include/config.h`. X11,
   freetype, fontconfig and vulkan are **required** (a missing one fails the build); audio and
   Wayland are reported, so a downgrade is at least visible.
2. **shape** — new-WoW64 invariants in the packaged tree: a 64-bit unix side, **no** native
   32-bit unix `.so`, both PE arches, and the Rust unix libs actually bundled (the project's
   whole thesis is Rust inside Wine — if the crates stopped being linked, everything else
   would still pass).
3. **boot** — `wineboot -u` in a fresh prefix, under a timeout.
4. **bitness** — a real PE in *both* bitnesses (`system32` and `syswow64` `cmd.exe`), checked
   by **exit code**: with std handles on a regular file this Wine drops a PE process's stdout
   *and* stderr entirely, and only the exit status survives.

Run it yourself after a local build:

```sh
./build.sh && ./packaging/package.sh && ./ci/smoke.sh
```

Knobs: `SMOKE_TIMEOUT=300`, `KEEP=1` (keep the prefix and the extracted tree).

## Closing the loop with Prater

SpritzWine is upstream: [SpritzWine-Prater](https://codeberg.org/SpritzWine/SpritzWine-Prater)
consumes it as a submodule and builds its `wine/`. Without a link between them, a wine change
lands here and nobody learns it broke Proton until someone bumps the pin by hand, weeks later.

So on a green build of `rust`, `build.yml` dispatches Prater's `integrate.yml`, which builds
and smokes *that exact commit* as Proton and — on green — opens the `spritzwine: bump to <sha>`
PR itself. It is fire-and-forget: a red Prater does not turn a green SpritzWine build red (the
failure belongs on Prater's run, where the log is), and Prater's nightly `integrate` run covers
the case where no dispatch fired.

**TODO(user):** `secrets.PRATER_DISPATCH_TOKEN` — an access token with **Actions write** on
SpritzWine-Prater. Without it the step just says so and moves on.

## Setup checklist

| what | where | notes |
|---|---|---|
| Enable Actions | Settings → Units | per repo |
| `RELEASE_TOKEN` | Settings → Actions → Secrets | repository write. The automatic Actions token **cannot** create releases. |
| `PRATER_DISPATCH_TOKEN` | Settings → Actions → Secrets | Actions write **on SpritzWine-Prater** |
| the runner | `beisl/ci/runner/` | **org-scoped**, label `spritzwine-local`. A repo-scoped runner silently ignores other repos' jobs. |
| the build image | on the runner host | see above |
| cache | `/cache/spritzwine/{src,ccache}`, `/cache/cargo` | the warm work tree; a cold Wine build is ~30 min. Budget ~30 GB. |

## A note on the submodule URLs

`.gitmodules` uses **https**, deliberately: CI has no SSH key, and neither does a drive-by
contributor running `git clone --recursive`. It used to say `ssh://git@codeberg.org/...`, which
only ever worked for the author.

Pushing still goes over SSH — that is a *local* setting, not a committed one:

```sh
git config url."ssh://git@codeberg.org/".pushInsteadOf "https://codeberg.org/"
```

(Already configured in this checkout, and in `wine/` and `beisl/`.) `lint.yml` fails if an
`ssh://` URL ever comes back into `.gitmodules`.
