---
session: "v1.1.0-spruce-up"
timestamp: "2026-05-12T06:05:53Z"
model: claude-opus-4-7
sequence: 3
---

## Human

(Continuation of v1.1.0-spruce-up; see sequence 1 for the original
chain.) Last chunk of the spruce-up PR: project paperwork that a
proper 1.x release deserves.

## Assistant

### Key decisions

**CHANGELOG.md follows Keep-a-Changelog plus reverse-chronological
sections.** Three releases recorded: 1.1.0 (this PR), 1.0.0
(yesterday — promptlog adoption + agent-first README), and a
one-line acknowledgement of 0.4.0 with a pointer to git log. The
1.1.0 section is broken into Security / Fixed / Added — CLI / Added
— Library / Added — Tooling / Changed — possibly breaking, in the
order a user scanning for whether to upgrade should encounter them.
The Send+Sync narrowing is called out under "Changed — possibly
breaking" so any consumer who pinned to 1.0.x sees it.

The auto-generated release notes from `softprops/action-gh-release`
keep working alongside this file; the curated changelog is for humans
who want a coherent narrative.

**SECURITY.md scopes what counts.** Lists path-traversal, parser
DoS / panic, information disclosure, and memory safety as in-scope;
explicitly de-scopes issues that require write access to the input
ISO and dep advisories already tracked by RustSec (since `cargo
audit` runs in CI). Two reporting paths: GitHub private advisories
(preferred) and the maintainer's email. Acknowledgement-within-7-days
commitment is realistic for a hobby-scale project; a longer commitment
would be performative.

The file also summarizes the actual hardening surface in 1.1.0 — the
path-traversal guard, the BrokenPipe path, the u64 clamp — so a
researcher reading SECURITY.md knows what's already been touched
before they file.

**Issue templates: bug, feature, no blank.** YAML form-style
templates, not Markdown. The bug template asks for version, platform,
exact command, expected, actual, and an ISO (with a `dd | gzip` head
sample suggestion for private images). The feature template leads
with "what problem are you trying to solve" rather than "describe the
feature" — better requests in my experience.

`config.yml` disables blank issues and pins two contact links:
private security advisory (so security reports don't end up as
public issues) and GitHub Discussions (for "how do I…" questions).

**PR template embeds the promptlog checkbox.** Six checkboxes:
build/test, fmt, clippy, promptlog file added, README/CHANGELOG
updated if user-visible, security-fix coordination. The promptlog
item now appears for the sixth time in this repo: CLAUDE.md, README
invariant 7, CONTRIBUTING, PROMPTLOG.md, the skill, and now the PR
template. The CI gate is the seventh and authoritative one.

**README badges.** Five chosen from the standard pile: crates.io
version, docs.rs, CI status, license, MSRV. Did not add:
"maintained" / "downloads/month" / "stars" — they age poorly or are
vanity. Kept the badges to one line at the very top so they don't
push the demo transcripts below the fold on mobile.

**README "Use as a library" section.** New section between
"Supported formats" and "Architecture" — the place a reader who's
just read "yes, ISO 9660 and UDF" would naturally go next. Lists the
six public items in a table (function/struct/method, one-line
description, link to docs.rs). Calls out that it's the same crate
that ships the CLI — so a reader who already installed via
`cargo install isomage` doesn't pull a duplicate dependency.

**README "Security" section.** A short stub that points at
SECURITY.md and summarizes the three hardening surfaces. The full
policy lives in the dedicated file; the README just makes sure
researchers can find it.

### What I skipped

- An ASCII demo SVG/GIF. The `doc/demo.svg` exists but isn't
  referenced from README. Adding it is one line but I haven't
  verified the SVG content matches current behaviour; defer.
- A GitHub repo topics/social-preview audit. Belongs to the repo
  settings page, not a PR.
- Funding / FUNDING.yml. Not requested and the project doesn't
  accept funding.
- A code-of-conduct file. For a small project with one maintainer,
  GitHub's site-wide CoC suffices; adding a project-level CoC
  without an enforcement plan is theatre.
- Backporting the promptlog checkbox into `CLAUDE.md` again. The
  rule is already there in stronger terms; the PR template is
  reinforcement for humans who skip CLAUDE.md.

### Verified locally

- `cargo build`, `cargo test`, `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings` — all pass.
- All five doc-tests still compile and run.
- Issue template YAML matches the GitHub schema (only-required-when-required,
  no `additionalProperties` accidentally introduced).
- README's docs.rs links use the `latest` channel so they keep
  working past 1.1.0.
