---
session: "v3-scope-2026-05-12"
timestamp: "2026-05-12T21:46:16Z"
model: claude-opus-4-7
sequence: 3
---

## Human

How can we improve this before moving forward? What validation
or feedback do we lack?

[Assistant identified gaps; user said:]

do that. And thoughtfully consider where docker helps us in our
test harness. Let's proceed through all this work.

## Assistant

### What this commit covers

The previous PR (A1) shipped the round-trip harness without any
real-tool round-trip having executed — every round-trip test
skipped locally because sfdisk/sgdisk weren't installed. This
commit closes the highest-confidence gap and broadens CI
coverage:

1. **End-to-end validation against real `sgdisk` 1.0.10.** Installed
   `gptfdisk` via Homebrew, ran `ISOMAGE_REQUIRE_TOOLS=1 cargo
   test --test gpt_round_trip` — both round-trip tests genuinely
   executed (no skip) and passed. First-time evidence the harness
   does what its docs claim.

2. **Committed snapshot golden.** `tests/snapshots/gpt-single-named.snap`
   captures the parsed tree output for the GPT single-Linux
   scenario. Header records reference-tool version
   (`GPT fdisk (sgdisk) version 1.0.10`) for provenance. Verified
   that subsequent runs compare against the committed golden,
   that intentional corruption triggers a snapshot diff, and that
   `ISOMAGE_UPDATE_SNAPSHOTS=1` re-pins cleanly.

3. **CI breadth.** The main `test` job now also runs:
   - `cargo build --no-default-features` (zero-formats — catches
     accidental hard deps on iso/udf in future features).
   - Per-feature isolation builds (`iso`, `udf`, `mmap`, `simd`,
     `mbr,gpt,raw`) so a new format compiles on its own.
   - `cargo test --features mbr,gpt,raw,mmap,simd` (all-features
     coverage on every runner).
   - `cargo bench --no-run` so a regression in the bench harness
     itself surfaces at build time (we don't run the benches
     because criterion measurements are noisy on shared CI).

4. **`.gitignore` expansion + stray artifact cleanup.** Removed
   `boot/grub.cfg` (stray from a misdirected `make test-data`
   run). Added gitignore rules for `boot/`, `$IMAGE`, `$SRC_DIR`,
   `$OUT_DIR`, and `isomage-rt-*` tempdir leakage so similar
   accidents don't recur.

5. **`HANDOFF.md` refresh.** Removed stale "test infra is a gap"
   bullets; added a "Test infrastructure available to follow-on
   agents" section pointing at `tests/common/`. Repositioned the
   parser-entry generalization from "blocked" to "next PR (A2)."

### Things still NOT validated, by design

- **Round-trip job in CI has not run.** Pushing the branch is the
  next task (#19) and is intentionally separated from this commit
  so failures cite a clear cause.
- **MBR round-trip didn't run locally** because `sfdisk` doesn't
  ship on macOS (or via Homebrew without `linuxbrew`). It's the
  Ubuntu CI runner's job to exercise it.
- **No MBR snapshot committed.** Same reason — without sfdisk
  output we'd be inventing a golden, which defeats the purpose.

### Why I didn't bundle fuzz target stubs into this commit

The fuzz harness needs cargo-fuzz, nightly toolchain machinery,
and a `fuzz/Cargo.toml` workspace. It deserves its own commit
with its own prompt log entry. Same for the Docker tool-venue
abstraction — both are independent additions, not validation
improvements to PR A1.
