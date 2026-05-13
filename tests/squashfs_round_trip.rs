//! Round-trip tests for the SquashFS reader in `src/formats/squashfs.rs`.
//!
//! All images are built with `mksquashfs` using `-noI -noD -noF -noX` so
//! the inode, data, fragment, and extended-attribute tables are stored
//! uncompressed. That matches the only mode our zero-dep reader supports.
//!
//! ## Availability
//!
//! `mksquashfs` ships in `squashfs-tools` on Debian/Ubuntu and in the
//! `squashfs` Homebrew tap on macOS. Tests skip if the tool isn't found
//! unless `ISOMAGE_REQUIRE_TOOLS=1` is set (then they panic — for CI).

mod common;

use std::io::Cursor;

use common::assertions::{assert_path_exists, assert_tree_invariants};
use common::tools;
use common::RoundTrip;

use isomage::formats::squashfs;

/// Flags passed to every mksquashfs invocation so all tables stay
/// uncompressed. `-noappend` prevents appending to an existing image.
const NOCOMPRESS_FLAGS: &[&str] = &["-noI", "-noD", "-noF", "-noX", "-noappend"];

/// Helper: build a SquashFS image from a populated `$SRC_DIR` and parse it.
fn build_and_parse(name: &str, sources: &[(&str, &[u8])]) -> Option<isomage::TreeNode> {
    let Some(_) = tools::MKSQUASHFS.require_or_skip() else {
        return None;
    };

    let mut rt = RoundTrip::new(name).with(&tools::MKSQUASHFS);
    for (relpath, data) in sources {
        rt = rt.source_file(*relpath, *data);
    }
    // mksquashfs <src_dir> <image> [flags]
    let mut args: Vec<&str> = vec!["$SRC_DIR", "$IMAGE"];
    args.extend_from_slice(NOCOMPRESS_FLAGS);
    let image_bytes = rt.args(args).build_bytes();

    let mut c = Cursor::new(&image_bytes);
    Some(squashfs::detect_and_parse(&mut c).expect("detect_and_parse failed"))
}

/// An empty source directory produces a valid SquashFS with just a root
/// directory node (and no children other than what mksquashfs adds by default).
#[test]
fn empty_squashfs() {
    let Some(_) = tools::MKSQUASHFS.require_or_skip() else {
        return;
    };

    let image_bytes = RoundTrip::new("sqfs-empty")
        .with(&tools::MKSQUASHFS)
        // A dummy file so mksquashfs has something; an entirely empty dir is
        // rejected by some versions of mksquashfs.
        .source_file(".keep", b"")
        .args([
            "$SRC_DIR",
            "$IMAGE",
            "-noI",
            "-noD",
            "-noF",
            "-noX",
            "-noappend",
        ])
        .build_bytes();

    let mut c = Cursor::new(&image_bytes);
    let tree = squashfs::detect_and_parse(&mut c).expect("parse");
    assert_tree_invariants(&tree);
    assert_eq!(tree.name, "/");
    assert!(tree.is_directory);
}

/// Single file in the root. Verifies name, size, and location fields.
#[test]
fn squashfs_single_file() {
    let content = b"hello from squashfs";
    let Some(tree) = build_and_parse("sqfs-single-file", &[("hello.txt", content)]) else {
        return;
    };

    assert_tree_invariants(&tree);
    let node = assert_path_exists(&tree, "hello.txt");
    assert!(!node.is_directory);
    assert_eq!(
        node.size,
        content.len() as u64,
        "file size should match content length"
    );
}

/// Two levels of nested directories.
#[test]
fn squashfs_nested_dirs() {
    let sources: &[(&str, &[u8])] = &[
        ("a/b/deep.txt", b"deep content"),
        ("a/shallow.txt", b"shallow"),
        ("top.txt", b"top level"),
    ];
    let Some(tree) = build_and_parse("sqfs-nested-dirs", sources) else {
        return;
    };

    assert_tree_invariants(&tree);
    assert_path_exists(&tree, "top.txt");
    assert_path_exists(&tree, "a/shallow.txt");
    assert_path_exists(&tree, "a/b/deep.txt");

    let deep = assert_path_exists(&tree, "a/b/deep.txt");
    assert_eq!(deep.size, b"deep content".len() as u64);
}

/// 30+ files in one directory to exercise multi-header directory listings.
#[test]
fn squashfs_many_files() {
    let mut sources: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..35 {
        let name = format!("file_{i:03}.txt");
        let data = format!("content of file {i}").into_bytes();
        sources.push((name, data));
    }
    let source_refs: Vec<(&str, &[u8])> = sources
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();

    let Some(tree) = build_and_parse("sqfs-many-files", &source_refs) else {
        return;
    };

    assert_tree_invariants(&tree);
    for (name, _) in &sources {
        assert_path_exists(&tree, name.as_str());
    }

    assert!(
        tree.children.len() >= 35,
        "expected at least 35 file children, got {}",
        tree.children.len()
    );
}

/// Read back the exact bytes of a file via `file_location` and verify
/// they match what was written.
#[test]
fn squashfs_file_contents_exact() {
    let Some(_) = tools::MKSQUASHFS.require_or_skip() else {
        return;
    };

    let content: &[u8] = b"exact byte verification content 12345";

    let rt = RoundTrip::new("sqfs-file-contents")
        .with(&tools::MKSQUASHFS)
        .source_file("verify.txt", content)
        .args([
            "$SRC_DIR",
            "$IMAGE",
            "-noI",
            "-noD",
            "-noF",
            "-noX",
            "-noappend",
        ]);
    let output = rt.build();
    let image_bytes = output.bytes();

    let mut c = Cursor::new(image_bytes);
    let tree = squashfs::detect_and_parse(&mut c).expect("parse");

    let node = assert_path_exists(&tree, "verify.txt");
    assert_eq!(node.size, content.len() as u64);

    // If file_location is set, read and verify the raw bytes from the image.
    if let Some(loc) = node.file_location {
        let end = loc as usize + content.len();
        assert!(
            end <= image_bytes.len(),
            "file_location {loc} + size {} extends past image ({} bytes)",
            content.len(),
            image_bytes.len()
        );
        let on_disk = &image_bytes[loc as usize..end];
        assert_eq!(
            on_disk, content,
            "bytes at file_location do not match original content"
        );
    }
}

/// File names containing non-ASCII Unicode characters.
#[test]
fn squashfs_unicode_filenames() {
    let sources: &[(&str, &[u8])] = &[
        ("café.txt", b"cafe content"),
        ("日本語.txt", b"japanese filename"),
        ("emoji_\u{1F389}.txt", b"party time"),
    ];
    let Some(tree) = build_and_parse("sqfs-unicode-filenames", sources) else {
        return;
    };

    assert_tree_invariants(&tree);
    assert_path_exists(&tree, "café.txt");
    assert_path_exists(&tree, "日本語.txt");
}
