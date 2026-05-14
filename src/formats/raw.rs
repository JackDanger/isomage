//! Raw / `.img` disk-image dispatcher (`raw` feature → pulls in `mbr` + `gpt`).
//!
//! A "raw" image is just a byte sequence. The interesting question is
//! whether it carries a partition table. [`detect_and_parse`] tries
//! GPT first (because a GPT disk has a protective MBR; trying MBR
//! would either succeed-as-protective or fall through anyway), then
//! MBR, and finally returns a single-partition tree representing the
//! whole image.
//!
//! This module deliberately does **not** recurse into the filesystems
//! that live inside each partition — that requires per-FS readers
//! (FAT, NTFS, ext, …) which are tracked under their own feature
//! flags. For now, callers get a `TreeNode` whose children point at
//! the partition byte ranges; `cat_node` will hand them the raw
//! partition contents.

use std::fs::File;

use crate::formats::{gpt, mbr};
use crate::tree::TreeNode;

#[derive(Debug)]
pub enum Error {
    /// Neither GPT nor MBR matched, *and* the caller asked for a
    /// strict result. The lax path returns a single-partition tree.
    NoPartitionTable,
    Mbr(mbr::Error),
    Gpt(gpt::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoPartitionTable => write!(f, "no MBR or GPT partition table"),
            Error::Mbr(e) => write!(f, "MBR: {e}"),
            Error::Gpt(e) => write!(f, "GPT: {e}"),
            Error::Io(e) => write!(f, "raw I/O error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Mbr(e) => Some(e),
            Error::Gpt(e) => Some(e),
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

/// Try GPT, then MBR. If both fail, returns a TreeNode whose only
/// child is the whole image, named `"image"`. This is the lenient
/// behaviour `cat_node` consumers usually want.
///
/// The `image` child's `file_length` is the file's actual size on
/// disk, looked up via `Metadata::len()`. If the metadata query
/// fails, a length-0 child is emitted and `cat_node` will refuse it.
pub fn detect_and_parse(file: &mut File) -> Result<TreeNode, Error> {
    // GPT first: a GPT disk has a protective MBR that would otherwise
    // get reported as "one weird partition".
    match gpt::detect_and_parse(file) {
        Ok(tree) => return Ok(tree),
        Err(gpt::Error::BadSignature) | Err(gpt::Error::TooShort) => { /* fall through */ }
        Err(e) => return Err(Error::Gpt(e)),
    }

    // MBR next. A protective-MBR error from MBR means a GPT disk
    // whose GPT header we couldn't read — propagate that, because
    // it's distinct from "no partition table at all".
    match mbr::detect_and_parse(file) {
        Ok(tree) => return Ok(tree),
        Err(mbr::Error::BadSignature) | Err(mbr::Error::TooShort) => { /* fall through */ }
        Err(mbr::Error::ProtectiveMbr) => return Err(Error::Mbr(mbr::Error::ProtectiveMbr)),
        Err(e) => return Err(Error::Mbr(e)),
    }

    // No partition table — treat the file as a single anonymous blob.
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut root = TreeNode::new_directory("/".to_string());
    let child = if size == 0 {
        TreeNode::new_file("image".to_string(), 0)
    } else {
        TreeNode::new_file_with_location("image".to_string(), size, 0, size)
    };
    root.add_child(child);
    root.calculate_directory_size();
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn scratch(bytes: &[u8], tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "isomage-raw-test-{}-{}.bin",
            std::process::id(),
            tag
        ));
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();
        path
    }

    #[test]
    fn unpartitioned_emits_single_image_child() {
        // 4 KiB of zeros — no partition table magic anywhere.
        let path = scratch(&vec![0u8; 4096], "noparts");
        let mut f = File::open(&path).unwrap();
        let tree = detect_and_parse(&mut f).unwrap();
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].name, "image");
        assert_eq!(tree.children[0].size, 4096);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mbr_path_taken_when_signature_present() {
        // Build a 1 MiB image with a single MBR partition spanning
        // sectors 1..32 (15.5 KiB).
        let mut img = vec![0u8; 1024 * 1024];
        img[0x1FE] = 0x55;
        img[0x1FF] = 0xAA;
        // Slot 0: type 0x83 (Linux), LBA 1, 31 sectors
        let off = 0x1BE;
        img[off + 4] = 0x83;
        img[off + 8..off + 12].copy_from_slice(&1u32.to_le_bytes());
        img[off + 12..off + 16].copy_from_slice(&31u32.to_le_bytes());
        let path = scratch(&img, "mbr");
        let mut f = File::open(&path).unwrap();
        let tree = detect_and_parse(&mut f).unwrap();
        assert_eq!(tree.children.len(), 1);
        assert!(tree.children[0].name.contains("type-83"));
        assert_eq!(tree.children[0].file_location, Some(512));
        assert_eq!(tree.children[0].size, 31 * 512);
        std::fs::remove_file(&path).ok();
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_no_partition_table() {
        let msg = format!("{}", Error::NoPartitionTable);
        assert!(msg.contains("MBR") || msg.contains("GPT") || msg.contains("partition"));
    }

    #[test]
    fn error_display_mbr_wraps_inner() {
        let msg = format!("{}", Error::Mbr(mbr::Error::TooShort));
        assert!(msg.contains("MBR"), "expected MBR prefix in: {msg}");
    }

    #[test]
    fn error_display_gpt_wraps_inner() {
        let msg = format!("{}", Error::Gpt(gpt::Error::TooShort));
        assert!(msg.contains("GPT"), "expected GPT prefix in: {msg}");
    }

    #[test]
    fn error_display_io() {
        let io = std::io::Error::other("disk");
        let msg = format!("{}", Error::Io(io));
        assert!(msg.contains("disk"), "expected cause in: {msg}");
    }

    #[test]
    fn error_source_mbr() {
        use std::error::Error as StdError;
        let e = Error::Mbr(mbr::Error::TooShort);
        assert!(e.source().is_some());
    }

    #[test]
    fn error_source_gpt() {
        use std::error::Error as StdError;
        let e = Error::Gpt(gpt::Error::TooShort);
        assert!(e.source().is_some());
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        let io = std::io::Error::other("src");
        assert!(Error::Io(io).source().is_some());
    }

    #[test]
    fn error_source_no_partition_table() {
        use std::error::Error as StdError;
        assert!(Error::NoPartitionTable.source().is_none());
    }

    // ── Zero-size image ───────────────────────────────────────────────────────

    #[test]
    fn empty_file_emits_zero_size_child() {
        let path = scratch(&[], "empty");
        let mut f = File::open(&path).unwrap();
        let tree = detect_and_parse(&mut f).unwrap();
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].name, "image");
        assert_eq!(tree.children[0].size, 0);
        assert!(
            tree.children[0].file_location.is_none(),
            "zero-size image should have no file_location"
        );
        std::fs::remove_file(&path).ok();
    }

    /// Protective MBR + valid GPT header. We exercise the fall-through
    /// chain: GPT tries first and succeeds.
    #[test]
    fn error_from_io_error() {
        let e = Error::from(std::io::Error::other("raw test"));
        assert!(matches!(e, Error::Io(_)));
    }

    #[test]
    fn gpt_unsupported_entry_size_propagates_as_raw_error() {
        // GPT signature present but entry_size=64 → UnsupportedEntrySize propagates via line 73.
        let mut img = vec![0u8; 8192];
        let h = 512;
        img[h..h + 8].copy_from_slice(b"EFI PART");
        img[h + 80..h + 84].copy_from_slice(&128u32.to_le_bytes()); // num_entries
        img[h + 84..h + 88].copy_from_slice(&64u32.to_le_bytes()); // entry_size < 128
        let path = scratch(&img, "gpt-badentry");
        let mut f = File::open(&path).unwrap();
        let result = detect_and_parse(&mut f);
        std::fs::remove_file(&path).ok();
        assert!(
            matches!(result, Err(Error::Gpt(_))),
            "should propagate GPT error; got {result:?}"
        );
    }

    #[test]
    fn protective_mbr_propagates_as_raw_error() {
        // A 512-byte file with a protective-MBR signature: one entry of type 0xEE.
        // GPT fails with TooShort (file has no LBA 1); MBR returns ProtectiveMbr.
        let mut sector = vec![0u8; 512];
        sector[0x1FE] = 0x55;
        sector[0x1FF] = 0xAA;
        // Slot 0: type_code=0xEE, LBA start=1, LBA count=0xFFFFFFFF.
        sector[0x1C2] = 0xEE;
        sector[0x1C6..0x1CA].copy_from_slice(&1u32.to_le_bytes());
        sector[0x1CA..0x1CE].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let path = scratch(&sector, "prot-mbr");
        let mut f = File::open(&path).unwrap();
        let result = detect_and_parse(&mut f);
        std::fs::remove_file(&path).ok();
        assert!(
            matches!(result, Err(Error::Mbr(mbr::Error::ProtectiveMbr))),
            "should propagate ProtectiveMbr; got {result:?}"
        );
    }

    #[test]
    fn gpt_path_taken_when_signature_present() {
        let mut img = vec![0u8; 32 * 1024]; // 32 KiB

        // GPT header at LBA 1 (offset 512). One entry of size 128 at LBA 2.
        let h = 512;
        img[h..h + 8].copy_from_slice(b"EFI PART");
        img[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes());
        img[h + 80..h + 84].copy_from_slice(&1u32.to_le_bytes());
        img[h + 84..h + 88].copy_from_slice(&128u32.to_le_bytes());

        // One entry at LBA 2 (offset 1024): non-zero type GUID
        // (set first byte to make it non-empty), first LBA 34, last LBA 65.
        let e = 1024;
        img[e] = 0xEF; // type_guid[0]
        img[e + 32..e + 40].copy_from_slice(&34u64.to_le_bytes());
        img[e + 40..e + 48].copy_from_slice(&65u64.to_le_bytes());
        // UTF-16LE name "X"
        img[e + 56..e + 58].copy_from_slice(&0x58u16.to_le_bytes());

        let path = scratch(&img, "gpt");
        let mut f = File::open(&path).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        let tree = detect_and_parse(&mut f).unwrap();
        assert_eq!(tree.children.len(), 1);
        assert!(tree.children[0].name.starts_with("X-"));
        assert_eq!(tree.children[0].file_location, Some(34 * 512));
        assert_eq!(tree.children[0].size, (65 - 34 + 1) * 512);
        std::fs::remove_file(&path).ok();
    }
}
