//! Round-trip tests for the GPT reader in `src/formats/gpt.rs`.
//!
//! Build a GPT image with `sgdisk` (from gptfdisk), parse it with
//! `isomage`, assert the parsed entries match.
//!
//! ## Availability
//!
//! `sgdisk` ships in the `gdisk` Debian package and is also
//! available on macOS via `brew install gptfdisk`. CI installs it on
//! both runners.

mod common;

use common::assertions::{assert_partition_at, assert_tree_invariants};
use common::snapshot::assert_snapshot_with_tool;
use common::tools;
use common::RoundTrip;

use isomage::formats::gpt;

/// One partition named "Linux", spanning ~10 MiB. We assert the
/// parsed entry's byte range matches the LBA arithmetic and the
/// name decodes correctly from UTF-16LE.
#[test]
fn single_named_partition() {
    let Some(_) = tools::SGDISK.require_or_skip() else {
        return;
    };

    let image = RoundTrip::new("gpt-single-named")
        .with(&tools::SGDISK)
        .image_size(50 * 1024 * 1024)
        .args([
            "--clear",
            "--new=1:2048:+10M",
            "--typecode=1:8300",
            "--change-name=1:Linux",
            "$IMAGE",
        ])
        .build_bytes();

    let partitions = gpt::parse_header_sector(&image[512..1024]).expect("parse GPT header");
    assert_eq!(partitions.num_entries, 128);
    assert_eq!(partitions.entry_size, 128);

    // Re-parse via the full read path (header + entries) using the
    // public detect_and_parse entry point on a temp File.
    let tree = parse_from_bytes(&image);
    assert_tree_invariants(&tree);
    assert_eq!(tree.children.len(), 1, "expected exactly one partition");

    // First LBA was 2048; +10 MiB at 512 B/sector is 20480 sectors,
    // so last_lba is 2048 + 20480 - 1 = 22527. sgdisk may round to
    // a sector multiple; assert start exactly, length within 1 MiB.
    let p = &tree.children[0];
    assert_eq!(p.file_location, Some(2048 * 512));
    let expected_length = 10 * 1024 * 1024;
    let tolerance = 1024 * 1024;
    let diff = (p.size as i64 - expected_length as i64).unsigned_abs();
    assert!(
        diff <= tolerance,
        "partition length {} differs from expected {} by more than 1 MiB ({})",
        p.size,
        expected_length,
        diff,
    );

    // The name should appear in the child name.
    assert!(
        p.name.starts_with("Linux-"),
        "expected partition name to start with 'Linux-', got {:?}",
        p.name,
    );

    let tool_version = tools::SGDISK.version();
    assert_snapshot_with_tool("gpt-single-named", &tree, tool_version.as_deref());
}

/// Two partitions with different names + type codes.
#[test]
fn two_partitions_different_types() {
    let Some(_) = tools::SGDISK.require_or_skip() else {
        return;
    };

    let image = RoundTrip::new("gpt-two-different")
        .with(&tools::SGDISK)
        .image_size(80 * 1024 * 1024)
        .args([
            "--clear",
            "--new=1:2048:+10M",
            "--typecode=1:EF00", // EFI System
            "--change-name=1:EFI",
            "--new=2:0:+20M",
            "--typecode=2:8300", // Linux Filesystem
            "--change-name=2:root",
            "$IMAGE",
        ])
        .build_bytes();

    let tree = parse_from_bytes(&image);
    assert_eq!(tree.children.len(), 2);
    let names: Vec<&str> = tree.children.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names[0].starts_with("EFI-"),
        "first partition name = {:?}",
        names[0]
    );
    assert!(
        names[1].starts_with("root-"),
        "second partition name = {:?}",
        names[1]
    );

    // First partition starts at 2048; second starts after it. The
    // exact LBA depends on sgdisk's alignment; assert ordering and
    // non-overlap rather than exact positions.
    let first_start = tree.children[0].file_location.unwrap();
    let first_end = first_start + tree.children[0].size;
    let second_start = tree.children[1].file_location.unwrap();
    assert_eq!(first_start, 2048 * 512);
    assert!(
        second_start >= first_end,
        "second partition (start={second_start}) overlaps first (end={first_end})"
    );
}

/// Helper: parse a GPT image straight from bytes by materializing
/// to a tempfile and calling the v2-style entry. When the parser
/// generalization PR lands this collapses to a single `Cursor::new`.
fn parse_from_bytes(image: &[u8]) -> isomage::TreeNode {
    use std::fs::File;
    use std::io::Write;
    let dir = tempfile::TempDir::with_prefix("isomage-gpt-").unwrap();
    let path = dir.path().join("image.bin");
    {
        let mut f = File::create(&path).unwrap();
        f.write_all(image).unwrap();
        f.sync_all().unwrap();
    }
    let mut f = File::open(&path).unwrap();
    gpt::detect_and_parse(&mut f).expect("parse GPT")
}

/// Silence dead-code warning for `partition_at` if no test uses it
/// directly in this binary.
#[allow(dead_code)]
fn _silence(_: &fn(&isomage::TreeNode, usize, u64, u64)) {}
#[allow(dead_code)]
fn _u(t: &isomage::TreeNode) {
    assert_partition_at(t, 0, 0, 0);
}
