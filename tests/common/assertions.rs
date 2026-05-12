//! Tree-shape assertions for round-trip tests.
//!
//! These are deliberately small: a few high-leverage predicates that
//! every format test needs, plus their negative counterparts. More
//! exotic checks (CRC byte-equality, deep-walk diffing) belong in
//! the relevant `tests/<format>_round_trip.rs` file — they're
//! format-specific and don't generalize.
//!
//! All assertions take `&TreeNode` and panic on failure with a
//! message that includes the tree shape, so a failing test points
//! at the actual problem rather than `assertion failed: …`.

use std::io::Cursor;

use isomage::TreeNode;

/// Assert there's a child of `root` at `slash_path` and return it.
/// `slash_path` is forward-slash-separated and may start with `/`.
pub fn assert_path_exists<'a>(root: &'a TreeNode, slash_path: &str) -> &'a TreeNode {
    match root.find_node(slash_path) {
        Some(n) => n,
        None => panic!(
            "expected path {slash_path:?} to exist in tree, but it does not.\n\
             Tree top-level children: {:?}",
            root.children.iter().map(|c| &c.name).collect::<Vec<_>>(),
        ),
    }
}

/// Assert no path matches `slash_path`. The inverse of
/// [`assert_path_exists`]; useful for confirming that a deleted /
/// hidden file really is absent.
pub fn assert_path_absent(root: &TreeNode, slash_path: &str) {
    if root.find_node(slash_path).is_some() {
        panic!("expected path {slash_path:?} to NOT exist in tree, but it did");
    }
}

/// Assert that a specific child of `root` (by index) is a file with
/// the byte range `(location, length)`. Reads against the
/// `partition-N-type-XX` style naming used by `formats::mbr` and
/// `formats::gpt`.
pub fn assert_partition_at(
    root: &TreeNode,
    child_index: usize,
    expected_start: u64,
    expected_length: u64,
) {
    let child = root.children.get(child_index).unwrap_or_else(|| {
        panic!(
            "expected partition #{child_index}, but tree only has {} children",
            root.children.len()
        )
    });
    assert!(
        !child.is_directory,
        "expected partition #{child_index} to be a file (leaf), but {:?} is a directory",
        child.name,
    );
    assert_eq!(
        child.file_location,
        Some(expected_start),
        "partition #{child_index} {:?}: expected start {expected_start}, got {:?}",
        child.name,
        child.file_location,
    );
    assert_eq!(
        child.size, expected_length,
        "partition #{child_index} {:?}: expected length {expected_length}, got {}",
        child.name, child.size,
    );
}

/// `cat_node` the file at `slash_path` and assert the bytes equal
/// `expected`. The image data is provided as `&[u8]` so the
/// assertion is decoupled from how the test got the bytes (mmap,
/// File, in-memory, etc.).
///
/// Currently `cat_node` requires `&mut File`, so this helper writes
/// `image` to a tempfile and opens it. When the parser-entry-point
/// generalization PR lands, this helper switches to `Cursor` in
/// place.
pub fn assert_file_contents(image: &[u8], root: &TreeNode, slash_path: &str, expected: &[u8]) {
    use std::fs::File;
    use std::io::Write;

    let dir = tempfile::TempDir::with_prefix("isomage-cat-").expect("tempdir");
    let path = dir.path().join("image.bin");
    {
        let mut f = File::create(&path).expect("create tempfile");
        f.write_all(image).expect("write image");
        f.sync_all().expect("sync");
    }
    let mut f = File::open(&path).expect("reopen tempfile");

    let node = assert_path_exists(root, slash_path);
    let mut got = Vec::with_capacity(expected.len());
    isomage::cat_node(&mut f, node, &mut got).expect("cat_node");
    if got != expected {
        panic!(
            "file {:?}: expected {} bytes, got {} bytes\n\
             expected[..min(64, len)] = {:?}\n\
             got     [..min(64, len)] = {:?}",
            slash_path,
            expected.len(),
            got.len(),
            &expected[..expected.len().min(64)],
            &got[..got.len().min(64)],
        );
    }
}

/// Shape-level sanity check for any parsed tree: root is "/",
/// directory, has children, recursive size is plausible.
pub fn assert_tree_invariants(root: &TreeNode) {
    assert_eq!(root.name, "/", "root must be named '/'");
    assert!(root.is_directory, "root must be a directory");
    let total: u64 = walk_size(root);
    assert_eq!(
        total, root.size,
        "root.size ({}) should equal sum of all descendant file sizes ({})",
        root.size, total,
    );
}

fn walk_size(node: &TreeNode) -> u64 {
    if node.is_directory {
        node.children.iter().map(walk_size).sum()
    } else {
        node.size
    }
}

/// Silence unused-import warnings when only some helpers are used.
/// (Per-test-binary unused-warning policy is strict.)
#[allow(dead_code)]
fn _silence_cursor_warning(_c: Cursor<&[u8]>) {}
