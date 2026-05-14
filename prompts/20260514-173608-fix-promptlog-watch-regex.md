---
session: "fix-promptlog"
timestamp: "2026-05-14T17:36:08Z"
model: claude-sonnet-4-6
---

## Human

Fix the promptlog (which is super weirdly implemented) to match
github.com/robotocore/robotocore's. Rewrite the whole dir as necessary.

## Assistant

### Key decisions

**Root cause of the weirdness**: The `WATCH_REGEX` in `scripts/check_promptlog.sh`
only watched `src/` and `Cargo.toml`, while the robotocore reference implementation
watches `src/`, `tests/`, and `scripts/`. This meant PRs that only touched the
`tests/` directory (integration tests, round-trip tests) or `scripts/` bypassed the
prompt-log gate silently — exactly the changes most likely to need provenance
documentation.

**Fix**: Updated `WATCH_REGEX` to `'^(src/|tests/|scripts/)'` to match robotocore.
Dropped `Cargo.toml` from the pattern; robotocore doesn't watch it, and any meaningful
`Cargo.toml` change (new feature flag, new dep) almost always accompanies a `src/`
change anyway.

**Docs kept in sync**: Updated `CLAUDE.md` (the hard-rules section), `prompts/PROMPTLOG.md`
(the CI gate section), and `.claude/skills/promptlog.md` (the description, "when to use",
and step-1 check) to all describe `src/`, `tests/`, `scripts/` consistently. Previously
each file described a slightly different set of watched paths, which was confusing.

**What I skipped**: Did not change the file format, frontmatter fields, or the
assistant-entry guidance — those are already aligned with robotocore's skill file.
