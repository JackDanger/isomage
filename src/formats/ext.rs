//! ext2 / ext3 / ext4 filesystem reader (`ext` feature).
//!
//! Reads the directory tree of an ext2/3/4 image and produces a
//! [`TreeNode`] compatible with `cat_node` / `extract_node`.
//!
//! Reference: Linux kernel Documentation/filesystems/ext4/,
//! and the e2fsprogs source tree.
//!
//! ## What is implemented
//!
//! - Superblock detection (magic `0xEF53` at offset 1080).
//! - Block group descriptor tables (32-byte "classic" and 64-byte
//!   "64bit" feature variants).
//! - Inode table reads for directories and regular files.
//! - Extent tree traversal (ext4, `EXT4_EXTENTS_FL`), up to depth 5.
//! - Classical block pointers: direct (i\_block\[0..11\]), single-indirect
//!   (i\_block\[12\]), double-indirect (i\_block\[13\]), and triple-indirect
//!   (i\_block\[14\]).
//! - Directory entry scanning (linear and htree-transparent: we walk the
//!   raw data blocks, so HTree is transparent).
//! - `INCOMPAT_FILETYPE` directories (file_type byte in each entry).
//! - `INCOMPAT_64BIT` high-32-bit block addresses in BGDs.
//! - Symlinks appear in the tree with correct size; devices/FIFOs/sockets
//!   are silently skipped.
//!
//! ## `file_location` semantics
//!
//! A file's `file_location` is set **only** when all data resides in a
//! single physically-contiguous block run. This is true for:
//! - Extent-tree depth=0 with exactly one extent.
//! - Classical mode with only direct blocks that happen to be contiguous
//!   and the file fits within the 12 direct pointers.
//!
//! Multi-extent, indirect-block, and inline-data files still appear in
//! the tree with correct `size`, but `file_location = None`. `cat_node`
//! will refuse those; `extract_node` can be extended later.

use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── ECMA / Linux kernel spec constants ───────────────────────────────────────

/// Superblock magic (Linux kernel fs/ext4/ext4.h `EXT4_SUPER_MAGIC`).
const EXT_MAGIC: u16 = 0xEF53;

/// Byte offset of the superblock from the start of the filesystem.
/// (ECMA — not applicable; Linux kernel convention: 1024 bytes.)
const SUPERBLOCK_OFFSET: u64 = 1024;

// Inode mode bits (stat(2) S_IF* family).
const S_IFMT: u16 = 0xF000;
const S_IFREG: u16 = 0x8000;
const S_IFDIR: u16 = 0x4000;
const S_IFLNK: u16 = 0xA000;

// Inode flag bits (i_flags).
const EXT4_EXTENTS_FL: u32 = 0x0008_0000;
const EXT4_INLINE_DATA_FL: u32 = 0x1000_0000;

// Incompatible feature bits (s_feature_incompat).
const INCOMPAT_FILETYPE: u32 = 0x0002;
// INCOMPAT_EXTENTS (0x0040): inodes use extent trees; we check EXT4_EXTENTS_FL
// per-inode instead, so this flag is informational here.
const INCOMPAT_64BIT: u32 = 0x0080;

/// Extent tree node header magic (little-endian `0xF30A`).
const EXTENT_MAGIC: u16 = 0xF30A;

/// Maximum directory recursion depth before we give up, to avoid
/// stack overflows on corrupted images.
const MAX_DEPTH: usize = 32;

// ── Error type ────────────────────────────────────────────────────────────────

/// Reasons `detect` or `detect_and_parse` can fail.
#[derive(Debug)]
pub enum Error {
    /// Image shorter than the minimum to hold a superblock.
    TooShort,
    /// Superblock magic was not `0xEF53`, or other structural
    /// inconsistency that makes parsing unsafe to continue.
    BadSuperblock,
    /// Underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image too short to contain an ext superblock"),
            Error::BadSuperblock => write!(f, "ext superblock magic 0xEF53 missing"),
            Error::Io(e) => write!(f, "ext I/O error: {e}"),
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

// ── Superblock ────────────────────────────────────────────────────────────────

/// Parsed ext superblock fields we actually use.
#[derive(Debug, Clone)]
struct Superblock {
    inodes_per_group: u32,
    first_data_block: u32, // 1 for 1 KiB blocks, 0 for larger
    log_block_size: u32,   // block_size = 1024 << log_block_size
    inode_size: u16,       // inode size in bytes
    feature_incompat: u32,
    desc_size: u16, // block group descriptor size (64-bit ext4)
}

impl Superblock {
    fn block_size(&self) -> u64 {
        1024u64 << self.log_block_size
    }

    fn has_incompat(&self, flag: u32) -> bool {
        self.feature_incompat & flag != 0
    }

    fn desc_size_effective(&self) -> u64 {
        // 32 bytes for non-64bit, s_desc_size for 64-bit (must be ≥ 64).
        if self.has_incompat(INCOMPAT_64BIT) && self.desc_size >= 64 {
            self.desc_size as u64
        } else {
            32
        }
    }

