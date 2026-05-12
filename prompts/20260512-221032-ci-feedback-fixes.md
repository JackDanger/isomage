---
session: "v3-scope-2026-05-12"
timestamp: "2026-05-12T22:10:32Z"
model: claude-opus-4-7
sequence: 6
---

## Human

[Implicit: address the failing CI checks on PR #10.]

## Assistant

### Three independent CI failures, three root causes

PR #10 ran 14 CI jobs after I pushed. 10 passed; 4 failed (one of
the round-trip jobs failed due to the same test bug as the main
test job, so really 3 root causes).

#### 1. MSRV — dev-deps drag MSRV up

The MSRV job ran `cargo build --all-targets`, which compiles
dev-deps including `criterion 0.5`. criterion transitively pulls
`clap 4.6.1`, whose `Cargo.toml` declares `edition = "2024"` —
not supported by Rust 1.74 (edition 2024 is stable from 1.85).

The conceptual mistake was treating MSRV as "what compiles
in CI." MSRV is what **downstream users** need; they never see
dev-deps. Fix: MSRV check switched to `cargo build --lib`,
which compiles the published library only. Also added per-feature
isolation builds (`--no-default-features --features <one>`) so a
new format flag can't accidentally raise MSRV via a non-default
codec dep.

#### 2. `three_partitions_with_gap` — wrong sfdisk DSL

I wrote the test directive as:

```
2048,51200,83
53248,51200,07
;
104448,51200,82
```

intending `;` to skip slot 3 so partition 4 lands in slot 4.
But sfdisk's `;` means "use defaults": the third entry took
all remaining space, leaving slot 4 with "Failed to add #4
partition: Invalid argument."

There's no clean way to ask sfdisk for "leave slot N empty
and define slot N+1," because its DSL fills slots in order
top-to-bottom. The empty-slot case is already covered by
`formats::mbr::tests::three_partitions_one_empty` in the
parser unit tests (synthetic sector, no external tool), so
nothing's lost.

Fix: rename the test to `three_partitions_different_types`,
drop the gap directive, and assert exact `start` offsets for
all three partitions — strictly more than the previous "3 or
4 partitions, in some order" assertion.

#### 3. fuzz-compile — `--locked` pinned a broken old `rustix`

`cargo install --locked cargo-fuzz` uses cargo-fuzz's committed
`Cargo.lock`, which pins `rustix 0.36.5`. That rustix version
has `#[cfg_attr(rustc_attrs, rustc_layout_scalar_valid_range_end(0xffff))]`
in its source — gated on `rustc_attrs` which is a nightly
internal attribute. Today's nightly rejects it (semantics
changed; the gate now requires unstable feature enable).

Fix: drop `--locked`, let cargo resolve a newer rustix that
compiles. Slightly slower on first install (deps to resolve)
but reliable.

### Validation

- `cargo test --features mbr,gpt,raw,mmap,simd --test mbr_round_trip`
  on this Mac — 15 tests pass; round-trip ones still skip
  because sfdisk isn't installed locally. Will run for real on
  Ubuntu CI.
- `cargo clippy --all-targets --features mbr,gpt,raw,mmap,simd
  -- -D warnings` — clean.
- YAML parses.

Pushing as a follow-up commit so PR #10's CI re-runs against
the fixes.

### What I'm watching for on the next CI run

- MSRV: must pass. `cargo build --lib` is the simplest check
  that's still meaningful; if it fails, something in `src/` is
  using a post-1.74 feature.
- `three_partitions_different_types` on both `test (ubuntu-latest)`
  and `round-trip (ubuntu-latest)`: must pass. If sfdisk's
  partition layout differs from what I asked for at exact byte
  level, the strict `assert_eq!` on `starts` will diff loudly,
  which is what we want.
- fuzz-compile: must build. cargo-fuzz install + the two fuzz
  targets.
- round-trip-pinned: continues to "pass" because the GHCR image
  still doesn't exist; this becomes meaningful only after the
  first `test-tools-v1` tag push.
