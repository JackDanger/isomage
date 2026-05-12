//! Parity test: parsing a disc image through a `File` and through an
//! `MmapImage` must yield identical `TreeNode` trees.
//!
//! This is the proof that the v3.0 parser-entry generalization (PR A2)
//! didn't quietly break either path. The test runs only against the
//! checked-in synthetic ISOs in `test_data/` — they're produced by
//! `make test-data` on first build.

#![cfg(feature = "mmap")]

mod common;

use std::fs::File;
use std::path::Path;

use isomage::detect_and_parse_filesystem;
use isomage::image_io::MmapImage;
use isomage::TreeNode;

/// Recursive structural equality: name + size + directory-ness +
/// byte-range. We deliberately don't derive `PartialEq` on `TreeNode`
/// itself (that would invite consumers to depend on field-order
/// stability we'd rather not promise).
fn trees_equal(a: &TreeNode, b: &TreeNode) -> bool {
    if a.name != b.name
        || a.size != b.size
        || a.is_directory != b.is_directory
        || a.file_location != b.file_location
        || a.file_length != b.file_length
    {
        return false;
    }
    if a.children.len() != b.children.len() {
        return false;
    }
    a.children
        .iter()
        .zip(b.children.iter())
        .all(|(ac, bc)| trees_equal(ac, bc))
}

fn parse_via_file(path: &Path) -> TreeNode {
    let mut f = File::open(path).expect("open test ISO");
    detect_and_parse_filesystem(&mut f, &path.to_string_lossy()).expect("parse via File")
}

fn parse_via_mmap(path: &Path) -> TreeNode {
    let mut img = MmapImage::open(path).expect("mmap test ISO");
    detect_and_parse_filesystem(&mut img, &path.to_string_lossy()).expect("parse via MmapImage")
}

#[test]
fn linux_iso_file_and_mmap_agree() {
    let path = Path::new("test_data/test_linux.iso");
    if !path.exists() {
        eprintln!("skip: test_data/test_linux.iso missing — run `make test-data`");
        return;
    }
    let by_file = parse_via_file(path);
    let by_mmap = parse_via_mmap(path);
    assert!(
        trees_equal(&by_file, &by_mmap),
        "tree from File != tree from MmapImage for {:?}",
        path,
    );
}

#[test]
fn macos_iso_file_and_mmap_agree() {
    let path = Path::new("test_data/test_macos.iso");
    if !path.exists() {
        eprintln!("skip: test_data/test_macos.iso missing — run `make test-data`");
        return;
    }
    let by_file = parse_via_file(path);
    let by_mmap = parse_via_mmap(path);
    assert!(
        trees_equal(&by_file, &by_mmap),
        "tree from File != tree from MmapImage for {:?}",
        path,
    );
}

/// Also exercise `cat_node` against an `MmapImage` source — the
/// per-file extract path that downstream consumers care about most.
#[test]
fn cat_node_via_mmap_matches_file() {
    let path = Path::new("test_data/test_linux.iso");
    if !path.exists() {
        eprintln!("skip: test_data/test_linux.iso missing");
        return;
    }

    // Same image, two access paths, one known file inside.
    let tree = parse_via_file(path);
    let hostname = tree
        .find_node("etc/hostname")
        .expect("test_linux.iso has etc/hostname");

    let mut from_file = Vec::new();
    {
        let mut f = File::open(path).unwrap();
        isomage::cat_node(&mut f, hostname, &mut from_file).expect("cat via File");
    }

    let mut from_mmap = Vec::new();
    {
        let mut img = MmapImage::open(path).unwrap();
        isomage::cat_node(&mut img, hostname, &mut from_mmap).expect("cat via MmapImage");
    }

    assert_eq!(from_file, from_mmap);
    assert_eq!(from_file, b"test-linux-system\n");
}
