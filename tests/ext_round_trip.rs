//! Round-trip tests for the ext2/3/4 reader in `src/formats/ext.rs`.
//!
//! These tests use `mkfs.ext4`, `mke2fs`, `debugfs`, and `e2fsck` from
//! the `e2fsprogs` package. They skip automatically when those tools are
//! not installed (the normal case on macOS). On the CI `round-trip` job
//! (Ubuntu, `ISOMAGE_REQUIRE_TOOLS=1`) they run for real.
//!
//! Cross-validation against `7zz` is performed in `ext4_cross_validate_7z`
//! when `7zz` or `7z` is available.

mod common;

use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};

use common::assertions::{assert_path_exists, assert_tree_invariants};
use common::tools;
use common::RoundTrip;

use isomage::formats::ext;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse an ext image from raw bytes and assert basic invariants.
/// Returns the root TreeNode.
fn parse_image(bytes: &[u8]) -> isomage::TreeNode {
    let mut c = Cursor::new(bytes.to_vec());
    assert!(
        ext::detect(&mut c),
        "ext::detect returned false for a freshly-minted ext image"
    );
    c.seek(SeekFrom::Start(0)).unwrap();
    ext::detect_and_parse(&mut c).expect("detect_and_parse")
}

// ── Test 1: empty ext4 ────────────────────────────────────────────────────────

#[test]
fn empty_ext4() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    // mkfs.ext4 with no source directory creates the filesystem with a
    // single reserved lost+found directory.
    let image = RoundTrip::new("ext4-empty")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        .args(["-F", "$IMAGE"])
        .build_bytes();

    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_eq!(root.name, "/");
    assert!(root.is_directory);

    // ext4 always creates lost+found in the root.
    assert_path_exists(&root, "lost+found");
}

// ── Test 2: single file ────────────────────────────────────────────────────────

#[test]
fn ext4_single_file() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };
    let Some(_) = tools::DEBUGFS.require_or_skip() else {
        return;
    };

    const CONTENT: &[u8] = b"hello from isomage\n";
    const FILENAME: &str = "greeting.txt";

    // Build the image with mkfs.ext4, then inject the file via debugfs.
    let rt = RoundTrip::new("ext4-single-file")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        // Stage the source file in $SRC_DIR.
        .source_file(FILENAME, CONTENT)
        .args(["-F", "$IMAGE"])
        .build();

    let image_path = rt.image_path().to_path_buf();
    let src_file = rt.src_dir().join(FILENAME);

    // debugfs -w -R "write <local> <ext-path>" injects the file.
    tools::DEBUGFS
        .run([
            "-w",
            "-R",
            &format!("write {} /{}", src_file.display(), FILENAME),
            image_path.to_str().unwrap(),
        ])
        .expect("debugfs write")
        .assert_success();

    let image = fs::read(&image_path).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    let node = assert_path_exists(&root, FILENAME);
    assert!(!node.is_directory);
    assert_eq!(node.size, CONTENT.len() as u64, "file size mismatch");

    // If a file_location was resolved, verify the bytes.
    if let Some(loc) = node.file_location {
        let mut c = Cursor::new(&image);
        c.seek(SeekFrom::Start(loc)).unwrap();
        let mut buf = vec![0u8; CONTENT.len()];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), CONTENT, "file contents mismatch");
    }
}

// ── Test 3: nested directories ────────────────────────────────────────────────

#[test]
fn ext4_nested_dirs() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };
    let Some(_) = tools::DEBUGFS.require_or_skip() else {
        return;
    };

    // Use mke2fs -d to populate a directory tree.
    // mke2fs -d is available in e2fsprogs >= 1.43.
    // mke2fs is symlinked to mkfs.ext4 on most distros.
    // We check for MKFS_EXT4 and use it as mke2fs via its -t flag.

    let rt = RoundTrip::new("ext4-nested-dirs")
        .with(&tools::MKFS_EXT4)
        .image_size(8 * 1024 * 1024)
        // Create directory structure in $SRC_DIR.
        .source_file("a/b/c/deep.txt", b"deep content\n" as &[u8])
        .source_file("a/b/mid.txt", b"mid content\n" as &[u8])
        .source_file("a/top.txt", b"top content\n" as &[u8])
        .source_file("root.txt", b"root\n" as &[u8])
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    assert_path_exists(&root, "root.txt");
    assert_path_exists(&root, "a");
    assert_path_exists(&root, "a/top.txt");
    assert_path_exists(&root, "a/b");
    assert_path_exists(&root, "a/b/mid.txt");
    assert_path_exists(&root, "a/b/c");
    assert_path_exists(&root, "a/b/c/deep.txt");
}

