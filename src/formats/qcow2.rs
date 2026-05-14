//! QCOW2 (QEMU Copy-On-Write v2) container reader (`qcow2` feature).
//!
//! QCOW2 is QEMU's native virtual disk format, documented in the QEMU
//! source tree (`docs/interop/qcow2.txt`) and in the libvirt wiki.
//! This reader handles:
//!
//! - **Version 2 and 3 QCOW2 images**: the 72-byte common header is
//!   parsed for both; version-3 extended fields are accepted but not
//!   acted upon.
//! - **Unencrypted images only**: `encryption_method` must be 0.
//!   AES (legacy, method=1) and LUKS (method=2) both return
//!   [`Error::Encrypted`].
//!
//! L1/L2 table traversal is not performed — we report `disk_size`
//! as the virtual disk size and set `file_location = None` because
//! QCOW2 data is addressed through copy-on-write cluster tables, never
//! as a single contiguous extent.
//!
//! ## Header layout (big-endian, bytes 0..72)
//!
//! ```text
//!  [0]   u32  magic              = 0x514649fb  ("QFI\xfb")
//!  [4]   u32  version            = 2 or 3
//!  [8]   u64  backing_file_offset
//! [16]   u32  backing_file_size
//! [20]   u32  cluster_bits       // cluster_size = 1 << cluster_bits (9..=21)
//! [24]   u64  disk_size          // virtual disk size in bytes
//! [32]   u32  encryption_method  // 0=none, 1=AES, 2=LUKS
//! [36]   u32  l1_size
//! [40]   u64  l1_table_offset
//! [48]   u64  refcount_table_offset
//! [56]   u32  refcount_table_clusters
//! [60]   u32  nb_snapshots
//! [64]   u64  snapshots_offset
//! ```

use std::io::{self, Read, Seek, SeekFrom};

use crate::tree::TreeNode;

/// Magic bytes at offset 0 of every QCOW2 file.
///
/// Encodes `"QFI\xfb"` as a big-endian `u32`.
const QCOW2_MAGIC: u32 = 0x5146_49fb;

/// Minimum header size we must read (covers the common v2/v3 fields).
const HEADER_SIZE: usize = 72;

/// Minimum valid `cluster_bits` (cluster_size = 512 bytes).
const CLUSTER_BITS_MIN: u32 = 9;

/// Maximum valid `cluster_bits` (cluster_size = 2 MiB).
const CLUSTER_BITS_MAX: u32 = 21;

// ── Error type ────────────────────────────────────────────────────────────────

