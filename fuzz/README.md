# Fuzzing isomage

`cargo-fuzz` targets for the format parsers. Every new format
under `src/formats/<name>.rs` should land with a fuzz target in
the same PR — the target is ~15 lines (see
[`fuzz_targets/mbr_parse_sector.rs`](fuzz_targets/mbr_parse_sector.rs)
for the template).

## Quick start

```sh
rustup install nightly
cargo install cargo-fuzz

# List available targets
cargo +nightly fuzz list

# Run a target until you Ctrl-C (or it finds a crash)
cargo +nightly fuzz run mbr_parse_sector

# Run for a bounded time (good for local sanity check)
cargo +nightly fuzz run mbr_parse_sector -- -max_total_time=60

# Replay a previously-found crash
cargo +nightly fuzz run mbr_parse_sector \
    fuzz/artifacts/mbr_parse_sector/crash-<hash>
```

## What's committed vs ignored

| Path | Tracked? | Why |
|------|----------|-----|
| `fuzz/Cargo.toml` | yes | crate manifest |
| `fuzz/fuzz_targets/*.rs` | yes | targets are source |
| `fuzz/seeds/<target>/*` | yes | bootstrap inputs |
| `fuzz/corpus/<target>/` | no | accumulated during runs |
| `fuzz/artifacts/<target>/` | no | crash reports |
| `fuzz/target/` | no | build output |
| `fuzz/Cargo.lock` | no | regenerated each build |

## Seed corpus

Each target has a seed directory at `fuzz/seeds/<target>/` with
small known-interesting inputs:

- valid minimal headers
- adversarial boundary values (e.g. `entry_size = 0`,
  `num_entries = u32::MAX`)
- protective-marker variants

cargo-fuzz copies seeds into the live corpus on first run.

## CI

A `fuzz-compile` job in `.github/workflows/ci.yml` checks that
every target compiles under nightly Rust. It does **not** run the
fuzzers (libFuzzer needs ten of seconds minimum to find anything,
and runner time is finite). Long-running fuzz campaigns live in a
separate workflow not yet wired (`fuzz-nightly.yml` is on the
plan).

## Adding a fuzz target for a new format

When `src/formats/<name>.rs` lands:

1. Add a feature dep in `fuzz/Cargo.toml`:

   ```toml
   [dependencies.isomage]
   features = ["mbr", "gpt", "<name>"]
   ```

2. Add a target binary:

   ```toml
   [[bin]]
   name = "<name>_parse"
   path = "fuzz_targets/<name>_parse.rs"
   test = false
   doc = false
   bench = false
   ```

3. Write `fuzz_targets/<name>_parse.rs` from the template.

4. Drop one or two known-good headers into
   `fuzz/seeds/<name>_parse/`.

5. The `fuzz-compile` CI job will validate the target builds on
   the next push.
