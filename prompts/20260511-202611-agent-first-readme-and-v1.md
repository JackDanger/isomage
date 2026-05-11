---
session: "introduce-promptlog"
timestamp: "2026-05-11T20:26:11Z"
model: claude-opus-4-7
sequence: 2
---

## Human

(Continuation of session — see sequence 1 at
`prompts/20260511-201219-introduce-promptlog-pattern.md` for the original
prompt chain. After the promptlog PR went green on CI, the user wanted
the README rewritten "agent-first friendly (full understanding of the
whole app in the README)" and the version bumped to 1.0.0.)

## Assistant

### Key decisions

**Agent-first means lossless, not chatty**. The rewritten README adds an
Architecture section, an Output Contract table, and an Invariants and
Extension Points section — three things an agent needs to be useful
without reading every `.rs` file. I kept the marketing-shaped front
matter (one-line description, install, the three example transcripts)
because humans land on the README first too, and burying the demo under
prose would make the file worse for them. The agent-readable structure
goes _after_ the human-readable hook, not before.

**Output contract as a table** instead of prose. Three modes × two
streams (stdout, stderr) is exactly what a table is good at. An agent
adding a new mode can read one row and know what it owes.

**Invariants list, numbered**. Seven invariants the codebase relies on,
each stated as a rule rather than a description. "stdout is sacred in
`-c` and `-x`" is more useful to an agent than "we try to keep stdout
clean." Numbering invites referencing — future PR reviews can say "this
violates invariant 3" and the reader knows where to look.

**Extension points table**. Maps intent ("I want to add a new
filesystem") to the file(s) to touch. This is the section an agent
reads when assigned an open issue. Keeping it concrete (column 1 is the
goal, column 2 is the files) avoids the "where do I even start" cold
start.

**Decided NOT to**:
- Inline the full clap definition or the public function signatures'
  doc comments. Those live in the source; the README points at the
  modules and lets the agent grep. Duplicating them invites drift.
- Add an ASCII directory tree of the whole repo. The tree of `src/`
  alone is what matters; `target/`, `releases/`, `test_data/` are
  obvious from name.
- Document the binary file formats themselves. There's a difference
  between "an agent that can work in this repo" and "an agent that
  can implement ISO 9660 from scratch." The README is for the former.

**Version bump to 1.0.0**: this PR makes the project's contract
explicit (output contract, invariants, promptlog gate). Calling that
1.0.0 is honest — there is now something to stay backward-compatible
with. The release workflow triggers on `v*` tags and publishes to
GitHub Releases, crates.io, and the Homebrew tap — so the tag itself
is intentionally _not_ pushed from this PR. The user is asked to push
the tag after merge.

**Cargo.lock regenerated** by `cargo build` rather than hand-edited.
The version number is duplicated into `Cargo.lock` and the lockfile
must stay in sync.

**Trigger demonstration**: this commit changes `Cargo.toml` (and
`Cargo.lock`), which is one of the gate-watched paths. It also adds a
new `prompts/` file. So this commit is the first one in the repo to
exercise the **positive** path of the prompt-log gate. The previous
commit's PR was docs-only and skipped the gate; this commit will pass
the gate by satisfying it.

### What I skipped

- A CHANGELOG.md. The release-notes step on `softprops/action-gh-release`
  generates notes from commits — a separate CHANGELOG would double-bookkeep
  and drift.
- Pinning the rust-toolchain version. The CI uses `dtolnay/rust-toolchain@stable`
  and the project compiles on whatever's current. Pinning is a future
  decision, not a 1.0 prerequisite.
- Updating the install.sh installer's pinned-version logic. It pulls from
  `releases/latest`, so the tag push handles it.

### Why this fits the "near-guarantee" goal

A first-time contributor agent now reads:

1. `CLAUDE.md` — "prompt log is a hard rule"
2. `README.md` — Invariant 7 says the same thing
3. `CONTRIBUTING.md` — same thing, human framing
4. `.claude/skills/promptlog.md` — step-by-step procedure
5. `prompts/PROMPTLOG.md` — full spec

…before the agent reaches the failing CI check. Five doors lead to the
same room. That is the design goal: a sufficiently lazy or distracted
agent still ends up doing the right thing because every entry point in
the repo points at the same rule.
