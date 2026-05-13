//! VHD (Virtual Hard Disk) container reader (`vhd` feature).
//!
//! VHD is Microsoft's virtual disk format, documented in the
//! "Virtual Hard Disk Image Format Specification" (Microsoft Open
//! Specification). This reader handles:
//!
//! - **Fixed VHDs**: virtual disk data starts at byte 0; the footer
//!   is the last 512 bytes of the file. Reported as a single extent
//!   with `file_location = Some(0)`.
//! - **Dynamic VHDs**: footer at byte 0, dynamic disk header at byte
//!   512, Block Allocation Table follows. Data is fragmented across
//!   BAT-indexed blocks. Reported with `file_location = None`.
//!
//! **Differencing VHDs** (disk_type 4) are not supported and return
//! `Error::UnsupportedType(4)`. Parent resolution requires traversing
//! a locator chain that complicates the reader significantly and is
//! deferred to a later PR.
//!
//! ## Footer layout (512 bytes, big-endian multi-byte integers)
//!
//! ```text
//!  [0]   u8[8]  cookie          = b"conectix"
//!  [8]   u32    features
//! [12]   u32    file_format_version
//! [16]   u64    data_offset     // 0xFFFFFFFFFFFFFFFF for Fixed; 512 for Dynamic
//! [24]   u32    timestamp
//! [28]   u8[4]  creator_application
//! [32]   u32    creator_version
//! [36]   u8[4]  creator_host_os
//! [40]   u64    original_size
//! [48]   u64    current_size
//! [56]   u32    disk_geometry
//! [60]   u32    disk_type       // 2=Fixed, 3=Dynamic, 4=Differencing
//! [64]   u32    checksum        // 1's-complement of all bytes (checksum field = 0)
//! [68]   u8[16] unique_id
//! [84]   u8     saved_state
//! [85]   u8[427] reserved
//! ```
//!
//! ## Dynamic Disk Header layout (1024 bytes at offset 512)
//!
//! ```text
//!  [0]   u8[8]  cookie          = b"cxsparse"
//!  [8]   u64    data_offset     // reserved, 0xFFFFFFFFFFFFFFFF
//! [16]   u64    table_offset    // byte offset of the BAT
//! [24]   u32    header_version  // 0x00010000
//! [28]   u32    max_table_entries
//! [32]   u32    block_size      // default 0x200000 (2 MB)
//! [36]   u32    checksum
//! ```

use std::io::{self, Read, Seek, SeekFrom};

use crate::tree::TreeNode;

/// Magic bytes in a VHD footer at offset 0.
const FOOTER_COOKIE: &[u8; 8] = b"conectix";

/// Magic bytes in a Dynamic Disk Header at offset 0.
const DYN_HEADER_COOKIE: &[u8; 8] = b"cxsparse";

/// VHD footer size in bytes.
const FOOTER_SIZE: u64 = 512;

/// disk_type = 2: Fixed VHD.
const DISK_TYPE_FIXED: u32 = 2;

/// disk_type = 3: Dynamic VHD.
const DISK_TYPE_DYNAMIC: u32 = 3;

/// Sentinel BAT entry meaning "block not allocated".
/// Used in tests to build minimal Dynamic VHD images.
#[cfg(test)]
const BAT_ENTRY_UNUSED: u32 = 0xFFFF_FFFF;

// ── Error type ────────────────────────────────────────────────────────────────