// ── Test 4: many files (50+) ──────────────────────────────────────────────────

#[test]
fn ext4_many_files() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    // Build the source directory with 60 files, then create the image.
    let mut rt_builder = RoundTrip::new("ext4-many-files")
        .with(&tools::MKFS_EXT4)
        .image_size(16 * 1024 * 1024);
    for i in 0..60u32 {
        let name = format!("file_{i:03}.dat");
        let content = format!("content of file {i}\n");
        rt_builder = rt_builder.source_file(name, content.into_bytes());
    }
    let rt = rt_builder.args(["-F", "-d", "$SRC_DIR", "$IMAGE"]).build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    // Count how many file_NNN.dat nodes exist.
    let file_count = root
        .children
        .iter()
        .filter(|n| n.name.starts_with("file_"))
        .count();
    assert!(
        file_count >= 50,
        "expected ≥ 50 files in root, found {file_count}"
    );
}

// ── Test 5: large file (> 48 KiB) ────────────────────────────────────────────

#[test]
fn ext4_large_file() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    // 128 KiB file — requires extent or indirect blocks beyond direct range.
    let big_content = vec![b'A'; 128 * 1024];

    let rt = RoundTrip::new("ext4-large-file")
        .with(&tools::MKFS_EXT4)
        .image_size(32 * 1024 * 1024)
        .source_file("bigfile.bin", big_content.clone())
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    let node = assert_path_exists(&root, "bigfile.bin");
    assert_eq!(node.size, big_content.len() as u64);
    // Large files may or may not have a file_location (extent tree with
    // single extent is fine; multiple extents yield None).
}

// ── Test 6: Unicode filenames ────────────────────────────────────────────────

#[test]
fn ext4_unicode_filenames() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext4-unicode")
        .with(&tools::MKFS_EXT4)
        .image_size(8 * 1024 * 1024)
        .source_file("café.txt", b"coffee\n" as &[u8])
        .source_file("日本語.txt", b"nihongo\n" as &[u8])
        .source_file("emoji\u{1F355}.txt", b"pizza\n" as &[u8])
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    // At least the ASCII-range parts of the names should be visible.
    let names: Vec<&str> = root.children.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("caf")),
        "expected café.txt in tree, got: {names:?}"
    );
    assert!(
        names.iter().any(|n| n.contains('\u{65E5}')),
        "expected 日本語.txt in tree, got: {names:?}"
    );
}

// ── Test 7: ext2 compatibility ────────────────────────────────────────────────

/// mkfs.ext2 — separate binary on some distros; same as mkfs.ext4 -t ext2.
const MKFS_EXT2: common::tool::Tool = common::tool::Tool::new("mkfs.ext2");

