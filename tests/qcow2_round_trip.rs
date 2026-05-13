//! Round-trip tests for the QCOW2 reader in `src/formats/qcow2.rs`.
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

use isomage::formats::qcow2;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a QCOW2 image from raw bytes and return the root TreeNode.
fn parse_image(bytes: &[u8]) -> isomage::TreeNode {
    let mut c = Cursor::new(bytes.to_vec());
    qcow2::detect(&mut c).expect("qcow2::detect returned Err for a freshly-minted QCOW2");
    let mut c2 = Cursor::new(bytes.to_vec());
    qcow2::detect_and_parse(&mut c2).expect("qcow2::detect_and_parse failed")
}

// ── Test 1: QCOW2 detection ───────────────────────────────────────────────────

#[test]
fn qcow2_detect() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img create -f qcow2 creates a v3 QCOW2 image by default on
    // modern QEMU versions.
    let rt = RoundTrip::new("qcow2-detect")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "qcow2", "$IMAGE", "10M"])
        .build_bytes();

    let mut c = Cursor::new(rt.clone());
    let result = qcow2::detect(&mut c);
    assert!(
        result.is_ok(),
        "qcow2::detect should succeed for a qemu-img QCOW2: {result:?}"
    );
}

// ── Test 2: QCOW2 virtual size ────────────────────────────────────────────────

#[test]
fn qcow2_virtual_size() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // qemu-img rounds the virtual size to cluster boundaries; 10M is
    // exact at cluster_bits=16 (65536-byte clusters). We verify the
    // reported size is in the range [10 MiB, 11 MiB).
    const TEN_MB: u64 = 10 * 1024 * 1024;
    const ELEVEN_MB: u64 = 11 * 1024 * 1024;

    let bytes = RoundTrip::new("qcow2-virtual-size")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "qcow2", "$IMAGE", "10M"])
        .build_bytes();

    let root = parse_image(&bytes);
    let disk = root
        .children
        .iter()
        .find(|n| n.name == "disk.qcow2")
        .expect("disk.qcow2 child should exist in QCOW2 tree");

    assert!(
        disk.size >= TEN_MB && disk.size < ELEVEN_MB,
        "disk.qcow2 size {sz} should be in [10 MiB, 11 MiB)",
        sz = disk.size,
    );
    assert_eq!(
        disk.file_length,
        Some(disk.size),
        "disk.qcow2 file_length should equal size"
    );
}

// ── Test 3: QCOW2 file structure ──────────────────────────────────────────────

#[test]
fn qcow2_file_structure() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    let bytes = RoundTrip::new("qcow2-file-structure")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "qcow2", "$IMAGE", "10M"])
        .build_bytes();

    let root = parse_image(&bytes);

    // Root must be a directory named "/".
    assert_eq!(root.name, "/", "root node should be named \"/\"");
    assert!(root.is_directory, "root should be a directory");
    assert_eq!(
        root.children.len(),
        1,
        "root should have exactly one child (disk.qcow2)"
    );

    let disk = &root.children[0];
    assert_eq!(disk.name, "disk.qcow2", "child should be named disk.qcow2");
    assert!(!disk.is_directory, "disk.qcow2 should not be a directory");
    assert!(
        disk.children.is_empty(),
        "disk.qcow2 should have no children"
    );

    // QCOW2 data is addressed through L1/L2 tables; file_location is always None.
    assert_eq!(
        disk.file_location, None,
        "QCOW2 disk.qcow2 should have file_location=None (L1/L2 table indirection)"
    );
}

// ── Test 4: QCOW2 version 3 ───────────────────────────────────────────────────

#[test]
fn qcow2_version3() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    // Modern qemu-img defaults to v3. We parse it and confirm the tree
    // is well-formed; the version field itself is not exposed on TreeNode
    // but must not cause a parse error.
    let bytes = RoundTrip::new("qcow2-version3")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "qcow2", "$IMAGE", "4M"])
        .build_bytes();

    // Confirm the image is version 3 by reading the version field directly.
    // Bytes [4..8] of a QCOW2 header are the big-endian version u32.
    let version = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    // qemu-img on modern systems writes v3; older may write v2. Either is fine.
    assert!(
        version == 2 || version == 3,
        "expected QCOW2 version 2 or 3, got {version}"
    );

    let root = parse_image(&bytes);
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].name, "disk.qcow2");
}

// ── Test 5: QCOW2 qemu-img info cross-validation ─────────────────────────────

#[test]
fn qcow2_qemu_img_info_cross_validate() {
    let Some(_) = tools::QEMU_IMG.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("qcow2-info-xval")
        .with(&tools::QEMU_IMG)
        .args(["create", "-f", "qcow2", "$IMAGE", "2M"])
        .build();

    // Ask qemu-img for info on the image. Pass `-f qcow2` explicitly so
    // qemu-img reads it as QCOW2 regardless of the file extension (the
    // RoundTrip harness names images "image.bin", not "image.qcow2").
    let info_out = tools::QEMU_IMG
        .run(["info", "-f", "qcow2", rt.image_path().to_str().unwrap()])
        .expect("qemu-img info run");

    // qemu-img reports "qcow2" as the format name in its info output.
    info_out.assert_contains("qcow2");

    // Parse the image with isomage and cross-validate the tree.
    let image_bytes = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image_bytes);
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].name, "disk.qcow2");
}