    /// Byte offset (from image start) of the first block group descriptor.
    fn bgd_table_offset(&self, base_offset: u64) -> u64 {
        // The BGD table immediately follows the superblock block.
        // For 1 KiB blocks: superblock is in block 1, BGD in block 2.
        // For ≥ 2 KiB blocks: superblock is in block 0, BGD in block 1.
        let first_bgd_block = self.first_data_block as u64 + 1;
        base_offset + first_bgd_block * self.block_size()
    }
}

fn read_superblock<R: Read + Seek>(file: &mut R, base_offset: u64) -> Result<Superblock, Error> {
    file.seek(SeekFrom::Start(base_offset + SUPERBLOCK_OFFSET))?;
    let mut sb = [0u8; 264]; // up to s_desc_size at offset 236+2=238, take 264 for safety
    file.read_exact(&mut sb).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;
    let magic = u16::from_le_bytes([sb[56], sb[57]]);
    if magic != EXT_MAGIC {
        return Err(Error::BadSuperblock);
    }
    // s_inodes_count, s_blocks_count_lo: only used for sanity checks.
    let blocks_count_lo = u32::from_le_bytes(sb[4..8].try_into().unwrap());
    let first_data_block = u32::from_le_bytes(sb[20..24].try_into().unwrap());
    let log_block_size = u32::from_le_bytes(sb[24..28].try_into().unwrap());
    let blocks_per_group = u32::from_le_bytes(sb[32..36].try_into().unwrap());
    let inodes_per_group = u32::from_le_bytes(sb[40..44].try_into().unwrap());

    // ext supports block sizes 1–64 KiB: log_block_size ∈ [0, 6].
    // Values ≥ 64 would panic the left-shift below; values > 6 indicate corruption.
    if log_block_size > 6 {
        return Err(Error::BadSuperblock);
    }

    let rev_level = u32::from_le_bytes(sb[76..80].try_into().unwrap());

    // Sanity: blocks_per_group must be non-zero (it's a divisor in inode location calc).
    if blocks_per_group == 0 {
        return Err(Error::BadSuperblock);
    }
    // Sanity: non-empty filesystem.
    if blocks_count_lo == 0 {
        return Err(Error::BadSuperblock);
    }
    // Sanity: inodes_per_group is a divisor when locating inodes.
    if inodes_per_group == 0 {
        return Err(Error::BadSuperblock);
    }

    let (inode_size, feature_incompat, desc_size) = if rev_level >= 1 {
        let inode_size = u16::from_le_bytes([sb[88], sb[89]]);
        let feature_incompat = u32::from_le_bytes(sb[96..100].try_into().unwrap());
        let desc_size = u16::from_le_bytes([sb[236], sb[237]]);
        (inode_size, feature_incompat, desc_size)
    } else {
        // rev_level 0: fixed 128-byte inodes, no extents.
        (128, 0, 32)
    };

    // Validate inode_size: must be a power of two ≥ 128 and ≤ block_size.
    let bs = 1024u64 << log_block_size;
    let eff_inode_size = if inode_size < 128 { 128u16 } else { inode_size };
    if eff_inode_size as u64 > bs {
        return Err(Error::BadSuperblock);
    }

    Ok(Superblock {
        inodes_per_group,
        first_data_block,
        log_block_size,
        inode_size: eff_inode_size,
        feature_incompat,
        desc_size,
    })
}

// ── Block Group Descriptor ────────────────────────────────────────────────────

/// Parsed block group descriptor, just the inode table address.
struct Bgd {
    inode_table: u64, // block number of the inode table
}

fn read_bgd<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    group: u64,
) -> Result<Bgd, Error> {
    let desc_size = sb.desc_size_effective();
    let offset = sb.bgd_table_offset(base_offset) + group * desc_size;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; desc_size as usize];
    file.read_exact(&mut buf)?;

    let inode_table_lo = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as u64;
    let inode_table = if sb.has_incompat(INCOMPAT_64BIT) && desc_size >= 64 {
        let hi = u32::from_le_bytes(buf[40..44].try_into().unwrap()) as u64;
        (hi << 32) | inode_table_lo
    } else {
        inode_table_lo
    };

    Ok(Bgd { inode_table })
}

// ── Inode ─────────────────────────────────────────────────────────────────────

/// Parsed inode fields.
struct Inode {
    mode: u16,
    size: u64, // full 64-bit size (lo | hi<<32 for regular files)
    flags: u32,
    i_block: [u32; 15], // raw 60-byte block-pointer / extent-root area
}

impl Inode {
    fn file_type_char(&self) -> u8 {
        ((self.mode & S_IFMT) >> 12) as u8
    }

    fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    fn is_reg(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }

    fn uses_extents(&self) -> bool {
        self.flags & EXT4_EXTENTS_FL != 0
    }

    fn is_inline(&self) -> bool {
        self.flags & EXT4_INLINE_DATA_FL != 0
    }
}

fn read_inode<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    inode_num: u32,
) -> Result<Inode, Error> {
    if inode_num == 0 {
        return Err(Error::BadSuperblock);
    }
    let group = (inode_num as u64 - 1) / sb.inodes_per_group as u64;
    let local_index = (inode_num as u64 - 1) % sb.inodes_per_group as u64;

    let bgd = read_bgd(file, sb, base_offset, group)?;
    let inode_offset =
        base_offset + bgd.inode_table * sb.block_size() + local_index * sb.inode_size as u64;

    file.seek(SeekFrom::Start(inode_offset))?;
    // Read at least 112 bytes (up through i_size_high at 108..112).
    let read_len = (sb.inode_size as usize).max(112);
    let mut buf = vec![0u8; read_len];
    file.read_exact(&mut buf)?;

    let mode = u16::from_le_bytes([buf[0], buf[1]]);
    let size_lo = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let flags = u32::from_le_bytes(buf[32..36].try_into().unwrap());

    // i_block: 15 u32s at offset 40.
    let mut i_block = [0u32; 15];
    for (i, slot) in i_block.iter_mut().enumerate() {
        let off = 40 + i * 4;
        *slot = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
    }

    // Size high word (i_size_high, previously i_dir_acl) at offset 108.
    // Only meaningful for regular files (not directories).
    let size_high = u32::from_le_bytes(buf[108..112].try_into().unwrap());
    let size = if mode & S_IFMT == S_IFREG {
        (size_high as u64) << 32 | size_lo as u64
    } else {
        size_lo as u64
    };

    Ok(Inode {
        mode,
        size,
        flags,
        i_block,
    })
}

// ── Block reading ─────────────────────────────────────────────────────────────

/// Read `block_num` (filesystem-relative block number) into `buf`.
fn read_block<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    block_num: u64,
    buf: &mut Vec<u8>,
) -> Result<(), Error> {
    buf.resize(sb.block_size() as usize, 0);
    file.seek(SeekFrom::Start(base_offset + block_num * sb.block_size()))?;
    file.read_exact(buf)?;
    Ok(())
}

// ── Extent tree ───────────────────────────────────────────────────────────────

/// One leaf extent: maps logical blocks [first_logical_block, +len) to
/// physical block phys.
#[derive(Debug, Clone, Copy)]
struct Extent {
    len: u16,        // number of blocks (ee_len & 0x7FFF)
    phys: u64,       // physical starting block
    unwritten: bool, // bit 15 of ee_len: preallocated but not yet written
}

/// One internal node index entry: covers logical blocks starting at
/// first_logical_block, child node at physical block leaf.
#[derive(Debug, Clone, Copy)]
struct ExtentIdx {
    leaf: u64,
}

/// Decode the 12-byte extent tree header.
fn parse_extent_header(data: &[u8]) -> Option<(u16, u16)> {
    // magic, entries, max, depth
    if data.len() < 12 {
        return None;
    }
    let magic = u16::from_le_bytes([data[0], data[1]]);
    if magic != EXTENT_MAGIC {
        return None;
    }
    let entries = u16::from_le_bytes([data[2], data[3]]);
    let depth = u16::from_le_bytes([data[6], data[7]]);
    Some((entries, depth))
}

