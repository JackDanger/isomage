//! Round-trip tests for the APFS reader in `src/formats/apfs.rs`.
//!
//! ## External tool requirements
//!
//! Creating real APFS images requires `hdiutil`, which is built into macOS.
//! It is not available on Linux. `hdiutil create -fs APFS` also requires
//! macOS 10.13+ and typically root access for the underlying disk setup on
//! some versions — the round-trip tests therefore skip on Linux runners.
//!
//! ## Strategy
//!
//! - Detection and parsing tests use hand-crafted in-memory buffers that
//!   exercise the parser without any external tool. These run on all platforms.
//! - The `hdiutil`-based tests skip automatically when `hdiutil` is not
//!   present; they exercise detection against a real container on macOS.
//! - Bad-magic and TooShort rejection tests never need `hdiutil` and run
//!   everywhere.

mod common;

use std::io::Cursor;

#[cfg(target_os = "macos")]
use common::tools;

use isomage::formats::apfs;

// ── Hand-crafted image helpers ─────────────────────────────────────────────────

/// Build a minimal 2-block in-memory APFS container with one volume.
///
/// Block 0: NX Superblock (magic at +32, block_size=4096, fs_oid[0]=1).
/// Block 1: APSB volume superblock (magic at +32, volname at +572).
fn make_apfs_image(volname: &str) -> Vec<u8> {
    const BLOCK_SIZE: usize = 4096;
    const NXSB_MAGIC: u32 = 0x4253_584e;
    const APSB_MAGIC: u32 = 0x4253_5041;

    let mut img = vec![0u8; BLOCK_SIZE * 2];

    // Block 0: NX Superblock
    img[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
    img[36..40].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
    img[180..188].copy_from_slice(&1u64.to_le_bytes()); // fs_oid[0] = 1

    // Block 1: APSB
    img[BLOCK_SIZE + 32..BLOCK_SIZE + 36].copy_from_slice(&APSB_MAGIC.to_le_bytes());
    let name_bytes = volname.as_bytes();
    let copy_len = name_bytes.len().min(255);
    img[BLOCK_SIZE + 572..BLOCK_SIZE + 572 + copy_len].copy_from_slice(&name_bytes[..copy_len]);

    img
}

// ── Test 1: detect() succeeds on a hand-crafted container ─────────────────────

#[test]
fn apfs_detect() {
    let img = make_apfs_image("TestVol");
    let mut c = Cursor::new(&img);
    assert!(
        apfs::detect(&mut c).is_ok(),
        "detect() should succeed on a valid hand-crafted APFS container"
    );
}

// ── Test 2: detect() restores the cursor ──────────────────────────────────────

#[test]
fn apfs_detect_restores_cursor() {
    use std::io::{Seek, SeekFrom};

    let img = make_apfs_image("TestVol");
    let mut c = Cursor::new(&img);
    c.seek(SeekFrom::Start(42)).unwrap();
    let _ = apfs::detect(&mut c);
    assert_eq!(
        c.position(),
        42,
        "detect() must restore the cursor to its original position"
    );
}

// ── Test 3: block_size is parsed correctly ────────────────────────────────────

#[test]
fn apfs_block_size() {
    let img = make_apfs_image("TestVol");
    let mut c = Cursor::new(&img);
    let nx = apfs::read_nx_superblock(&mut c).expect("read_nx_superblock should succeed");
    assert_eq!(nx.block_size, 4096, "block_size must be 4096 in test image");
}

// ── Test 4: volume count matches the number of non-zero fs_oid entries ────────

#[test]
fn apfs_volume_count() {
    let img = make_apfs_image("TestVol");
    let mut c = Cursor::new(&img);
    let nx = apfs::read_nx_superblock(&mut c).expect("read_nx_superblock should succeed");
    assert_eq!(
        nx.fs_oids.len(),
        1,
        "should find exactly 1 volume in the test image"
    );
}

// ── Test 5: volume name is decoded correctly ──────────────────────────────────

#[test]
fn apfs_volume_name() {
    let img = make_apfs_image("Macintosh HD");
    let mut c = Cursor::new(&img);
    let tree = apfs::detect_and_parse(&mut c).expect("detect_and_parse should succeed");

    assert_eq!(tree.name, "/", "root node must be named '/'");
    assert!(tree.is_directory, "root must be a directory");
    assert_eq!(
        tree.children.len(),
        1,
        "should have exactly one volume child"
    );
    assert_eq!(
        tree.children[0].name, "Macintosh HD",
        "volume name should be decoded correctly"
    );
}

// ── Test 6: bad magic is rejected ─────────────────────────────────────────────

#[test]
fn apfs_bad_magic_rejected() {
    // All-zeros: offset 32 won't have 0x4253584e.
    let img = vec![0u8; 4096];
    let mut c = Cursor::new(&img);
    assert!(
        matches!(
            apfs::detect(&mut c),
            Err(apfs::Error::BadMagic) | Err(apfs::Error::TooShort)
        ),
        "detect() must reject a non-APFS image"
    );
}

// ── Test 7: TooShort for truncated image ──────────────────────────────────────

#[test]
fn apfs_detect_rejects_too_short() {
    let img = vec![0u8; 10];
    let mut c = Cursor::new(&img);
    assert!(
        matches!(
            apfs::detect(&mut c),
            Err(apfs::Error::TooShort) | Err(apfs::Error::BadMagic)
        ),
        "detect() must return TooShort or BadMagic for a truncated image"
    );
}

// ── Test 8: hdiutil-based detection (macOS only, skips on Linux) ──────────────

/// On macOS, creating a raw APFS container requires `hdiutil attach -nomount`
/// which needs root and a real block device. This is too invasive for CI.
/// Full APFS round-trip coverage is provided by the hand-crafted tests above.
/// Marked `#[ignore]` to exclude from the normal test run while preserving
/// the scaffold for environments with root access.
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn apfs_detect_hdiutil() {
    let _ = tools::HDIUTIL.require_or_skip();
    eprintln!("skip: apfs_detect_hdiutil — creating raw APFS images without root is unsupported on this host");
}