/// Reasons `detect` or `detect_and_parse` can fail.
#[derive(Debug)]
pub enum Error {
    /// File is shorter than one VHD footer (512 bytes).
    TooShort,
    /// Footer cookie is not `b"conectix"`.
    BadMagic,
    /// Footer checksum does not match the computed 1's-complement sum.
    BadChecksum,
    /// `disk_type` is not 2 (Fixed) or 3 (Dynamic). The value is included
    /// for diagnostic purposes.
    UnsupportedType(u32),
    /// Underlying I/O error.
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "VHD file is shorter than 512 bytes (no footer)"),
            Error::BadMagic => write!(f, "VHD footer cookie b\"conectix\" not found"),
            Error::BadChecksum => write!(f, "VHD footer checksum mismatch"),
            Error::UnsupportedType(t) => write!(
                f,
                "VHD disk_type {t} is not supported (only Fixed=2, Dynamic=3)"
            ),
            Error::Io(e) => write!(f, "VHD I/O error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

// ── Footer ────────────────────────────────────────────────────────────────────

/// Parsed VHD footer fields we actually use.
struct Footer {
    /// `data_offset` field: 0xFFFF_FFFF_FFFF_FFFF for Fixed, 512 for Dynamic.
    data_offset: u64,
    /// `current_size`: virtual disk size in bytes.
    current_size: u64,
    /// `disk_type`: 2=Fixed, 3=Dynamic, 4=Differencing, …
    disk_type: u32,
}

/// Compute the 1's-complement checksum of a 512-byte footer buffer,
/// with the checksum field (bytes 64..68) treated as zero.
///
/// From the VHD spec: sum all bytes, treating the checksum bytes as 0,
/// then take the 1's complement (bitwise NOT) of the low 32 bits.
fn verify_checksum(buf: &[u8; 512]) -> bool {
    let stored = u32::from_be_bytes([buf[64], buf[65], buf[66], buf[67]]);

    let mut sum: u32 = 0;
    for (i, &b) in buf.iter().enumerate() {
        if (64..68).contains(&i) {
            // Treat checksum field as 0 during computation.
            continue;
        }
        sum = sum.wrapping_add(b as u32);
    }
    let computed = !sum;
    computed == stored
}

/// Read and validate the VHD footer at `offset` from the start of the file.
fn read_footer<R: Read + Seek>(r: &mut R, offset: u64) -> Result<Footer, Error> {
    r.seek(SeekFrom::Start(offset))?;
    let mut buf = [0u8; 512];
    r.read_exact(&mut buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;

    // Check magic.
    if &buf[0..8] != FOOTER_COOKIE {
        return Err(Error::BadMagic);
    }

    // Verify 1's-complement checksum.
    if !verify_checksum(&buf) {
        return Err(Error::BadChecksum);
    }

    let data_offset = u64::from_be_bytes([
        buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
    ]);
    let current_size = u64::from_be_bytes([
        buf[48], buf[49], buf[50], buf[51], buf[52], buf[53], buf[54], buf[55],
    ]);
    let disk_type = u32::from_be_bytes([buf[60], buf[61], buf[62], buf[63]]);

    Ok(Footer {
        data_offset,
        current_size,
        disk_type,
    })
}

// ── Detection ─────────────────────────────────────────────────────────────────

/// Detect whether `r` is a VHD image by checking the footer magic.
///
/// Seeks to `file_end - 512`, reads 8 bytes, checks for `b"conectix"`.
/// Restores the stream position on both success and error paths.
///
/// Returns `Ok(())` if detection succeeds.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let pos = r.stream_position()?;
    let result = detect_inner(r);
    let _ = r.seek(SeekFrom::Start(pos));
    result
}

fn detect_inner<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    // Find file length.
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < FOOTER_SIZE {
        return Err(Error::TooShort);
    }

    // Seek to footer.
    let footer_offset = file_len - FOOTER_SIZE;
    r.seek(SeekFrom::Start(footer_offset))?;

    // Read only the 8-byte cookie for detection.
    let mut cookie = [0u8; 8];
    match r.read_exact(&mut cookie) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    if &cookie != FOOTER_COOKIE {
        return Err(Error::BadMagic);
    }

    Ok(())
}

// ── Tree building ─────────────────────────────────────────────────────────────

/// Parse the VHD at `r` and return a [`TreeNode`] tree.
///
/// The tree always has the shape:
///
/// ```text
/// / (dir)
/// └── disk.img (file, size = virtual disk size)
/// ```
///
/// For Fixed VHDs, `disk.img` has `file_location = Some(0)` because
/// the raw disk data occupies bytes `0..current_size` of the file.
///
/// For Dynamic VHDs, `disk.img` has `file_location = None` because
/// data is scattered across BAT-indexed blocks. The virtual size is
/// still reported correctly in `file_length`.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    // Find file length and read footer from the end.
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < FOOTER_SIZE {
        return Err(Error::TooShort);
    }
    let footer_offset = file_len - FOOTER_SIZE;
    let footer = read_footer(r, footer_offset)?;

    match footer.disk_type {
        DISK_TYPE_FIXED => parse_fixed(footer.current_size, file_len),
        DISK_TYPE_DYNAMIC => parse_dynamic(r, &footer, file_len),
        other => Err(Error::UnsupportedType(other)),
    }
}

