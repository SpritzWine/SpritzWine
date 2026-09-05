# SpritzWine

Wine fork migrating subsystems to Rust, plus the SpritzWine-Prater Proton fork. The branch model (authored `soda-*` tiers, generated `spritzwine`/`spritzvalve` consumers, never hand-edit a consumer) is in `docs/BRANCHING.md`; `tools/sodactl` drives it. The `wine` submodule has no manual of its own: this file governs work in it. `AGENTS.md` is the OpenCode counterpart and carries the same rules.

## Mandatory rules (all projects)

Identical in every SpritzWine, Prater, beisl and gamebus repo and vault. Change
them everywhere or nowhere. They apply to everything an agent writes: code,
comments, commit messages, docs, notes.

1. Attribution: the only AI trailer is
   `Assisted-By: Claude <model> <noreply@anthropic.com>`, one line, last.
   Never `Co-Authored-By`. Never a `Claude-Session:` line or any session or
   tracking URL. Commits that only package the maintainer's own edits carry no
   AI trailer at all.
2. Plain ASCII punctuation: no em, en or figure dashes, no Unicode minus or
   non-breaking hyphen, no non-breaking spaces, no zero-width or bidi
   characters. Write `-` or `--`, or restructure the sentence. Before
   committing, run the unicode-hygiene skill
   (`python3 ~/.claude/skills/unicode-hygiene/scripts/scan_unicode.py`) on
   what you touched: hidden characters fail the run, dashes are reported.
   Vendored third-party files (SDK headers, LICENSE texts) are left as they are.
3. Comments: concise and load-bearing. Say why, not what. No narration of
   obvious code, no essays, no restating the commit message. Match the
   surrounding density; when in doubt, fewer.
