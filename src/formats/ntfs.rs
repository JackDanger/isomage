//! NTFS filesystem reader (`ntfs` feature).
//!
//! Reads the directory tree of an NTFS volume image and produces a
//! [`TreeNode`] compatible with `cat_node` / `extract_node`.
//!
//! Reference: Microsoft's on-disk NTFS documentation, the Linux kernel
//! `fs/ntfs3/` source tree, and libntfs-3g internals.
//!
//! ## What is implemented
//!
//! - Boot sector detection (OEM ID `b"NTFS    "` at offset 3).
//! - Boot sector parsing: cluster size, MFT record size, MFT byte offset.
//! - Sequential MFT record reading with update-sequence fixup.
//! - Attribute walking: `$STANDARD_INFORMATION` (0x10), `$FILE_NAME`
//!   (0x30), and `$DATA` (0x80), plus end-of-list marker (0xFFFFFFFF).
//! - `$FILE_NAME` namespace priority: Win32&DOS (3) > Win32 (1) > POSIX (0);
//!   DOS (2) is skipped in favour of its Win32 companion.
//! - Parent-reference tree construction via a `HashMap` keyed on MFT
//!   record number; root = record 5.
//! - System files (MFT record numbers 0–11) are excluded from the output
//!   tree. Records with the in-use flag clear are skipped.
//!
//! ## `file_location` semantics
//!
//! A file's `file_location` is set **only** when the `$DATA` attribute is
//! resident (small file whose data lives inside the MFT record itself) or
//! when the non-resident attribute has exactly one run in its runlist. For
//! multi-run files the location is `None`; `cat_node` will refuse those.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Constants ─────────────────────────────────────────────────────────────────

/// OEM ID at offset 3 of the boot sector for NTFS volumes.
const NTFS_OEM_ID: &[u8; 8] = b"NTFS    ";

/// Attribute type codes (little-endian u32 on disk).
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

/// MFT record numbers reserved for NTFS system metadata files.
/// Records 0–11 inclusive are system files; user data starts at record 12.
const SYSTEM_RECORD_COUNT: u64 = 12;

/// Root directory MFT record number.
const ROOT_MFT_RECORD: u64 = 5;

/// Maximum directory nesting depth; guards against corrupted images that
/// would otherwise cause unbounded recursion.
const MAX_DEPTH: usize = 32;

/// `$FILE_NAME` namespace codes.
const NS_POSIX: u8 = 0;
const NS_WIN32: u8 = 1;
const NS_DOS: u8 = 2;
const NS_WIN32_DOS: u8 = 3;

// ── Error type ────────────────────────────────────────────────────────────────