/// Parse `entries` leaf extents from `data` starting at byte 12.
fn parse_leaf_extents(data: &[u8], entries: u16) -> Vec<Extent> {
    let mut out = Vec::with_capacity(entries as usize);
    for i in 0..entries as usize {
        let off = 12 + i * 12;
        if off + 12 > data.len() {
            break;
        }
        let ee_len = u16::from_le_bytes([data[off + 4], data[off + 5]]);
        let ee_start_hi = u16::from_le_bytes([data[off + 6], data[off + 7]]) as u64;
        let ee_start_lo = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap()) as u64;
        let phys = (ee_start_hi << 32) | ee_start_lo;
        let unwritten = ee_len & 0x8000 != 0;
        let len = ee_len & 0x7FFF;
        out.push(Extent {
            len,
            phys,
            unwritten,
        });
    }
    out
}

/// Parse `entries` internal index entries from `data` starting at byte 12.
fn parse_idx_entries(data: &[u8], entries: u16) -> Vec<ExtentIdx> {
    let mut out = Vec::with_capacity(entries as usize);
    for i in 0..entries as usize {
        let off = 12 + i * 12;
        if off + 12 > data.len() {
            break;
        }
        let leaf_lo = u32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()) as u64;
        let leaf_hi = u16::from_le_bytes([data[off + 8], data[off + 9]]) as u64;
        let leaf = (leaf_hi << 32) | leaf_lo;
        out.push(ExtentIdx { leaf });
    }
    out
}

/// Walk the extent tree rooted at `node_data` and collect all leaf extents.
/// `node_data` is the raw 60 bytes of i_block, or a block read from disk.
///
/// Recursion is bounded by the depth field in each header; max depth 5 per
/// kernel, and we hard-cap at 5 to be safe on corrupted images.
fn collect_extents<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    node_data: &[u8],
    remaining_depth: u8,
) -> Result<Vec<Extent>, Error> {
    let Some((entries, depth)) = parse_extent_header(node_data) else {
        return Ok(Vec::new());
    };

    if depth == 0 {
        // Leaf node.
        return Ok(parse_leaf_extents(node_data, entries));
    }

    if remaining_depth == 0 {
        // Depth exceeded our safety cap — treat as empty.
        return Ok(Vec::new());
    }

    // Internal node: recurse into each child block.
    let idx_entries = parse_idx_entries(node_data, entries);
    let mut block_buf = Vec::new();
    let mut all_extents = Vec::new();
    for idx in idx_entries {
        read_block(file, sb, base_offset, idx.leaf, &mut block_buf)?;
        let child_extents =
            collect_extents(file, sb, base_offset, &block_buf, remaining_depth - 1)?;
        all_extents.extend(child_extents);
    }
    Ok(all_extents)
}

// ── Classical block pointer iteration ────────────────────────────────────────

/// Read one block of u32 block pointers from `block_num`.
fn read_ptr_block<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    block_num: u64,
) -> Result<Vec<u32>, Error> {
    let bs = sb.block_size() as usize;
    let mut buf = vec![0u8; bs];
    file.seek(SeekFrom::Start(base_offset + block_num * sb.block_size()))?;
    file.read_exact(&mut buf)?;
    Ok(buf
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect())
}

/// Collect all physical data block numbers for an inode using classical
/// (non-extent) block addressing, in logical order, stopping once `size`
/// bytes are covered.
///
/// Handles direct (0..11), single-indirect (12), double-indirect (13),
/// and triple-indirect (14) pointers.
fn collect_classical_blocks<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    inode: &Inode,
    size: u64,
) -> Result<Vec<u64>, Error> {
    let bs = sb.block_size();
    let mut blocks: Vec<u64> = Vec::new();
    let mut covered: u64 = 0;

    // Direct blocks: i_block[0..11].
    for &blk in &inode.i_block[0..12] {
        if blk == 0 || covered >= size {
            break;
        }
        blocks.push(blk as u64);
        covered += bs;
    }

    if covered >= size {
        return Ok(blocks);
    }

    // Single-indirect: i_block[12].
    let si = inode.i_block[12];
    if si != 0 {
        let ptrs = read_ptr_block(file, sb, base_offset, si as u64)?;
        for blk in ptrs {
            if blk == 0 || covered >= size {
                break;
            }
            blocks.push(blk as u64);
            covered += bs;
        }
    }

    if covered >= size {
        return Ok(blocks);
    }

    // Double-indirect: i_block[13].
    let di = inode.i_block[13];
    if di != 0 {
        let l1 = read_ptr_block(file, sb, base_offset, di as u64)?;
        'di_outer: for l1ptr in l1 {
            if l1ptr == 0 || covered >= size {
                break;
            }
            let l2 = read_ptr_block(file, sb, base_offset, l1ptr as u64)?;
            for blk in l2 {
                if blk == 0 || covered >= size {
                    break 'di_outer;
                }
                blocks.push(blk as u64);
                covered += bs;
            }
        }
    }

    if covered >= size {
        return Ok(blocks);
    }

    // Triple-indirect: i_block[14].
    let ti = inode.i_block[14];
    if ti != 0 {
        let l1 = read_ptr_block(file, sb, base_offset, ti as u64)?;
        'ti_outer: for l1ptr in l1 {
            if l1ptr == 0 || covered >= size {
                break;
            }
            let l2 = read_ptr_block(file, sb, base_offset, l1ptr as u64)?;
            'ti_middle: for l2ptr in l2 {
                if l2ptr == 0 || covered >= size {
                    break 'ti_outer;
                }
                let l3 = read_ptr_block(file, sb, base_offset, l2ptr as u64)?;
                for blk in l3 {
                    if blk == 0 || covered >= size {
                        break 'ti_middle;
                    }
                    blocks.push(blk as u64);
                    covered += bs;
                }
            }
        }
    }

    Ok(blocks)
}

// ── Directory parsing ─────────────────────────────────────────────────────────

/// One directory entry (partially parsed — just what we need).
#[derive(Debug)]
struct DirEntry {
    inode: u32,
    name: String,
}

