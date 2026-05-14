//! GUID Partition Table (`gpt` feature).
//!
//! GPT is the UEFI-era successor to MBR. The header lives at LBA 1
//! (offset 512), starting with the ASCII signature `"EFI PART"`.
//! Partition entries (typically 128 bytes each, with a typical array
//! of 128 entries → 16 KiB of partition metadata) live at the LBA
//! pointed to by the header (usually LBA 2).
//!
//! Reference: UEFI spec §5 "GUID Partition Table (GPT) Disk Layout".
//!
//! ## Scope of this implementation
//!
//! - Reads the primary header at LBA 1. Does **not** check the backup
//!   header at the last LBA; a follow-on commit can add that. Real
//!   tools (parted, fdisk) cross-check both.
//! - Validates the `"EFI PART"` signature. Does **not** validate the
//!   header CRC32 or the partition-entry CRC32 — a TODO for the
//!   `simd` feature's CRC32 routines, once they exist.
//! - Reads the entry array sequentially. Skips entries whose
//!   `type_guid` is all zeros (the "empty slot" convention).
//! - Sector size is assumed to be 512 bytes. UEFI permits 4 KiB
//!   sectors (and the header records the logical block size via its
//!   LBA fields), but every real disk image we care about uses 512.
//!   Detecting 4K sectors is in scope for v3.1.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

const SECTOR_SIZE: u64 = 512;
const SIGNATURE: &[u8; 8] = b"EFI PART";

/// One parsed GPT partition entry. Inactive (all-zero) entries are
/// filtered out during parsing.
#[derive(Debug, Clone)]
pub struct Partition {
    /// 0-indexed entry slot.
    pub index: u32,
    /// Type GUID (raw 16 bytes, little-endian per UEFI §5.3.3).
    pub type_guid: [u8; 16],
    /// Per-partition unique GUID.
    pub unique_guid: [u8; 16],
    /// First byte of the partition in the image.
    pub start: u64,
    /// Length of the partition in bytes (inclusive of the last LBA).
    pub length: u64,
    /// UTF-16LE name from the partition entry, decoded lossily.
    pub name: String,
}

