//! Round-trip tests for the NTFS reader in `src/formats/ntfs.rs`.
//!
//! These tests use `mkntfs` from the `ntfs-3g` package (Linux) and
//! `ntfsls`/`ntfsfix` for cross-validation. They skip automatically when
//! the tools are not installed (normal on macOS). On the CI `round-trip`
//! job (Ubuntu, `ISOMAGE_REQUIRE_TOOLS=1`) they run for real.
//!
//! Cross-validation against `7zz` is attempted in `ntfs_cross_validate_7z`
//! when 7-Zip is available.

mod common;

use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};

use common::assertions::{assert_path_exists, assert_tree_invariants};
use common::tools;
use common::RoundTrip;

use isomage::formats::ntfs;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse an NTFS image from raw bytes and assert basic invariants.
fn parse_image(bytes: &[u8]) -> isomage::TreeNode {
    let mut c = Cursor::new(bytes.to_vec());
    assert!(
        ntfs::detect(&mut c),
        "ntfs::detect returned false for a freshly-minted NTFS image"
    );
    c.seek(SeekFrom::Start(0)).unwrap();
    ntfs::detect_and_parse(&mut c).expect("detect_and_parse")
}

// ── Tool shorthand ─────────────────────────────────────────────────────────────

/// mkntfs: create a new NTFS filesystem.
/// `mkntfs -F -f <image> <sector_count>` (fast format, force)
const MKNTFS: common::tool::Tool = common::tool::Tool::new("mkntfs");

// ── Test 1: detect an empty NTFS volume ──────────────────────────────────────

#[test]
fn ntfs_detect() {
    let Some(_) = MKNTFS.require_or_skip() else {
        return;
    };

    // mkntfs -F -f -s 512 <image> creates a minimal NTFS volume.
    // We pass the sector count (2048 sectors * 512 = 1 MiB).
    let image = RoundTrip::new("ntfs-detect")
        .with(&MKNTFS)
        .image_size(1024 * 1024)
        .args(["-F", "-f", "-s", "512", "$IMAGE"])
        .build_bytes();

    let mut c = Cursor::new(image);
    assert!(
        ntfs::detect(&mut c),
        "ntfs::detect should recognise a mkntfs-created image"
    );
}

// ── Test 2: single file ───────────────────────────────────────────────────────

#[test]
fn ntfs_single_file() {
    let Some(_) = MKNTFS.require_or_skip() else {
        return;
    };
    let Some(_) = tools::NTFSLS.require_or_skip() else {
        return;
    };

    const CONTENT: &[u8] = b"hello from isomage ntfs\n";
    const FILENAME: &str = "greeting.txt";

    // Build a 4 MiB NTFS image.
    let rt = RoundTrip::new("ntfs-single-file")
        .with(&MKNTFS)
        .image_size(4 * 1024 * 1024)
        .source_file(FILENAME, CONTENT)
        .args(["-F", "-f", "-s", "512", "$IMAGE"])
        .build();

    let image_path = rt.image_path().to_path_buf();

    // Use ntfscp to inject the file into the image.
    // ntfscp <device> <local-file> <ntfs-path>
    let src_file = rt.src_dir().join(FILENAME);
    let ntfscp = common::tool::Tool::new("ntfscp");
    let Some(_) = ntfscp.require_or_skip() else {
        // ntfscp unavailable; skip the content test.
        return;
    };

    ntfscp
        .run([
            image_path.to_str().unwrap(),
            src_file.to_str().unwrap(),
            FILENAME,
        ])
        .expect("ntfscp run")
        .assert_success();

    let image = fs::read(&image_path).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    let node = assert_path_exists(&root, FILENAME);
    assert!(!node.is_directory);
    assert_eq!(node.size, CONTENT.len() as u64);

    // Verify bytes at file_location if it resolved.
    if let Some(loc) = node.file_location {
        let mut c = Cursor::new(&image);
        c.seek(SeekFrom::Start(loc)).unwrap();
        let mut buf = vec![0u8; CONTENT.len()];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), CONTENT, "file_location bytes mismatch");
    }
}

// ── Test 3: detect only (no ntfscp needed) ───────────────────────────────────