/// Scan a raw directory data block for entries, pushing valid ones into `out`.
fn scan_dir_block(data: &[u8], has_filetype: bool, out: &mut Vec<DirEntry>) {
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let inode = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        let rec_len = u16::from_le_bytes([data[pos + 4], data[pos + 5]]) as usize;
        let name_len = data[pos + 6] as usize;
        // file_type is at pos+7 when INCOMPAT_FILETYPE; otherwise name_len_high.
        // Either way, the name starts at pos+8.

        if rec_len < 8 || pos + rec_len > data.len() {
            break;
        }
        if inode != 0 && name_len > 0 && pos + 8 + name_len <= data.len() {
            let raw = &data[pos + 8..pos + 8 + name_len];
            let name = String::from_utf8_lossy(raw).into_owned();
            if name != "." && name != ".." {
                let _file_type = if has_filetype { data[pos + 7] } else { 0 };
                out.push(DirEntry { inode, name });
            }
        }
        pos += rec_len.max(1); // guard against rec_len=0 infinite loop
    }
}

/// Read all directory entries from an inode.
fn read_dir_entries<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    inode: &Inode,
) -> Result<Vec<DirEntry>, Error> {
    let has_filetype = sb.has_incompat(INCOMPAT_FILETYPE);
    let mut entries = Vec::new();
    let mut block_buf = Vec::new();

    if inode.uses_extents() {
        // i_block holds the extent tree root (60 bytes = 12-byte header + up
        // to 4 leaf extents at depth=0, or index entries at depth>0).
        let root_bytes: Vec<u8> = inode
            .i_block
            .iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();
        let extents = collect_extents(file, sb, base_offset, &root_bytes, 5)?;
        for ext in extents {
            for i in 0..ext.len as u64 {
                read_block(file, sb, base_offset, ext.phys + i, &mut block_buf)?;
                scan_dir_block(&block_buf, has_filetype, &mut entries);
            }
        }
    } else {
        // Classical block pointers: collect block numbers first, then read
        // each block in a separate pass to avoid split-borrow issues.
        let blk_nums = collect_classical_blocks(file, sb, base_offset, inode, inode.size)?;
        for blk in blk_nums {
            read_block(file, sb, base_offset, blk, &mut block_buf)?;
            scan_dir_block(&block_buf, has_filetype, &mut entries);
        }
    }

    Ok(entries)
}

// ── File location detection ────────────────────────────────────────────────────

/// Try to find a single contiguous physical location for `inode`'s data.
/// Returns `Some(byte_offset_in_image)` only when the entire file content
/// lives in one unbroken physical run.
fn single_run_location<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    inode: &Inode,
) -> Result<Option<u64>, Error> {
    if inode.size == 0 {
        return Ok(None);
    }
    if inode.is_inline() {
        return Ok(None);
    }

    if inode.uses_extents() {
        let root_bytes: Vec<u8> = inode
            .i_block
            .iter()
            .flat_map(|&w| w.to_le_bytes())
            .collect();
        let extents = collect_extents(file, sb, base_offset, &root_bytes, 5)?;
        // Single contiguous run: exactly one non-unwritten extent whose block
        // count covers the full file. Unwritten extents are preallocated but
        // contain stale on-disk data; reading them would return garbage.
        if extents.len() == 1 {
            let ext = &extents[0];
            if !ext.unwritten {
                let needed_blocks = inode.size.div_ceil(sb.block_size());
                if ext.len as u64 >= needed_blocks {
                    return Ok(Some(base_offset + ext.phys * sb.block_size()));
                }
            }
        }
        return Ok(None);
    }

    // Classical mode: check that the direct blocks are a contiguous run.
    // We only check if the file fits within the direct pointer range
    // (i_block[0..11]) to keep the logic simple and reliable.
    let bs = sb.block_size();
    let needed_blocks = inode.size.div_ceil(bs);
    if needed_blocks > 12 {
        // Would require indirect blocks; skip contiguous check.
        return Ok(None);
    }

    // Require all needed direct blocks to be consecutive.
    let first = inode.i_block[0] as u64;
    if first == 0 {
        return Ok(None);
    }
    for i in 1..needed_blocks as usize {
        if inode.i_block[i] as u64 != first + i as u64 {
            return Ok(None);
        }
    }
    Ok(Some(base_offset + first * bs))
}

// ── Tree building ─────────────────────────────────────────────────────────────