/// Build the tree for a Fixed VHD. Data occupies bytes 0..current_size.
fn parse_fixed(current_size: u64, file_len: u64) -> Result<TreeNode, Error> {
    // Validate that current_size doesn't exceed the available data region.
    // A fixed VHD is: [disk data: current_size bytes] [footer: 512 bytes].
    let data_region = file_len.saturating_sub(FOOTER_SIZE);
    if current_size > data_region {
        return Err(Error::TooShort);
    }

    let mut root = TreeNode::new_directory("/".to_string());

    // For a Fixed VHD the raw disk data starts at byte 0 and is
    // current_size bytes long, followed immediately by the 512-byte footer.
    let disk_node =
        TreeNode::new_file_with_location("disk.img".to_string(), current_size, 0, current_size);

    root.add_child(disk_node);
    root.calculate_directory_size();
    Ok(root)
}

/// Build the tree for a Dynamic VHD. Data is fragmented across BAT blocks;
/// we report the virtual size but set `file_location = None`.
fn parse_dynamic<R: Read + Seek>(r: &mut R, footer: &Footer, file_len: u64) -> Result<TreeNode, Error> {
    // The Dynamic Disk Header lives at data_offset (= 512 for standard VHDs).
    // Validate data_offset before seeking to avoid seeking past EOF on corrupt images.
    let dyn_header_offset = footer.data_offset;
    if dyn_header_offset > file_len.saturating_sub(1024) {
        return Err(Error::TooShort);
    }

    // Read 1024 bytes of the Dynamic Disk Header.
    r.seek(SeekFrom::Start(dyn_header_offset))?;
    let mut hdr = [0u8; 1024];
    match r.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    // Verify the Dynamic Disk Header cookie.
    if &hdr[0..8] != DYN_HEADER_COOKIE {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "VHD Dynamic Disk Header cookie b\"cxsparse\" not found",
        )));
    }

    // For Dynamic VHDs we report the virtual size without mapping blocks.
    let current_size = footer.current_size;

    let mut root = TreeNode::new_directory("/".to_string());

    // file_location = None: data is scattered across BAT blocks.
    let mut disk_node = TreeNode::new_file("disk.img".to_string(), current_size);
    disk_node.file_length = Some(current_size);

    root.add_child(disk_node);
    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Minimal Fixed VHD builder ─────────────────────────────────────────
    //
    // Layout:
    //   [0..512]     512 bytes of "disk data" (all zeros)
    //   [512..1024]  512-byte Fixed VHD footer
    //
    // This is a degenerate 512-byte Fixed VHD — just enough to satisfy
    // the parser. Real Fixed VHDs have current_size = file_size - 512.

    fn build_footer(disk_type: u32, current_size: u64, data_offset: u64) -> [u8; 512] {
        let mut buf = [0u8; 512];

        // Cookie.
        buf[0..8].copy_from_slice(FOOTER_COOKIE);

        // features = 0x00000002 (reserved bit must be set per spec).
        buf[8..12].copy_from_slice(&2u32.to_be_bytes());

        // file_format_version = 0x00010000.
        buf[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());

        // data_offset.
        buf[16..24].copy_from_slice(&data_offset.to_be_bytes());

        // timestamp = 0.
        buf[24..28].copy_from_slice(&0u32.to_be_bytes());

        // creator_application = "test".
        buf[28..32].copy_from_slice(b"test");

        // creator_version = 0.
        buf[32..36].copy_from_slice(&0u32.to_be_bytes());

        // creator_host_os = "Wi2k".
        buf[36..40].copy_from_slice(b"Wi2k");

        // original_size.
        buf[40..48].copy_from_slice(&current_size.to_be_bytes());

        // current_size.
        buf[48..56].copy_from_slice(&current_size.to_be_bytes());

        // disk_geometry = 0 (we don't validate it).
        buf[56..60].copy_from_slice(&0u32.to_be_bytes());

        // disk_type.
        buf[60..64].copy_from_slice(&disk_type.to_be_bytes());

        // Compute and insert checksum (checksum field = 0 during computation).
        // buf[64..68] is already 0.
        let mut sum: u32 = 0;
        for &b in buf.iter() {
            sum = sum.wrapping_add(b as u32);
        }
        let checksum: u32 = !sum;
        buf[64..68].copy_from_slice(&checksum.to_be_bytes());

        buf
    }

    /// Build a minimal Fixed VHD: 512 bytes of data + 512-byte footer.
    fn build_fixed_vhd(data_size: u64) -> Vec<u8> {
        let data_offset = 0xFFFF_FFFF_FFFF_FFFFu64; // Fixed VHD sentinel.
        let footer = build_footer(DISK_TYPE_FIXED, data_size, data_offset);
        let mut image = vec![0u8; data_size as usize];
        image.extend_from_slice(&footer);
        image
    }

    /// Build a minimal Dynamic VHD: footer-copy at 0, dynamic disk header at 512.
    fn build_dynamic_vhd(virtual_size: u64) -> Vec<u8> {
        // data_offset = 512 (offset of Dynamic Disk Header).
        let footer = build_footer(DISK_TYPE_DYNAMIC, virtual_size, 512);

        // Dynamic Disk Header (1024 bytes).
        let mut dyn_hdr = [0u8; 1024];
        dyn_hdr[0..8].copy_from_slice(DYN_HEADER_COOKIE);

        // data_offset (reserved): 0xFFFFFFFFFFFFFFFF.
        dyn_hdr[8..16].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_be_bytes());

        // table_offset: BAT starts at 512 + 1024 = 1536.
        let table_offset: u64 = 512 + 1024;
        dyn_hdr[16..24].copy_from_slice(&table_offset.to_be_bytes());

        // header_version = 0x00010000.
        dyn_hdr[24..28].copy_from_slice(&0x0001_0000u32.to_be_bytes());

        // max_table_entries: ceil(virtual_size / block_size).
        let block_size: u32 = 0x0020_0000; // 2 MB default.
        let max_table_entries = virtual_size.div_ceil(block_size as u64) as u32;
        dyn_hdr[28..32].copy_from_slice(&max_table_entries.to_be_bytes());

        // block_size.
        dyn_hdr[32..36].copy_from_slice(&block_size.to_be_bytes());

        // Checksum for dyn header (field at [36..40], compute over [0..1024]).
        // buf[36..40] already 0.
        let mut sum: u32 = 0;
        for &b in dyn_hdr.iter() {
            sum = sum.wrapping_add(b as u32);
        }
        let hdr_checksum: u32 = !sum;
        dyn_hdr[36..40].copy_from_slice(&hdr_checksum.to_be_bytes());

        // BAT: all entries = 0xFFFFFFFF (no blocks allocated).
        let bat_entries = max_table_entries as usize;
        let mut bat = vec![0xFFu8; bat_entries * 4];
        // Ensure all entries are 0xFFFFFFFF.
        for i in 0..bat_entries {
            bat[i * 4..i * 4 + 4].copy_from_slice(&BAT_ENTRY_UNUSED.to_be_bytes());
        }

        // Layout: footer (512) + dyn_header (1024) + BAT + footer-copy at end.
        let mut image: Vec<u8> = Vec::new();
        image.extend_from_slice(&footer); // Copy of footer at byte 0.
        image.extend_from_slice(&dyn_hdr);
        image.extend_from_slice(&bat);
        image.extend_from_slice(&footer); // Footer at end.
        image
    }

    // ── Detection tests ───────────────────────────────────────────────────

    #[test]
    fn detect_fixed_vhd_ok() {
        let img = build_fixed_vhd(512);
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok(), "should detect fixed VHD");
    }

    #[test]
    fn detect_dynamic_vhd_ok() {
        let img = build_dynamic_vhd(2 * 1024 * 1024);
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok(), "should detect dynamic VHD");
    }

    #[test]
    fn detect_restores_position() {
        let img = build_fixed_vhd(512);
        let mut c = Cursor::new(&img);
        c.seek(SeekFrom::Start(7)).unwrap();
        detect(&mut c).unwrap();
        assert_eq!(
            c.stream_position().unwrap(),
            7,
            "detect() must restore stream position"
        );
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let img = vec![0u8; 1024];
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect(&mut c), Err(Error::BadMagic)),
            "all-zeros should fail with BadMagic"
        );
    }

    #[test]
    fn detect_rejects_too_short() {
        let img = vec![0u8; 256];
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect(&mut c), Err(Error::TooShort)),
            "256-byte image should fail with TooShort"
        );
    }

    // ── Checksum tests ────────────────────────────────────────────────────

    #[test]
    fn checksum_valid_footer_passes() {
        let img = build_fixed_vhd(512);
        // Footer is the last 512 bytes.
        let footer_slice: &[u8; 512] = img[img.len() - 512..].try_into().unwrap();
        assert!(
            verify_checksum(footer_slice),
            "freshly built footer checksum should pass"
        );
    }

    #[test]
    fn checksum_corrupted_footer_fails() {
        let img = build_fixed_vhd(512);
        let mut patched = img.clone();
        let footer_start = patched.len() - 512;
        patched[footer_start + 10] ^= 0xFF; // Flip bits in the features field (byte 10).
        let footer_slice: &[u8; 512] = patched[footer_start..].try_into().unwrap();
        assert!(
            !verify_checksum(footer_slice),
            "corrupted footer should fail checksum"
        );
    }

    #[test]
    fn bad_checksum_returns_error() {
        let mut img = build_fixed_vhd(512);
        // Corrupt checksum bytes in the footer (bytes 64..68 from footer start).
        let footer_start = img.len() - 512;
        img[footer_start + 64] ^= 0xFF;
        let mut c = Cursor::new(&img);
        // detect_and_parse calls read_footer which checks the checksum.
        let result = detect_and_parse(&mut c);
        assert!(
            matches!(result, Err(Error::BadChecksum)),
            "corrupted checksum should yield BadChecksum"
        );
    }

    // ── Parse tests ───────────────────────────────────────────────────────

    #[test]
    fn fixed_vhd_tree_shape() {
        let img = build_fixed_vhd(512);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse fixed VHD");
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.name, "disk.img");
        assert!(!child.is_directory);
    }

    #[test]
    fn fixed_vhd_file_location_is_zero() {
        let data_size: u64 = 512;
        let img = build_fixed_vhd(data_size);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse fixed VHD");
        let disk = &root.children[0];
        assert_eq!(
            disk.file_location,
            Some(0),
            "fixed VHD disk.img should have file_location=Some(0)"
        );
        assert_eq!(
            disk.file_length,
            Some(data_size),
            "fixed VHD disk.img should have file_length=Some(current_size)"
        );
        assert_eq!(disk.size, data_size);
    }

    #[test]
    fn dynamic_vhd_tree_shape() {
        let virtual_size: u64 = 10 * 1024 * 1024; // 10 MB
        let img = build_dynamic_vhd(virtual_size);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse dynamic VHD");
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.name, "disk.img");
        assert!(!child.is_directory);
    }

    #[test]
    fn dynamic_vhd_no_file_location() {
        let virtual_size: u64 = 10 * 1024 * 1024;
        let img = build_dynamic_vhd(virtual_size);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse dynamic VHD");
        let disk = &root.children[0];
        assert_eq!(
            disk.file_location, None,
            "dynamic VHD disk.img should have file_location=None (fragmented BAT)"
        );
        assert_eq!(
            disk.file_length,
            Some(virtual_size),
            "dynamic VHD disk.img should report virtual size in file_length"
        );
    }

    #[test]
    fn unsupported_differencing_type_returns_error() {
        // disk_type = 4 (Differencing).
        const DISK_TYPE_DIFFERENCING: u32 = 4;
        let data_offset = 512u64;
        let footer = build_footer(DISK_TYPE_DIFFERENCING, 1024 * 1024, data_offset);

        // Build a minimal image: 512 bytes data + footer at end.
        let mut img = vec![0u8; 512];
        img.extend_from_slice(&footer);

        let mut c = Cursor::new(&img);
        let result = detect_and_parse(&mut c);
        assert!(
            matches!(result, Err(Error::UnsupportedType(4))),
            "differencing VHD should return UnsupportedType(4)"
        );
    }
}
