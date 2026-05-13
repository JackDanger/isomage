//! Round-trip tests for the FAT12/16/32 reader in `src/formats/fat.rs`.
//!
//! Creates FAT images with `mkfs.fat` / `mkfs.vfat`, optionally populates
//! them with files via `mcopy` (mtools), then parses with `isomage` and
//! verifies the [`TreeNode`] tree matches expectations.
//!
//! ## Availability
//!
//! `mkfs.fat` ships in `dosfstools` (Linux) and `brew install dosfstools`
//! (macOS). `mcopy` ships in `mtools`. Tests skip cleanly when either is
//! absent; set `ISOMAGE_REQUIRE_TOOLS=1` to turn skips into panics in CI.

mod common;

use std::io::{Cursor, Read, Seek, SeekFrom};

use common::tools;
use common::Tool;

use isomage::formats::fat;

/// `mcopy` from the mtools suite. Not in the shared registry because it's
/// only needed by FAT tests; declared here to keep the registry compact.
const MCOPY: Tool = Tool::new("mcopy");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a zero-filled file at `path` of exactly `size` bytes.
fn preallocate(path: &std::path::Path, size: u64) {
    let f = std::fs::File::create(path).unwrap();
    f.set_len(size).unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Format a 16 MiB FAT32 image with `mkfs.fat -F 32` and parse it.
/// A freshly-formatted image has no user files — the root must be empty.
#[test]
fn empty_fat32() {
    let Some(_) = tools::MKFS_VFAT.require_or_skip() else {
        return;
    };

    let dir = tempfile::TempDir::new().unwrap();
    let img = dir.path().join("fat32.img");
    preallocate(&img, 16 * 1024 * 1024);

    tools::MKFS_VFAT
        .run(["-F", "32", "-n", "TESTDISK", img.to_str().unwrap()])
        .expect("mkfs.fat invocation failed")
        .assert_success();

    let bytes = std::fs::read(&img).unwrap();
    let tree = fat::detect_and_parse(&mut Cursor::new(&bytes)).expect("FAT32 parse");

    assert_eq!(tree.name, "/");
    assert!(tree.is_directory);
    assert_eq!(
        tree.children.len(),
        0,
        "freshly-formatted FAT32 image should have no user files"
    );
}

/// Format a 4 MiB FAT16 image (forced with `-F 16`) and parse it.
/// Verifies that `detect` returns `true` and `detect_and_parse` succeeds.
#[test]
fn empty_fat16() {
    let Some(_) = tools::MKFS_VFAT.require_or_skip() else {
        return;
    };

    let dir = tempfile::TempDir::new().unwrap();
    let img = dir.path().join("fat16.img");
    // 32 MiB: ensures enough clusters (≥ 4086) for FAT16 regardless of the
    // cluster size mkfs.fat picks for the given image size.
    preallocate(&img, 32 * 1024 * 1024);

    tools::MKFS_VFAT
        .run(["-F", "16", img.to_str().unwrap()])
        .expect("mkfs.fat invocation failed")
        .assert_success();

    let bytes = std::fs::read(&img).unwrap();
    assert!(
        fat::detect(&mut Cursor::new(&bytes)),
        "FAT16 image must be detected"
    );

    let tree = fat::detect_and_parse(&mut Cursor::new(&bytes)).expect("FAT16 parse");
    assert_eq!(tree.name, "/");
    assert!(tree.is_directory);
}

/// Format FAT32, copy one file in with `mcopy`, parse and verify tree + data.
///
/// Requires both `mkfs.fat` and `mcopy` (mtools). Skips if either is absent.
#[test]
fn fat32_single_file() {
    let Some(_) = tools::MKFS_VFAT.require_or_skip() else {
        return;
    };
    let Some(_) = MCOPY.require_or_skip() else {
        return;
    };

    let content = b"Hello from the FAT32 round-trip test!\n";
    let dir = tempfile::TempDir::new().unwrap();
    let img = dir.path().join("fat32.img");
    let src = dir.path().join("hello.txt");

    preallocate(&img, 16 * 1024 * 1024);
    tools::MKFS_VFAT
        .run(["-F", "32", img.to_str().unwrap()])
        .expect("mkfs.fat invocation failed")
        .assert_success();

    std::fs::write(&src, content).unwrap();

    // `mcopy -i image.img src ::/DEST` copies src into the image's root.
    MCOPY
        .run([
            "-i",
            img.to_str().unwrap(),
            src.to_str().unwrap(),
            "::/HELLO.TXT",
        ])
        .expect("mcopy invocation failed")
        .assert_success();

    let bytes = std::fs::read(&img).unwrap();
    let mut cursor = Cursor::new(&bytes);
    let tree = fat::detect_and_parse(&mut cursor).expect("FAT32 parse with one file");

    assert_eq!(tree.children.len(), 1, "expected exactly one file at root");

    let node = &tree.children[0];
    // mtools may keep the 8.3 name in uppercase; accept both.
    assert_eq!(
        node.name.to_ascii_uppercase(),
        "HELLO.TXT",
        "unexpected file name: {}",
        node.name
    );
    assert_eq!(node.size, content.len() as u64, "file size mismatch");
    assert!(!node.is_directory);

    // If clusters are contiguous (typical for a freshly-written image),
    // verify the bytes are accessible at file_location.
    if let Some(loc) = node.file_location {
        cursor.seek(SeekFrom::Start(loc)).unwrap();
        let len = node.file_length.unwrap() as usize;
        let mut buf = vec![0u8; len];
        cursor.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, content, "file contents mismatch via file_location");
    }
}

