//! VMDK (VMware Virtual Machine Disk) sparse extent reader (`vmdk` feature).
//!
//! VMware's VMDK format has several sub-types; this reader handles the
//! **sparse extent** variant, which is the most common form produced by
//! `qemu-img` and VMware workstation:
//!
//! - **monolithicSparse**: one file, magic `0x564d444b` at byte 0.
//! - **twoGbMaxExtentSparse**: same magic, same header, split into 2-GB files.
//!
//! Flat/raw extents and descriptor-only files do not carry the magic and
//! are rejected with [`Error::BadMagic`]. Compressed VMDKs (`compress_algorithm
//! = 1`, i.e. deflate / streamOptimized) are rejected with [`Error::Compressed`].
//!
//! ## SparseExtentHeader layout (512 bytes at offset 0, all fields little-endian)
//!
//! ```text
//!  [0]   u32  magic_number        = 0x564d444b
//!  [4]   u32  version             = 1 or 3
//!  [8]   u32  flags
//! [12]   u64  capacity            // disk size in 512-byte sectors
//! [20]   u64  grain_size          // grain size in sectors (default 128 = 64 KB)
//! [28]   u64  descriptor_offset   // sector offset of embedded descriptor (0 if none)
//! [36]   u64  descriptor_size     // descriptor size in sectors
//! [44]   u32  num_gtes_per_gt     // GTEs per grain table (always 512)
//! [48]   u64  rgd_offset          // redundant grain directory sector offset
//! [56]   u64  gd_offset           // grain directory sector offset
//! [64]   u64  overhead            // overhead sectors before grain data starts
//! [72]   u8   unclean_shutdown
//! [73]   u8   single_end_line_char    = '\n'
//! [74]   u8   non_end_line_char       = ' '
//! [75]   u8   double_end_line_char1   = '\r'
//! [76]   u8   double_end_line_char2   = '\n'
//! [77]   u8   compress_algorithm      // 0=none, 1=deflate (rejected)
//! [78]   u8[433] pad
//! ```
//!
//! ## TreeNode output
//!
//! Because VMDK grain data is addressed through a grain directory /
//! grain table indirection it is not contiguous in the file. We therefore
//! set `file_location = None` and report only the virtual size in
//! `file_length`:
//!
//! ```text
//! / (dir)
//! └─ disk.vmdk (file, file_location=None, file_length=capacity*512)
//! ```

