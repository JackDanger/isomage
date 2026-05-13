//! Round-trip tests for the VMDK reader in `src/formats/vmdk.rs`.
//!
//! These tests use `qemu-img` from the `qemu-utils` package (Linux) or
//! `brew install qemu` (macOS). They skip automatically when `qemu-img`
//! is not installed. On the CI `round-trip` job (Ubuntu,
//! `ISOMAGE_REQUIRE_TOOLS=1`) they run for real.

mod common;

use std::fs;
use std::io::Cursor;

use common::tools;
use common::RoundTrip;

use isomage::formats::vmdk;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a VMDK image from raw bytes and return the root TreeNode.
fn parse_image(bytes: &[u8]) -> isomage::TreeNode {
    let mut c = Cursor::new(bytes.to_vec());
    vmdk::detect(&mut c).expect("vmdk::detect returned Err for a freshly-minted VMDK");
    let mut c2 = Cursor::new(bytes.to_vec());
    vmdk::detect_and_parse(&mut c2).expect("vmdk::detect_and_parse failed")
}

// ── Test 1: Sparse VMDK detection ────────────────────────────────────────────

#[test]
fn sparse_vmdk_detect() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img create -f vmdk creates a monolithicSparse VMDK by default.
    let rt = RoundTrip::new("vmdk-sparse-detect")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "vmdk", "$IMAGE", "10M"])
        .build_bytes();

    let mut c = Cursor::new(rt.clone());
    let result = vmdk::detect(&mut c);
    assert!(
        result.is_ok(),
        "vmdk::detect should succeed for a qemu-img sparse VMDK: {result:?}"
    );
}

// ── Test 2: VMDK virtual size ─────────────────────────────────────────────────

#[test]
fn vmdk_virtual_size() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img rounds up to the nearest sector boundary; 10M is exact so
    // we get exactly 10 MiB. Verify the range [10 MiB, 11 MiB).
    const TEN_MB: u64 = 10 * 1024 * 1024;
    const ELEVEN_MB: u64 = 11 * 1024 * 1024;

    let bytes = RoundTrip::new("vmdk-virtual-size")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "vmdk", "$IMAGE", "10M"])
        .build_bytes();

    let root = parse_image(&bytes);
    let disk = root
        .children
        .iter()
        .find(|n| n.name == "disk.vmdk")
        .expect("disk.vmdk child should exist in VMDK tree");

    assert!(
        disk.size >= TEN_MB && disk.size < ELEVEN_MB,
        "disk.vmdk size {sz} should be in [10 MiB, 11 MiB)",
        sz = disk.size,
    );
    assert_eq!(
        disk.file_length,
        Some(disk.size),
        "disk.vmdk file_length should equal size"
    );
}

// ── Test 3: VMDK file structure ───────────────────────────────────────────────

#[test]
fn vmdk_file_structure() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    let bytes = RoundTrip::new("vmdk-file-structure")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "vmdk", "$IMAGE", "10M"])
        .build_bytes();

    let root = parse_image(&bytes);

    // Root must be a directory named "/".
    assert_eq!(root.name, "/", "root node should be named \"/\"");
    assert!(root.is_directory, "root should be a directory");
    assert_eq!(
        root.children.len(),
        1,
        "root should have exactly one child (disk.vmdk)"
    );

    let disk = &root.children[0];
    assert_eq!(disk.name, "disk.vmdk", "child should be named disk.vmdk");
    assert!(!disk.is_directory, "disk.vmdk should not be a directory");
    assert!(
        disk.children.is_empty(),
        "disk.vmdk should have no children"
    );

    // Sparse VMDK: grain data is fragmented via the GD; file_location=None.
    assert_eq!(
        disk.file_location, None,
        "sparse VMDK disk.vmdk should have file_location=None (grain directory indirection)"
    );
}

// ── Test 4: VMDK version 1 header ────────────────────────────────────────────

#[test]
fn vmdk_version1() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img creates version=1 monolithicSparse VMDKs by default.
    let bytes = RoundTrip::new("vmdk-version1")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "vmdk", "$IMAGE", "4M"])
        .build_bytes();

    // Verify we can parse without error — version 1 is explicitly supported.
    let root = parse_image(&bytes);
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].name, "disk.vmdk");
}

// ── Test 5: qemu-img info cross-validation + twoGbMaxExtentSparse ─────────────

#[test]
fn vmdk_two_gb_split() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // twoGbMaxExtentSparse: qemu-img creates a text descriptor at `$IMAGE`
    // and one or more sparse extent files named `<stem>-s001.<ext>`,
    // `<stem>-s002.<ext>`, etc. in the same directory. The descriptor file
    // is plain text (not a sparse extent), so we detect on the first extent
    // file (`image-s001.bin` for our `image.bin` placeholder path).
    let rt = RoundTrip::new("vmdk-twogb-split")
        .with(&tools::QEMU_IMG)
        .args([
            "create",
            "-f",
            "vmdk",
            "-o",
            "subformat=twoGbMaxExtentSparse",
            "$IMAGE",
            "8M",
        ])
        .build();

    // Ask qemu-img for info on the descriptor to confirm it's VMDK.
    let info_out = tools::QEMU_IMG
        .run(["info", "-f", "vmdk", rt.image_path().to_str().unwrap()])
        .expect("qemu-img info run");

    info_out.assert_contains("vmdk");

    // The first extent file lives next to $IMAGE with the pattern
    // `<stem>-s001.<ext>`. For our placeholder `image.bin` that is
    // `image-s001.bin`.
    let extent_path = rt.tempdir().join("image-s001.bin");
    assert!(
        extent_path.exists(),
        "expected twoGbMaxExtentSparse first extent at {extent_path:?}"
    );

    let extent_bytes = fs::read(&extent_path).expect("read extent file");
    let mut c = Cursor::new(extent_bytes);
    let result = vmdk::detect(&mut c);
    assert!(
        result.is_ok(),
        "vmdk::detect should succeed for twoGbMaxExtentSparse first extent: {result:?}"
    );
}
