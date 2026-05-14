//! Round-trip tests for the TAR reader in `src/formats/tar.rs`.
//!
//! Archives are built with the system `tar` tool and then parsed by
//! `isomage::formats::tar`. We verify that every file present in the
//! source tree appears in the parsed tree with the correct size, and
//! that files can be read back via `cat_node`.
//!
//! ## Availability
//!
//! `tar` ships on every POSIX system. Tests skip if it is absent
//! unless `ISOMAGE_REQUIRE_TOOLS=1` is set (then they panic — for CI).

mod common;

use std::io::Cursor;

use common::assertions::{assert_path_exists, assert_tree_invariants};
use common::tools;
use common::RoundTrip;

use isomage::formats::tar;

/// Build a ustar TAR from `sources` (relative path → bytes).
/// Returns the parsed `TreeNode` tree, or `None` if `tar` is not available.
fn build_tar(name: &str, sources: &[(&str, &[u8])]) -> Option<isomage::TreeNode> {
    let _ = tools::TAR.require_or_skip()?;

    let mut rt = RoundTrip::new(name).with(&tools::TAR);
    for (relpath, data) in sources {
        rt = rt.source_file(relpath, *data);
    }
    // `tar -C $SRC_DIR -cf $IMAGE .` stores relative paths from inside
    // $SRC_DIR. The `-C` flag makes tar change to $SRC_DIR before
    // collecting entries, so stored paths start with `./` (stripped by
    // our parser).
    let image_bytes = rt
        .args(["-C", "$SRC_DIR", "-cf", "$IMAGE", "."])
        .build_bytes();

    let mut c = Cursor::new(&image_bytes);
    Some(tar::detect_and_parse(&mut c).expect("tar::detect_and_parse failed"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn single_file() {
    let root = match build_tar("tar-single", &[("hello.txt", b"Hello, world!")]) {
        Some(t) => t,
        None => return,
    };
    assert_tree_invariants(&root);
    assert_path_exists(&root, "/hello.txt");
    let node = root.find_node("/hello.txt").unwrap();
    assert_eq!(node.size, 13);
    assert!(
        node.file_location.is_some(),
        "regular file must have file_location"
    );
}

#[test]
fn nested_directory_structure() {
    let root = match build_tar(
        "tar-nested",
        &[
            ("docs/readme.txt", b"readme"),
            ("docs/api.txt", b"api docs"),
            ("src/main.rs", b"fn main() {}"),
        ],
    ) {
        Some(t) => t,
        None => return,
    };
    assert_tree_invariants(&root);
    assert_path_exists(&root, "/docs/readme.txt");
    assert_path_exists(&root, "/docs/api.txt");
    assert_path_exists(&root, "/src/main.rs");
}

#[test]
fn file_readable_via_cat_node() {
    let payload = b"the quick brown fox";
    let image_bytes = {
        let _ = tools::TAR.require_or_skip();
        let rt = RoundTrip::new("tar-cat")
            .with(&tools::TAR)
            .source_file("fox.txt", payload)
            .args(["-C", "$SRC_DIR", "-cf", "$IMAGE", "."]);
        rt.build_bytes()
    };
    if image_bytes.is_empty() {
        return; // tool not available
    }

    let mut cursor = Cursor::new(&image_bytes);
    let root = tar::detect_and_parse(&mut cursor).expect("parse failed");

    let node = root
        .find_node("/fox.txt")
        .expect("fox.txt not found in tree");
    assert_eq!(node.size, payload.len() as u64);

    let mut out = Vec::new();
    isomage::cat_node(&mut cursor, node, &mut out).expect("cat_node failed");
    assert_eq!(out, payload);
}

#[test]
fn detect_accepts_valid_tar() {
    let _ = match tools::TAR.require_or_skip() {
        Some(t) => t,
        None => return,
    };
    let image_bytes = RoundTrip::new("tar-detect")
        .with(&tools::TAR)
        .source_file("f.txt", b"x")
        .args(["-C", "$SRC_DIR", "-cf", "$IMAGE", "."])
        .build_bytes();
    if image_bytes.is_empty() {
        return;
    }
    let mut c = Cursor::new(&image_bytes);
    assert!(tar::detect(&mut c).is_ok());
    // Position restored after detect.
    assert_eq!(c.position(), 0);
}

#[test]
fn multiple_files_all_found() {
    let files = [
        ("a.txt", b"aaa" as &[u8]),
        ("b.txt", b"bbbb"),
        ("c.txt", b"ccccc"),
    ];
    let root = match build_tar("tar-multi", &files) {
        Some(t) => t,
        None => return,
    };
    assert_tree_invariants(&root);
    for (name, data) in &files {
        let path = format!("/{}", name);
        assert_path_exists(&root, &path);
        let node = root.find_node(&path).unwrap();
        assert_eq!(node.size, data.len() as u64, "size mismatch for {}", name);
    }
}

#[test]
fn empty_file_in_archive() {
    let root = match build_tar("tar-empty-file", &[("empty.bin", b"")]) {
        Some(t) => t,
        None => return,
    };
    assert_tree_invariants(&root);
    assert_path_exists(&root, "/empty.bin");
    let node = root.find_node("/empty.bin").unwrap();
    assert_eq!(node.size, 0);
}