/// Reasons [`detect`] or [`detect_and_parse`] can fail.
#[derive(Debug)]
pub enum Error {
    /// File is shorter than the 72-byte QCOW2 header.
    TooShort,
    /// Magic bytes at offset 0 are not `0x514649fb`.
    BadMagic,
    /// `version` field is not 2 or 3. The observed value is included.
    UnsupportedVersion(u32),
    /// `encryption_method` is non-zero (AES or LUKS). Encrypted images
    /// cannot be read without the key.
    Encrypted,
    /// `cluster_bits` is outside `9..=21`. The observed value is included.
    BadClusterBits(u32),
    /// Underlying I/O error.
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => {
                write!(f, "QCOW2 file is shorter than the 72-byte header")
            }
            Error::BadMagic => {
                write!(f, "QCOW2 magic 0x514649fb not found at offset 0")
            }
            Error::UnsupportedVersion(v) => {
                write!(f, "QCOW2 version {v} is not supported (only 2 and 3)")
            }
            Error::Encrypted => {
                write!(f, "QCOW2 image is encrypted; cannot read without key")
            }
            Error::BadClusterBits(b) => {
                write!(f, "QCOW2 cluster_bits {b} is out of range (must be 9..=21)")
            }
            Error::Io(e) => write!(f, "QCOW2 I/O error: {e}"),
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

/// Parsed QCOW2 header — only the fields used after validation.
///
/// `version`, `cluster_bits`, and `encryption_method` are validated
/// inside `read_header` and not stored; only `disk_size` is returned.
struct Header {
    /// `disk_size`: virtual disk size in bytes (offset 24).
    disk_size: u64,
}

/// Read and validate a QCOW2 header from the current stream position.
///
/// Expects the stream to be positioned at byte 0 before the call.
fn read_header<R: Read + Seek>(r: &mut R) -> Result<Header, Error> {
    let mut buf = [0u8; HEADER_SIZE];
    r.read_exact(&mut buf).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;

    // Verify magic.
    let magic = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if magic != QCOW2_MAGIC {
        return Err(Error::BadMagic);
    }

    // Version must be 2 or 3.
    let version = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if version != 2 && version != 3 {
        return Err(Error::UnsupportedVersion(version));
    }

    // cluster_bits at [20].
    let cluster_bits = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
    if !(CLUSTER_BITS_MIN..=CLUSTER_BITS_MAX).contains(&cluster_bits) {
        return Err(Error::BadClusterBits(cluster_bits));
    }

    // disk_size at [24].
    let disk_size = u64::from_be_bytes([
        buf[24], buf[25], buf[26], buf[27], buf[28], buf[29], buf[30], buf[31],
    ]);

    // encryption_method at [32].
    let encryption_method = u32::from_be_bytes([buf[32], buf[33], buf[34], buf[35]]);
    if encryption_method != 0 {
        return Err(Error::Encrypted);
    }

    Ok(Header { disk_size })
}

// ── Detection ─────────────────────────────────────────────────────────────────

/// Detect whether `r` is a QCOW2 image by checking the magic bytes.
///
/// Reads 4 bytes at offset 0 and checks for `0x514649fb`. The stream
/// position is restored on both success and failure.
///
/// Returns `Ok(())` if detection succeeds.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let pos = r.stream_position()?;
    let result = detect_inner(r);
    let _ = r.seek(SeekFrom::Start(pos));
    result
}

fn detect_inner<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    r.seek(SeekFrom::Start(0))?;

    let mut magic_buf = [0u8; 4];
    match r.read_exact(&mut magic_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    let magic = u32::from_be_bytes(magic_buf);
    if magic != QCOW2_MAGIC {
        return Err(Error::BadMagic);
    }

    Ok(())
}

// ── Tree building ─────────────────────────────────────────────────────────────

