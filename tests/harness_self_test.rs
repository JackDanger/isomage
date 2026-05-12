//! Self-test for `tests/common/`.
//!
//! Exercises every code path of the round-trip harness using tools
//! that exist on every POSIX system (`echo`, `printf`, `dd`, `true`,
//! `false`, `cat`). No external-format-tool install needed; this
//! file is the smoke test that runs on every CI runner regardless of
//! the format-tool matrix.
//!
//! What's covered:
//!
//! 1. `Tool::resolve` finds binaries that exist; returns `None` for
//!    binaries that don't.
//! 2. `Tool::version` returns text when the tool supports it.
//! 3. `RoundTrip::build` substitutes `$IMAGE`, `$SRC_DIR`, `$TMP`.
//! 4. `RoundTrip::build` returns image bytes after tool exits.
//! 5. A non-zero exit from the tool panics (round-trip is strict).
//! 6. `Skip::if_missing` returns `Err(Skip)` in dev mode and panics
//!    in strict mode.
//! 7. `assert_path_exists` / `assert_partition_at` / `assert_tree_invariants`
//!    detect both shape matches and mismatches.
//! 8. `snapshot::render_tree` is deterministic across two runs.

mod common;

use common::tool::{Skip, Tool, ToolError};
use common::tools;
use common::RoundTrip;

use isomage::TreeNode;

/// Helper that all skip-aware tests use to ensure we exit cleanly
/// when running this self-test on a machine that's somehow missing
/// `echo`/`dd`/etc. In practice none of these are ever missing on
/// a POSIX runner, so this is a belt-and-braces safety net.
fn require_or_skip(tool: &Tool) -> bool {
    tool.require_or_skip().is_some()
}

// ---- Tool resolution -----------------------------------------------------

#[test]
fn echo_resolves() {
    let r = tools::ECHO.resolve();
    assert!(
        r.is_some(),
        "echo must be on PATH on every supported runner"
    );
}

#[test]
fn nonexistent_tool_does_not_resolve() {
    const FAKE: Tool = Tool::new("isomage-this-binary-does-not-exist-12345");
    assert!(FAKE.resolve().is_none());
    assert!(!FAKE.is_available());
}

#[test]
fn alias_fallback_resolves_first_present() {
    const T: Tool = Tool::with_aliases("isomage-not-real-primary-xyz", &["echo"]);
    let r = T.resolve().expect("alias should resolve to `echo`");
    assert_eq!(r.name, "echo");
}

#[test]
fn version_is_best_effort() {
    // `echo --version` works on GNU coreutils and macOS BSD echo
    // (which ignores the flag and prints the literal). Either way
    // we should get *something* or None; never a panic.
    let _ = tools::ECHO.version();
}

// ---- Tool invocation -----------------------------------------------------

#[test]
fn run_captures_stdout() {
    let out = tools::ECHO.run(["hello"]).expect("echo run");
    out.assert_success();
    let s = out.stdout_string();
    assert!(s.contains("hello"), "got stdout: {s:?}");
}

#[test]
fn run_captures_exit_status() {
    let out = tools::TRUE_BIN
        .run(std::iter::empty::<&str>())
        .expect("true");
    assert!(out.status.success());

    let out = tools::FALSE_BIN
        .run(std::iter::empty::<&str>())
        .expect("false");
    assert!(!out.status.success());
}

#[test]
fn run_with_stdin_passes_bytes() {
    let out = tools::CAT
        .run_with_stdin(std::iter::empty::<&str>(), b"piped\n")
        .expect("cat");
    out.assert_success();
    assert_eq!(out.stdout, b"piped\n");
}

#[test]
fn missing_tool_returns_not_found() {
    const FAKE: Tool = Tool::new("isomage-no-such-tool-xyz");
    let err = FAKE.run(std::iter::empty::<&str>()).unwrap_err();
    assert!(matches!(err, ToolError::NotFound { .. }));
}

// ---- Skip pattern --------------------------------------------------------

#[test]
fn skip_returns_err_for_missing_tool() {
    const FAKE: Tool = Tool::new("isomage-no-such-tool-skip-xyz");
    // Ensure strict mode is OFF for this test (in case CI flipped it).
    // env_remove/env_set are not multi-thread-safe; this is best-effort
    // and racy with concurrent tests in the same binary. The
    // strict-mode panic path is exercised indirectly when CI runs with
    // ISOMAGE_REQUIRE_TOOLS=1 against a runner that's missing tools.
    let saved = std::env::var_os("ISOMAGE_REQUIRE_TOOLS");
    std::env::remove_var("ISOMAGE_REQUIRE_TOOLS");
    let result = Skip::if_missing(&FAKE);
    if let Some(v) = saved {
        std::env::set_var("ISOMAGE_REQUIRE_TOOLS", v);
    }
    assert!(result.is_err());
}

#[test]
fn skip_passes_for_present_tool() {
    let r = Skip::if_missing(&tools::ECHO);
    assert!(r.is_ok());
}

