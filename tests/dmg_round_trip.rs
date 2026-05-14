//! Round-trip tests for the DMG reader in `src/formats/dmg.rs`.
//!
//! ## External tool requirements
//!
//! Creating DMG files requires `hdiutil`, which is built into macOS.
//! It is not available on Linux. Tests that call `hdiutil` skip
//! automatically when the tool is not installed (i.e. on Linux runners).
//!
//! ## Strategy
//!
//! - On macOS: use `hdiutil create` to produce a minimal UDIF DMG and
//!   verify detection, koly fields, and partition name extraction.
//! - On Linux (and macOS without `hdiutil`): use hand-crafted minimal
//!   koly trailers to exercise detection and the fallback tree path.
//! - Tests that test bad-magic / TooShort rejection never need `hdiutil`
//!   and run everywhere.

mod common;

use std::io::Cursor;

#[cfg(target_os = "macos")]
use std::io::{Seek, SeekFrom};

#[cfg(target_os = "macos")]
use common::tools;
#[cfg(target_os = "macos")]
use common::RoundTrip;

use isomage::formats::dmg;

// ── hdiutil helper ────────────────────────────────────────────────────────────

/// Build a minimal HFS+ DMG using `hdiutil create -srcfolder`.
///
/// Only compiled on macOS where `hdiutil` is available. Linux round-trip
/// coverage uses hand-crafted koly trailers (see the `hand_crafted_dmg` tests
/// below) and is not gated on `ISOMAGE_REQUIRE_TOOLS`.
#[cfg(target_os = "macos")]
fn make_hdiutil_dmg() -> Option<Vec<u8>> {
    let _ = tools::HDIUTIL.require_or_skip()?;

    // hdiutil create -srcfolder <dir> -format UDRO <outpath>
    // writes <outpath>.dmg. We use $SRC_DIR as the source folder and
    // stage a dummy file there. $IMAGE is the destination path;
    // hdiutil appends ".dmg" to it automatically, so we read back the
    // resulting file manually below.
    let rt = RoundTrip::new("dmg-hdiutil")
        .with(&tools::HDIUTIL)
        .source_file("hello.txt", b"hello from isomage" as &[u8])
        .args([
            "create",
            "-srcfolder",
            "$SRC_DIR",
            "-fs",
            "HFS+",
            "-volname",
            "TestVol",
            "-format",
            "UDRO",
            "$IMAGE",
        ])
        .build();

    // hdiutil appends ".dmg" to the output path. Read that file.
    let dmg_path = {
        let mut p = rt.image_path().to_owned();
        let mut name = p.file_name().unwrap().to_owned();
        name.push(".dmg");
        p.set_file_name(name);
        p
    };

    let bytes = if dmg_path.exists() {
        std::fs::read(&dmg_path).expect("read hdiutil-produced .dmg")
    } else {
        // hdiutil may have written directly to the given path if it
        // already ends in .dmg (some versions skip appending).
        rt.into_bytes()
    };

    Some(bytes)
}

// ── Hand-crafted minimal DMG ──────────────────────────────────────────────────

/// Build a minimal buffer whose last 512 bytes are a valid koly trailer.
///
/// `xml` (if non-empty) is placed immediately before the koly so
/// xml_offset and xml_length are set correctly. `sector_count` is
/// embedded in the koly at offset 492.
fn hand_crafted_dmg(xml: &str, sector_count: u64) -> Vec<u8> {
    const KOLY_SIZE: usize = 512;
    const KOLY_MAGIC: &[u8; 4] = b"koly";

    let xml_bytes = xml.as_bytes();
    let xml_len = xml_bytes.len();

    // layout: [64-byte padding] [xml] [512-byte koly]
    let prefix_pad = 64usize;
    let total = prefix_pad + xml_len + KOLY_SIZE;
    let mut buf = vec![0u8; total];

    // Place xml right before koly.
    let xml_start = prefix_pad;
    buf[xml_start..xml_start + xml_len].copy_from_slice(xml_bytes);

    // Build koly trailer.
    let k = total - KOLY_SIZE;
    buf[k..k + 4].copy_from_slice(KOLY_MAGIC);
    buf[k + 4..k + 8].copy_from_slice(&4u32.to_be_bytes()); // version

    if xml_len > 0 {
        buf[k + 216..k + 224].copy_from_slice(&(xml_start as u64).to_be_bytes());
        buf[k + 224..k + 232].copy_from_slice(&(xml_len as u64).to_be_bytes());
    }
    buf[k + 492..k + 500].copy_from_slice(&sector_count.to_be_bytes());

    buf
}

