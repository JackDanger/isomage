//! APFS (Apple File System) container reader (`apfs` feature).
//!
//! APFS is Apple's copy-on-write filesystem introduced in macOS 10.13 (High
//! Sierra) and now the default on all Apple platforms. A single APFS
//! **container** can host multiple logical **volumes** — each volume appears
//! as a subtree in the output tree.
//!
//! All multi-byte fields are **little-endian** (the APFS on-disk format is
//! entirely LE, unlike its HFS+ predecessor which is big-endian).
//!
//! ## Scope of this implementation
//!
//! - Detects an APFS container by the 4-byte magic `b"NXSB"` at block
//!   offset 32 of the container (offset 32 from the start of the file).
//! - Parses the NX Superblock (container superblock) at block 0: reads
//!   `block_size` and the `fs_oid[]` volume-OID array.
//! - Treats each non-zero `fs_oid` entry as a **physical block address**
//!   and reads the APSB (volume superblock) from that block.
//! - Extracts the null-terminated UTF-8 volume name from each APSB.
//! - Returns a [`TreeNode`] tree with `"/"` at the root and one directory
//!   child per APFS volume found.
//!
//! ## What is NOT implemented
//!
//! Full APFS B-tree traversal (to list files within each volume) is out of
//! scope for this initial reader. The volume-level tree matches the view
//! presented by 7-Zip and other container-aware tools. Implementing full
//! per-volume FS trees would require the APFS object map (omap B-tree) and
//! the volume FS B-tree — deferred to a future PR.

use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Magic numbers ─────────────────────────────────────────────────────────────

/// NX Superblock magic: `b"NXSB"` stored as a LE u32.
///
/// Bytes: 'N'=0x4e, 'X'=0x58, 'S'=0x53, 'B'=0x42.
/// As LE u32: 0x4253584e.
const NXSB_MAGIC: u32 = 0x4253_584e;

/// APSB (volume superblock) magic: `b"APSB"` stored as a LE u32.
///
/// Bytes: 'A'=0x41, 'P'=0x50, 'S'=0x53, 'B'=0x42.
/// As LE u32: 0x42535041.
const APSB_MAGIC: u32 = 0x4253_5041;

// ── On-disk layout offsets ─────────────────────────────────────────────────────

/// Every APFS object starts with a 32-byte object header. The NX Superblock
/// magic field immediately follows at block offset 32.
const NXSB_MAGIC_OFFSET: u64 = 32;

/// Offset of the `block_size` u32 field within the NX Superblock block.
const NXSB_BLOCK_SIZE_OFFSET: u64 = 36;

/// Offset of the `fs_oid[0]` u64 field within the NX Superblock block.
/// The array holds up to 100 volume OIDs (terminated by 0).
const NXSB_FS_OID_OFFSET: u64 = 180;

/// Maximum number of volume slots in the NX Superblock `fs_oid` array.
const NXSB_MAX_FS_OIDS: usize = 100;

/// Offset of the APSB magic u32 within a volume superblock block.
const APSB_MAGIC_OFFSET: u64 = 32;

/// Offset of the volume name (`apfs_volname`) within a volume superblock block.
///
/// Layout from block start:
///   [0..32]   object header (32 bytes)
///   [32..36]  APSB magic
///   ...
///   [284..316] apfs_formatted_by (32 bytes)
///   [316..572] apfs_modified_by (8 × 32 = 256 bytes)
///   [572..828] apfs_volname (256 bytes, null-terminated UTF-8)
const APSB_VOLNAME_OFFSET: u64 = 572;

/// Byte length of the `apfs_volname` field (includes the null terminator).
const APSB_VOLNAME_LEN: usize = 256;

/// Minimum block size we will accept. Any value smaller than 4096 is not a
/// valid APFS block size per the Apple APFS reference.
const MIN_BLOCK_SIZE: u32 = 4096;

/// Maximum block size we will accept. Real APFS containers use 4096 or 65536.
const MAX_BLOCK_SIZE: u32 = 65536;

// ── Error type ─────────────────────────────────────────────────────────────────