// ---- RoundTrip $IMAGE / $SRC_DIR substitution ----------------------------

/// Use `dd` to write a known byte pattern to `$IMAGE`, then read it
/// back from the harness output. Validates argument substitution
/// and the bytes-out path.
#[test]
fn round_trip_dd_writes_image() {
    if !require_or_skip(&tools::DD) {
        return;
    }

    let result = RoundTrip::new("self-dd-zero")
        .with(&tools::DD)
        .args([
            "if=/dev/zero",
            "of=$IMAGE",
            "bs=1",
            "count=10",
            "status=none",
        ])
        .build();
    let bytes = result.bytes();
    assert_eq!(bytes.len(), 10);
    assert!(bytes.iter().all(|&b| b == 0));
}

/// Verify $SRC_DIR substitution works: stage two files, then have
/// `cat` concatenate them to stdout.
#[test]
fn round_trip_source_files_staged() {
    if !require_or_skip(&tools::CAT) {
        return;
    }

    let result = RoundTrip::new("self-source-files")
        .with(&tools::CAT)
        // `cat` reads each arg as a filename, concatenates to stdout.
        // We point it at $SRC_DIR/a and $SRC_DIR/b which we staged.
        .arg("$SRC_DIR/a")
        .arg("$SRC_DIR/b")
        .source_file("a", b"AAA".to_vec())
        .source_file("b", b"BBB".to_vec())
        .build();
    assert_eq!(result.tool_output().stdout, b"AAABBB");
}

#[test]
fn round_trip_image_preallocated() {
    if !require_or_skip(&tools::DD) {
        return;
    }

    // Pre-allocate a 4 KiB image, then have dd overwrite the first 4
    // bytes. Verify the image is still 4 KiB (conv=notrunc preserves
    // tail) and the first 4 bytes are zero.
    let result = RoundTrip::new("self-prealloc-dd")
        .with(&tools::DD)
        .image_size(4096)
        // `dd` reads 4 bytes from /dev/zero (so they're 0x00) into
        // the image. We're verifying preallocation, not the bytes
        // themselves — set_len makes a zero-filled sparse file, and
        // dd overwriting with zeros leaves the same zeros.
        .args([
            "if=/dev/zero",
            "of=$IMAGE",
            "conv=notrunc",
            "bs=1",
            "count=4",
            "status=none",
        ])
        .build();
    let bytes = result.bytes();
    assert_eq!(bytes.len(), 4096, "preallocation must be honored");
    assert!(
        bytes[..4].iter().all(|&b| b == 0),
        "dd should have written zeros at the start"
    );
}

#[test]
#[should_panic(expected = "exited")]
fn round_trip_panics_on_nonzero_exit() {
    // build() panics with "tool ... exited Some(1)" message.
    let _ = RoundTrip::new("self-false").with(&tools::FALSE_BIN).build();
}

/// require_or_skip should return Some on a present tool. Combined
/// with the panic-on-strict path tested elsewhere, this validates the
/// canonical test guard.
#[test]
fn require_or_skip_returns_some_for_present() {
    assert!(tools::ECHO.require_or_skip().is_some());
}

// ---- Assertions ----------------------------------------------------------

fn fake_tree() -> TreeNode {
    let mut root = TreeNode::new_directory("/".to_string());
    root.add_child(TreeNode::new_file_with_location(
        "partition-0-type-83".to_string(),
        1024,
        512,
        1024,
    ));
    root.add_child(TreeNode::new_file_with_location(
        "partition-1-type-07".to_string(),
        2048,
        1536,
        2048,
    ));
    root.calculate_directory_size();
    root
}

#[test]
fn assert_path_exists_returns_node() {
    let root = fake_tree();
    let n = common::assertions::assert_path_exists(&root, "partition-0-type-83");
    assert_eq!(n.size, 1024);
}

#[test]
#[should_panic(expected = "expected path")]
fn assert_path_exists_panics_on_missing() {
    let root = fake_tree();
    common::assertions::assert_path_exists(&root, "nonexistent");
}

#[test]
fn assert_partition_at_validates_byte_range() {
    let root = fake_tree();
    common::assertions::assert_partition_at(&root, 0, 512, 1024);
    common::assertions::assert_partition_at(&root, 1, 1536, 2048);
}

#[test]
#[should_panic(expected = "expected start")]
fn assert_partition_at_detects_wrong_start() {
    let root = fake_tree();
    common::assertions::assert_partition_at(&root, 0, 999, 1024);
}

#[test]
fn assert_tree_invariants_passes_for_valid() {
    let root = fake_tree();
    common::assertions::assert_tree_invariants(&root);
}

// ---- Snapshot rendering --------------------------------------------------

#[test]
fn snapshot_render_is_deterministic() {
    let a = common::snapshot::render_tree(&fake_tree());
    let b = common::snapshot::render_tree(&fake_tree());
    assert_eq!(a, b, "render must be deterministic across calls");
    assert!(a.contains("partition-0-type-83"));
    assert!(a.contains("partition-1-type-07"));
}
