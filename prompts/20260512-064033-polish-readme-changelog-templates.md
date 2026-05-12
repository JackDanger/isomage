---
session: "v2.0.0-library-only"
timestamp: "2026-05-12T06:40:33Z"
model: claude-opus-4-7
sequence: 3
---

## Human

(Continuation of v2.0.0-library-only; see sequences 1 and 2.) Final
chunk of the v2.0.0 PR: rewrite the README around the library
identity and write the project-paperwork files (CHANGELOG, SECURITY,
issue/PR templates, refreshed CLAUDE.md / CONTRIBUTING.md).

## Assistant

### Key decisions

**README rewritten top to bottom.** The previous README was an
"agent-first CLI tool" pitch with a library section bolted on; the
new one is the inverse. Structure:

1. **One-line value prop** + a single quoted blurb: "Pure-Rust
   reader for ISO 9660 and UDF. Zero deps. Read-only. No mount,
   no FUSE, no `unsafe`."
2. **Immediate code sample** — `Cargo.toml` + 20 lines of Rust
   showing list, cat, and extract. A reader knows in 10 seconds
   whether this crate solves their problem.
3. **What it parses** — the formats and their extensions.
4. **Why a new crate** — honest about 7z. Argues for the specific
   niche (Rust embed, parser diagnostics, auditable source). Tells
   anyone "I just want to extract a disc on the CLI" to use 7z.
5. **Public API table** — every `pub` item linked to docs.rs.
6. **Safety guarantees** — the two hardened surfaces called out
   explicitly so reviewers can find what changed.
7. **Examples** — four runnable doc-tests covering list / cat /
   extract / verbose-investigate.
8. **Architecture** — module map and `TreeNode` shape.
9. **Invariants and extension points** — eight invariants
   (added "zero runtime dependencies" as invariant 7, kept the
   promptlog gate as invariant 8). Extension points table.
10. **If you want a CLI** — a real, working ~50-line `main.rs`
    that reproduces v1 behaviour. So users who came for the CLI
    aren't left empty-handed, but the maintenance burden lives on
    their fork, not here.
11. **Build / test / release flow.**
12. **Security**, **Contributing**, **Changelog**, **License**.

**README badges** chosen for library context: crates.io version,
docs.rs, CI, license, MSRV, and a custom "Zero deps" badge. Dropped
"downloads/month" — vanity for a brand-new library. Kept the badges
on a single line at the top so the demo code is still above the
fold on mobile.

**Honest 7z disclosure in the README.** The "Why a new crate"
section names `7z` and concedes that for general CLI use, `7z` is
faster to reach for than Rust. This is unusual for a library README
and intentional — readers who land here from a search for "ISO
parsing" deserve to know whether the library is for them.

**CHANGELOG.md** records v2.0.0, v1.0.0, and a one-line v0.4.0
acknowledgment. The v2.0.0 section leads with "Repositioning" (why)
then "Removed — BREAKING" (what), then "Security / Fixed / Added /
Changed". Reviewers deciding whether to migrate from 1.x see the
breaking-change section second, before any of the additive stuff.

**SECURITY.md** is library-shaped: scope is `extract_node` path
traversal, parser DoS/panic, info disclosure, memory safety. Notes
that the crate has zero `unsafe` blocks — useful for security
researchers deciding where to look first.

**Issue templates** rewritten for library context:
- `bug.yml`: asks for `isomage` version (from Cargo.lock), rustc
  version, platform, minimal Rust reproduction (rendered as `rust`
  not `shell`), expected vs. actual, ISO sample link/description.
- `feature.yml`: leads with "what problem are you trying to solve"
  and refers to CONTRIBUTING.md's scope section.
- `config.yml`: pins private-advisory and Discussions links;
  disables blank issues.

**`pull_request_template.md`** adds two new checkboxes vs. the v1.1
draft: "rustdoc on any `pub` API change" and "explain any new
dependency in the description." Both encode invariants from
README.md.

**CLAUDE.md rewritten** with three new hard rules at the top: no
new dependencies without justification, no CLI / no binary
reintroduction, no `unsafe`. The "project facts" section is now
library-shaped (no `[[bin]]`, public API named explicitly, doc-tests
mentioned). Removed every mention of `isomage -c` / stdout/stderr
contract — that was a CLI invariant, not a library one.

**CONTRIBUTING.md rewritten** similarly: explicit "out of scope"
list now includes "distributing a CLI executable" with a pointer to
the README's example. Suggests forking under a different name for
anyone who wants to maintain a CLI.

### What I skipped

- Re-evaluating the promptlog skill file (`.claude/skills/promptlog.md`).
  It's still accurate; the trigger is still `src/` or `Cargo.toml`
  changes, which is unchanged. No edit needed.
- Auditing the README of the homebrew-tap repo. External repo,
  out of this PR's reach.
- A migration script for v1 → v2 users (there isn't anything to
  migrate; either the lib API works for them or they need their own
  CLI). The README's "If you want a CLI" section is the migration
  guide.

### Verified

- README's doc-test code paths all use the actual public API names
  and signatures from the live `src/lib.rs`. No drift.
- All five CHANGELOG / SECURITY links resolve.
- `cargo build`, `test`, `clippy`, `fmt`, `doc` all clean (commits
  1+2 already established this; commit 3 is doc-only).
