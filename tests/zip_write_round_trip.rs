//! Write round-trip tests for `zip::write_stored` (`zip` + `write` features).
//!
//! Each test:
//! 1. Calls `zip::write_stored` to produce bytes in memory.
//! 2. Verifies the bytes with the system `unzip -t` tool (skip if absent).
//! 3. Parses them back with `zip::detect_and_parse` and checks the tree.
//!
//! This validates that isomage can produce ZIP archives that real tools accept,
//! not just archives that isomage itself can read back.

mod common;

use std::io::Cursor;

use common::tools;

use isomage::formats::zip;

fn write_and_verify(name: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    zip::write_stored(&mut buf, entries).expect("write_stored failed");

    // Verify with system unzip when available.
    if tools::UNZIP.require_or_skip().is_some() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), &buf).expect("write temp zip");
        let out = std::process::Command::new("unzip")
            .arg("-t")
            .arg(tmp.path())
            .output()
            .expect("spawn unzip");
        assert!(
            out.status.success(),
            "unzip -t rejected the ZIP written by isomage ({})\nstdout: {}\nstderr: {}",
            name,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    buf
}

#[test]
fn write_single_file() {
    let entries = [("hello.txt", b"Hello, world!" as &[u8])];
    let buf = write_and_verify("single-file", &entries);

    let mut c = Cursor::new(&buf);
    let root = zip::detect_and_parse(&mut c).expect("parse failed");
    assert_eq!(root.children.len(), 1);
    let node = root.find_node("/hello.txt").expect("hello.txt missing");
    assert_eq!(node.size, 13);
    assert!(node.file_location.is_some());

    let mut out = Vec::new();
    isomage::cat_node(&mut c, node, &mut out).expect("cat_node failed");
    assert_eq!(out, b"Hello, world!");
}

#[test]
fn write_multiple_files() {
    let entries = [
        ("a.txt", b"aaa" as &[u8]),
        ("b.txt", b"bbbb"),
        ("c.txt", b"ccccc"),
    ];
    let buf = write_and_verify("multi", &entries);

    let mut c = Cursor::new(&buf);
    let root = zip::detect_and_parse(&mut c).expect("parse failed");

    for (name, data) in &entries {
        let path = format!("/{name}");
        let node = root
            .find_node(&path)
            .unwrap_or_else(|| panic!("{path} missing"));
        assert_eq!(node.size, data.len() as u64);

        let mut out = Vec::new();
        isomage::cat_node(&mut c, node, &mut out).expect("cat_node failed");
        assert_eq!(&out, data);
    }
}

#[test]
fn write_nested_paths() {
    let entries = [
        ("docs/readme.txt", b"readme content" as &[u8]),
        ("src/main.rs", b"fn main() {}"),
    ];
    let buf = write_and_verify("nested", &entries);

    let mut c = Cursor::new(&buf);
    let root = zip::detect_and_parse(&mut c).expect("parse failed");
    assert!(root.find_node("/docs/readme.txt").is_some());
    assert!(root.find_node("/src/main.rs").is_some());
}

#[test]
fn write_empty_file() {
    let entries = [("empty.bin", b"" as &[u8])];
    let buf = write_and_verify("empty-file", &entries);

    let mut c = Cursor::new(&buf);
    let root = zip::detect_and_parse(&mut c).expect("parse failed");
    let node = root.find_node("/empty.bin").expect("empty.bin missing");
    assert_eq!(node.size, 0);
}

#[test]
fn write_crc32_is_correct() {
    // A ZIP with a wrong CRC will be flagged by `unzip -t`. This test checks
    // that our CRC-32 implementation produces the right value for known data.
    assert_eq!(zip::crc32(b""), 0x0000_0000);
    assert_eq!(zip::crc32(b"123456789"), 0xCBF4_3926);
}