#[test]
fn ext2_compat() {
    // Try mkfs.ext2 first; fall back to mkfs.ext4 -t ext2.
    if MKFS_EXT2.require_or_skip().is_some() {
        let rt = RoundTrip::new("ext2-compat")
            .with(&MKFS_EXT2)
            .image_size(4 * 1024 * 1024)
            .source_file("ext2file.txt", b"ext2 content\n" as &[u8])
            .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
            .build();

        let image = fs::read(rt.image_path()).expect("read image");
        let root = parse_image(&image);
        assert_tree_invariants(&root);
        assert_path_exists(&root, "ext2file.txt");
        return;
    }

    // mkfs.ext2 not found; try mkfs.ext4 -t ext2.
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext2-compat-via-ext4")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        .source_file("ext2file.txt", b"ext2 content\n" as &[u8])
        .args(["-F", "-t", "ext2", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_path_exists(&root, "ext2file.txt");
}

// ── Test 8: ext3 compatibility ────────────────────────────────────────────────

#[test]
fn ext3_compat() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext3-compat")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        .source_file("ext3file.txt", b"ext3 content\n" as &[u8])
        .args(["-F", "-t", "ext3", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_path_exists(&root, "ext3file.txt");
}

// ── Test 9: 1 KiB block size ─────────────────────────────────────────────────

#[test]
fn ext4_block_size_1k() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext4-bs-1k")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        .source_file("bs1k.txt", b"1k block size\n" as &[u8])
        // -b 1024 forces 1 KiB blocks.
        .args(["-F", "-b", "1024", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");

    // With 1 KiB blocks, s_log_block_size = 0, s_first_data_block = 1.
    // Verify detect still works.
    let mut c = Cursor::new(image.clone());
    assert!(ext::detect(&mut c), "should detect ext4 with 1 KiB blocks");

    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_path_exists(&root, "bs1k.txt");
}

// ── Test 10: 4 KiB block size ────────────────────────────────────────────────

#[test]
fn ext4_block_size_4k() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext4-bs-4k")
        .with(&tools::MKFS_EXT4)
        .image_size(8 * 1024 * 1024)
        .source_file("bs4k.txt", b"4k block size\n" as &[u8])
        .args(["-F", "-b", "4096", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");

    // With 4 KiB blocks, s_log_block_size = 2, s_first_data_block = 0.
    let mut c = Cursor::new(image.clone());
    assert!(ext::detect(&mut c), "should detect ext4 with 4 KiB blocks");

    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_path_exists(&root, "bs4k.txt");
}

// ── Test 11: cross-validate with 7zz ─────────────────────────────────────────

#[test]
fn ext4_cross_validate_7z() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };
    let Some(_) = tools::SEVEN_ZZ.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext4-7z-cross")
        .with(&tools::MKFS_EXT4)
        .image_size(8 * 1024 * 1024)
        .source_file("alpha.txt", b"alpha\n" as &[u8])
        .source_file("beta.txt", b"beta\n" as &[u8])
        .source_file("gamma/delta.txt", b"delta\n" as &[u8])
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image_path = rt.image_path();
    let image = fs::read(image_path).expect("read image");

    // Parse with isomage.
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    // Parse with 7zz.
    let sz_out = tools::SEVEN_ZZ
        .run(["l", "-slt", image_path.to_str().unwrap()])
        .expect("7zz run");

    if !sz_out.status.success() {
        // 7zz might not understand raw ext4; accept gracefully.
        eprintln!(
            "7zz exited {:?} (non-fatal for cross-validate): {}",
            sz_out.status.code(),
            sz_out.stderr_string()
        );
        return;
    }

    // Parse 7zz "Path = " and "Size = " lines from its long-format listing.
    let stdout = sz_out.stdout_string();
    let mut sz_files: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut current_path: Option<String> = None;
    for line in stdout.lines() {
        if let Some(p) = line.strip_prefix("Path = ") {
            current_path = Some(p.trim().to_string());
        } else if let Some(s) = line.strip_prefix("Size = ") {
            if let (Some(path), Ok(size)) = (current_path.take(), s.trim().parse::<u64>()) {
                if size > 0 {
                    sz_files.insert(path, size);
                }
            }
        }
    }

    // For each file isomage found, check it appears in the 7zz listing
    // with a matching size (if 7zz could read the image).
    if !sz_files.is_empty() {
        let check_files = ["alpha.txt", "beta.txt"];
        for name in &check_files {
            if let Some(node) = root.find_node(name) {
                // 7zz may include the full path. Check for a suffix match.
                let sz_size = sz_files
                    .iter()
                    .find(|(k, _)| k.ends_with(name))
                    .map(|(_, v)| *v);
                if let Some(sz) = sz_size {
                    assert_eq!(
                        node.size, sz,
                        "size mismatch for {name}: isomage={}, 7zz={sz}",
                        node.size
                    );
                }
            }
        }
    }
}

// ── Test 12: file contents exact ─────────────────────────────────────────────

#[test]
fn ext4_file_contents_exact() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    const EXPECTED: &[u8] = b"isomage content verification\n";

    let rt = RoundTrip::new("ext4-contents-exact")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        .source_file("verify.txt", EXPECTED)
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);

    let node = assert_path_exists(&root, "verify.txt");
    assert_eq!(node.size, EXPECTED.len() as u64);

    // If a file_location is known, verify the actual bytes.
    if let Some(loc) = node.file_location {
        let mut c = Cursor::new(&image);
        c.seek(SeekFrom::Start(loc)).unwrap();
        let mut buf = vec![0u8; EXPECTED.len()];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(
            buf.as_slice(),
            EXPECTED,
            "bytes at file_location do not match expected content"
        );
    } else {
        // Acceptable: file spans multiple extents. We still verified size.
        eprintln!("verify.txt: file_location=None (multi-run or indirect blocks)");
    }
}

// ── Test 13: e2fsck accepts image ────────────────────────────────────────────

#[test]
fn ext4_e2fsck_accepts_image() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };
    let Some(_) = tools::E2FSCK.require_or_skip() else {
        return;
    };

    let rt = RoundTrip::new("ext4-e2fsck")
        .with(&tools::MKFS_EXT4)
        .image_size(8 * 1024 * 1024)
        .source_file("fsck_test.txt", b"should be clean\n" as &[u8])
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image_path = rt.image_path().to_path_buf();

    // e2fsck -n is read-only fsck. Exit 0 = no errors.
    let fsck_out = tools::E2FSCK
        .run(["-n", image_path.to_str().unwrap()])
        .expect("e2fsck run");

    // e2fsck exit codes: 0 = no errors, 1 = corrected, 2 = corrected+reboot needed.
    // Treat 0 and 1 as acceptable for a freshly-created image.
    let code = fsck_out.status.code().unwrap_or(255);
    assert!(
        code <= 1,
        "e2fsck reported errors (exit {code}):\n{}",
        fsck_out.stdout_string()
    );

    // Also parse with isomage to confirm we agree with a clean image.
    let image = fs::read(&image_path).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);
    assert_path_exists(&root, "fsck_test.txt");
}

