//! Master Boot Record partition table (`mbr` feature).
//!
//! The MBR lives in the first 512-byte sector of a disk image. Bytes
//! 0x1FE–0x1FF must be `0x55 0xAA`. The partition table is four
//! 16-byte entries starting at offset 0x1BE; each entry describes
//! one primary partition by LBA start and length in 512-byte sectors.
//!
//! Reference: IBM PC technical reference, replicated in every UEFI
//! and Linux kernel doc; the layout has not changed since 1983.
//!
//! ## Protective MBR
//!
//! GPT-formatted disks carry a "protective MBR" — a single partition
//! of type `0xEE` covering the whole disk — so legacy tools see
//! "something is here" rather than empty space. [`parse`] recognises
//! this and returns [`Error::ProtectiveMbr`], so callers (typically
//! [`super::raw`]) can fall through to GPT.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

/// Byte size of one logical sector in the MBR scheme. The partition
/// table LBA fields are u32 sector counts; with a 512-byte sector
/// the max partition is 2 TiB, which is why GPT exists.
pub const SECTOR_SIZE: u64 = 512;

/// One parsed MBR partition entry. The byte range `(start..start+length)`
/// is suitable for `cat_node`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Partition {
    /// 0-indexed slot in the four-entry table.
    pub index: u8,
    /// Status byte: 0x80 = active/bootable, 0x00 = inactive.
    pub status: u8,
    /// Partition type code (e.g. 0x07 = NTFS/exFAT, 0x83 = Linux,
    /// 0xEE = GPT protective). The full list is conventionally
    /// documented in `parted`'s source.
    pub type_code: u8,
    /// First byte of the partition in the image.
    pub start: u64,
    /// Length of the partition in bytes.
    pub length: u64,
}

/// MBR parse errors. The `ProtectiveMbr` variant is the only one
/// callers typically inspect — it signals "fall through to GPT."
#[derive(Debug)]
pub enum Error {
    /// File too short to contain an MBR.
    TooShort,
    /// Boot signature bytes (0x1FE/0x1FF) were not `0x55 0xAA`.
    BadSignature,
    /// Exactly one entry of type `0xEE` spanning the whole disk: this
    /// is a GPT protective MBR, the partition table lives elsewhere.
    ProtectiveMbr,
    /// Underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image is shorter than one MBR sector (512 bytes)"),
            Error::BadSignature => write!(f, "MBR boot signature 0x55AA missing"),
            Error::ProtectiveMbr => write!(f, "protective MBR (GPT disk)"),
            Error::Io(e) => write!(f, "MBR I/O error: {e}"),
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

/// Read and parse the MBR. Returns the list of non-empty partitions.
///
/// An entry is "non-empty" if its `type_code` is non-zero and its
/// sector-count field is non-zero — empty slots are legal and common.
///
/// The returned partitions are in slot order (0…3), not LBA order.
pub fn parse(file: &mut File) -> Result<Vec<Partition>, Error> {
    file.seek(SeekFrom::Start(0))?;
    let mut sector = [0u8; SECTOR_SIZE as usize];
    if file.read(&mut sector)? < SECTOR_SIZE as usize {
        return Err(Error::TooShort);
    }
    parse_sector(&sector)
}

/// Pure parsing of a 512-byte boot sector. Exposed for testing and
/// for [`super::raw`]'s detection path, which reads the sector once
/// and tries both MBR and GPT against it.
pub fn parse_sector(sector: &[u8]) -> Result<Vec<Partition>, Error> {
    if sector.len() < SECTOR_SIZE as usize {
        return Err(Error::TooShort);
    }
    if sector[0x1FE] != 0x55 || sector[0x1FF] != 0xAA {
        return Err(Error::BadSignature);
    }

    let mut partitions = Vec::with_capacity(4);
    let mut all_ee = true;
    let mut had_any = false;
    for i in 0..4 {
        let off = 0x1BE + 16 * i;
        let entry = &sector[off..off + 16];
        let status = entry[0];
        let type_code = entry[4];
        let lba_start = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        let num_sectors = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);

        // An "empty" slot has type_code == 0 and zero sectors. Skip it
        // but don't reject the MBR — empty slots are normal.
        if type_code == 0 && num_sectors == 0 {
            continue;
        }
        had_any = true;
        if type_code != 0xEE {
            all_ee = false;
        }
        partitions.push(Partition {
            index: i as u8,
            status,
            type_code,
            start: (lba_start as u64) * SECTOR_SIZE,
            length: (num_sectors as u64) * SECTOR_SIZE,
        });
    }

