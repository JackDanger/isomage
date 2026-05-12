---
session: "v3-scope-2026-05-12"
timestamp: "2026-05-12T21:02:23Z"
model: claude-opus-4-7
sequence: 2
---

## Human

Commit it on a branch and then make a plan to iterate through the
rest of the implementation one PR at a time. Start with the work
that makes the rest of the work easy. Make big up-front investments
and then use them.

Do it, and do it thoroughly. Imagine what a version of this harness
after 2 years of OSS development might look like and build it right
from the start.

## Assistant

### Goal

The next ~30 weeks of v3.0 work is 13+ format readers and 9+
writers. Every one of them needs: an external-tool round-trip
test, deterministic tree assertions, golden-file snapshots, a
fuzz target, and skip-or-fail policy that's strict in CI but
quiet for local dev. Building each PR's harness from scratch
would be wildly redundant; this PR lands the harness once.

### Design choices that wouldn't be obvious in six months

**Alias chains** (`tools::MKISOFS = ["mkisofs", "xorriso",
"genisoimage"]`). The same tool ships under three different
binary names depending on distro and era. Hard-coding one fails
on a third of runners; testing in CI for each variant is hours
of YAML. The `Tool::with_aliases` constructor + a single
resolved-name return value handles all of it. `Resolved.name`
is exposed so a test can switch on which alias hit if the CLIs
diverge.

**`require_or_skip` returning `Option<Resolved>`, not
`Result<(), Skip>`.** Rust's default test harness reports
`#[test] fn x() -> Result<(), E>` returns of `Err(_)` as test
failures. The natural-feeling `let resolved = SGDISK.resolve()?`
flow looks like coverage but turns missing tools into red CI.
`let Some(_) = SGDISK.require_or_skip() else { return; };` is
slightly more verbose but exits the test `Ok` while still
panicking under `ISOMAGE_REQUIRE_TOOLS=1`. That env-var-gated
strict mode is the only thing keeping silent-coverage-loss out
of CI.

**`$IMAGE` / `$SRC_DIR` substitution with identifier-boundary
checks.** Tools take paths in three shapes: bare positional
(`$IMAGE`), `dd`-style key=value (`of=$IMAGE`), and path-suffix
(`$SRC_DIR/a`). The first iteration of substitution used
exact-arg match and broke all three real-world cases except #1.
The boundary-aware expander (`$IMAGES` ≠ `$IMAGE`) is what every
tool harness re-derives independently; doing it once here is
worth the 30 LOC. Test coverage: 7 unit tests in
`round_trip.rs::tests` cover the substitution corner cases.

**Snapshot headers carry tool version.** The body of a snapshot
file is the parsed tree; the header records the reference-tool
version that generated the golden. A future change to
`sgdisk` 2.0's output (e.g. it renames a default partition GUID)
will diff the body, but the header tells the reviewer "the
reference tool changed since this snapshot was pinned" — they
can re-pin intentionally rather than chase a phantom regression.
Headers are stripped before body comparison so re-running
doesn't flap on timestamps.

**No `insta`.** It would pull `serde`, `regex`, and a TOML
parser. The whole snapshot module is ~120 lines of stdlib;
adding the dep budget is unwarranted for a feature that's
exercised twice (mbr+gpt) so far and that has a clear,
documented file format.

**Strict mode is opt-in via env, not Cargo feature.** A cargo
feature would mean the `ISOMAGE_REQUIRE_TOOLS=1` behaviour is
chosen at compile time, and you'd rebuild between dev and CI
runs. An env var means one set of compiled tests, two policies.

**`tools::ECHO` / `tools::DD` / `tools::CAT` as
universally-available smoke-test tools.** The harness self-test
runs on every CI runner without any format-tool install. This
catches regressions in the harness itself (substitution,
detection, exit-code handling, panic-on-failure) that would
otherwise show up as cascading failures across the format-tool
matrix.

### Scope deliberately deferred

- **No proptest infra.** The harness is for round-trip tests
  against reference tools, not property-based parser fuzzing.
  Fuzz harness lands in a separate PR (A7 per the plan).
- **No write-side round trips.** `RoundTrip::overwrite_image`
  is exposed as the primitive, but no `tests/*_write.rs` lands
  yet — there are no writers to test.
- **No `tests/common/` graduating to a published crate.** The
  module is designed so the move is mechanical (no
  `crate::common` references, only `crate::common::tool` etc.)
  but the dep tree currently shows no consumer outside this
  repo. Moving is v3.1 territory.

### CI exposure

`.github/workflows/ci.yml` gains a `round-trip` job that runs
on ubuntu-latest AND macos-latest. Ubuntu installs the full
tool list via apt and runs strict-mode (`ISOMAGE_REQUIRE_TOOLS=1`).
macOS runs the same tests non-strict because several tools
(`sfdisk`, `mkntfs`, `debugfs`) don't have Homebrew formulas;
the policy is "macOS catches tests that depend on
brew-installable tools; Ubuntu is the canary for full coverage."

### Test results at commit time

- `cargo test --test harness_self_test` — 28 tests, all pass.
- `cargo test --features mbr,gpt --test mbr_round_trip` — 10 tests
  (7 inline substitution unit-tests + 3 round-trip tests that
  skip cleanly on macOS dev machines without sfdisk).
- `cargo test --features mbr,gpt --test gpt_round_trip` — 9 tests
  (same pattern; 2 skip without sgdisk).
- `ISOMAGE_REQUIRE_TOOLS=1` on the dev box correctly panics with
  "refusing to skip silently" so CI's strict-mode invariant is
  validated.
- All existing lib unit + doc tests still green (47 + 7).