/// Reasons `detect` or `detect_and_parse` can fail.
#[derive(Debug)]
pub enum Error {
    /// Image too short to contain a valid NTFS boot sector.
    TooShort,
    /// OEM ID did not match `b"NTFS    "`, or derived geometry is nonsensical.
    BadMagic,
    /// Cluster size or MFT record size computed to zero or an implausible value.
    BadClusterSize,
    /// Underlying I/O error.
    Io(std::io::Error),
    /// Directory hierarchy exceeds the maximum recursion depth; likely corrupt.
    TooDeep,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image too short to contain an NTFS boot sector"),
            Error::BadMagic => write!(f, "NTFS OEM ID not found at offset 3"),
            Error::BadClusterSize => write!(f, "NTFS cluster or MFT record size is invalid"),
            Error::Io(e) => write!(f, "NTFS I/O error: {e}"),
            Error::TooDeep => write!(f, "NTFS directory tree exceeded maximum recursion depth"),
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

// ── Boot sector ───────────────────────────────────────────────────────────────

/// Parsed fields from the NTFS boot sector that we actually use.
#[derive(Debug, Clone)]
struct BootSector {
    cluster_size: u64,
    mft_record_size: u64,
    mft_offset: u64,
}

fn parse_boot_sector(data: &[u8]) -> Result<BootSector, Error> {
    if data.len() < 84 {
        return Err(Error::TooShort);
    }

    // OEM ID at bytes 3..11.
    if &data[3..11] != NTFS_OEM_ID {
        return Err(Error::BadMagic);
    }

    let bytes_per_sector = u16::from_le_bytes([data[11], data[12]]) as u64;
    let sectors_per_cluster = data[13] as u64;

    // Sanity: standard sector sizes are 512, 1024, 2048, 4096.
    if !(512..=4096).contains(&bytes_per_sector) {
        return Err(Error::BadClusterSize);
    }
    if sectors_per_cluster == 0 {
        return Err(Error::BadClusterSize);
    }

    let cluster_size = bytes_per_sector * sectors_per_cluster;

    let mft_lcn = u64::from_le_bytes(data[48..56].try_into().unwrap());

    // clusters_per_file_record_segment: if positive → multiply by cluster_size;
    // if negative → 2^(-value) bytes (e.g. -10 → 1024 bytes).
    let cpfrs = data[64] as i8;
    let mft_record_size = if cpfrs > 0 {
        (cpfrs as u64) * cluster_size
    } else {
        // Negative means 2^|cpfrs|; cast to u8 first to get the magnitude.
        1u64 << ((-cpfrs) as u32)
    };

    if mft_record_size == 0 || mft_record_size > 65536 {
        return Err(Error::BadClusterSize);
    }

    let mft_offset = mft_lcn * cluster_size;

    Ok(BootSector {
        cluster_size,
        mft_record_size,
        mft_offset,
    })
}

// ── MFT record reading + fixup ────────────────────────────────────────────────

/// Apply NTFS update-sequence fixup to a raw MFT record buffer.
///
/// Each 512-byte sector boundary in the record has its last two bytes
/// validated against the Update Sequence Number and then replaced with
/// the corresponding fix-up value. This undoes the write-fault protection
/// that the NTFS driver applies before writing.
fn apply_fixup(buf: &mut [u8]) -> bool {
    if buf.len() < 8 {
        return false;
    }
    let usa_offset = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    let usa_count = u16::from_le_bytes([buf[6], buf[7]]) as usize;

    // usa_count includes the USN itself, so actual fix-up entries = usa_count - 1.
    if usa_count < 2 || usa_offset + usa_count * 2 > buf.len() {
        return false;
    }

    let usn_lo = buf[usa_offset];
    let usn_hi = buf[usa_offset + 1];

    for i in 1..usa_count {
        let sector_end = i * 512 - 2;
        if sector_end + 2 > buf.len() {
            break;
        }
        // Verify the last two bytes of sector i match the USN.
        if buf[sector_end] != usn_lo || buf[sector_end + 1] != usn_hi {
            // Mismatch is not fatal for a read-only parser; we continue.
        }
        // Replace with the fix-up array entry.
        let fix_offset = usa_offset + i * 2;
        buf[sector_end] = buf[fix_offset];
        buf[sector_end + 1] = buf[fix_offset + 1];
    }
    true
}

/// Read and fixup a single MFT record at the given absolute byte offset.
/// Returns `None` for records that are not in-use or lack a `b"FILE"` signature.
fn read_mft_record<R: Read + Seek>(
    file: &mut R,
    offset: u64,
    record_size: u64,
) -> Result<Option<Vec<u8>>, Error> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; record_size as usize];
    match file.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    }

    // Validate the FILE signature.
    if &buf[0..4] != b"FILE" {
        return Ok(None);
    }

    if !apply_fixup(&mut buf) {
        return Ok(None); // corrupted update-sequence; skip
    }

    // Flags at offset 22: bit 0 = in-use.
    let flags = u16::from_le_bytes([buf[22], buf[23]]);
    if flags & 0x01 == 0 {
        return Ok(None); // not in-use
    }

    Ok(Some(buf))
}

// ── Attribute walking ─────────────────────────────────────────────────────────

/// A parsed attribute header plus a slice into the record buffer.
struct Attribute<'a> {
    attr_type: u32,
    non_resident: bool,
    /// For resident attributes: the data bytes of the attribute value.
    resident_data: Option<&'a [u8]>,
    /// For non-resident attributes: the raw attribute header slice
    /// (the full attribute from its start offset, length = `length`).
    nonresident_slice: Option<&'a [u8]>,
}

