//! Round-trip tests for the VHD reader in `src/formats/vhd.rs`.
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

use isomage::formats::vhd;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a VHD image from raw bytes and return the root TreeNode.
fn parse_image(bytes: &[u8]) -> isomage::TreeNode {
    let mut c = Cursor::new(bytes.to_vec());
    vhd::detect(&mut c).expect("vhd::detect returned Err for a freshly-minted VHD");
    let mut c2 = Cursor::new(bytes.to_vec());
    vhd::detect_and_parse(&mut c2).expect("vhd::detect_and_parse failed")
}

// ── Test 1: Fixed VHD detection ───────────────────────────────────────────────

#[test]
fn fixed_vhd_detect() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img create -f vpc -o subformat=fixed creates a Fixed VHD.
    let rt = RoundTrip::new("vhd-fixed-detect")
        .with(&tools::QEMU_IMG)
        .args([
            "create",
            "-f",
            "vpc",
            "-o",
            "subformat=fixed",
            "$IMAGE",
            "1M",
        ])
        .build_bytes();

    let mut c = Cursor::new(rt.clone());
    let result = vhd::detect(&mut c);
    assert!(
        result.is_ok(),
        "vhd::detect should succeed for a qemu-img fixed VHD: {result:?}"
    );
}

// ── Test 2: Dynamic VHD detection ────────────────────────────────────────────

#[test]
fn dynamic_vhd_detect() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img create -f vpc (default) creates a Dynamic VHD.
    let rt = RoundTrip::new("vhd-dynamic-detect")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "vpc", "$IMAGE", "10M"])
        .build_bytes();

    let mut c = Cursor::new(rt.clone());
    let result = vhd::detect(&mut c);
    assert!(
        result.is_ok(),
        "vhd::detect should succeed for a qemu-img dynamic VHD: {result:?}"
    );
}

// ── Test 3: VHD virtual size ──────────────────────────────────────────────────

#[test]
fn vhd_virtual_size() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // Create a ~1 MiB Fixed VHD. qemu-img aligns the virtual size to CHS
    // geometry boundaries, so the actual size may be slightly larger than
    // 1M. We verify that:
    //   1. `disk.img` exists in the tree.
    //   2. `size` and `file_length` agree.
    //   3. The size is in the range [1 MiB, 2 MiB) — a reasonable sanity bound.
    const ONE_MB: u64 = 1024 * 1024;
    const TWO_MB: u64 = 2 * ONE_MB;

    let bytes = RoundTrip::new("vhd-virtual-size")
        .with(&tools::QEMU_IMG)
        .args([
            "create",
            "-f",
            "vpc",
            "-o",
            "subformat=fixed",
            "$IMAGE",
            "1M",
        ])
        .build_bytes();

    let root = parse_image(&bytes);
    let disk = root
        .children
        .iter()
        .find(|n| n.name == "disk.img")
        .expect("disk.img child should exist in VHD tree");

    assert!(
        disk.size >= ONE_MB && disk.size < TWO_MB,
        "disk.img size {sz} should be in [1 MiB, 2 MiB)",
        sz = disk.size,
    );
    assert_eq!(
        disk.file_length,
        Some(disk.size),
        "disk.img file_length should equal size"
    );
}

// ── Test 4: Fixed VHD file structure ─────────────────────────────────────────

#[test]
fn vhd_file_structure() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    let bytes = RoundTrip::new("vhd-file-structure")
        .with(&tools::QEMU_IMG)
        .args([
            "create",
            "-f",
            "vpc",
            "-o",
            "subformat=fixed",
            "$IMAGE",
            "1M",
        ])
        .build_bytes();

    let root = parse_image(&bytes);

    // Root must be a directory named "/".
    assert_eq!(root.name, "/", "root node should be named \"/\"");
    assert!(root.is_directory, "root should be a directory");
    assert_eq!(
        root.children.len(),
        1,
        "root should have exactly one child (disk.img)"
    );

    let disk = &root.children[0];
    assert_eq!(disk.name, "disk.img", "child should be named disk.img");
    assert!(!disk.is_directory, "disk.img should not be a directory");
    assert!(disk.children.is_empty(), "disk.img should have no children");

    // Fixed VHD: file_location should be Some(0).
    assert_eq!(
        disk.file_location,
        Some(0),
        "fixed VHD disk.img should have file_location=Some(0)"
    );
}

// ── Test 5: Dynamic VHD tree shape ───────────────────────────────────────────

#[test]
fn dynamic_vhd_tree_shape() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    let bytes = RoundTrip::new("vhd-dynamic-shape")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "vpc", "$IMAGE", "10M"])
        .build_bytes();

    let root = parse_image(&bytes);

    assert_eq!(root.name, "/");
    assert!(root.is_directory);
    assert_eq!(root.children.len(), 1);

    let disk = &root.children[0];
    assert_eq!(disk.name, "disk.img");
    assert!(!disk.is_directory);

    // Dynamic VHD: file_location should be None (fragmented blocks).
    assert_eq!(
        disk.file_location, None,
        "dynamic VHD disk.img should have file_location=None"
    );
    assert!(
        disk.file_length.is_some(),
        "dynamic VHD disk.img should have a file_length"
    );
}

// ── Test 6: qemu-img info cross-validation ────────────────────────────────────

#[test]
fn vhd_qemu_img_info_cross_validate() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("vhd-info-xval")
        .with(&tools::QEMU_IMG)
        .args([
            "create",
            "-f",
            "vpc",
            "-o",
            "subformat=fixed",
            "$IMAGE",
            "2M",
        ])
        .build();

    // Ask qemu-img for info on the image. Pass `-f vpc` so qemu-img reads
    // it as VHD regardless of the file extension (the RoundTrip harness
    // names images "image.bin", not "image.vhd").
    // Note: `-f` comes after the subcommand in qemu-img's CLI.
    let info_out = tools::QEMU_IMG
        .run(["info", "-f", "vpc", rt.image_path().to_str().unwrap()])
        .expect("qemu-img info run");

    // qemu-img uses "vpc" as its internal format name for VHD.
    info_out.assert_contains("vpc");

    // Parse the image with isomage.
    let image_bytes = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image_bytes);
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].name, "disk.img");
}