/// Errors that can arise while detecting or parsing an APFS container.
#[derive(Debug)]
pub enum Error {
    /// The stream is too short to contain a valid APFS container superblock.
    TooShort,
    /// The 4-byte magic at offset 32 was not `b"NXSB"`.
    BadMagic,
    /// `block_size` was 0, not a power of 2, or outside the valid range.
    BadBlockSize,
    /// An underlying I/O error occurred.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image too short for an APFS container superblock"),
            Error::BadMagic => write!(
                f,
                "APFS NX magic 'NXSB' (0x4253584e) not found at offset 32"
            ),
            Error::BadBlockSize => write!(
                f,
                "APFS block_size is 0, not a power of two, or outside 4096–65536"
            ),
            Error::Io(e) => write!(f, "APFS I/O error: {e}"),
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

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

// ── Parsed NX Superblock ───────────────────────────────────────────────────────

/// Fields from the NX Superblock (container superblock) that we need.
#[derive(Debug)]
pub struct NxSuperblock {
    /// Size of one block in bytes (always a power of two; typically 4096).
    pub block_size: u32,
    /// Non-zero OIDs of APFS volumes within this container.
    /// Each entry is a physical block number for the corresponding APSB.
    pub fs_oids: Vec<u64>,
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Detect whether the reader contains an APFS container.
///
/// Seeks to offset 32, reads the 4-byte NX magic, then **restores the
/// cursor** to its position before the call. Returns `Ok(())` on a match.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let saved = r.stream_position()?;
    let result = do_detect(r);
    r.seek(SeekFrom::Start(saved))?;
    result
}

fn do_detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    r.seek(SeekFrom::Start(NXSB_MAGIC_OFFSET))?;
    let mut buf = [0u8; 4];
    match r.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }
    let magic = u32::from_le_bytes(buf);
    if magic != NXSB_MAGIC {
        return Err(Error::BadMagic);
    }
    Ok(())
}

/// Read the NX Superblock at the start of the stream.
///
/// Validates the magic, reads `block_size`, and collects non-zero entries
/// from the `fs_oid[100]` array.
pub fn read_nx_superblock<R: Read + Seek>(r: &mut R) -> Result<NxSuperblock, Error> {
    // ── magic ──
    r.seek(SeekFrom::Start(NXSB_MAGIC_OFFSET))?;
    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;
    let magic = u32::from_le_bytes(buf4);
    if magic != NXSB_MAGIC {
        return Err(Error::BadMagic);
    }

    // ── block_size ──
    r.seek(SeekFrom::Start(NXSB_BLOCK_SIZE_OFFSET))?;
    r.read_exact(&mut buf4).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;
    let block_size = u32::from_le_bytes(buf4);
    if !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
        || (block_size & (block_size - 1)) != 0
    {
        return Err(Error::BadBlockSize);
    }

    // ── fs_oid array ──
    r.seek(SeekFrom::Start(NXSB_FS_OID_OFFSET))?;
    let mut fs_oids: Vec<u64> = Vec::new();
    let mut buf8 = [0u8; 8];
    for _ in 0..NXSB_MAX_FS_OIDS {
        match r.read_exact(&mut buf8) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(Error::Io(e)),
        }
        let oid = u64::from_le_bytes(buf8);
        if oid == 0 {
            break;
        }
        fs_oids.push(oid);
    }

    Ok(NxSuperblock {
        block_size,
        fs_oids,
    })
}