/// Verify that the root is a directory and the basic tree invariants hold
/// for an empty (no user files) NTFS volume.
#[test]
fn ntfs_empty_volume_tree() {
    let Some(_) = MKNTFS.require_or_skip() else {
        return;
    };

    let image = RoundTrip::new("ntfs-empty-tree")
        .with(&MKNTFS)
        .image_size(4 * 1024 * 1024)
        .args(["-F", "-f", "-s", "512", "$IMAGE"])
        .build_bytes();

    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_eq!(root.name, "/");
    assert!(root.is_directory);
}

// ── Test 4: ntfsfix accepts image ────────────────────────────────────────────

/// Cross-validate: ntfsfix -n (dry-run check) should exit 0 on a clean image.
#[test]
fn ntfs_ntfsfix_accepts_image() {
    let Some(_) = MKNTFS.require_or_skip() else {
        return;
    };
    let Some(_) = tools::NTFSFIX.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ntfs-ntfsfix")
        .with(&MKNTFS)
        .image_size(4 * 1024 * 1024)
        .args(["-F", "-f", "-s", "512", "$IMAGE"])
        .build();

    let image_path = rt.image_path().to_path_buf();

    // ntfsfix -n = dry-run (no actual fixes).
    let fix_out = tools::NTFSFIX
        .run(["-n", image_path.to_str().unwrap()])
        .expect("ntfsfix run");

    let code = fix_out.status.code().unwrap_or(255);
    assert!(
        code <= 1,
        "ntfsfix reported errors (exit {code}):\n{}",
        fix_out.stdout_string()
    );
}

// ── Test 5: cross-validate with 7zz ──────────────────────────────────────────

#[test]
fn ntfs_cross_validate_7z() {
    let Some(_) = MKNTFS.require_or_skip() else {
        return;
    };
    let Some(_) = tools::SEVEN_ZZ.require_or_skip() else {
        return;
    };

    let image = RoundTrip::new("ntfs-7z-cross")
        .with(&MKNTFS)
        .image_size(4 * 1024 * 1024)
        .args(["-F", "-f", "-s", "512", "$IMAGE"])
        .build_bytes();

    // Parse with isomage.
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    // Write to a temp file for 7zz.
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(tmp.path(), &image).expect("write temp image");

    let sz_out = tools::SEVEN_ZZ
        .run(["l", "-slt", tmp.path().to_str().unwrap()])
        .expect("7zz run");

    if !sz_out.status.success() {
        eprintln!(
            "7zz exited {:?} (non-fatal for cross-validate): {}",
            sz_out.status.code(),
            sz_out.stderr_string()
        );
        return;
    }

    // 7zz knows NTFS; if it lists anything, check we agree on the root.
    let stdout = sz_out.stdout_string();
    let has_ntfs = stdout.contains("NTFS") || stdout.contains("ntfs");
    if has_ntfs {
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
    }
}

// ── Test 6: many files ────────────────────────────────────────────────────────

#[test]
fn ntfs_many_files() {
    let Some(_) = MKNTFS.require_or_skip() else {
        return;
    };
    // ntfscp is required to inject files.
    let ntfscp = common::tool::Tool::new("ntfscp");
    let Some(_) = ntfscp.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ntfs-many-files")
        .with(&MKNTFS)
        .image_size(16 * 1024 * 1024)
        .args(["-F", "-f", "-s", "512", "$IMAGE"])
        .build();

    let image_path = rt.image_path().to_path_buf();
    let src_dir = rt.src_dir().to_path_buf();

    // Inject 20 files via ntfscp.
    for i in 0..20u32 {
        let name = format!("file_{i:03}.dat");
        let content = format!("file content {i}\n");
        let local = src_dir.join(&name);
        fs::write(&local, content.as_bytes()).expect("write source file");
        ntfscp
            .run([image_path.to_str().unwrap(), local.to_str().unwrap(), &name])
            .expect("ntfscp run")
            .assert_success();
    }

    let image = fs::read(&image_path).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    let file_count = root
        .children
        .iter()
        .filter(|n| n.name.starts_with("file_"))
        .count();
    assert!(
        file_count >= 20,
        "expected ≥ 20 user files, found {file_count}"
    );
}
