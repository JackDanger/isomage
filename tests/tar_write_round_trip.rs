//! Write round-trip tests for `tar::write` (`tar` + `write` features).
//!
//! Each test:
//! 1. Calls `tar::write` to produce bytes in memory.
//! 2. Verifies the bytes with the system `tar -tvf` tool (skip if absent).
//! 3. Parses them back with `tar::detect_and_parse` and checks the tree.

mod common;

use std::io::Cursor;

use common::tools;

use isomage::formats::tar;

fn write_and_verify(name: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    tar::write(&mut buf, entries).expect("tar::write failed");

    // Verify with system tar when available.
    if tools::TAR.require_or_skip().is_some() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), &buf).expect("write temp tar");
        // `tar -tvf` lists the archive — success means the archive is valid.
        let out = std::process::Command::new("tar")
            .arg("-tvf")
            .arg(tmp.path())
            .output()
            .expect("spawn tar");
        assert!(
            out.status.success(),
            "tar -tvf rejected the TAR written by isomage ({})\nstdout: {}\nstderr: {}",
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
    let root = tar::detect_and_parse(&mut c).expect("parse failed");
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
    let root = tar::detect_and_parse(&mut c).expect("parse failed");

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
    let root = tar::detect_and_parse(&mut c).expect("parse failed");
    assert!(root.find_node("/docs/readme.txt").is_some());
    assert!(root.find_node("/src/main.rs").is_some());
}

#[test]
fn write_empty_file() {
    let entries = [("empty.bin", b"" as &[u8])];
    let buf = write_and_verify("empty-file", &entries);

    let mut c = Cursor::new(&buf);
    let root = tar::detect_and_parse(&mut c).expect("parse failed");
    let node = root.find_node("/empty.bin").expect("empty.bin missing");
    assert_eq!(node.size, 0);
}

#[test]
fn write_produces_valid_magic() {
    let mut buf = Vec::new();
    tar::write(&mut buf, &[("f.txt", b"x")]).expect("write failed");
    // ustar magic at offset 257
    assert_eq!(&buf[257..262], b"ustar");
}