/// Copy multiple files including one inside a subdirectory; verify the
/// resulting TreeNode hierarchy.
#[test]
fn fat32_nested_directory() {
    let Some(_) = tools::MKFS_VFAT.require_or_skip() else {
        return;
    };
    let Some(_) = MCOPY.require_or_skip() else {
        return;
    };

    let dir = tempfile::TempDir::new().unwrap();
    let img = dir.path().join("fat32.img");

    preallocate(&img, 16 * 1024 * 1024);
    tools::MKFS_VFAT
        .run(["-F", "32", img.to_str().unwrap()])
        .expect("mkfs.fat invocation failed")
        .assert_success();

    // Create source tree: root/file.txt and root/subdir/child.txt.
    let src_root = dir.path().join("src");
    std::fs::create_dir_all(src_root.join("subdir")).unwrap();
    std::fs::write(src_root.join("file.txt"), b"top-level file\n").unwrap();
    std::fs::write(src_root.join("subdir").join("child.txt"), b"nested file\n").unwrap();

    // Copy the top-level file and the subdirectory separately. Using
    // `src/.` as a source makes mcopy try to create a `.` entry, which FAT
    // forbids. Instead, list each item explicitly; `-s` recurses into dirs.
    MCOPY
        .run([
            "-s",
            "-i",
            img.to_str().unwrap(),
            src_root.join("file.txt").to_str().unwrap(),
            src_root.join("subdir").to_str().unwrap(),
            "::/",
        ])
        .expect("mcopy invocation failed")
        .assert_success();

    let bytes = std::fs::read(&img).unwrap();
    let tree = fat::detect_and_parse(&mut Cursor::new(&bytes)).expect("FAT32 nested parse");

    // Find the file at root level (name is case-insensitive).
    let file_node = tree
        .children
        .iter()
        .find(|n| !n.is_directory && n.name.eq_ignore_ascii_case("FILE.TXT"));
    assert!(file_node.is_some(), "FILE.TXT must appear at root");

    // Find the subdirectory.
    let sub_node = tree
        .children
        .iter()
        .find(|n| n.is_directory && n.name.eq_ignore_ascii_case("SUBDIR"));
    assert!(sub_node.is_some(), "SUBDIR directory must appear at root");

    // The subdirectory must contain child.txt.
    let sub = sub_node.unwrap();
    let child = sub
        .children
        .iter()
        .find(|n| n.name.eq_ignore_ascii_case("CHILD.TXT"));
    assert!(child.is_some(), "CHILD.TXT must appear inside SUBDIR");
}