// ── Test 1: detect() succeeds on a real hdiutil DMG ─────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn dmg_detect() {
    let Some(bytes) = make_hdiutil_dmg() else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    assert!(
        dmg::detect(&mut c).is_ok(),
        "dmg::detect should succeed on a DMG produced by hdiutil"
    );
}

// ── Test 2: detect() restores the cursor ─────────────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn dmg_detect_restores_cursor() {
    let Some(bytes) = make_hdiutil_dmg() else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    c.seek(SeekFrom::Start(42)).unwrap();
    let _ = dmg::detect(&mut c);
    assert_eq!(
        c.position(),
        42,
        "dmg::detect must restore the cursor to its original position"
    );
}

// ── Test 3: detect_and_parse() returns valid root on a real DMG ──────────────

#[cfg(target_os = "macos")]
#[test]
fn dmg_koly_fields() {
    let Some(bytes) = make_hdiutil_dmg() else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    let root =
        dmg::detect_and_parse(&mut c).expect("detect_and_parse should succeed on a real DMG");

    assert_eq!(root.name, "/", "root node must be named '/'");
    assert!(root.is_directory, "root must be a directory");
    // A real HFS+ DMG from hdiutil has at least one blkx entry.
    assert!(
        !root.children.is_empty(),
        "real DMG should have at least one partition entry"
    );
}

// ── Test 4: partition names are non-empty ────────────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn dmg_partition_names() {
    let Some(bytes) = make_hdiutil_dmg() else {
        return;
    };

    let mut c = Cursor::new(&bytes);
    let root =
        dmg::detect_and_parse(&mut c).expect("detect_and_parse should succeed on a real DMG");

    for child in &root.children {
        assert!(
            !child.name.is_empty(),
            "every partition entry must have a non-empty name"
        );
    }
}

// ── Test 5: sector_count is reflected when XML has no blkx ───────────────────

#[test]
fn dmg_sector_count() {
    let sector_count: u64 = 2048;
    let bytes = hand_crafted_dmg("", sector_count);
    let mut c = Cursor::new(&bytes);
    let root = dmg::detect_and_parse(&mut c).expect("detect_and_parse should succeed");

    // No XML → fallback to synthetic disk.dmg child.
    assert_eq!(root.children.len(), 1);
    assert_eq!(root.children[0].name, "disk.dmg");
    assert_eq!(
        root.children[0].file_length,
        Some(sector_count * 512),
        "disk.dmg file_length must be sector_count × 512"
    );
}

// ── Test 6: detect() returns BadMagic for a random buffer ────────────────────

#[test]
fn dmg_bad_magic_rejected() {
    let image = vec![0u8; 4096];
    let mut c = Cursor::new(&image);
    assert!(
        matches!(dmg::detect(&mut c), Err(dmg::Error::BadMagic)),
        "detect() must reject a non-DMG image with BadMagic"
    );
}

// ── Test 7: detect() returns TooShort for a truncated buffer ─────────────────

#[test]
fn dmg_detect_rejects_too_short() {
    let image = vec![0u8; 100];
    let mut c = Cursor::new(&image);
    assert!(
        matches!(dmg::detect(&mut c), Err(dmg::Error::TooShort)),
        "detect() must return TooShort for a buffer shorter than 512 bytes"
    );
}

// ── Test 8: hand-crafted plist produces named partition entries ───────────────

#[test]
fn dmg_hand_crafted_xml_partition_names() {
    let xml = r#"<plist><dict>
  <key>resource-fork</key><dict>
  <key>blkx</key><array>
    <dict><key>CFName</key><string>Apple_HFS : TestVol</string></dict>
    <dict><key>CFName</key><string>free</string></dict>
  </array></dict></dict></plist>"#;

    let bytes = hand_crafted_dmg(xml, 4096);
    let mut c = Cursor::new(&bytes);
    let root = dmg::detect_and_parse(&mut c).expect("detect_and_parse should succeed");

    assert_eq!(root.children.len(), 2);
    assert_eq!(root.children[0].name, "Apple_HFS : TestVol");
    assert_eq!(root.children[1].name, "free");
}
