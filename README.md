# SpritzWine — landing page

The public landing page for [SpritzWine](https://codeberg.org/SpritzWine/SpritzWine).

Plain static files, no build step, no external requests — it works offline from a
`file://` open and is ready for Codeberg Pages as-is.

```
index.html            the page
assets/style.css      styles (light + dark via prefers-color-scheme, with a manual toggle)
assets/site.js        theme toggle, the sparkling hero, the release-notes renderer
assets/spritzwine.png the mark
releasenotes.md       verbatim copy of the repo's RELEASENOTES.md (rust branch),
                      rendered into the "Release notes" section by site.js
                      (same-origin fetch; under file:// the section falls back to
                      a link to the Codeberg releases page)
.domains              custom-domain config for Codeberg Pages (commented example)
```

It is a sibling of the [beisl page](https://spritzwine.codeberg.page/beisl/page/) and shares
its design system — the tokens, the type scale, the section rhythm, the theme toggle. The
palette is its own: a G'spritzter is white wine cut with soda, so this one is lighter and
more effervescent than the pub next door.

## Deploying to Codeberg Pages

This is the **`pages` branch** of
[`SpritzWine/SpritzWine`](https://codeberg.org/SpritzWine/SpritzWine). Codeberg serves a
repo's `pages` branch directly — no build step, no webhook. Pushing the branch republishes
it:

```sh
# from a checkout of the SpritzWine repo, on the pages branch
git show rust:RELEASENOTES.md > releasenotes.md   # refresh at each release
git add -A
git commit -m "page: ..."
git push codeberg pages
```

Published at:

    https://spritzwine.codeberg.page/SpritzWine/

Everything uses relative paths (`assets/…`), so it works unchanged under that sub-path — no
base-href to adjust. Updates go live on push (allow a few minutes for the Pages cache).

The layout differs from beisl's on purpose: beisl keeps its site under `page/` because that
branch also carries a brand-reference board. This page sits at the branch root, which keeps
the URL short. If a reference board ever lands here too, move the site into `page/` then —
and expect the URL to gain that segment.

## Keeping the release notes current

`releasenotes.md` is a **copy**, not a symlink — the `pages` branch cannot reach the `rust`
branch's files when Codeberg serves it. Refresh it whenever a release lands (the one-liner
above). The page renders the **two most recent versions** and scrolls the rest, so the file
can grow without the section growing with it.