#[derive(Debug)]
pub enum Error {
    TooShort,
    /// Bytes 0..8 of LBA 1 were not `"EFI PART"`.
    BadSignature,
    /// Header reported a partition-entry size we can't parse.
    /// Practical implementations all use 128.
    UnsupportedEntrySize(u32),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image is shorter than the GPT header sector"),
            Error::BadSignature => write!(f, "GPT signature 'EFI PART' missing at LBA 1"),
            Error::UnsupportedEntrySize(n) => {
                write!(
                    f,
                    "unsupported GPT partition-entry size: {n} (expected 128)"
                )
            }
            Error::Io(e) => write!(f, "GPT I/O error: {e}"),
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

/// Parsed GPT header. Only the fields we need to walk the entry
/// array; the others (revision, header size, CRCs, disk GUID) are
/// available in a follow-on if/when we want them.
#[derive(Debug)]
pub struct Header {
    pub entries_lba: u64,
    pub num_entries: u32,
    pub entry_size: u32,
}

/// Read and parse the GPT header at LBA 1, then the partition entry
/// array it points at. Returns one [`Partition`] per non-empty entry.
pub fn parse(file: &mut File) -> Result<Vec<Partition>, Error> {
    let header = read_header(file)?;
    read_entries(file, &header)
}

fn read_header(file: &mut File) -> Result<Header, Error> {
    file.seek(SeekFrom::Start(SECTOR_SIZE))?;
    let mut sector = [0u8; SECTOR_SIZE as usize];
    if file.read(&mut sector)? < SECTOR_SIZE as usize {
        return Err(Error::TooShort);
    }
    parse_header_sector(&sector)
}

/// Pure parse of one 512-byte GPT-header sector. Exposed for tests
/// and for callers that have already read LBA 1 themselves.
pub fn parse_header_sector(sector: &[u8]) -> Result<Header, Error> {
    if sector.len() < SECTOR_SIZE as usize {
        return Err(Error::TooShort);
    }
    if &sector[0..8] != SIGNATURE {
        return Err(Error::BadSignature);
    }
    let entries_lba = u64::from_le_bytes(sector[72..80].try_into().unwrap());
    let num_entries = u32::from_le_bytes(sector[80..84].try_into().unwrap());
    let entry_size = u32::from_le_bytes(sector[84..88].try_into().unwrap());
    if entry_size < 128 {
        // The spec lets vendors grow the entry size for future fields;
        // shrinking it below 128 would put us in undefined territory.
        return Err(Error::UnsupportedEntrySize(entry_size));
    }
    Ok(Header {
        entries_lba,
        num_entries,
        entry_size,
    })
}

fn read_entries(file: &mut File, header: &Header) -> Result<Vec<Partition>, Error> {
    let total = (header.num_entries as u64).saturating_mul(header.entry_size as u64);
    // 16 KiB for a typical 128 × 128 layout. Cap to prevent a
    // pathological header from triggering a multi-gigabyte alloc.
    const MAX_ARRAY: u64 = 1024 * 1024; // 1 MiB worth of entries
    if total > MAX_ARRAY {
        return Err(Error::UnsupportedEntrySize(header.entry_size));
    }

    file.seek(SeekFrom::Start(header.entries_lba * SECTOR_SIZE))?;
    let mut buf = vec![0u8; total as usize];
    file.read_exact(&mut buf)?;

    let mut partitions = Vec::new();
    for i in 0..header.num_entries {
        let start = (i as u64 * header.entry_size as u64) as usize;
        let entry = &buf[start..start + header.entry_size as usize];
        let type_guid: [u8; 16] = entry[0..16].try_into().unwrap();
        // All-zero type GUID == empty slot. Filter early.
        if type_guid.iter().all(|&b| b == 0) {
            continue;
        }
        let unique_guid: [u8; 16] = entry[16..32].try_into().unwrap();
        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let last_lba = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        // last_lba is inclusive (UEFI §5.3.3 table 5-7).
        let length = last_lba
            .saturating_add(1)
            .saturating_sub(first_lba)
            .saturating_mul(SECTOR_SIZE);
        let name = decode_utf16le(&entry[56..56 + 72]);
        partitions.push(Partition {
            index: i,
            type_guid,
            unique_guid,
            start: first_lba * SECTOR_SIZE,
            length,
            name,
        });
    }
    Ok(partitions)
}

/// Decode the 72-byte UTF-16LE partition name. Stops at the first
/// NUL code unit; replaces invalid surrogates with U+FFFD. Empty
/// names come back as the empty string.
fn decode_utf16le(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

/// Convert a parsed partition list to the `TreeNode` shape `cat_node`
/// expects. Mirrors `mbr::to_tree`.
pub fn to_tree(partitions: &[Partition]) -> TreeNode {
    let mut root = TreeNode::new_directory("/".to_string());
    for p in partitions {
        // Prefer the human-readable name if non-empty; fall back to
        // an indexed slot name so the result is always uniquely
        // resolvable via TreeNode::find_node.
        let basename = if p.name.is_empty() {
            format!("partition-{}", p.index)
        } else {
            sanitize(&p.name)
        };
        let name = format!("{}-{}", basename, p.index);
        let node = if p.length == 0 {
            TreeNode::new_file(name, 0)
        } else {
            TreeNode::new_file_with_location(name, p.length, p.start, p.length)
        };
        root.add_child(node);
    }
    root.calculate_directory_size();
    root
}

/// Replace path separators and other troublesome characters in a
/// partition name so it can be used unsafely by `extract_node`.
/// Conservative: anything outside `[A-Za-z0-9._-]` becomes `_`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// One-call detect + parse + tree.
pub fn detect_and_parse(file: &mut File) -> Result<TreeNode, Error> {
    let parts = parse(file)?;
    Ok(to_tree(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_sector(entries_lba: u64, num_entries: u32, entry_size: u32) -> [u8; 512] {
        let mut s = [0u8; 512];
        s[0..8].copy_from_slice(SIGNATURE);
        s[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        s[80..84].copy_from_slice(&num_entries.to_le_bytes());
        s[84..88].copy_from_slice(&entry_size.to_le_bytes());
        s
    }

    #[test]
    fn rejects_missing_signature() {
        let s = [0u8; 512];
        assert!(matches!(parse_header_sector(&s), Err(Error::BadSignature)));
    }

    #[test]
    fn parses_minimal_header() {
        let s = header_sector(2, 128, 128);
        let h = parse_header_sector(&s).unwrap();
        assert_eq!(h.entries_lba, 2);
        assert_eq!(h.num_entries, 128);
        assert_eq!(h.entry_size, 128);
    }

    #[test]
    fn rejects_undersized_entry() {
        let s = header_sector(2, 128, 64);
        assert!(matches!(
            parse_header_sector(&s),
            Err(Error::UnsupportedEntrySize(64))
        ));
    }

    #[test]
    fn decodes_utf16_name() {
        // "Linux" in UTF-16LE, padded with zeros.
        let mut buf = vec![0u8; 72];
        let units: [u16; 5] = [0x4c, 0x69, 0x6e, 0x75, 0x78];
        for (i, u) in units.iter().enumerate() {
            buf[i * 2..i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_utf16le(&buf), "Linux");
    }

    #[test]
    fn sanitize_drops_slashes() {
        assert_eq!(sanitize("foo/bar"), "foo_bar");
        assert_eq!(sanitize("good-name_1.0"), "good-name_1.0");
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        let msg = format!("{}", Error::TooShort);
        assert!(msg.contains("short") || msg.contains("GPT"), "got: {msg}");
    }

    #[test]
    fn error_display_bad_signature() {
        let msg = format!("{}", Error::BadSignature);
        assert!(
            msg.contains("EFI PART") || msg.contains("signature"),
            "got: {msg}"
        );
    }

    #[test]
    fn error_display_unsupported_entry_size() {
        let msg = format!("{}", Error::UnsupportedEntrySize(64));
        assert!(msg.contains("64"), "got: {msg}");
    }

    #[test]
    fn error_display_io() {
        let io = std::io::Error::other("disk");
        let msg = format!("{}", Error::Io(io));
        assert!(msg.contains("disk"), "got: {msg}");
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        assert!(Error::Io(std::io::Error::other("s")).source().is_some());
    }

    #[test]
    fn error_source_non_io() {
        use std::error::Error as StdError;
        assert!(Error::TooShort.source().is_none());
        assert!(Error::BadSignature.source().is_none());
        assert!(Error::UnsupportedEntrySize(128).source().is_none());
    }

    #[test]
    fn error_from_io_error() {
        let e = Error::from(std::io::Error::other("gpt test"));
        assert!(matches!(e, Error::Io(_)));
    }

    #[test]
    fn parse_header_sector_too_short_returns_error() {
        let short = vec![0u8; 100];
        assert!(matches!(parse_header_sector(&short), Err(Error::TooShort)));
    }

    #[test]
    fn read_entries_exceeds_max_array_returns_error() {
        use std::io::Write;
        let path = std::env::temp_dir().join("isomage_gpt_max_array_test.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0u8; 512]).unwrap();
        drop(f);
        // num_entries=10000, entry_size=200 → total=2_000_000 > MAX_ARRAY(1_048_576)
        let header = Header {
            entries_lba: 0,
            num_entries: 10000,
            entry_size: 200,
        };
        let mut f = std::fs::File::open(&path).unwrap();
        let result = read_entries(&mut f, &header);
        std::fs::remove_file(&path).ok();
        assert!(matches!(result, Err(Error::UnsupportedEntrySize(200))));
    }

    #[test]
    fn read_entries_skips_empty_slots() {
        use std::io::Write;
        let entry_size: u32 = 128;
        let num_entries: u32 = 2;
        // Two entries: first all-zero (empty), second has a non-zero type GUID.
        let mut entries = vec![0u8; entry_size as usize * 2];
        entries[128..144].copy_from_slice(&[1u8; 16]); // type GUID
        entries[144..160].copy_from_slice(&[2u8; 16]); // unique GUID
        entries[160..168].copy_from_slice(&1u64.to_le_bytes()); // first_lba=1
        entries[168..176].copy_from_slice(&1u64.to_le_bytes()); // last_lba=1
        let path = std::env::temp_dir().join("isomage_gpt_skip_empty_test.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&[0u8; 512]).unwrap(); // LBA 0 padding
        f.write_all(&entries).unwrap(); // entries at LBA 1
        drop(f);
        let header = Header {
            entries_lba: 1,
            num_entries,
            entry_size,
        };
        let mut f = std::fs::File::open(&path).unwrap();
        let parts = read_entries(&mut f, &header).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(parts.len(), 1, "empty slot should be skipped");
    }

    #[test]
    fn to_tree_empty_partition_name_uses_fallback() {
        let parts = vec![Partition {
            index: 3,
            type_guid: [1u8; 16],
            unique_guid: [2u8; 16],
            start: 0,
            length: 512,
            name: String::new(), // empty → fallback to "partition-3"
        }];
        let tree = to_tree(&parts);
        assert!(
            tree.children[0].name.starts_with("partition-3"),
            "empty name should use indexed fallback; got {}",
            tree.children[0].name
        );
    }

    #[test]
    fn to_tree_zero_length_partition_has_no_location() {
        let parts = vec![Partition {
            index: 0,
            type_guid: [1u8; 16],
            unique_guid: [2u8; 16],
            start: 512,
            length: 0, // zero-length → file_location = None
            name: "EFI".to_string(),
        }];
        let tree = to_tree(&parts);
        assert!(
            tree.children[0].file_location.is_none(),
            "zero-length partition should have file_location=None"
        );
    }
}