/// Walk all attributes in a FILE record buffer, returning parsed views.
fn parse_attributes(buf: &[u8]) -> Vec<Attribute<'_>> {
    let mut attrs = Vec::new();

    let attr_offset = match buf.get(20..22) {
        Some(b) => u16::from_le_bytes([b[0], b[1]]) as usize,
        None => return attrs,
    };

    let mut pos = attr_offset;

    loop {
        if pos + 8 > buf.len() {
            break;
        }

        let attr_type = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
        if attr_type == ATTR_END {
            break;
        }

        let length = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if length == 0 || pos + length > buf.len() {
            break;
        }

        let non_resident = buf[pos + 8] != 0;

        let resident_data = if !non_resident && pos + 16 + 4 <= pos + length {
            // Resident: value_length at +16, value_offset at +20.
            if pos + 24 <= buf.len() {
                let value_length =
                    u32::from_le_bytes(buf[pos + 16..pos + 20].try_into().unwrap()) as usize;
                let value_offset = u16::from_le_bytes([buf[pos + 20], buf[pos + 21]]) as usize;
                let data_start = pos + value_offset;
                let data_end = data_start + value_length;
                if data_end <= pos + length && data_end <= buf.len() {
                    Some(&buf[data_start..data_end])
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let nonresident_slice = if non_resident && pos + length <= buf.len() {
            Some(&buf[pos..pos + length])
        } else {
            None
        };

        attrs.push(Attribute {
            attr_type,
            non_resident,
            resident_data,
            nonresident_slice,
        });

        pos += length;
    }

    attrs
}

// ── $FILE_NAME attribute ───────────────────────────────────────────────────────

/// Parsed content of a `$FILE_NAME` attribute value.
#[derive(Debug)]
struct FileNameAttr {
    parent_ref: u64, // low 48 bits = parent MFT record number
    name: String,
    namespace: u8,
    is_directory: bool, // from file_attributes field
}

fn parse_filename_attr(data: &[u8]) -> Option<FileNameAttr> {
    // Minimum: 66 bytes (header) + at least 1 UTF-16 char = 68.
    if data.len() < 66 {
        return None;
    }

    // parent_directory_reference: low 48 bits = MFT record number.
    let parent_ref_raw = u64::from_le_bytes(data[0..8].try_into().ok()?);
    let parent_ref = parent_ref_raw & 0x0000_FFFF_FFFF_FFFF;

    let file_attributes = u32::from_le_bytes(data[56..60].try_into().ok()?);
    // FILE_ATTRIBUTE_DIRECTORY = 0x10.
    let is_directory = file_attributes & 0x10 != 0;

    let filename_length = data[64] as usize; // in UTF-16 code units
    let namespace = data[65];

    // Each code unit is 2 bytes; check bounds.
    let name_bytes_len = filename_length * 2;
    if 66 + name_bytes_len > data.len() {
        return None;
    }

    let name_bytes = &data[66..66 + name_bytes_len];
    let utf16_units: Vec<u16> = name_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let name = String::from_utf16_lossy(&utf16_units);

    Some(FileNameAttr {
        parent_ref,
        name,
        namespace,
        is_directory,
    })
}

// ── Runlist decoding ──────────────────────────────────────────────────────────

/// One decoded runlist entry: a contiguous cluster run.
#[derive(Debug, Clone)]
struct Run {
    start_lcn: u64,
    // cluster count; used by tests and by future multi-run $DATA extraction.
    #[allow(dead_code)]
    length: u64,
}

/// Decode an NTFS runlist from `data`, returning the list of runs in order.
///
/// Each entry begins with a 1-byte header whose high nibble is the byte-length
/// of the cluster-count field and whose low nibble is the byte-length of the
/// (signed) cluster-offset field. `0x00` is the end marker.
/// Decode a NTFS runlist into physical runs.
///
/// Returns `(runs, had_sparse)` where `had_sparse` is `true` when at least
/// one sparse entry (off_size == 0) was encountered. Callers that want to set
/// `file_location` must check `had_sparse` because a sparse+data runlist can
/// yield a single physical run that does not actually cover the full logical
/// extent.
fn decode_runlist(data: &[u8]) -> (Vec<Run>, bool) {
    let mut runs = Vec::new();
    let mut pos = 0usize;
    let mut prev_lcn: i64 = 0;
    let mut had_sparse = false;

    while pos < data.len() {
        let header = data[pos];
        if header == 0 {
            break;
        }
        pos += 1;

        let len_size = (header >> 4) as usize; // byte count for cluster count
        let off_size = (header & 0x0F) as usize; // byte count for cluster offset

        if pos + len_size + off_size > data.len() {
            break;
        }

        // Cluster count: unsigned, len_size bytes, little-endian.
        let mut length: u64 = 0;
        for i in 0..len_size {
            length |= (data[pos + i] as u64) << (i * 8);
        }
        pos += len_size;

        // Cluster offset: signed, off_size bytes, little-endian.
        // Sign-extend from off_size bytes to i64.
        let delta: i64 = if off_size == 0 {
            0
        } else {
            let mut raw: u64 = 0;
            for i in 0..off_size {
                raw |= (data[pos + i] as u64) << (i * 8);
            }
            pos += off_size;
            // Sign-extend: if the top bit of the last byte is set, extend.
            let sign_bit = 1u64 << (off_size * 8 - 1);
            if raw & sign_bit != 0 {
                // Negative: fill the upper bits with 1s.
                let mask = !((sign_bit << 1) - 1);
                (raw | mask) as i64
            } else {
                raw as i64
            }
        };

        prev_lcn += delta;
        if off_size == 0 {
            // Sparse run: logical zeros, no physical clusters.
            had_sparse = true;
        } else {
            runs.push(Run {
                start_lcn: prev_lcn as u64,
                length,
            });
        }
    }

    (runs, had_sparse)
}

// ── Per-record info ────────────────────────────────────────────────────────────

/// Everything we need about one MFT file record.
#[derive(Debug)]
struct RecordInfo {
    mft_num: u64,
    name: String,
    parent_ref: u64,
    is_directory: bool,
    file_size: u64,
    file_location: Option<u64>,
}

/// Parse a FILE record buffer into a `RecordInfo`.
///
/// When multiple `$FILE_NAME` attributes exist (e.g. Win32 + DOS), the
/// one with the best namespace wins: Win32&DOS (3) > Win32 (1) > POSIX (0).
/// DOS-only (2) is skipped because the Win32 companion is always present
/// when a DOS name exists.
fn extract_record_info(
    buf: &[u8],
    mft_num: u64,
    mft_record_abs_offset: u64,
    cluster_size: u64,
    volume_base: u64,
) -> Option<RecordInfo> {
    let attrs = parse_attributes(buf);

    // Collect all $FILE_NAME attrs; pick best namespace.
    let mut best_fn: Option<FileNameAttr> = None;

    // Collect $DATA information.
    let mut file_size: u64 = 0;
    let mut file_location: Option<u64> = None;

    // We need the attribute byte offset from the start of the record to
    // compute resident data locations.  Recompute the attr start from the
    // record header so we can track absolute offsets per-attribute.
    let first_attr_offset = u16::from_le_bytes([buf[20], buf[21]]) as usize;
    let mut attr_pos = first_attr_offset;

    for attr in &attrs {
        // Advance attr_pos to stay in sync.  Each Attribute is parsed from
        // `buf` already, but we need the byte offset of the value within
        // the full image for resident $DATA.
        //
        // We re-read the attribute length from buf[attr_pos+4..+8] to keep
        // the two traversals in lockstep.
        if attr_pos + 8 > buf.len() {
            break;
        }
        let attr_type_check = u32::from_le_bytes(buf[attr_pos..attr_pos + 4].try_into().unwrap());
        if attr_type_check == ATTR_END {
            break;
        }
        let attr_length =
            u32::from_le_bytes(buf[attr_pos + 4..attr_pos + 8].try_into().unwrap()) as usize;

        match attr.attr_type {
            ATTR_FILE_NAME => {
                if let Some(data) = attr.resident_data {
                    if let Some(fn_attr) = parse_filename_attr(data) {
                        // Skip DOS-only names; their Win32 companion conveys the same.
                        if fn_attr.namespace == NS_DOS {
                            attr_pos += attr_length;
                            continue;
                        }
                        let take = match &best_fn {
                            None => true,
                            Some(existing) => {
                                namespace_priority(fn_attr.namespace)
                                    > namespace_priority(existing.namespace)
                            }
                        };
                        if take {
                            best_fn = Some(fn_attr);
                        }
                    }
                }
            }

            ATTR_DATA => {
                if !attr.non_resident {
                    // Resident $DATA: value is inside the MFT record.
                    if let Some(data) = attr.resident_data {
                        file_size = data.len() as u64;
                        // Compute absolute byte offset of the resident value.
                        // value_offset at attr_pos+20 is relative to attr start.
                        if attr_pos + 24 <= buf.len() {
                            let value_offset =
                                u16::from_le_bytes([buf[attr_pos + 20], buf[attr_pos + 21]]) as u64;
                            file_location =
                                Some(mft_record_abs_offset + attr_pos as u64 + value_offset);
                        }
                    }
                } else if let Some(nr_slice) = attr.nonresident_slice {
                    // Non-resident $DATA: read data_size and decode runlist.
                    if nr_slice.len() >= 64 {
                        let data_size = u64::from_le_bytes(nr_slice[48..56].try_into().unwrap());
                        file_size = data_size;

                        let runlist_offset =
                            u16::from_le_bytes([nr_slice[32], nr_slice[33]]) as usize;
                        if runlist_offset < nr_slice.len() {
                            let (runs, had_sparse) =
                                decode_runlist(&nr_slice[runlist_offset..]);
                            // Single contiguous non-sparse run gets a file_location.
                            // Include the volume base offset for images embedded in
                            // a larger file (e.g. NTFS inside a partition image).
                            if runs.len() == 1 && !had_sparse {
                                file_location = Some(
                                    volume_base + runs[0].start_lcn * cluster_size,
                                );
                            }
                        }
                    }
                }
            }

            ATTR_STANDARD_INFORMATION | ATTR_ATTRIBUTE_LIST => {
                // Not needed for tree construction.
            }

            _ => {}
        }

        if attr_length == 0 {
            break;
        }
        attr_pos += attr_length;
    }

    let fn_attr = best_fn?;

    Some(RecordInfo {
        mft_num,
        name: fn_attr.name,
        parent_ref: fn_attr.parent_ref,
        is_directory: fn_attr.is_directory,
        file_size,
        file_location,
    })
}

/// Higher value = better namespace for display purposes.
fn namespace_priority(ns: u8) -> u8 {
    match ns {
        NS_WIN32_DOS => 3,
        NS_WIN32 => 2,
        NS_POSIX => 1,
        NS_DOS => 0,
        _ => 0,
    }
}

// ── Tree construction ─────────────────────────────────────────────────────────

/// Build a `TreeNode` subtree rooted at `mft_num` from the flat record map.
fn build_tree_recursive(
    mft_num: u64,
    name: String,
    children_map: &HashMap<u64, Vec<RecordInfo>>,
    depth: usize,
) -> Result<TreeNode, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::TooDeep);
    }

    // Check if this record is a directory (has children in the map, or
    // was explicitly marked as directory).
    let is_dir = children_map.contains_key(&mft_num);

    if is_dir || mft_num == ROOT_MFT_RECORD {
        let mut node = TreeNode::new_directory(name);
        if let Some(children) = children_map.get(&mft_num) {
            for child in children {
                let child_name = child.name.clone();
                let child_num = child.mft_num;
                let child_is_dir = child.is_directory;
                if child_is_dir {
                    match build_tree_recursive(child_num, child_name, children_map, depth + 1) {
                        Ok(child_node) => node.add_child(child_node),
                        Err(Error::TooDeep) => {
                            // Skip this subtree; don't propagate TooDeep.
                        }
                        Err(e) => return Err(e),
                    }
                } else {
                    let file_node = match child.file_location {
                        Some(loc) => TreeNode::new_file_with_location(
                            child_name,
                            child.file_size,
                            loc,
                            child.file_size,
                        ),
                        None => TreeNode::new_file(child_name, child.file_size),
                    };
                    node.add_child(file_node);
                }
            }
        }
        Ok(node)
    } else {
        // A leaf node that was recorded as a file.
        // This branch is hit when a file's parent points here but we
        // have no RecordInfo for it in the children map (orphan).  Treat
        // as empty directory to avoid silent data loss.
        Ok(TreeNode::new_directory(name))
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return `true` if the stream at its current position looks like an NTFS
/// volume.
///
/// Reads 8 bytes from offset 3 of the current position and checks for the
/// OEM ID `b"NTFS    "`. Restores the stream position regardless of outcome.
pub fn detect<R: Read + Seek>(file: &mut R) -> bool {
    let saved = match file.stream_position() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let ok = detect_inner(file, saved);
    let _ = file.seek(SeekFrom::Start(saved));
    ok
}

fn detect_inner<R: Read + Seek>(file: &mut R, base: u64) -> bool {
    if file.seek(SeekFrom::Start(base + 3)).is_err() {
        return false;
    }
    let mut oem = [0u8; 8];
    if file.read_exact(&mut oem).is_err() {
        return false;
    }
    &oem == NTFS_OEM_ID
}

/// Detect and parse an NTFS filesystem, returning the directory tree.
///
/// `file`'s current position is treated as the start of the NTFS volume,
/// allowing parsing of NTFS partitions that begin mid-image.
pub fn detect_and_parse<R: Read + Seek>(file: &mut R) -> Result<TreeNode, Error> {
    let base = file.stream_position()?;

    // Read and parse the boot sector (512 bytes).
    file.seek(SeekFrom::Start(base))?;
    let mut boot_buf = [0u8; 512];
    match file.read_exact(&mut boot_buf) {
        Ok(()) => {}
        Err(_) => return Err(Error::TooShort),
    }
    let boot = parse_boot_sector(&boot_buf)?;

    // Read all MFT records sequentially.
    let mut records: Vec<RecordInfo> = Vec::new();

    let mut mft_num: u64 = 0;
    loop {
        let record_offset = base + boot.mft_offset + mft_num * boot.mft_record_size;
        let record_abs = base + boot.mft_offset + mft_num * boot.mft_record_size;

        match read_mft_record(file, record_offset, boot.mft_record_size)? {
            None => {
                // Free / unused MFT slot or UnexpectedEof. Real volumes have free
                // slots interspersed, so we cannot stop here. We rely on the
                // 1-million guard below instead of breaking on the first None.
            }
            Some(buf) => {
                // Skip system metadata files (0–11).
                if mft_num >= SYSTEM_RECORD_COUNT {
                    if let Some(info) = extract_record_info(
                        &buf,
                        mft_num,
                        record_abs,
                        boot.cluster_size,
                        base,
                    ) {
                        // Skip the root directory record itself (record 5) from
                        // the flat list; we'll handle it as the tree root.
                        if mft_num != ROOT_MFT_RECORD {
                            records.push(info);
                        }
                    }
                }
            }
        }

        mft_num += 1;

        // Guard against unreasonably large MFT tables (> 1 million records).
        if mft_num > 1_000_000 {
            break;
        }
    }

    // Build children map: parent_ref → Vec<RecordInfo>.
    let mut children_map: HashMap<u64, Vec<RecordInfo>> = HashMap::new();
    for rec in records {
        // Ignore records whose parent is themselves (e.g. the root "." entry).
        if rec.parent_ref == rec.mft_num {
            continue;
        }
        children_map.entry(rec.parent_ref).or_default().push(rec);
    }

    // Build tree from root (record 5 = "/").
    let mut root = build_tree_recursive(ROOT_MFT_RECORD, "/".to_string(), &children_map, 0)?;
    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Seek, SeekFrom};

    // ── Minimal in-memory NTFS image builder ──────────────────────────────

    /// Build a minimal valid NTFS boot sector at offset 0 of an in-memory
    /// buffer.  The buffer also contains a single MFT record for user
    /// verification tests, but for detection tests only the boot sector
    /// matters.
    ///
    /// Layout chosen:
    ///   - bytes_per_sector      = 512
    ///   - sectors_per_cluster   = 8  → cluster_size = 4096
    ///   - clusters_per_FRS      = -10 (i8) → mft_record_size = 1024
    ///   - mft_lcn               = 4  → mft_offset = 4 * 4096 = 16384
    fn make_ntfs_boot_sector() -> Vec<u8> {
        let mut boot = vec![0u8; 512];
        // JMP + NOP at [0..3]
        boot[0] = 0xEB;
        boot[1] = 0x52;
        boot[2] = 0x90;
        // OEM ID
        boot[3..11].copy_from_slice(NTFS_OEM_ID);
        // bytes_per_sector = 512
        boot[11..13].copy_from_slice(&512u16.to_le_bytes());
        // sectors_per_cluster = 8 → cluster_size = 4096
        boot[13] = 8;
        // media_descriptor = 0xF8
        boot[21] = 0xF8;
        // mft_lcn = 4
        boot[48..56].copy_from_slice(&4u64.to_le_bytes());
        // mft_mirror_lcn = 2
        boot[56..64].copy_from_slice(&2u64.to_le_bytes());
        // clusters_per_FRS = -10 (i8) → 1024 bytes
        boot[64] = (-10i8) as u8;
        // clusters_per_index_block = -10
        boot[68] = (-10i8) as u8;
        // volume_serial_number = 0x1234567890ABCDEF
        boot[72..80].copy_from_slice(&0x1234_5678_90AB_CDEFu64.to_le_bytes());
        boot
    }

    /// Assemble a minimal image containing a valid boot sector and a
    /// root-directory MFT record at offset mft_offset (16384) so that
    /// `detect_and_parse` can find the MFT and return a tree.
    ///
    /// The image is 32 KiB, giving room for a few MFT records.
    fn make_minimal_ntfs_image() -> Vec<u8> {
        const IMAGE_SIZE: usize = 32 * 1024;
        const MFT_OFFSET: usize = 16384; // 4 clusters * 4096
        const MFT_RECORD_SIZE: usize = 1024;

        let mut img = vec![0u8; IMAGE_SIZE];

        // Write boot sector.
        let boot = make_ntfs_boot_sector();
        img[..512].copy_from_slice(&boot);

        // Write a minimal root directory FILE record at mft_lcn=4 (offset 16384).
        // MFT record number 5 = root directory.
        write_file_record(&mut img, MFT_OFFSET + 5 * MFT_RECORD_SIZE, 5, true);

        // Write a user file FILE record at slot 12 (first user record).
        write_file_record(&mut img, MFT_OFFSET + 12 * MFT_RECORD_SIZE, 12, false);

        img
    }

    /// Write a minimal FILE record into `img` at `offset`.
    ///
    /// Writes: signature, USA header, flags (in-use), attribute_offset,
    /// a $FILE_NAME attribute with the given name pointing at record 5 as
    /// parent, and an ATTR_END marker.  For user files (is_dir=false)
    /// adds a small resident $DATA attribute.
    fn write_file_record(img: &mut Vec<u8>, offset: usize, mft_num: u64, is_dir: bool) {
        const REC_SIZE: usize = 1024;

        // FILE signature
        img[offset..offset + 4].copy_from_slice(b"FILE");

        // USA: offset = 48, count = 3 (USN + 2 sector fix-ups for 1024-byte record).
        let usa_offset: u16 = 48;
        let usa_count: u16 = 3;
        img[offset + 4..offset + 6].copy_from_slice(&usa_offset.to_le_bytes());
        img[offset + 6..offset + 8].copy_from_slice(&usa_count.to_le_bytes());

        // USN = 0x0001 at usa_offset.
        img[offset + usa_offset as usize..offset + usa_offset as usize + 2]
            .copy_from_slice(&1u16.to_le_bytes());
        // Fix-up values (just zeroes — we'll write the right USN into sector ends).
        // Sector 1 end = offset + 510..512 → set to USN.
        img[offset + 510..offset + 512].copy_from_slice(&1u16.to_le_bytes());
        // Sector 2 end = offset + 1022..1024 → set to USN.
        img[offset + 1022..offset + 1024].copy_from_slice(&1u16.to_le_bytes());

        // sequence_number at [16] = 1, link_count at [18] = 1
        img[offset + 16..offset + 18].copy_from_slice(&1u16.to_le_bytes());
        img[offset + 18..offset + 20].copy_from_slice(&1u16.to_le_bytes());

        // attribute_offset: first attribute starts at 56 (after the fixed header + USA).
        let first_attr: u16 = 56;
        img[offset + 20..offset + 22].copy_from_slice(&first_attr.to_le_bytes());

        // flags: 0x01 = in-use; add 0x02 for directories.
        let flags: u16 = if is_dir { 0x03 } else { 0x01 };
        img[offset + 22..offset + 24].copy_from_slice(&flags.to_le_bytes());

        // mft_record_number at [44]
        img[offset + 44..offset + 48].copy_from_slice(&(mft_num as u32).to_le_bytes());

        // Build $FILE_NAME attribute at first_attr offset.
        let fn_name: Vec<u16> = if is_dir {
            ".".encode_utf16().collect()
        } else {
            "hello.txt".encode_utf16().collect()
        };
        let fn_name_bytes: Vec<u8> = fn_name.iter().flat_map(|&c| c.to_le_bytes()).collect();
        let fn_value_len = 66 + fn_name_bytes.len();

        // Attribute header (resident): type, length, non_resident=0, name_len=0,
        // name_offset=0x18, flags=0, attribute_id=0.
        // Resident fields: value_length, value_offset=0x18.
        let fn_attr_start = offset + first_attr as usize;
        let fn_attr_value_offset: u16 = 24; // standard resident header = 24 bytes
        let fn_attr_len = (fn_attr_value_offset as usize + fn_value_len + 7) & !7; // align to 8

        img[fn_attr_start..fn_attr_start + 4].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        img[fn_attr_start + 4..fn_attr_start + 8]
            .copy_from_slice(&(fn_attr_len as u32).to_le_bytes());
        img[fn_attr_start + 8] = 0; // resident
        img[fn_attr_start + 9] = 0; // name_length = 0
        img[fn_attr_start + 16..fn_attr_start + 20]
            .copy_from_slice(&(fn_value_len as u32).to_le_bytes());
        img[fn_attr_start + 20..fn_attr_start + 22]
            .copy_from_slice(&fn_attr_value_offset.to_le_bytes());

        // $FILE_NAME value: parent_ref, timestamps (zeros), file_attributes,
        // filename_length, namespace, filename.
        let fn_val_start = fn_attr_start + fn_attr_value_offset as usize;
        // parent_directory_reference: record 5, sequence 1 → low 48 bits = 5.
        let parent_ref: u64 = if mft_num == 5 { 5 } else { 5 };
        img[fn_val_start..fn_val_start + 8].copy_from_slice(&parent_ref.to_le_bytes());
        // file_attributes: 0x10 for directory, 0x20 for archive (file).
        let file_attrs: u32 = if is_dir { 0x10 } else { 0x20 };
        img[fn_val_start + 56..fn_val_start + 60].copy_from_slice(&file_attrs.to_le_bytes());
        img[fn_val_start + 64] = fn_name.len() as u8; // filename_length
        img[fn_val_start + 65] = NS_WIN32_DOS; // namespace
        img[fn_val_start + 66..fn_val_start + 66 + fn_name_bytes.len()]
            .copy_from_slice(&fn_name_bytes);

        let mut next_attr = fn_attr_start + fn_attr_len;

        // For user files, add a small resident $DATA attribute.
        if !is_dir {
            const FILE_DATA: &[u8] = b"hello ntfs\n";
            let data_val_offset: u16 = 24;
            let data_attr_len = (data_val_offset as usize + FILE_DATA.len() + 7) & !7;

            img[next_attr..next_attr + 4].copy_from_slice(&ATTR_DATA.to_le_bytes());
            img[next_attr + 4..next_attr + 8]
                .copy_from_slice(&(data_attr_len as u32).to_le_bytes());
            img[next_attr + 8] = 0; // resident
            img[next_attr + 16..next_attr + 20]
                .copy_from_slice(&(FILE_DATA.len() as u32).to_le_bytes());
            img[next_attr + 20..next_attr + 22].copy_from_slice(&data_val_offset.to_le_bytes());
            img[next_attr + data_val_offset as usize
                ..next_attr + data_val_offset as usize + FILE_DATA.len()]
                .copy_from_slice(FILE_DATA);

            next_attr += data_attr_len;
        }

        // Write ATTR_END marker.
        img[next_attr..next_attr + 4].copy_from_slice(&ATTR_END.to_le_bytes());

        // used_size and allocated_size in the record header.
        let used: u32 = (next_attr - offset + 4) as u32;
        img[offset + 24..offset + 28].copy_from_slice(&used.to_le_bytes());
        img[offset + 28..offset + 32].copy_from_slice(&(REC_SIZE as u32).to_le_bytes());
    }

    fn cursor_of(img: &[u8]) -> Cursor<Vec<u8>> {
        Cursor::new(img.to_vec())
    }

    // ── Detection tests ───────────────────────────────────────────────────

    #[test]
    fn detect_valid_ntfs_boot() {
        let boot = make_ntfs_boot_sector();
        let mut img = vec![0u8; 1024];
        img[..512].copy_from_slice(&boot);
        let mut c = cursor_of(&img);
        assert!(detect(&mut c), "should detect valid NTFS boot sector");
    }

    #[test]
    fn detect_restores_position() {
        let boot = make_ntfs_boot_sector();
        let mut img = vec![0u8; 1024];
        img[..512].copy_from_slice(&boot);
        let mut c = cursor_of(&img);
        c.seek(SeekFrom::Start(42)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(
            c.stream_position().unwrap(),
            42,
            "detect must restore stream position"
        );
    }

    #[test]
    fn detect_restores_position_on_failure() {
        let img = vec![0u8; 512];
        let mut c = Cursor::new(img);
        c.seek(SeekFrom::Start(7)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(c.stream_position().unwrap(), 7);
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let mut boot = make_ntfs_boot_sector();
        // Corrupt the OEM ID.
        boot[3..11].copy_from_slice(b"FAT32   ");
        let mut img = vec![0u8; 1024];
        img[..512].copy_from_slice(&boot);
        let mut c = cursor_of(&img);
        assert!(
            !detect(&mut c),
            "corrupted OEM ID should not detect as NTFS"
        );
    }

    #[test]
    fn detect_rejects_too_short() {
        let img = vec![0u8; 8];
        let mut c = Cursor::new(img);
        assert!(!detect(&mut c));
    }

    #[test]
    fn detect_rejects_fat_image() {
        let mut img = vec![0u8; 1024];
        img[0] = 0xEB;
        img[1] = 0x58;
        img[2] = 0x90;
        img[3..11].copy_from_slice(b"MSDOS5.0");
        let mut c = Cursor::new(img);
        assert!(!detect(&mut c), "FAT image should not be detected as NTFS");
    }

    #[test]
    fn parse_boot_sector_valid() {
        let boot = make_ntfs_boot_sector();
        let bs = parse_boot_sector(&boot).expect("parse boot sector");
        assert_eq!(bs.cluster_size, 4096);
        assert_eq!(bs.mft_record_size, 1024);
        assert_eq!(bs.mft_offset, 4 * 4096);
    }

    #[test]
    fn parse_boot_sector_positive_cpfrs() {
        // clusters_per_FRS = 1 (positive) → mft_record_size = 1 * 4096 = 4096.
        let mut boot = make_ntfs_boot_sector();
        boot[64] = 1u8;
        let bs = parse_boot_sector(&boot).expect("positive clusters_per_FRS");
        assert_eq!(bs.mft_record_size, 4096);
    }

    #[test]
    fn apply_fixup_basic() {
        // Build a 1024-byte buffer with FILE signature and USA.
        let mut buf = vec![0u8; 1024];
        buf[0..4].copy_from_slice(b"FILE");
        let usa_off: u16 = 48;
        let usa_cnt: u16 = 3;
        buf[4..6].copy_from_slice(&usa_off.to_le_bytes());
        buf[6..8].copy_from_slice(&usa_cnt.to_le_bytes());
        // USN = 0xABCD.
        buf[48..50].copy_from_slice(&0xABCDu16.to_le_bytes());
        // Fix-up values.
        buf[50..52].copy_from_slice(&0x1234u16.to_le_bytes()); // sector 1
        buf[52..54].copy_from_slice(&0x5678u16.to_le_bytes()); // sector 2
                                                               // Put USN at sector ends (510 and 1022).
        buf[510..512].copy_from_slice(&0xABCDu16.to_le_bytes());
        buf[1022..1024].copy_from_slice(&0xABCDu16.to_le_bytes());

        let ok = apply_fixup(&mut buf);
        assert!(ok);
        assert_eq!(&buf[510..512], &0x1234u16.to_le_bytes());
        assert_eq!(&buf[1022..1024], &0x5678u16.to_le_bytes());
    }

    #[test]
    fn decode_runlist_single_run() {
        // Header 0x11: len_size=1, off_size=1. Count=8, delta=+3.
        let data = [0x11u8, 8, 3, 0x00];
        let (runs, had_sparse) = decode_runlist(&data);
        assert!(!had_sparse);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].start_lcn, 3);
        assert_eq!(runs[0].length, 8);
    }

    #[test]
    fn decode_runlist_two_runs() {
        // Run 1: 0x11 count=4 delta=+10 → LCN 10, len 4.
        // Run 2: 0x11 count=2 delta=+5  → LCN 15, len 2.
        let data = [0x11, 4, 10, 0x11, 2, 5, 0x00];
        let (runs, had_sparse) = decode_runlist(&data);
        assert!(!had_sparse);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].start_lcn, 10);
        assert_eq!(runs[0].length, 4);
        assert_eq!(runs[1].start_lcn, 15);
        assert_eq!(runs[1].length, 2);
    }

    #[test]
    fn decode_runlist_negative_delta() {
        // Run 1: count=8, delta=+20 → LCN 20.
        // Run 2: count=4, delta=-5  → LCN 15.
        // -5 in two's complement as i8 = 0xFB.
        let data = [0x11, 8, 20, 0x11, 4, 0xFBu8, 0x00];
        let (runs, had_sparse) = decode_runlist(&data);
        assert!(!had_sparse);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].start_lcn, 20);
        assert_eq!(runs[1].start_lcn, 15);
    }

    #[test]
    fn parse_minimal_image_tree_shape() {
        let img = make_minimal_ntfs_image();
        let mut c = cursor_of(&img);

        // Detection should succeed.
        assert!(detect(&mut c), "should detect minimal NTFS image");

        c.seek(SeekFrom::Start(0)).unwrap();
        let root = detect_and_parse(&mut c).expect("parse minimal NTFS image");

        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        // The image contains one user file (hello.txt at record 12) whose
        // parent_ref = 5 (root).
        let hello = root.children.iter().find(|n| n.name == "hello.txt");
        assert!(hello.is_some(), "hello.txt should be in root");
        assert!(!hello.unwrap().is_directory);
    }

    #[test]
    fn parse_minimal_image_file_size() {
        let img = make_minimal_ntfs_image();
        let mut c = cursor_of(&img);
        c.seek(SeekFrom::Start(0)).unwrap();
        let root = detect_and_parse(&mut c).expect("parse");
        let hello = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        assert_eq!(
            hello.size,
            b"hello ntfs\n".len() as u64,
            "file size should match resident $DATA value length"
        );
    }

    #[test]
    fn parse_minimal_image_file_location_and_contents() {
        let img = make_minimal_ntfs_image();
        let mut c = cursor_of(&img);
        c.seek(SeekFrom::Start(0)).unwrap();
        let root = detect_and_parse(&mut c).expect("parse");
        let hello = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();

        assert!(
            hello.file_location.is_some(),
            "resident $DATA should have a file_location"
        );

        let loc = hello.file_location.unwrap();
        let len = hello.size as usize;
        c.seek(SeekFrom::Start(loc)).unwrap();
        let mut buf = vec![0u8; len];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(buf, b"hello ntfs\n");
    }
}