/// Read the APSB (volume superblock) at the given physical block number and
/// return the volume name. Returns `None` if the block has no APSB magic or
/// the name is not valid UTF-8.
fn read_volume_name<R: Read + Seek>(r: &mut R, block_num: u64, block_size: u32) -> Option<String> {
    let block_start = block_num * block_size as u64;

    // ── validate APSB magic ──
    let magic_offset = block_start + APSB_MAGIC_OFFSET;
    if r.seek(SeekFrom::Start(magic_offset)).is_err() {
        return None;
    }
    let mut buf4 = [0u8; 4];
    r.read_exact(&mut buf4).ok()?;
    let magic = u32::from_le_bytes(buf4);
    if magic != APSB_MAGIC {
        return None;
    }

    // ── read volume name ──
    let name_offset = block_start + APSB_VOLNAME_OFFSET;
    if r.seek(SeekFrom::Start(name_offset)).is_err() {
        return None;
    }
    let mut name_buf = [0u8; APSB_VOLNAME_LEN];
    r.read_exact(&mut name_buf).ok()?;

    // Find null terminator and decode as UTF-8.
    let end = name_buf
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(APSB_VOLNAME_LEN);
    let name = std::str::from_utf8(&name_buf[..end]).ok()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Detect an APFS container, then parse its volume list into a [`TreeNode`] tree.
///
/// The tree root is `"/"` (a directory) representing the container. Each APFS
/// volume found via the NX Superblock's `fs_oid[]` array becomes a directory
/// child named after the volume's `apfs_volname` field.
///
/// Per-volume file trees are **not** traversed in this implementation: only
/// the container-level volume list is returned. Full FS B-tree traversal is
/// deferred to a future PR.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let nx = read_nx_superblock(r)?;
    let mut root = TreeNode::new_directory("/".to_string());

    for &fs_oid in &nx.fs_oids {
        // For the container-level omap, fs_oid entries are physical block
        // addresses — we read the APSB directly at that block.
        let name = read_volume_name(r, fs_oid, nx.block_size)
            .unwrap_or_else(|| format!("volume_{fs_oid}"));
        root.add_child(TreeNode::new_directory(name));
    }

    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Seek, SeekFrom};

    // ── In-memory image builder ────────────────────────────────────────────────

    /// Build a minimal 2-block in-memory APFS container image.
    ///
    /// Layout:
    ///   Block 0 — NX Superblock: magic at offset 32, block_size=4096,
    ///             fs_oid[0] = 1 (block 1 holds the single volume).
    ///   Block 1 — APSB: magic at offset 32, volname at offset 572.
    fn make_apfs_image(volname: &str) -> Vec<u8> {
        const BLOCK_SIZE: usize = 4096;
        let mut img = vec![0u8; BLOCK_SIZE * 2];

        // ── Block 0: NX Superblock ──
        // magic at offset 32
        img[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
        // block_size at offset 36
        img[36..40].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        // fs_oid[0] at offset 180 → block 1
        img[180..188].copy_from_slice(&1u64.to_le_bytes());
        // fs_oid[1] = 0 (terminator — already zero from vec init)

        // ── Block 1: APSB ──
        // magic at block_start + 32
        img[BLOCK_SIZE + 32..BLOCK_SIZE + 36].copy_from_slice(&APSB_MAGIC.to_le_bytes());
        // volname at block_start + 572
        let name_bytes = volname.as_bytes();
        let copy_len = name_bytes.len().min(APSB_VOLNAME_LEN - 1);
        img[BLOCK_SIZE + 572..BLOCK_SIZE + 572 + copy_len].copy_from_slice(&name_bytes[..copy_len]);
        // null terminator already present (vec is zero-initialised)

        img
    }

    // ── Detection tests ────────────────────────────────────────────────────────

    #[test]
    fn detect_valid_apfs() {
        let img = make_apfs_image("TestVol");
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok(), "should detect valid APFS magic");
    }

    #[test]
    fn detect_restores_cursor() {
        let img = make_apfs_image("TestVol");
        let mut c = Cursor::new(&img);
        c.seek(SeekFrom::Start(42)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(
            c.stream_position().unwrap(),
            42,
            "detect must restore the cursor position"
        );
    }

    #[test]
    fn detect_restores_cursor_on_failure() {
        let img = vec![0u8; 512];
        let mut c = Cursor::new(&img);
        c.seek(SeekFrom::Start(7)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(
            c.stream_position().unwrap(),
            7,
            "detect must restore cursor even on failure"
        );
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let mut img = make_apfs_image("TestVol");
        // Overwrite the NXSB magic with garbage.
        img[32..36].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect(&mut c), Err(Error::BadMagic)),
            "should reject non-APFS magic"
        );
    }

    #[test]
    fn detect_rejects_too_short() {
        let img = vec![0u8; 10];
        let mut c = Cursor::new(&img);
        assert!(
            matches!(detect(&mut c), Err(Error::TooShort)),
            "should return TooShort for a truncated image"
        );
    }

    // ── NX Superblock parsing tests ────────────────────────────────────────────

    #[test]
    fn nx_superblock_block_size() {
        let img = make_apfs_image("TestVol");
        let mut c = Cursor::new(&img);
        let nx = read_nx_superblock(&mut c).expect("parse NX superblock");
        assert_eq!(nx.block_size, 4096, "block_size should be 4096");
    }

    #[test]
    fn nx_superblock_fs_oid_count() {
        let img = make_apfs_image("TestVol");
        let mut c = Cursor::new(&img);
        let nx = read_nx_superblock(&mut c).expect("parse NX superblock");
        assert_eq!(
            nx.fs_oids.len(),
            1,
            "should find exactly one non-zero fs_oid"
        );
        assert_eq!(nx.fs_oids[0], 1, "fs_oid[0] should be 1");
    }

    #[test]
    fn nx_superblock_rejects_bad_block_size_zero() {
        let mut img = make_apfs_image("TestVol");
        // Set block_size to 0.
        img[36..40].copy_from_slice(&0u32.to_le_bytes());
        let mut c = Cursor::new(&img);
        assert!(
            matches!(read_nx_superblock(&mut c), Err(Error::BadBlockSize)),
            "block_size=0 should be rejected"
        );
    }

    #[test]
    fn nx_superblock_rejects_non_power_of_two_block_size() {
        let mut img = make_apfs_image("TestVol");
        // Set block_size to 5000 (not a power of 2).
        img[36..40].copy_from_slice(&5000u32.to_le_bytes());
        let mut c = Cursor::new(&img);
        assert!(
            matches!(read_nx_superblock(&mut c), Err(Error::BadBlockSize)),
            "block_size=5000 (not a power of 2) should be rejected"
        );
    }

    // ── Full parse tests ───────────────────────────────────────────────────────

    #[test]
    fn detect_and_parse_volume_name() {
        let img = make_apfs_image("Macintosh HD");
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).expect("detect_and_parse should succeed");

        assert_eq!(tree.name, "/", "root node must be named '/'");
        assert!(tree.is_directory, "root must be a directory");
        assert_eq!(
            tree.children.len(),
            1,
            "should have exactly one volume child"
        );
        assert_eq!(
            tree.children[0].name, "Macintosh HD",
            "volume name should match"
        );
        assert!(
            tree.children[0].is_directory,
            "volume node must be a directory"
        );
    }

    #[test]
    fn detect_and_parse_no_volumes() {
        // An image with no non-zero fs_oid entries should return an empty root.
        const BLOCK_SIZE: usize = 4096;
        let mut img = vec![0u8; BLOCK_SIZE];
        img[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
        img[36..40].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        // All fs_oid entries remain zero → no volumes.

        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).expect("detect_and_parse with no volumes");
        assert_eq!(tree.name, "/");
        assert!(tree.children.is_empty(), "no volumes → no children");
    }

    #[test]
    fn detect_and_parse_bad_magic_rejected() {
        let img = vec![0u8; 4096];
        let mut c = Cursor::new(&img);
        assert!(
            matches!(
                detect_and_parse(&mut c),
                Err(Error::BadMagic) | Err(Error::BadBlockSize)
            ),
            "all-zeros image should be rejected"
        );
    }

    #[test]
    fn volume_node_has_no_file_location() {
        let img = make_apfs_image("Preboot");
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).expect("parse");
        assert!(
            tree.children[0].file_location.is_none(),
            "volume directory node should have no file_location"
        );
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        let msg = format!("{}", Error::TooShort);
        assert!(
            msg.contains("too short") || msg.contains("short"),
            "got: {msg}"
        );
    }

    #[test]
    fn error_display_bad_magic() {
        let msg = format!("{}", Error::BadMagic);
        assert!(msg.contains("NXSB") || msg.contains("magic"), "got: {msg}");
    }

    #[test]
    fn error_display_bad_block_size() {
        let msg = format!("{}", Error::BadBlockSize);
        assert!(
            msg.contains("block_size") || msg.contains("block"),
            "got: {msg}"
        );
    }

    #[test]
    fn error_display_io() {
        let io = std::io::Error::other("disk error");
        let msg = format!("{}", Error::Io(io));
        assert!(msg.contains("disk error"), "got: {msg}");
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        let io = std::io::Error::other("src");
        assert!(Error::Io(io).source().is_some());
    }

    #[test]
    fn error_source_non_io() {
        use std::error::Error as StdError;
        assert!(Error::TooShort.source().is_none());
        assert!(Error::BadMagic.source().is_none());
        assert!(Error::BadBlockSize.source().is_none());
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn nx_superblock_rejects_block_size_too_large() {
        let mut img = make_apfs_image("TestVol");
        // Set block_size to 131072 (> MAX_BLOCK_SIZE=65536).
        img[36..40].copy_from_slice(&131072u32.to_le_bytes());
        let mut c = Cursor::new(&img);
        assert!(
            matches!(read_nx_superblock(&mut c), Err(Error::BadBlockSize)),
            "block_size > MAX should be rejected"
        );
    }

    #[test]
    fn detect_and_parse_bad_apsb_uses_fallback_name() {
        // fs_oid[0]=1 but block 1 has wrong APSB magic → fallback name "volume_1"
        const BLOCK_SIZE: usize = 4096;
        let mut img = vec![0u8; BLOCK_SIZE * 2];
        img[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
        img[36..40].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        // fs_oid[0] = 1 (at offset 180)
        img[180..188].copy_from_slice(&1u64.to_le_bytes());
        // Block 1: wrong APSB magic (zeros)
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).expect("fallback parse should succeed");
        assert_eq!(tree.children.len(), 1);
        assert_eq!(
            tree.children[0].name, "volume_1",
            "bad APSB → fallback name"
        );
    }

    #[test]
    fn detect_and_parse_empty_volume_name_uses_fallback() {
        // APSB with all-zero volname field → name.is_empty() → fallback
        const BLOCK_SIZE: usize = 4096;
        let mut img = vec![0u8; BLOCK_SIZE * 2];
        img[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
        img[36..40].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        img[180..188].copy_from_slice(&1u64.to_le_bytes()); // fs_oid[0] = 1 (offset 180)
                                                            // Block 1: valid APSB magic but empty volname (all zeros = null terminator at [0])
        img[BLOCK_SIZE + 32..BLOCK_SIZE + 36].copy_from_slice(&APSB_MAGIC.to_le_bytes());
        // volname at BLOCK_SIZE + APSB_VOLNAME_OFFSET is all zeros = empty string
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).expect("empty name parse should succeed");
        assert_eq!(tree.children.len(), 1);
        assert_eq!(
            tree.children[0].name, "volume_1",
            "empty volname → fallback"
        );
    }

    #[test]
    fn nx_superblock_multiple_volumes() {
        // Image with two non-zero fs_oids
        const BLOCK_SIZE: usize = 4096;
        let mut img = vec![0u8; BLOCK_SIZE * 3];
        img[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
        img[36..40].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        img[180..188].copy_from_slice(&1u64.to_le_bytes()); // fs_oid[0] = 1
        img[188..196].copy_from_slice(&2u64.to_le_bytes()); // fs_oid[1] = 2
        let mut c = Cursor::new(&img);
        let nx = read_nx_superblock(&mut c).expect("parse");
        assert_eq!(nx.fs_oids.len(), 2);
        assert_eq!(nx.fs_oids[0], 1);
        assert_eq!(nx.fs_oids[1], 2);
    }

    #[test]
    fn error_from_io_error() {
        let io = std::io::Error::other("apfs read failed");
        let e = Error::from(io);
        assert!(matches!(e, Error::Io(_)));
    }

    #[test]
    fn read_nx_superblock_magic_too_short_returns_error() {
        // 34 bytes: seek to offset 32 succeeds, but read_exact(4) gets only 2 bytes → TooShort.
        let data = vec![0u8; 34];
        let mut c = Cursor::new(data);
        assert!(matches!(read_nx_superblock(&mut c), Err(Error::TooShort)));
    }

    #[test]
    fn read_nx_superblock_block_size_too_short_returns_error() {
        // 38 bytes: magic at [32..36] is correct, seek to block_size at 36 succeeds,
        // but read_exact(4) from position 36 can only read 2 bytes → TooShort.
        let mut data = vec![0u8; 38];
        data[32..36].copy_from_slice(&NXSB_MAGIC.to_le_bytes());
        let mut c = Cursor::new(data);
        assert!(matches!(read_nx_superblock(&mut c), Err(Error::TooShort)));
    }
}
