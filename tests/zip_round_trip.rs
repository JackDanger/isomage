//! Round-trip tests for the ZIP reader in `src/formats/zip.rs`.
//!
//! Archives are built with the system `zip` tool and then parsed by
//! `isomage::formats::zip`. We verify that every file present in the
//! source tree appears in the parsed tree with the correct size, and
//! that stored (uncompressed) files can be read back via `cat_node`.
//!
//! ## Availability
//!
//! `zip` ships on every POSIX system. Tests skip if it is absent
//! unless `ISOMAGE_REQUIRE_TOOLS=1` is set (then they panic — for CI).

mod common;

use std::io::Cursor;

use common::assertions::{assert_path_exists, assert_tree_invariants};
use common::tools;
use common::RoundTrip;

use isomage::formats::zip;

/// Build a ZIP from `sources` (relative path → bytes) with `zip -0` (stored).
/// Returns the parsed `TreeNode` tree, or `None` if `zip` is not available.
fn build_stored_zip(name: &str, sources: &[(&str, &[u8])]) -> Option<isomage::TreeNode> {
    let _ = tools::ZIP.require_or_skip()?;

    let mut rt = RoundTrip::new(name).with(&tools::ZIP);
    for (relpath, data) in sources {
        rt = rt.source_file(relpath, *data);
    }
    // zip -0 -r $IMAGE -j $SRC_DIR/* would flatten; use -r from inside src dir.
    // The harness sets $SRC_DIR and $IMAGE; zip's working dir is set to $SRC_DIR.
    let image_bytes = rt
        .args(["-0", "-r", "$IMAGE", "."])
        .working_dir_is_src()
        .build_bytes();

    let mut c = Cursor::new(&image_bytes);
    Some(zip::detect_and_parse(&mut c).expect("zip::detect_and_parse failed"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn single_stored_file() {
    let root = match build_stored_zip("zip-single", &[("hello.txt", b"Hello, world!")]) {
        Some(t) => t,
        None => return,
    };
    assert_tree_invariants(&root);
    assert_path_exists(&root, "/hello.txt");
    let node = root.find_node("/hello.txt").unwrap();
    assert_eq!(node.size, 13);
    assert!(
        node.file_location.is_some(),
        "stored file must have file_location"
    );
}

#[test]
fn nested_directory_structure() {
    let root = match build_stored_zip(
        "zip-nested",
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
    assert_path_exists(&root, "/docs");
    assert_path_exists(&root, "/docs/readme.txt");
    assert_path_exists(&root, "/docs/api.txt");
    assert_path_exists(&root, "/src/main.rs");
}

#[test]
fn stored_file_readable_via_cat_node() {
    let payload = b"the quick brown fox";
    let image_bytes = {
        let _ = tools::ZIP.require_or_skip();
        let rt = RoundTrip::new("zip-cat")
            .with(&tools::ZIP)
            .source_file("fox.txt", payload)
            .args(["-0", "-r", "$IMAGE", "."])
            .working_dir_is_src();
        rt.build_bytes()
    };
    if image_bytes.is_empty() {
        return; // tool not available
    }

    let mut cursor = Cursor::new(&image_bytes);
    let root = zip::detect_and_parse(&mut cursor).expect("parse failed");

    let node = root
        .find_node("/fox.txt")
        .expect("fox.txt not found in tree");
    assert_eq!(node.size, payload.len() as u64);

    let mut out = Vec::new();
    isomage::cat_node(&mut cursor, node, &mut out).expect("cat_node failed");
    assert_eq!(out, payload);
}

#[test]
fn detect_accepts_valid_zip() {
    let _ = match tools::ZIP.require_or_skip() {
        Some(t) => t,
        None => return,
    };
    let image_bytes = RoundTrip::new("zip-detect")
        .with(&tools::ZIP)
        .source_file("f.txt", b"x")
        .args(["-0", "-r", "$IMAGE", "."])
        .working_dir_is_src()
        .build_bytes();
    if image_bytes.is_empty() {
        return;
    }
    let mut c = Cursor::new(&image_bytes);
    assert!(zip::detect(&mut c).is_ok());
    // Position restored after detect.
    assert_eq!(c.position(), 0);
}

#[test]
fn empty_zip() {
    // zip refuses to create a truly empty archive, so use a single tiny file
    // and verify the reader produces a non-empty tree.
    let root = match build_stored_zip("zip-empty", &[(".keep", b"")]) {
        Some(t) => t,
        None => return,
    };
    assert_tree_invariants(&root);
}

#[test]
fn multiple_files_all_found() {
    let files = [
        ("a.txt", b"aaa" as &[u8]),
        ("b.txt", b"bbbb"),
        ("c.txt", b"ccccc"),
    ];
    let root = match build_stored_zip("zip-multi", &files) {
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