    // Single 0xEE partition = GPT protective MBR. Signal it so
    // callers can fall through rather than expose this as a "partition".
    if had_any && all_ee && partitions.len() == 1 {
        return Err(Error::ProtectiveMbr);
    }

    Ok(partitions)
}

/// Convert a parsed partition list into the [`TreeNode`] shape used
/// by `cat_node` / `extract_node`. Each partition becomes a child
/// file of the root, named `partition-0`/`partition-1`/… with the
/// type code in hex appended for disambiguation in `ls`-style tools.
pub fn to_tree(partitions: &[Partition]) -> TreeNode {
    let mut root = TreeNode::new_directory("/".to_string());
    for p in partitions {
        let name = format!("partition-{}-type-{:02x}", p.index, p.type_code);
        // Length == 0 partitions can legally exist (gross, but legal).
        // Emit them with `file_location = None` so cat_node refuses
        // rather than seeking off the end.
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

/// One-call detect + parse + tree. Returns the same [`TreeNode`]
/// shape the v2 ISO/UDF parsers do.
pub fn detect_and_parse(file: &mut File) -> Result<TreeNode, Error> {
    let parts = parse(file)?;
    Ok(to_tree(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_sector() -> [u8; 512] {
        let mut s = [0u8; 512];
        s[0x1FE] = 0x55;
        s[0x1FF] = 0xAA;
        s
    }

    fn write_entry(
        sector: &mut [u8; 512],
        slot: usize,
        status: u8,
        type_code: u8,
        lba_start: u32,
        num_sectors: u32,
    ) {
        let off = 0x1BE + 16 * slot;
        sector[off] = status;
        sector[off + 4] = type_code;
        sector[off + 8..off + 12].copy_from_slice(&lba_start.to_le_bytes());
        sector[off + 12..off + 16].copy_from_slice(&num_sectors.to_le_bytes());
    }

    #[test]
    fn rejects_missing_signature() {
        let s = [0u8; 512];
        assert!(matches!(parse_sector(&s), Err(Error::BadSignature)));
    }

    #[test]
    fn empty_partition_table_ok() {
        let s = empty_sector();
        let parts = parse_sector(&s).unwrap();
        assert!(parts.is_empty());
    }

    #[test]
    fn one_linux_partition() {
        let mut s = empty_sector();
        // Linux partition starting at LBA 2048, 100 MiB long.
        write_entry(&mut s, 0, 0x80, 0x83, 2048, 100 * 1024 * 2);
        let parts = parse_sector(&s).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].status, 0x80);
        assert_eq!(parts[0].type_code, 0x83);
        assert_eq!(parts[0].start, 2048 * 512);
        assert_eq!(parts[0].length, 100 * 1024 * 1024);
    }

    #[test]
    fn three_partitions_one_empty() {
        let mut s = empty_sector();
        write_entry(&mut s, 0, 0x00, 0x07, 2048, 1024);
        write_entry(&mut s, 1, 0x00, 0x83, 4096, 2048);
        // slot 2 left empty
        write_entry(&mut s, 3, 0x00, 0x82, 8192, 512);
        let parts = parse_sector(&s).unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts.iter().map(|p| p.index).collect::<Vec<_>>(),
            vec![0, 1, 3]
        );
    }

    #[test]
    fn protective_mbr_detected() {
        let mut s = empty_sector();
        write_entry(&mut s, 0, 0x00, 0xEE, 1, u32::MAX);
        assert!(matches!(parse_sector(&s), Err(Error::ProtectiveMbr)));
    }

    #[test]
    fn to_tree_shapes_children() {
        let mut s = empty_sector();
        write_entry(&mut s, 0, 0x00, 0x07, 2048, 1024);
        write_entry(&mut s, 1, 0x00, 0x83, 4096, 2048);
        let parts = parse_sector(&s).unwrap();
        let root = to_tree(&parts);
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 2);
        assert!(root.children[0].name.starts_with("partition-0-type-07"));
        assert_eq!(root.children[0].size, 1024 * 512);
        assert_eq!(root.children[0].file_location, Some(2048 * 512));
    }
}
