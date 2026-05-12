# Integration tests

Per-format round-trip tests live here, one file per format,
plus a self-test that validates the harness itself.

## What's in this tree

| Path | Purpose |
|------|---------|
| `common/` | Shared infrastructure. Tool detection, `RoundTrip` builder, tree assertions, golden-file snapshots. |
| `harness_self_test.rs` | Smoke test for `common/` using universally-available tools (`echo`, `dd`, `cat`). Runs on every CI runner with no extra installs. |
| `<format>_round_trip.rs` | One file per format. Builds an image with the reference tool (`sfdisk`, `qemu-img`, `mksquashfs`, …), parses it with `isomage`, asserts the tree matches. |
| `snapshots/` | Golden files written by `common::snapshot::assert_snapshot`. Refresh with `ISOMAGE_UPDATE_SNAPSHOTS=1 cargo test`. |

## Running

```sh
# All tests, default features, skip if format tools are missing.
cargo test

# Specific format only.
cargo test --features mbr --test mbr_round_trip

# Strict mode: a missing format tool fails the test instead of skipping.
# Use this in CI on a runner where you've installed the tool list.
ISOMAGE_REQUIRE_TOOLS=1 cargo test --test mbr_round_trip

# Refresh golden snapshots after intentional output changes.
ISOMAGE_UPDATE_SNAPSHOTS=1 cargo test
```

## Skip-or-fail policy

Reference tools (`sfdisk`, `sgdisk`, `qemu-img`, `mkfs.vfat`, …)
are not installed everywhere. The default policy:

- **Tool missing** → test logs `skip: <tool> not installed` and
  returns success. Coverage is lost silently.
- **Tool present, parser wrong** → test fails normally.

In CI we don't want silent coverage loss. Set the env var
`ISOMAGE_REQUIRE_TOOLS=1` and the skip path panics. The
`round-trip` job in `.github/workflows/ci.yml` installs the tool
list and sets this flag.

## Adding a new format

When a new `src/formats/<name>.rs` lands, the corresponding
`tests/<name>_round_trip.rs` lands in the same PR. Template:

```rust
//! Round-trip tests for the <FORMAT> reader.

mod common;

use common::{
    assertions::{assert_tree_invariants, assert_path_exists},
    snapshot::assert_snapshot_with_tool,
    tools,
    RoundTrip,
};

use isomage::formats::<format>;

#[test]
fn minimal_image() {
    // 1. Guard on tool availability. Skip in dev, fail in strict mode.
    let Some(_) = tools::<TOOL>.require_or_skip() else { return; };

    // 2. Build an image with the reference tool.
    let image = RoundTrip::new("<format>-minimal")
        .with(&tools::<TOOL>)
        .image_size(50 * 1024 * 1024)
        .args(["...", "$IMAGE"])
        .build_bytes();

    // 3. Parse with isomage.
    let tree = <format>::detect_and_parse_bytes(&image)
        .expect("parse <format>");

    // 4. Assert tree shape.
    assert_tree_invariants(&tree);
    assert_path_exists(&tree, "expected/path");

    // 5. Optional: snapshot for future regression detection.
    let v = tools::<TOOL>.version();
    assert_snapshot_with_tool("<format>-minimal", &tree, v.as_deref());
}
```

Then update `tests/common/binaries.rs` if the tool is new, and
add the package name to both the Ubuntu apt-install and macOS
brew-install lists in `.github/workflows/ci.yml`.

## What the harness does **not** do (intentional gaps)

- **Run tests in parallel against the same image file.** Each
  `RoundTrip::build` allocates its own tempdir, so test
  concurrency is fine; but if a format-specific test wants
  shared fixtures, it must build them once in a `static` or
  `OnceLock` and the cost amortizes across tests in that binary
  only.
- **Validate snapshots in CI without `ISOMAGE_UPDATE_SNAPSHOTS`**.
  First-run snapshots are created with a warning printed to
  stderr. Inspect them in PR review before merging. CI catches
  unintended changes on subsequent runs.
- **Test write-side correctness** until Phase 4 writers land.
  When they do, `RoundTrip::overwrite_image` provides the
  reverse-direction primitive: isomage writes → reference tool
  validates via fsck/info.