/// Parse the QCOW2 image at `r` and return a [`TreeNode`] tree.
///
/// The tree always has the shape:
///
/// ```text
/// / (dir)
/// └── disk.qcow2 (file, size = virtual disk size)
/// ```
///
/// `file_location` is always `None`: QCOW2 data is addressed through
/// L1/L2 cluster tables with copy-on-write semantics, never as a
/// single contiguous extent. `file_length` reports the virtual disk
/// size from the header.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    r.seek(SeekFrom::Start(0))?;
    let hdr = read_header(r)?;
    let disk_size = hdr.disk_size;

    let mut root = TreeNode::new_directory("/".to_string());

    // file_location = None: data is addressed via L1/L2 tables with
    // copy-on-write semantics, not as a contiguous byte range.
    let mut disk_node = TreeNode::new_file("disk.qcow2".to_string(), disk_size);
    disk_node.file_length = Some(disk_size);

    root.add_child(disk_node);
    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Minimal QCOW2 v2 header builder ──────────────────────────────────
    //
    // Layout: 72-byte header at offset 0 (no backing file, no snapshots,
    // no L1 table data — we only parse the header fields).
    //
    // Field defaults used in build_header():
    //   magic              = 0x514649fb
    //   version            = 2
    //   backing_file_offset = 0
    //   backing_file_size  = 0
    //   cluster_bits       = 16  (cluster_size = 65536)
    //   disk_size          = 10485760 (10 MiB)
    //   encryption_method  = 0  (none)
    //   l1_size            = 1
    //   l1_table_offset    = 196608
    //   refcount_table_offset = 65536
    //   refcount_table_clusters = 1
    //   nb_snapshots       = 0
    //   snapshots_offset   = 0

    const DEFAULT_VERSION: u32 = 2;
    const DEFAULT_CLUSTER_BITS: u32 = 16;
    const DEFAULT_DISK_SIZE: u64 = 10 * 1024 * 1024; // 10 MiB
    const DEFAULT_ENCRYPTION: u32 = 0;

    fn build_header(
        version: u32,
        cluster_bits: u32,
        disk_size: u64,
        encryption_method: u32,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; HEADER_SIZE];

        // [0] magic
        buf[0..4].copy_from_slice(&QCOW2_MAGIC.to_be_bytes());

        // [4] version
        buf[4..8].copy_from_slice(&version.to_be_bytes());

        // [8] backing_file_offset = 0
        // [16] backing_file_size = 0  (already zero)

        // [20] cluster_bits
        buf[20..24].copy_from_slice(&cluster_bits.to_be_bytes());

        // [24] disk_size
        buf[24..32].copy_from_slice(&disk_size.to_be_bytes());

        // [32] encryption_method
        buf[32..36].copy_from_slice(&encryption_method.to_be_bytes());

        // [36] l1_size = 1
        buf[36..40].copy_from_slice(&1u32.to_be_bytes());

        // [40] l1_table_offset = 3 * 65536 = 196608
        buf[40..48].copy_from_slice(&196608u64.to_be_bytes());

        // [48] refcount_table_offset = 65536
        buf[48..56].copy_from_slice(&65536u64.to_be_bytes());

        // [56] refcount_table_clusters = 1
        buf[56..60].copy_from_slice(&1u32.to_be_bytes());

        // [60] nb_snapshots = 0
        // [64] snapshots_offset = 0  (already zero)

        buf
    }

    fn minimal_image(
        version: u32,
        cluster_bits: u32,
        disk_size: u64,
        encryption_method: u32,
    ) -> Vec<u8> {
        build_header(version, cluster_bits, disk_size, encryption_method)
    }

    // ── Detection tests ───────────────────────────────────────────────────

    #[test]
    fn detect_v2_ok() {
        let img = minimal_image(
            DEFAULT_VERSION,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok(), "should detect valid QCOW2 v2");
    }

    #[test]
    fn detect_v3_ok() {
        let img = minimal_image(
            3,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok(), "should detect valid QCOW2 v3");
    }

    #[test]
    fn detect_restores_position() {
        let img = minimal_image(
            DEFAULT_VERSION,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
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
        let img = vec![0u8; HEADER_SIZE];
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect(&mut c), Err(Error::BadMagic)),
            "all-zeros should fail with BadMagic"
        );
    }

    #[test]
    fn detect_rejects_too_short() {
        let img = vec![0u8; 3]; // fewer than 4 bytes
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect(&mut c), Err(Error::TooShort)),
            "3-byte image should fail with TooShort"
        );
    }

    // ── Parse tests ───────────────────────────────────────────────────────

    #[test]
    fn parse_v2_tree_shape() {
        let img = minimal_image(
            DEFAULT_VERSION,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse QCOW2 v2");

        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 1);

        let child = &root.children[0];
        assert_eq!(child.name, "disk.qcow2");
        assert!(!child.is_directory);
        assert!(child.children.is_empty());
    }

    #[test]
    fn parse_disk_size_reported() {
        let disk_size: u64 = 20 * 1024 * 1024; // 20 MiB
        let img = minimal_image(
            DEFAULT_VERSION,
            DEFAULT_CLUSTER_BITS,
            disk_size,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse QCOW2");

        let disk = &root.children[0];
        assert_eq!(
            disk.size, disk_size,
            "disk.qcow2 size should equal disk_size from header"
        );
        assert_eq!(
            disk.file_length,
            Some(disk_size),
            "disk.qcow2 file_length should equal disk_size"
        );
    }

    #[test]
    fn parse_file_location_is_none() {
        let img = minimal_image(
            DEFAULT_VERSION,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse QCOW2");

        let disk = &root.children[0];
        assert_eq!(
            disk.file_location, None,
            "QCOW2 disk.qcow2 must have file_location=None (L1/L2 indirection)"
        );
    }

    #[test]
    fn parse_v3_ok() {
        let img = minimal_image(
            3,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        let root = detect_and_parse(&mut c).expect("parse QCOW2 v3");
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "disk.qcow2");
    }

    // ── Error tests ───────────────────────────────────────────────────────

    #[test]
    fn parse_rejects_unsupported_version() {
        let img = minimal_image(
            1,
            DEFAULT_CLUSTER_BITS,
            DEFAULT_DISK_SIZE,
            DEFAULT_ENCRYPTION,
        );
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::UnsupportedVersion(1))),
            "version=1 should fail with UnsupportedVersion(1)"
        );
    }

    #[test]
    fn parse_rejects_encrypted() {
        // encryption_method=1 (AES legacy).
        let img = minimal_image(DEFAULT_VERSION, DEFAULT_CLUSTER_BITS, DEFAULT_DISK_SIZE, 1);
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::Encrypted)),
            "encryption_method=1 should fail with Encrypted"
        );
    }

    #[test]
    fn parse_rejects_bad_cluster_bits() {
        // cluster_bits=8, which is below the minimum of 9.
        let img = minimal_image(DEFAULT_VERSION, 8, DEFAULT_DISK_SIZE, DEFAULT_ENCRYPTION);
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::BadClusterBits(8))),
            "cluster_bits=8 should fail with BadClusterBits(8)"
        );
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        assert!(
            format!("{}", Error::TooShort).contains("72")
                || format!("{}", Error::TooShort).contains("short")
        );
    }

    #[test]
    fn error_display_bad_magic() {
        assert!(
            format!("{}", Error::BadMagic).contains("514649fb")
                || format!("{}", Error::BadMagic).contains("magic")
        );
    }

    #[test]
    fn error_display_unsupported_version() {
        assert!(format!("{}", Error::UnsupportedVersion(5)).contains('5'));
    }

    #[test]
    fn error_display_encrypted() {
        assert!(format!("{}", Error::Encrypted).contains("encrypt"));
    }

    #[test]
    fn error_display_bad_cluster_bits() {
        assert!(format!("{}", Error::BadClusterBits(7)).contains('7'));
    }

    #[test]
    fn error_display_io() {
        let io = io::Error::other("disk");
        assert!(format!("{}", Error::Io(io)).contains("disk"));
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        assert!(Error::Io(io::Error::other("s")).source().is_some());
    }

    #[test]
    fn error_source_non_io() {
        use std::error::Error as StdError;
        assert!(Error::TooShort.source().is_none());
        assert!(Error::BadMagic.source().is_none());
        assert!(Error::Encrypted.source().is_none());
        assert!(Error::UnsupportedVersion(2).source().is_none());
        assert!(Error::BadClusterBits(5).source().is_none());
    }

    #[test]
    fn error_from_io_error() {
        let e = Error::from(io::Error::other("qcow2 test"));
        assert!(matches!(e, Error::Io(_)));
    }

    #[test]
    fn read_header_too_short_returns_error() {
        // Empty buffer → read_exact fails with UnexpectedEof → TooShort.
        let data: &[u8] = &[];
        let mut c = Cursor::new(data);
        assert!(matches!(read_header(&mut c), Err(Error::TooShort)));
    }

    #[test]
    fn read_header_bad_magic_returns_error() {
        // HEADER_SIZE bytes of zeros: magic=0 ≠ QCOW2_MAGIC → BadMagic.
        let data = vec![0u8; HEADER_SIZE];
        let mut c = Cursor::new(data);
        assert!(matches!(read_header(&mut c), Err(Error::BadMagic)));
    }
}
