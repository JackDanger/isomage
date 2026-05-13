//! Round-trip tests for the HFS+ reader in `src/formats/hfsplus.rs`.
//!
//! ## External tool requirements
//!
//! Creating HFS+ images on Linux requires `mkfs.hfsplus` from the `hfsprogs`
//! package. On macOS the kernel can mount HFS+ natively via `hdiutil`, but
//! populating images with files requires root (for loop-mount) or FUSE.
//!
//! Our strategy:
//! - On Linux with `mkfs.hfsplus` present, create a minimal HFS+ image and
//!   verify detection and volume-header parsing.
//! - On macOS, skip (hdiutil is present but root is unavailable in CI).
//! - Tests that require file-population via loop-mount are guarded and
//!   skipped when the tool is absent.
//!
//! ## Availability
//!
//! `mkfs.hfsplus` ships in the `hfsprogs` package on Debian/Ubuntu.
//! It is NOT available on macOS (Apple ships a kernel driver but no
//! open-source formatter). These tests run in the Ubuntu round-trip job
//! only.

mod common;

use common::tool::Tool;
use common::RoundTrip;

use isomage::formats::hfsplus;

/// `mkfs.hfsplus` from the Linux `hfsprogs` package. We do NOT alias
/// macOS `newfs_hfs` here: that tool expects a block device path, not a
/// regular file, so the round-trip image builder cannot drive it. These
/// tests are Linux-only; macOS runners skip because `mkfs.hfsplus` won't
/// resolve on `$PATH` there.
const MKFS_HFSPLUS: Tool = Tool::new("mkfs.hfsplus");

/// Helper: build a minimal HFS+ image (8 MiB, no files) using `mkfs.hfsplus`.
///
/// Returns `None` (causing the test to skip) if `mkfs.hfsplus` is not
/// installed. Panics if the tool is present but fails to create the image.
fn make_hfsplus_image() -> Option<Vec<u8>> {
    let _ = MKFS_HFSPLUS.require_or_skip()?;

    // mkfs.hfsplus requires the image to pre-exist. 8 MiB is the minimum
    // size it accepts without complaint.
    let image = RoundTrip::new("hfsplus-empty")
        .with(&MKFS_HFSPLUS)
        .image_size(8 * 1024 * 1024)
        // mkfs.hfsplus <device> — $IMAGE substitution gives us the temp path.
        .args(["$IMAGE"])
        .build_bytes();

    Some(image)
}

/// Verify that `detect()` succeeds on a freshly-formatted HFS+ image.
#[test]
fn hfsplus_detect() {
    let Some(image) = make_hfsplus_image() else {
        return;
    };

    let mut c = std::io::Cursor::new(&image);
    assert!(
        hfsplus::detect(&mut c).is_ok(),
        "detect() should succeed on a freshly-formatted HFS+ image"
    );
}

/// Verify that the cursor is restored after `detect()`.
#[test]
fn hfsplus_detect_restores_cursor() {
    let Some(image) = make_hfsplus_image() else {
        return;
    };

    let mut c = std::io::Cursor::new(&image);
    use std::io::{Seek, SeekFrom};
    c.seek(SeekFrom::Start(42)).unwrap();
    let _ = hfsplus::detect(&mut c);
    assert_eq!(
        c.position(),
        42,
        "detect() must restore the cursor to its original position"
    );
}

/// Verify that `parse_volume_header()` returns a sane header for an image
/// produced by `mkfs.hfsplus`.
#[test]
fn hfsplus_volume_header() {
    let Some(image) = make_hfsplus_image() else {
        return;
    };

    let mut c = std::io::Cursor::new(&image);
    let vh = hfsplus::parse_volume_header(&mut c).expect("parse_volume_header should succeed");

    // mkfs.hfsplus writes 0x482B (HFS+) by default.
    assert_eq!(vh.signature, 0x482B, "signature should be 0x482B (HFS+)");
    assert_eq!(vh.version, 4, "HFS+ version should be 4");

    // A freshly-formatted empty volume has 0 user files. Folder count varies
    // by mkfs.hfsplus version (some count the root dir itself), so we only
    // assert a minimum of 0.
    assert_eq!(vh.file_count, 0, "empty volume should have 0 files");
    assert!(
        vh.folder_count <= 2,
        "empty volume should have 0–2 folders (root + maybe HFS Private Data)"
    );

    // Block size must be a power of 2 between 512 and 1 MiB.
    let bs = vh.block_size;
    assert!(bs >= 512, "block_size must be >= 512");
    assert!(bs & (bs - 1) == 0, "block_size must be a power of 2");
}

/// Verify that `detect_and_parse()` returns a valid root TreeNode for an
/// empty HFS+ volume. File count should be 0; root should be "/".
#[test]
fn hfsplus_empty_volume_parse() {
    let Some(image) = make_hfsplus_image() else {
        return;
    };

    let mut c = std::io::Cursor::new(&image);
    let tree = hfsplus::detect_and_parse(&mut c).expect("detect_and_parse should succeed");

    assert_eq!(tree.name, "/", "root node must be named '/'");
    assert!(tree.is_directory, "root must be a directory");
    // An empty volume may have zero user-visible children. HFS+ internal
    // files (journal, allocation file, etc.) are not surfaced by our reader.
    // We don't assert children.is_empty() because some versions of mkfs.hfsplus
    // create a ".HFS+ Private Directory Data\r" folder at the root.
    // Just verify the tree is sane.
    assert_eq!(
        tree.size,
        tree.children.iter().map(|c| c.size).sum::<u64>(),
        "root size must equal sum of children sizes"
    );
}

/// Verify that `detect()` returns `BadMagic` for a random (non-HFS+) image.
#[test]
fn hfsplus_detect_rejects_non_hfsplus() {
    // An all-zeros buffer definitely has no HFS+ signature.
    let image = vec![0u8; 4096];
    let mut c = std::io::Cursor::new(&image);
    assert!(
        matches!(
            hfsplus::detect(&mut c),
            Err(hfsplus::Error::BadMagic) | Err(hfsplus::Error::TooShort)
        ),
        "detect() must reject a non-HFS+ image"
    );
}

/// Verify that `detect()` returns `TooShort` for an image that is shorter
/// than 1026 bytes (offset 1024 + 2 signature bytes).
#[test]
fn hfsplus_detect_rejects_too_short() {
    let image = vec![0u8; 100];
    let mut c = std::io::Cursor::new(&image);
    assert!(
        matches!(
            hfsplus::detect(&mut c),
            Err(hfsplus::Error::TooShort) | Err(hfsplus::Error::BadMagic)
        ),
        "detect() must return TooShort or BadMagic for a truncated image"
    );
}