use std::io::{self, Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Magic bytes for a VMDK sparse extent: "VMDK" in little-endian u32.
const VMDK_MAGIC: u32 = 0x564d_444b;

/// Minimum file length to hold the 512-byte SparseExtentHeader.
const HEADER_SIZE: u64 = 512;

/// Sector size in bytes (always 512 for VMDK).
const SECTOR_SIZE: u64 = 512;

// ── Error type ────────────────────────────────────────────────────────────────

/// Reasons `detect` or `detect_and_parse` can fail.
#[derive(Debug)]
pub enum Error {
    /// File is shorter than one VMDK SparseExtentHeader (512 bytes).
    TooShort,
    /// Magic `0x564d444b` not found at byte 0.
    BadMagic,
    /// Header version is not 1 or 3. The value is included for diagnostics.
    UnsupportedVersion(u32),
    /// `compress_algorithm == 1` (deflate / streamOptimized). Not supported.
    Compressed,
    /// Underlying I/O error.
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "VMDK file is shorter than 512 bytes (no header)"),
            Error::BadMagic => write!(
                f,
                "VMDK sparse extent magic 0x564d444b not found at offset 0"
            ),
            Error::UnsupportedVersion(v) => write!(
                f,
                "VMDK header version {v} is not supported (only 1 and 3 are)"
            ),
            Error::Compressed => write!(
                f,
                "VMDK streamOptimized (deflate-compressed) images are not supported"
            ),
            Error::Io(e) => write!(f, "VMDK I/O error: {e}"),
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

// ── Header ────────────────────────────────────────────────────────────────────

/// Parsed VMDK SparseExtentHeader fields we actually use downstream.
///
/// `version` and `compress_algorithm` are validated inside `read_header`
/// before this struct is constructed; they are not propagated here because
/// the caller needs only the virtual disk size.
struct Header {
    /// Virtual disk capacity in 512-byte sectors.
    capacity: u64,
}

/// Read and parse the 512-byte SparseExtentHeader from `r`.
///
/// `r` must be positioned at the start of the file (byte 0) when called.
/// On success the cursor is left at byte 512.
fn read_header<R: Read + Seek>(r: &mut R) -> Result<Header, Error> {
    r.seek(SeekFrom::Start(0))?;

    let mut buf = [0u8; 512];
    r.read_exact(&mut buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;

    // Check magic (little-endian u32 at offset 0).
    let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if magic != VMDK_MAGIC {
        return Err(Error::BadMagic);
    }

    // Version (little-endian u32 at offset 4). Only 1 and 3 are supported.
    let version = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if version != 1 && version != 3 {
        return Err(Error::UnsupportedVersion(version));
    }

    // Capacity in sectors (little-endian u64 at offset 12).
    let capacity = u64::from_le_bytes([
        buf[12], buf[13], buf[14], buf[15], buf[16], buf[17], buf[18], buf[19],
    ]);

    // compress_algorithm (u8 at offset 77). 1 = deflate (streamOptimized);
    // rejected because grain decompression would require a codec dep.
    if buf[77] == 1 {
        return Err(Error::Compressed);
    }

    Ok(Header { capacity })
}

// ── Detection ─────────────────────────────────────────────────────────────────

/// Detect whether `r` is a VMDK sparse extent by checking the magic at offset 0.
///
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
    // Need at least the 512-byte header.
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < HEADER_SIZE {
        return Err(Error::TooShort);
    }

    // Read only the 4-byte magic for detection.
    r.seek(SeekFrom::Start(0))?;
    let mut magic_bytes = [0u8; 4];
    match r.read_exact(&mut magic_bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    let magic = u32::from_le_bytes(magic_bytes);
    if magic != VMDK_MAGIC {
        return Err(Error::BadMagic);
    }

    Ok(())
}

// ── Tree building ─────────────────────────────────────────────────────────────

/// Parse the VMDK sparse extent at `r` and return a [`TreeNode`] tree.
///
/// The tree always has the shape:
///
/// ```text
/// / (dir)
/// └── disk.vmdk (file, size = virtual disk size in bytes)
/// ```
///
/// `file_location` is always `None` because VMDK grain data is addressed
/// through a grain directory / grain table indirection and is not
/// contiguous in the file. The virtual size is reported via `file_length`.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let header = read_header(r)?;

    // Virtual disk size in bytes.
    let virtual_size = header.capacity * SECTOR_SIZE;

    let mut root = TreeNode::new_directory("/".to_string());

    // file_location = None: grain data is fragmented via the grain directory.
    let mut disk_node = TreeNode::new_file("disk.vmdk".to_string(), virtual_size);
    disk_node.file_length = Some(virtual_size);

    root.add_child(disk_node);
    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Minimal sparse VMDK builder ──────────────────────────────────────
    //
    // Builds a 512-byte SparseExtentHeader with:
    //   magic          = 0x564d444b  (little-endian)
    //   version        = 1
    //   capacity       = 2048 sectors (= 1 MiB)
    //   grain_size     = 128 sectors (= 64 KB)
    //   num_gtes_per_gt = 512
    //   compress_algorithm = 0 (none)
    //
    // Followed by enough zero padding that the file is at least 512 bytes.

    fn build_sparse_header(version: u32, capacity: u64, compress_algorithm: u8) -> [u8; 512] {
        let mut buf = [0u8; 512];

        // magic (LE u32 at [0]).
        buf[0..4].copy_from_slice(&VMDK_MAGIC.to_le_bytes());

        // version (LE u32 at [4]).
        buf[4..8].copy_from_slice(&version.to_le_bytes());

        // flags (LE u32 at [8]) — 0 for our test images.
        buf[8..12].copy_from_slice(&0u32.to_le_bytes());

        // capacity (LE u64 at [12]).
        buf[12..20].copy_from_slice(&capacity.to_le_bytes());

        // grain_size = 128 sectors (LE u64 at [20]).
        buf[20..28].copy_from_slice(&128u64.to_le_bytes());

        // descriptor_offset = 0 (LE u64 at [28]).
        buf[28..36].copy_from_slice(&0u64.to_le_bytes());

        // descriptor_size = 0 (LE u64 at [36]).
        buf[36..44].copy_from_slice(&0u64.to_le_bytes());

        // num_gtes_per_gt = 512 (LE u32 at [44]).
        buf[44..48].copy_from_slice(&512u32.to_le_bytes());

        // rgd_offset = 1 (LE u64 at [48]) — not meaningful for detection.
        buf[48..56].copy_from_slice(&1u64.to_le_bytes());

        // gd_offset = 2 (LE u64 at [56]) — not meaningful for detection.
        buf[56..64].copy_from_slice(&2u64.to_le_bytes());

        // overhead = 128 (LE u64 at [64]).
        buf[64..72].copy_from_slice(&128u64.to_le_bytes());

        // unclean_shutdown = 0 (u8 at [72]).
        buf[72] = 0;

        // newline markers per spec (u8 at [73..77]).
        buf[73] = b'\n';
        buf[74] = b' ';
        buf[75] = b'\r';
        buf[76] = b'\n';

        // compress_algorithm (u8 at [77]).
        buf[77] = compress_algorithm;

        // Bytes [78..512] are zero-padded.
        buf
    }

    /// Build a minimal sparse VMDK image (just the 512-byte header).
    fn build_vmdk(version: u32, capacity_sectors: u64, compress_algorithm: u8) -> Vec<u8> {
        let header = build_sparse_header(version, capacity_sectors, compress_algorithm);
        header.to_vec()
    }

    // ── Detection tests ───────────────────────────────────────────────────

    #[test]
    fn detect_sparse_vmdk_ok() {
        // capacity = 2048 sectors = 1 MiB, version = 1, no compression.
        let img = build_vmdk(1, 2048, 0);
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok(), "should detect sparse VMDK");
    }

    #[test]
    fn detect_restores_position() {
        let img = build_vmdk(1, 2048, 0);
        let mut c = Cursor::new(&img);
        c.seek(SeekFrom::Start(7)).unwrap();
        detect(&mut c).unwrap();
        assert_eq!(
            c.stream_position().unwrap(),
            7,
            "detect() must restore the stream position"
        );
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let img = vec![0u8; 512];
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

    // ── Version tests ─────────────────────────────────────────────────────

    #[test]
    fn version_3_accepted() {
        let img = build_vmdk(3, 2048, 0);
        let mut c = Cursor::new(&img);
        assert!(
            detect_and_parse(&mut c).is_ok(),
            "version=3 should be accepted"
        );
    }

    #[test]
    fn version_2_rejected() {
        let img = build_vmdk(2, 2048, 0);
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::UnsupportedVersion(2))),
            "version=2 should return UnsupportedVersion(2)"
        );
    }

    // ── Compression rejection test ────────────────────────────────────────

    #[test]
    fn compressed_vmdk_rejected() {
        // compress_algorithm = 1 (deflate / streamOptimized).
        let img = build_vmdk(1, 2048, 1);
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::Compressed)),
            "compress_algorithm=1 should return Error::Compressed"
        );
    }

    // ── Parse / tree shape tests ──────────────────────────────────────────

    #[test]
    fn parse_tree_shape() {
        // 2048 sectors * 512 bytes = 1 MiB virtual disk.
        let img = build_vmdk(1, 2048, 0);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse sparse VMDK");

        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 1);

        let child = &root.children[0];
        assert_eq!(child.name, "disk.vmdk");
        assert!(!child.is_directory);
        assert!(child.children.is_empty());
    }

    #[test]
    fn parse_virtual_size() {
        // 4096 sectors * 512 bytes = 2 MiB virtual disk.
        let capacity_sectors: u64 = 4096;
        let expected_size = capacity_sectors * 512;

        let img = build_vmdk(1, capacity_sectors, 0);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse sparse VMDK");

        let disk = &root.children[0];
        assert_eq!(
            disk.size, expected_size,
            "disk.vmdk size should equal capacity_sectors * 512"
        );
        assert_eq!(
            disk.file_length,
            Some(expected_size),
            "disk.vmdk file_length should equal capacity_sectors * 512"
        );
    }

    #[test]
    fn parse_file_location_is_none() {
        // Grain data is fragmented via the grain directory; location is always None.
        let img = build_vmdk(1, 2048, 0);
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse sparse VMDK");
        let disk = &root.children[0];
        assert_eq!(
            disk.file_location, None,
            "sparse VMDK disk.vmdk should have file_location=None (grain directory indirection)"
        );
    }
}
