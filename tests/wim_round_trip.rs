//! Round-trip tests for the WIM reader in `src/formats/wim.rs`.
//!
//! ## External tool requirements
//!
//! Creating WIM files requires `wimlib-imagex` (aliased `imagex`) from
//! the wimlib package.
//!
//! - Linux: `sudo apt-get install wimtools` provides `wimlib-imagex`
//!   and the `wimcapture` / `wimapply` aliases.
//! - macOS: `brew install wimlib` provides `wimlib-imagex`.
//!
//! ## Strategy
//!
//! 1. Use `wimlib-imagex capture $SRC_DIR $IMAGE image_name` to create
//!    a minimal single-image WIM from a source directory.
//! 2. Parse the resulting WIM with `isomage::formats::wim::detect_and_parse`.
//! 3. Assert the resulting TreeNode tree matches expectations.
//!
//! Tests skip automatically when `wimlib-imagex` is not installed.
//! On the CI `round-trip` job (`ISOMAGE_REQUIRE_TOOLS=1`) they run
//! for real on both Ubuntu and macOS.

mod common;

use std::io::Cursor;

use common::tools;
use common::RoundTrip;

use isomage::formats::wim;

// ── Helper ────────────────────────────────────────────────────────────────────

/// Build a minimal single-image WIM from a source directory using
/// `wimlib-imagex capture`. The image is named `image_name`.
///
/// Returns `None` (causing test to skip) when `wimlib-imagex` is not installed.
fn make_single_image_wim(image_name: &str) -> Option<Vec<u8>> {
    let _ = tools::WIMLIB_IMAGEX.require_or_skip()?;

    let bytes = RoundTrip::new(format!("wim-single-{image_name}"))
        .with(&tools::WIMLIB_IMAGEX)
        // Stage a small file so the WIM is non-trivial.
        .source_file("hello.txt", b"hello from isomage" as &[u8])
        // wimlib-imagex capture <source_dir> <dest_wim> [image_name]
        .args(["capture", "$SRC_DIR", "$IMAGE", image_name])
        .build_bytes();

    Some(bytes)
}

// ── Test 1: detect() succeeds on a real WIM ──────────────────────────────────

#[test]
fn wim_detect() {
    let Some(bytes) = make_single_image_wim("TestImage") else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    assert!(
        wim::detect(&mut c).is_ok(),
        "wim::detect should succeed on a WIM produced by wimlib-imagex"
    );
}

// ── Test 2: detect() restores the cursor position ────────────────────────────

#[test]
fn wim_detect_restores_cursor() {
    let Some(bytes) = make_single_image_wim("TestImage") else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    use std::io::{Seek, SeekFrom};
    c.seek(SeekFrom::Start(42)).unwrap();
    let _ = wim::detect(&mut c);
    assert_eq!(
        c.position(),
        42,
        "wim::detect must restore the cursor to its original position"
    );
}

// ── Test 3: image count is 1 for a single-image WIM ─────────────────────────

#[test]
fn wim_image_count() {
    let Some(bytes) = make_single_image_wim("TestImage") else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    let root =
        wim::detect_and_parse(&mut c).expect("detect_and_parse should succeed on a real WIM");

    assert_eq!(root.name, "/", "root node must be named '/'");
    assert!(root.is_directory, "root must be a directory");
    assert_eq!(
        root.children.len(),
        1,
        "a single-image WIM must have exactly one child"
    );
}

// ── Test 4: image name matches what wimlib-imagex was told ──────────────────

#[test]
fn wim_image_name() {
    let Some(bytes) = make_single_image_wim("MyWindowsImage") else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    let root =
        wim::detect_and_parse(&mut c).expect("detect_and_parse should succeed on a real WIM");

    assert_eq!(
        root.children.len(),
        1,
        "single-image WIM must have exactly one child"
    );
    assert_eq!(
        root.children[0].name, "MyWindowsImage",
        "image name must match the name passed to wimlib-imagex capture"
    );
    assert!(
        root.children[0].is_directory,
        "each WIM image must be represented as a directory"
    );
}

// ── Test 5: detect() returns BadMagic for a random buffer ────────────────────

#[test]
fn wim_detect_rejects_non_wim() {
    // An all-zeros buffer has no WIM magic.
    let image = vec![0u8; 4096];
    let mut c = Cursor::new(&image);
    assert!(
        matches!(wim::detect(&mut c), Err(wim::Error::BadMagic)),
        "detect() must reject a non-WIM image with BadMagic"
    );
}

// ── Test 6: detect() returns TooShort for a truncated buffer ─────────────────

#[test]
fn wim_detect_rejects_too_short() {
    let image = vec![0u8; 100];
    let mut c = Cursor::new(&image);
    assert!(
        matches!(wim::detect(&mut c), Err(wim::Error::TooShort)),
        "detect() must return TooShort for a buffer shorter than 208 bytes"
    );
}

// ── Test 7: empty source directory produces a WIM with 1 image ──────────────

#[test]
fn wim_empty_image() {
    let Some(_) = tools::WIMLIB_IMAGEX.require_or_skip() else {
        return;
    };

    let bytes = RoundTrip::new("wim-empty-image")
        .with(&tools::WIMLIB_IMAGEX)
        // No source_file: capture an empty directory.
        .args(["capture", "$SRC_DIR", "$IMAGE", "EmptyImage"])
        .build_bytes();

    let mut c = Cursor::new(&bytes);
    let root = wim::detect_and_parse(&mut c)
        .expect("detect_and_parse should succeed on an empty-image WIM");

    assert_eq!(root.children.len(), 1, "must have one image");
    assert_eq!(
        root.children[0].name, "EmptyImage",
        "image name must be EmptyImage"
    );
}

// ── Test 8: multiple images in one WIM file ──────────────────────────────────

#[test]
fn wim_multiple_images() {
    let Some(_) = tools::WIMLIB_IMAGEX.require_or_skip() else {
        return;
    };

    // Create a single-image WIM, then append a second image with
    // `wimlib-imagex append`.
    let first = RoundTrip::new("wim-multi-image")
        .with(&tools::WIMLIB_IMAGEX)
        .source_file("file1.txt", b"image one" as &[u8])
        .args(["capture", "$SRC_DIR", "$IMAGE", "ImageOne"])
        .build();

    // Append a second image into the same WIM file using `append`.
    // We invoke wimlib-imagex directly on the image path.
    let image_path_str = first.image_path().to_str().unwrap().to_string();

    tools::WIMLIB_IMAGEX
        .run([
            "append",
            first.src_dir().to_str().unwrap(),
            &image_path_str,
            "ImageTwo",
        ])
        .expect("wimlib-imagex append invocation")
        .assert_success();

    let bytes = std::fs::read(first.image_path()).expect("read multi-image WIM");

    let mut c = Cursor::new(&bytes);
    let root = wim::detect_and_parse(&mut c)
        .expect("detect_and_parse should succeed on a multi-image WIM");

    assert_eq!(
        root.children.len(),
        2,
        "multi-image WIM must have exactly 2 children"
    );
    assert_eq!(root.children[0].name, "ImageOne");
    assert_eq!(root.children[1].name, "ImageTwo");
}