/// Recursively build a `TreeNode` tree rooted at `inode_num`.
/// `depth` is the current recursion depth (starts at 0 for root).
fn build_tree<R: Read + Seek>(
    file: &mut R,
    sb: &Superblock,
    base_offset: u64,
    name: String,
    inode_num: u32,
    depth: usize,
) -> Result<Option<TreeNode>, Error> {
    if depth > MAX_DEPTH {
        return Ok(None);
    }

    let inode = read_inode(file, sb, base_offset, inode_num)?;

    if inode.is_dir() {
        let mut node = TreeNode::new_directory(name);
        let entries = read_dir_entries(file, sb, base_offset, &inode)?;
        for entry in entries {
            if let Some(child) =
                build_tree(file, sb, base_offset, entry.name, entry.inode, depth + 1)?
            {
                node.add_child(child);
            }
        }
        Ok(Some(node))
    } else if inode.is_reg() {
        // Inline-data files: in tree but no location.
        if inode.is_inline() {
            return Ok(Some(TreeNode::new_file(name, inode.size)));
        }
        let loc = single_run_location(file, sb, base_offset, &inode)?;
        let node = match loc {
            Some(offset) => TreeNode::new_file_with_location(name, inode.size, offset, inode.size),
            None => TreeNode::new_file(name, inode.size),
        };
        Ok(Some(node))
    } else if inode.is_symlink() {
        // Fast symlinks store the target path in i_block directly; there are
        // no data blocks. Non-fast symlinks do use blocks, but we can't
        // reliably distinguish without reading more state. Never set
        // file_location for symlinks to avoid returning a bogus offset.
        Ok(Some(TreeNode::new_file(name, inode.size)))
    } else {
        // Block/char devices, FIFOs, sockets — skip.
        let _ = inode.file_type_char(); // suppress unused warning
        Ok(None)
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return `true` if the stream at its current position looks like an ext
/// filesystem.
///
/// Reads 2 bytes from the superblock's magic field and restores the stream
/// position whether or not detection succeeds.
pub fn detect<R: Read + Seek>(file: &mut R) -> bool {
    let saved = match file.stream_position() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let ok = detect_inner(file);
    let _ = file.seek(SeekFrom::Start(saved));
    ok
}

fn detect_inner<R: Read + Seek>(file: &mut R) -> bool {
    // We need to know where "base" is — use current position.
    let base = match file.stream_position() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let magic_offset = base + SUPERBLOCK_OFFSET + 56;
    if file.seek(SeekFrom::Start(magic_offset)).is_err() {
        return false;
    }
    let mut magic_buf = [0u8; 2];
    if file.read_exact(&mut magic_buf).is_err() {
        return false;
    }
    u16::from_le_bytes(magic_buf) == EXT_MAGIC
}

/// Detect and parse an ext2/3/4 filesystem, returning the directory tree.
///
/// `file`'s current position is treated as the filesystem's base offset,
/// allowing this function to parse ext partitions that start mid-image.
pub fn detect_and_parse<R: Read + Seek>(file: &mut R) -> Result<TreeNode, Error> {
    let base_offset = file.stream_position()?;

    let sb = read_superblock(file, base_offset)?;

    // Root inode is always #2.
    let mut root =
        build_tree(file, &sb, base_offset, "/".to_string(), 2, 0)?.ok_or(Error::BadSuperblock)?;

    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Minimal in-memory ext2 image builder ──────────────────────────────

    /// Build a minimal valid ext2 image in memory.
    ///
    /// Layout (1 KiB blocks, 256 KiB total, 1 block group):
    ///
    /// - Block 0: boot block (empty)
    /// - Block 1: superblock
    /// - Block 2: block group descriptor table (one entry, 32 bytes)
    /// - Block 3: block bitmap (all zeros except blocks 0-7 marked used)
    /// - Block 4: inode bitmap (inodes 1-3 marked used)
    /// - Block 5: inode table (inode 2 = root dir, inode 3 = hello.txt)
    /// - Block 6: root directory data block
    /// - Block 7: hello.txt data block
    ///
    /// Features: INCOMPAT_FILETYPE, no extents (classical block pointers).
    fn make_ext2_image() -> Vec<u8> {
        const BS: usize = 1024;
        const TOTAL_BLOCKS: usize = 256;
        const IMAGE_SIZE: usize = TOTAL_BLOCKS * BS;
        const INODE_SIZE: usize = 128;
        const INODES_PER_GROUP: usize = 256;

        // Block assignments.
        const BLOCK_BITMAP_BLK: usize = 3;
        const INODE_BITMAP_BLK: usize = 4;
        const INODE_TABLE_BLK: usize = 5;
        const ROOT_DATA_BLK: usize = 6;
        const FILE_DATA_BLK: usize = 7;

        // Inode numbers.
        const ROOT_INUM: usize = 2;
        const FILE_INUM: usize = 3;

        let mut img = vec![0u8; IMAGE_SIZE];

        // ── Superblock (block 1 = offset 1024) ──────────────────────────
        {
            let sb = &mut img[1024..1024 + 264];
            // s_inodes_count
            sb[0..4].copy_from_slice(&(INODES_PER_GROUP as u32).to_le_bytes());
            // s_blocks_count_lo
            sb[4..8].copy_from_slice(&(TOTAL_BLOCKS as u32).to_le_bytes());
            // s_first_data_block = 1 (for 1 KiB blocks)
            sb[20..24].copy_from_slice(&1u32.to_le_bytes());
            // s_log_block_size = 0  →  block_size = 1024
            sb[24..28].copy_from_slice(&0u32.to_le_bytes());
            // s_blocks_per_group
            sb[32..36].copy_from_slice(&(TOTAL_BLOCKS as u32).to_le_bytes());
            // s_inodes_per_group
            sb[40..44].copy_from_slice(&(INODES_PER_GROUP as u32).to_le_bytes());
            // s_magic = 0xEF53
            sb[56..58].copy_from_slice(&EXT_MAGIC.to_le_bytes());
            // s_rev_level = 1 (dynamic)
            sb[76..80].copy_from_slice(&1u32.to_le_bytes());
            // s_first_ino = 11
            sb[84..88].copy_from_slice(&11u32.to_le_bytes());
            // s_inode_size = 128
            sb[88..90].copy_from_slice(&(INODE_SIZE as u16).to_le_bytes());
            // s_feature_compat = 0
            sb[92..96].copy_from_slice(&0u32.to_le_bytes());
            // s_feature_incompat = INCOMPAT_FILETYPE
            sb[96..100].copy_from_slice(&INCOMPAT_FILETYPE.to_le_bytes());
            // s_feature_ro_compat = 0
            sb[100..104].copy_from_slice(&0u32.to_le_bytes());
            // s_desc_size = 32
            sb[236..238].copy_from_slice(&32u16.to_le_bytes());
        }

        // ── Block group descriptor (block 2 = offset 2048) ──────────────
        {
            // BGD is right after the superblock block. For 1 KiB blocks,
            // s_first_data_block = 1, so BGD starts at block 2.
            let bgd = &mut img[2 * BS..2 * BS + 32];
            // bg_block_bitmap_lo
            bgd[0..4].copy_from_slice(&(BLOCK_BITMAP_BLK as u32).to_le_bytes());
            // bg_inode_bitmap_lo
            bgd[4..8].copy_from_slice(&(INODE_BITMAP_BLK as u32).to_le_bytes());
            // bg_inode_table_lo
            bgd[8..12].copy_from_slice(&(INODE_TABLE_BLK as u32).to_le_bytes());
        }

        // ── Inode table ──────────────────────────────────────────────────
        // Inode #2 (root directory), stored at index 1 (0-indexed).
        {
            let inode_base = INODE_TABLE_BLK * BS;
            let root_off = inode_base + (ROOT_INUM - 1) * INODE_SIZE;
            let ino = &mut img[root_off..root_off + INODE_SIZE];
            // i_mode = S_IFDIR | 0755
            let mode: u16 = S_IFDIR | 0o755;
            ino[0..2].copy_from_slice(&mode.to_le_bytes());
            // i_size_lo (will be filled after directory block is built)
            ino[4..8].copy_from_slice(&(BS as u32).to_le_bytes());
            // i_links_count
            ino[26..28].copy_from_slice(&2u16.to_le_bytes());
            // i_flags = 0 (no extents, no inline)
            ino[32..36].copy_from_slice(&0u32.to_le_bytes());
            // i_block[0] = ROOT_DATA_BLK
            ino[40..44].copy_from_slice(&(ROOT_DATA_BLK as u32).to_le_bytes());
        }

        // Inode #3 (hello.txt), stored at index 2.
        {
            let inode_base = INODE_TABLE_BLK * BS;
            let file_off = inode_base + (FILE_INUM - 1) * INODE_SIZE;
            let ino = &mut img[file_off..file_off + INODE_SIZE];
            let mode: u16 = S_IFREG | 0o644;
            ino[0..2].copy_from_slice(&mode.to_le_bytes());
            // i_size_lo = 12 ("hello world\n")
            ino[4..8].copy_from_slice(&12u32.to_le_bytes());
            ino[26..28].copy_from_slice(&1u16.to_le_bytes());
            ino[32..36].copy_from_slice(&0u32.to_le_bytes());
            // i_block[0] = FILE_DATA_BLK
            ino[40..44].copy_from_slice(&(FILE_DATA_BLK as u32).to_le_bytes());
        }

        // ── Root directory data block ────────────────────────────────────
        // Two entries: "." → inode 2, "hello.txt" → inode 3.
        // With INCOMPAT_FILETYPE, byte 7 of each entry is file_type.
        {
            let dblk = &mut img[ROOT_DATA_BLK * BS..ROOT_DATA_BLK * BS + BS];

            // Entry 0: "."  inode=2  rec_len=12  name_len=1  file_type=2(dir)
            let e0_rec: u16 = 12;
            dblk[0..4].copy_from_slice(&(ROOT_INUM as u32).to_le_bytes());
            dblk[4..6].copy_from_slice(&e0_rec.to_le_bytes());
            dblk[6] = 1; // name_len
            dblk[7] = 2; // file_type = directory
            dblk[8] = b'.';

            // Entry 1: ".."  inode=2  rec_len=12  name_len=2  file_type=2
            let e1_off = 12;
            let e1_rec: u16 = 12;
            dblk[e1_off..e1_off + 4].copy_from_slice(&(ROOT_INUM as u32).to_le_bytes());
            dblk[e1_off + 4..e1_off + 6].copy_from_slice(&e1_rec.to_le_bytes());
            dblk[e1_off + 6] = 2; // name_len
            dblk[e1_off + 7] = 2; // file_type = directory
            dblk[e1_off + 8] = b'.';
            dblk[e1_off + 9] = b'.';

            // Entry 2: "hello.txt"  inode=3  rec_len=(1024-24)  name_len=9  file_type=1
            let e2_off = 24;
            let e2_rec: u16 = (BS - e2_off) as u16;
            dblk[e2_off..e2_off + 4].copy_from_slice(&(FILE_INUM as u32).to_le_bytes());
            dblk[e2_off + 4..e2_off + 6].copy_from_slice(&e2_rec.to_le_bytes());
            dblk[e2_off + 6] = 9; // name_len = len("hello.txt")
            dblk[e2_off + 7] = 1; // file_type = regular
            dblk[e2_off + 8..e2_off + 17].copy_from_slice(b"hello.txt");
        }

        // ── File data block ──────────────────────────────────────────────
        {
            let fblk = &mut img[FILE_DATA_BLK * BS..FILE_DATA_BLK * BS + 12];
            fblk.copy_from_slice(b"hello world\n");
        }

        img
    }

    // ── Test helpers ──────────────────────────────────────────────────────

    fn cursor_of(img: &[u8]) -> Cursor<Vec<u8>> {
        Cursor::new(img.to_vec())
    }

    // ── Detection tests ───────────────────────────────────────────────────

    #[test]
    fn detect_valid_ext2() {
        let img = make_ext2_image();
        let mut c = cursor_of(&img);
        assert!(detect(&mut c), "should detect valid ext2 image");
    }

    #[test]
    fn detect_restores_position() {
        let img = make_ext2_image();
        let mut c = cursor_of(&img);
        c.seek(SeekFrom::Start(42)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(
            c.stream_position().unwrap(),
            42,
            "detect() must restore stream position"
        );
    }

    #[test]
    fn detect_restores_position_on_failure() {
        // Too-short image — detect returns false, position should still be 0.
        let img = vec![0u8; 512];
        let mut c = Cursor::new(img);
        c.seek(SeekFrom::Start(7)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(c.stream_position().unwrap(), 7);
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let mut img = make_ext2_image();
        // Corrupt the magic bytes.
        img[1024 + 56] = 0xDE;
        img[1024 + 57] = 0xAD;
        let mut c = cursor_of(&img);
        assert!(!detect(&mut c), "corrupted magic should not detect as ext");
    }

    #[test]
    fn detect_rejects_too_short() {
        // Image too short to even contain the magic field.
        let img = vec![0u8; 512];
        let mut c = Cursor::new(img);
        assert!(!detect(&mut c));
    }

    #[test]
    fn detect_rejects_fat_image() {
        // A FAT12 boot sector starts with 0xEB 0x?? 0x90 and "FAT" at 54.
        let mut img = vec![0u8; 2048];
        img[0] = 0xEB;
        img[1] = 0x58;
        img[2] = 0x90;
        img[3..8].copy_from_slice(b"FAT12");
        let mut c = Cursor::new(img);
        assert!(!detect(&mut c), "FAT image should not be detected as ext");
    }

    // ── Parse tests ───────────────────────────────────────────────────────

    #[test]
    fn parse_ext2_tree_shape() {
        let img = make_ext2_image();
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("parse ext2 image");
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(
            root.children.len(),
            1,
            "root should have exactly 1 child (hello.txt)"
        );
        assert_eq!(root.children[0].name, "hello.txt");
        assert!(!root.children[0].is_directory);
        assert_eq!(root.children[0].size, 12);
    }

    #[test]
    fn parse_ext2_file_location() {
        let img = make_ext2_image();
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("parse");
        let file = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        // file_location should be Some(...) — classical contiguous direct blocks.
        assert!(
            file.file_location.is_some(),
            "hello.txt should have a file_location"
        );
        // Should point to block 7 (FILE_DATA_BLK) in the image.
        let expected = 7 * 1024u64;
        assert_eq!(
            file.file_location.unwrap(),
            expected,
            "file_location should point to block 7"
        );
    }

    #[test]
    fn parse_ext2_file_contents() {
        let img = make_ext2_image();
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("parse");
        let file = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        let loc = file.file_location.unwrap();
        let len = file.size as usize;
        c.seek(SeekFrom::Start(loc)).unwrap();
        let mut buf = vec![0u8; len];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(buf, b"hello world\n");
    }

    #[test]
    fn ext2_empty_root_ok() {
        // Build an image where the root directory has no children
        // (only "." and ".." entries).
        let mut img = make_ext2_image();
        // Overwrite the third directory entry (hello.txt) to be all-zeros
        // so inode = 0, which the scanner skips.
        let root_data_start = 6 * 1024 + 24; // offset of third entry
                                             // Just zero the inode field so the entry is skipped.
        img[root_data_start..root_data_start + 4].copy_from_slice(&0u32.to_le_bytes());
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("parse should succeed even with no children");
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
    }

    #[test]
    fn ext2_large_file_no_crash() {
        // Inode with i_block[12] (single-indirect) non-zero but pointing
        // to block 0 (which will read as all zeros = no blocks). Should not
        // panic; just returns file_location = None.
        let mut img = make_ext2_image();
        // Modify hello.txt inode to claim size > 12 blocks.
        let inode_table_start = 5 * 1024;
        let file_inode_off = inode_table_start + (3 - 1) * 128; // inode #3 = index 2
                                                                // Set size to 1 MiB (many blocks needed).
        let new_size: u32 = 1024 * 1024;
        img[file_inode_off + 4..file_inode_off + 8].copy_from_slice(&new_size.to_le_bytes());
        // Set i_block[12] (single indirect) to block 8 (beyond our data).
        // Block 8 will read as zeros → all zero block pointers → no actual data.
        img[file_inode_off + 40 + 12 * 4..file_inode_off + 40 + 13 * 4]
            .copy_from_slice(&8u32.to_le_bytes());
        let mut c = cursor_of(&img);
        // Should parse without panic; file_location will be None.
        let root = detect_and_parse(&mut c).expect("parse should not crash");
        let file = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        assert_eq!(
            file.file_location, None,
            "large file has no single-run location"
        );
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        let msg = format!("{}", Error::TooShort);
        assert!(
            msg.contains("short") || msg.contains("superblock"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_bad_superblock() {
        let msg = format!("{}", Error::BadSuperblock);
        assert!(
            msg.contains("superblock") || msg.contains("0xEF53"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_io() {
        let io = std::io::Error::other("disk fail");
        let msg = format!("{}", Error::Io(io));
        assert!(msg.contains("disk fail"), "unexpected: {msg}");
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
        assert!(Error::BadSuperblock.source().is_none());
    }

    // ── Superblock sanity checks ──────────────────────────────────────────────

    fn make_corrupt_image<F: Fn(&mut Vec<u8>)>(corrupt: F) -> Vec<u8> {
        let mut img = make_ext2_image();
        corrupt(&mut img);
        img
    }

    #[test]
    fn rejects_log_block_size_too_large() {
        let img = make_corrupt_image(|img| {
            // s_log_block_size at offset 1024+24: set to 7 (would be 128 KiB).
            img[1024 + 24..1024 + 28].copy_from_slice(&7u32.to_le_bytes());
        });
        let mut c = cursor_of(&img);
        assert!(detect_and_parse(&mut c).is_err());
    }

    #[test]
    fn rejects_blocks_per_group_zero() {
        let img = make_corrupt_image(|img| {
            img[1024 + 32..1024 + 36].copy_from_slice(&0u32.to_le_bytes());
        });
        let mut c = cursor_of(&img);
        assert!(detect_and_parse(&mut c).is_err());
    }

    #[test]
    fn rejects_blocks_count_zero() {
        let img = make_corrupt_image(|img| {
            img[1024 + 4..1024 + 8].copy_from_slice(&0u32.to_le_bytes());
        });
        let mut c = cursor_of(&img);
        assert!(detect_and_parse(&mut c).is_err());
    }

    #[test]
    fn rejects_inodes_per_group_zero() {
        let img = make_corrupt_image(|img| {
            img[1024 + 40..1024 + 44].copy_from_slice(&0u32.to_le_bytes());
        });
        let mut c = cursor_of(&img);
        assert!(detect_and_parse(&mut c).is_err());
    }

    // ── Rev_level 0 path ──────────────────────────────────────────────────────

    #[test]
    fn rev_level_zero_uses_defaults() {
        // Set rev_level = 0; parser should use fixed 128-byte inodes, no extents.
        let mut img = make_ext2_image();
        img[1024 + 76..1024 + 80].copy_from_slice(&0u32.to_le_bytes());
        // Also zero the feature_incompat field to be safe.
        img[1024 + 96..1024 + 100].copy_from_slice(&0u32.to_le_bytes());
        let mut c = cursor_of(&img);
        // Should still parse OK; rev_level 0 just sets defaults.
        let root = detect_and_parse(&mut c).expect("rev_level 0 should still parse");
        assert_eq!(root.name, "/");
    }

    // ── Inode size > block_size rejects ───────────────────────────────────────

    #[test]
    fn rejects_inode_size_larger_than_block_size() {
        // inode_size = 4096, block_size = 1024 → invalid.
        let img = make_corrupt_image(|img| {
            img[1024 + 88..1024 + 90].copy_from_slice(&4096u16.to_le_bytes());
        });
        let mut c = cursor_of(&img);
        assert!(detect_and_parse(&mut c).is_err());
    }

    // ── Extent tree path ──────────────────────────────────────────────────────

    fn make_ext4_extent_image() -> Vec<u8> {
        // Like make_ext2_image but the file inode uses an extent tree.
        const BS: usize = 1024;
        const INODE_SIZE: usize = 128;
        const INODE_TABLE_BLK: usize = 5;
        const FILE_DATA_BLK: usize = 7;
        const FILE_INUM: usize = 3;

        let mut img = make_ext2_image(); // start from the classical image

        // Enable EXT4_EXTENTS_FL on the file inode (#3).
        let inode_base = INODE_TABLE_BLK * BS;
        let file_off = inode_base + (FILE_INUM - 1) * INODE_SIZE;
        // Set i_flags = EXT4_EXTENTS_FL (0x80000)
        img[file_off + 32..file_off + 36].copy_from_slice(&0x0008_0000u32.to_le_bytes());

        // Build an extent tree in i_block (60 bytes at file_off+40).
        // Header: magic=0xF30A, entries=1, max=4, depth=0, generation=0
        let extent_area = &mut img[file_off + 40..file_off + 100];
        extent_area[..2].copy_from_slice(&0xF30Au16.to_le_bytes()); // magic
        extent_area[2..4].copy_from_slice(&1u16.to_le_bytes()); // entries
        extent_area[4..6].copy_from_slice(&4u16.to_le_bytes()); // max
        extent_area[6..8].copy_from_slice(&0u16.to_le_bytes()); // depth = 0 (leaf)
        extent_area[8..12].copy_from_slice(&0u32.to_le_bytes()); // generation
                                                                 // Leaf extent at offset 12: ee_block=0, ee_len=1, ee_start_hi=0, ee_start_lo=FILE_DATA_BLK
        extent_area[12..16].copy_from_slice(&0u32.to_le_bytes()); // ee_block (logical)
        extent_area[16..18].copy_from_slice(&1u16.to_le_bytes()); // ee_len = 1 block
        extent_area[18..20].copy_from_slice(&0u16.to_le_bytes()); // ee_start_hi
        extent_area[20..24].copy_from_slice(&(FILE_DATA_BLK as u32).to_le_bytes()); // ee_start_lo

        img
    }

    #[test]
    fn parse_ext4_extent_tree_file() {
        let img = make_ext4_extent_image();
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("extent tree parse failed");
        let file = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        assert_eq!(file.size, 12);
        // Single extent → file_location should be set.
        assert!(
            file.file_location.is_some(),
            "single-extent file should have file_location"
        );
    }

    // ── parse_extent_header with bad magic ───────────────────────────────────

    #[test]
    fn parse_extent_header_bad_magic_returns_none() {
        let mut data = vec![0u8; 60];
        data[0..2].copy_from_slice(&0xDEADu16.to_le_bytes()); // wrong magic
        assert!(parse_extent_header(&data).is_none());
    }

    #[test]
    fn parse_extent_header_too_short_returns_none() {
        let data = vec![0u8; 4]; // < 12 bytes required
        assert!(parse_extent_header(&data).is_none());
    }

    // ── Inline data (EXT4_INLINE_DATA_FL) ────────────────────────────────────

    #[test]
    fn inline_data_file_has_no_location() {
        let mut img = make_ext2_image();
        const INODE_TABLE_BLK: usize = 5;
        const BS: usize = 1024;
        const INODE_SIZE: usize = 128;
        const FILE_INUM: usize = 3;
        let file_off = INODE_TABLE_BLK * BS + (FILE_INUM - 1) * INODE_SIZE;
        // Set EXT4_INLINE_DATA_FL (bit 28) in i_flags.
        let flags: u32 = 0x1000_0000;
        img[file_off + 32..file_off + 36].copy_from_slice(&flags.to_le_bytes());
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("inline data parse failed");
        let file = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        assert!(
            file.file_location.is_none(),
            "inline-data file should have no file_location"
        );
    }

    // ── Symlink inode in tree ────────────────────────────────────────────────

    fn make_ext2_with_symlink() -> Vec<u8> {
        // Extend the base image: add a symlink inode (#4) to the root dir.
        const BS: usize = 1024;
        const INODE_TABLE_BLK: usize = 5;
        const INODE_SIZE: usize = 128;
        const ROOT_DATA_BLK: usize = 6;
        const SYMLINK_INUM: usize = 4;

        let mut img = make_ext2_image();

        // Set up symlink inode #4 at index 3 in the inode table.
        let symlink_off = INODE_TABLE_BLK * BS + (SYMLINK_INUM - 1) * INODE_SIZE;
        let mode: u16 = S_IFLNK | 0o777;
        img[symlink_off..symlink_off + 2].copy_from_slice(&mode.to_le_bytes());
        img[symlink_off + 4..symlink_off + 8].copy_from_slice(&7u32.to_le_bytes()); // size=7 "foo/bar"

        // Add a new directory entry for "link" → inode 4.
        // We'll insert it after the existing entries in the root data block.
        // Existing entries end at offset 24+9=33, rounded up to 36.
        // We need to shrink the last entry's rec_len to make room.
        let dir_base = ROOT_DATA_BLK * BS;
        // The last entry (hello.txt at offset 24) currently spans to end of block.
        // Resize it to fit exactly: name_len=9 → rec_len=20 (8 header + 9 name + 3 pad).
        let new_rec_len: u16 = 20;
        img[dir_base + 24 + 4..dir_base + 24 + 6].copy_from_slice(&new_rec_len.to_le_bytes());

        // New entry at offset 44: "link" → inode 4.
        let e_off = 44;
        let e_rec: u16 = (BS - e_off) as u16;
        img[dir_base + e_off..dir_base + e_off + 4]
            .copy_from_slice(&(SYMLINK_INUM as u32).to_le_bytes());
        img[dir_base + e_off + 4..dir_base + e_off + 6].copy_from_slice(&e_rec.to_le_bytes());
        img[dir_base + e_off + 6] = 4; // name_len = "link"
        img[dir_base + e_off + 7] = 7; // file_type = symlink
        img[dir_base + e_off + 8..dir_base + e_off + 12].copy_from_slice(b"link");

        img
    }

    #[test]
    fn symlink_appears_in_tree() {
        let img = make_ext2_with_symlink();
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("symlink image parse failed");
        let link = root.find_node("/link");
        assert!(link.is_some(), "symlink should appear in the tree");
        let link = link.unwrap();
        assert!(!link.is_directory);
        // Symlinks don't get file_location.
        assert!(link.file_location.is_none());
    }

    // ── Non-contiguous blocks → no file_location ─────────────────────────────

    #[test]
    fn discontiguous_blocks_no_file_location() {
        let mut img = make_ext2_image();
        const INODE_TABLE_BLK: usize = 5;
        const BS: usize = 1024;
        const INODE_SIZE: usize = 128;
        const FILE_INUM: usize = 3;
        let file_off = INODE_TABLE_BLK * BS + (FILE_INUM - 1) * INODE_SIZE;
        // Make file large enough to need 2 blocks, but make them non-contiguous.
        img[file_off + 4..file_off + 8].copy_from_slice(&(2048u32).to_le_bytes()); // 2 KB
                                                                                   // i_block[0] = 7 (existing), i_block[1] = 9 (skipping 8 → discontiguous).
        img[file_off + 44..file_off + 48].copy_from_slice(&9u32.to_le_bytes()); // i_block[1] = 9
        let mut c = cursor_of(&img);
        let root = detect_and_parse(&mut c).expect("parse failed");
        let file = root
            .children
            .iter()
            .find(|n| n.name == "hello.txt")
            .unwrap();
        assert!(
            file.file_location.is_none(),
            "discontiguous blocks should yield no file_location"
        );
    }
}
