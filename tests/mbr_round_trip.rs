//! Round-trip tests for the MBR partition-table reader in
//! `src/formats/mbr.rs`.
//!
//! Build an MBR image with `sfdisk` (util-linux), parse it with
//! `isomage`, assert the parsed partition list matches the directive
//! we gave to `sfdisk`.
//!
//! ## Availability
//!
//! `sfdisk` ships with `util-linux` on every modern Linux distro.
//! It is **not** available on macOS by default; these tests skip
//! on macOS unless the user has installed it (e.g. via the
//! `linuxbrew` overlay). CI runs them on Ubuntu, where they
//! exercise the real format-tool path.

mod common;

use common::assertions::{assert_partition_at, assert_path_exists, assert_tree_invariants};
use common::snapshot::assert_snapshot_with_tool;
use common::tools;
use common::RoundTrip;

use isomage::formats::mbr;

/// Bare minimum: one Linux partition (type 0x83) starting at LBA
/// 2048, 100 MiB long. The first sector after that is left empty.
#[test]
fn single_linux_partition() {
    let Some(_) = tools::SFDISK.require_or_skip() else {
        return;
    };

    // sfdisk needs a pre-existing file of at least the target size.
    // 200 MiB gives us slack for the partition + header room.
    let image = RoundTrip::new("mbr-single-linux")
        .with(&tools::SFDISK)
        .image_size(200 * 1024 * 1024)
        // --wipe=always: never prompt for a re-read after layout change.
        // Standard input is the layout directive.
        .args(["--wipe=always", "--no-tell-kernel", "$IMAGE"])
        .stdin(
            "label: dos\n\
             unit: sectors\n\
             2048,204800,83\n",
        )
        .build_bytes();

    // Parse with our MBR module.
    let partitions = mbr::parse_sector(&image[..512]).expect("parse MBR");
    assert_eq!(partitions.len(), 1);
    let p = &partitions[0];
    assert_eq!(p.type_code, 0x83, "Linux partition type");
    assert_eq!(p.start, 2048 * 512);
    assert_eq!(p.length, 204800 * 512);

    // Round trip through TreeNode shape — what `cat_node` consumers see.
    let tree = mbr::to_tree(&partitions);
    assert_tree_invariants(&tree);
    assert_eq!(tree.children.len(), 1);
    assert_partition_at(&tree, 0, 2048 * 512, 204800 * 512);
    assert_path_exists(&tree, "partition-0-type-83");

    let tool_version = tools::SFDISK.version();
    assert_snapshot_with_tool("mbr-single-linux", &tree, tool_version.as_deref());
}

/// Three primary partitions of different types, contiguous in LBA
/// order. Verifies multi-partition parsing and per-partition byte-
/// range arithmetic across an MBR with non-trivial diversity.
///
/// Earlier draft used sfdisk's `;` directive to try to skip slot 2,
/// but `;` means "use defaults" in sfdisk's DSL — partition 3 then
/// swallowed all remaining space and a fourth partition request
/// failed with "All space for primary partitions is in use." The
/// "empty slot" case is covered by `formats::mbr::tests` directly
/// (synthetic sector + parse), where we don't need sfdisk to
/// express it.
#[test]
fn three_partitions_different_types() {
    let Some(_) = tools::SFDISK.require_or_skip() else {
        return;
    };

    let image = RoundTrip::new("mbr-three-different")
        .with(&tools::SFDISK)
        .image_size(300 * 1024 * 1024)
        .args(["--wipe=always", "--no-tell-kernel", "$IMAGE"])
        .stdin(
            "label: dos\n\
             unit: sectors\n\
             2048,51200,83\n\
             53248,51200,07\n\
             104448,51200,82\n",
        )
        .build_bytes();

    let partitions = mbr::parse_sector(&image[..512]).expect("parse MBR");
    assert_eq!(
        partitions.len(),
        3,
        "expected 3 partitions, got {}",
        partitions.len()
    );
    let mut types: Vec<u8> = partitions.iter().map(|p| p.type_code).collect();
    types.sort_unstable();
    assert_eq!(types, vec![0x07, 0x82, 0x83]);

    // Each partition starts where its `start=` field declared.
    let starts: Vec<u64> = partitions.iter().map(|p| p.start).collect();
    assert_eq!(starts, vec![2048 * 512, 53248 * 512, 104448 * 512]);
}

/// Protective-MBR detection: when `sgdisk` writes a GPT, it leaves
/// a protective MBR with a single 0xEE partition. Our parser must
/// recognise that and report `Error::ProtectiveMbr` rather than
/// expose 0xEE as a real partition.
///
/// This test depends on `sgdisk`, so it's in the *MBR* file but
/// skips if `sgdisk` is missing — they share the same image, just
/// looked at from different ends.
#[test]
fn protective_mbr_signaled() {
    let Some(_) = tools::SGDISK.require_or_skip() else {
        return;
    };

    let image = RoundTrip::new("mbr-protective")
        .with(&tools::SGDISK)
        .image_size(50 * 1024 * 1024)
        // sgdisk needs `--clear` for a fresh table on a virgin file.
        .args([
            "--clear",
            "--new=1:2048:+10M",
            "--typecode=1:8300",
            "$IMAGE",
        ])
        .build_bytes();

    let result = mbr::parse_sector(&image[..512]);
    match result {
        Err(mbr::Error::ProtectiveMbr) => {}
        Err(e) => panic!("expected ProtectiveMbr error, got: {e}"),
        Ok(parts) => panic!(
            "expected ProtectiveMbr error, got {} partitions: {parts:?}",
            parts.len()
        ),
    }
}