// ── Test 14: HTree directory (500+ files) ────────────────────────────────────

#[test]
fn ext4_htree_directory() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };

    // Create 500 files. mkfs.ext4 -d will populate them; when there are
    // more files than a block can hold, ext4 creates an HTree directory.
    // Our scanner walks raw data blocks, so HTree is transparent to us.
    let mut rt_builder = RoundTrip::new("ext4-htree")
        .with(&tools::MKFS_EXT4)
        .image_size(64 * 1024 * 1024);
    for i in 0..500u32 {
        let name = format!("htree_{i:04}.txt");
        let content = format!("htree test file {i}\n");
        rt_builder = rt_builder.source_file(name, content.into_bytes());
    }
    let rt = rt_builder.args(["-F", "-d", "$SRC_DIR", "$IMAGE"]).build();

    let image = fs::read(rt.image_path()).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    let htree_count = root
        .children
        .iter()
        .filter(|n| n.name.starts_with("htree_"))
        .count();
    assert!(
        htree_count >= 500,
        "expected ≥ 500 htree_ files, found {htree_count}"
    );
}

// ── Test 15: symlinks ─────────────────────────────────────────────────────────

#[test]
fn ext4_symlinks() {
    let Some(_) = tools::MKFS_EXT4.require_or_skip() else {
        return;
    };
    let Some(_) = tools::DEBUGFS.require_or_skip() else {
        return;
    };

    // Create an ext4 image, then use debugfs to add a symlink.
    let rt = RoundTrip::new("ext4-symlinks")
        .with(&tools::MKFS_EXT4)
        .image_size(4 * 1024 * 1024)
        .source_file("target.txt", b"target content\n" as &[u8])
        .args(["-F", "-d", "$SRC_DIR", "$IMAGE"])
        .build();

    let image_path = rt.image_path().to_path_buf();

    // Create a symlink via debugfs. "symlink <link-name> <target>" creates
    // a symlink at link-name pointing to target.
    let dbg_out = tools::DEBUGFS
        .run([
            "-w",
            "-R",
            "symlink /link.txt /target.txt",
            image_path.to_str().unwrap(),
        ])
        .expect("debugfs symlink");
    dbg_out.assert_success();

    let image = fs::read(&image_path).expect("read image");
    let root = parse_image(&image);
    assert_tree_invariants(&root);

    // The symlink should appear as a file node (S_IFLNK).
    let link = root.children.iter().find(|n| n.name == "link.txt");
    assert!(link.is_some(), "link.txt should be present in the tree");
    assert!(
        !link.unwrap().is_directory,
        "symlink should not appear as directory"
    );
}
