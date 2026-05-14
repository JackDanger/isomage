//! HFS+ (Mac OS Extended) filesystem reader (`hfsplus` feature).
//!
//! HFS+ is Apple's journaled filesystem introduced in Mac OS 8.1 and still
//! the default on macOS through 10.12. It stores a catalog B-tree (a sorted,
//! multi-level balanced tree) containing all file and directory metadata.
//!
//! All multi-byte fields are **big-endian** (§ references below are to the
//! Apple TN1150 "HFS Plus Volume Format" technical note, the de-facto spec).
//!
//! ## Scope of this implementation
//!
//! - Detects bare HFS+ and HFSX (case-sensitive HFS+) volumes by signature.
//! - Parses the volume header at byte offset 1024 (§4).
//! - Walks the catalog B-tree *leaf-node chain* (§4.3): starts at the first
//!   leaf node and follows `f_link` until the chain ends, collecting folder
//!   and file records. This avoids full B-tree traversal while still visiting
//!   every leaf record in key order.
//! - Decodes UTF-16 BE filenames (§2.1, HFSPlusUniStr255).
//! - Builds a [`TreeNode`] tree rooted at the HFS+ root folder (CNID 2).
//! - Does **not** read resource forks, extended attributes, or file data. The
//!   `file_location` field is populated for files whose data fork fits in a
//!   single contiguous extent so `cat_node` can serve it; multi-extent files
//!   get `file_location = None`.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Magic numbers (§4.2 Volume Header signature field) ─────────────────────

/// HFS+ signature: bytes 0x48 0x2B = 'H' '+' at the start of the volume header.
const HFS_PLUS_MAGIC: u16 = 0x482B;
/// HFSX (case-sensitive HFS+) signature: bytes 0x48 0x58 = 'H' 'X'.
const HFSX_MAGIC: u16 = 0x4858;

/// Volume header is always 512 bytes into the HFS+ volume, which itself
/// starts at byte offset 1024 from the beginning of the image (the first
/// 1024 bytes are the "boot blocks").
const VOLUME_HEADER_OFFSET: u64 = 1024;
const VOLUME_HEADER_SIZE: usize = 512;

// ── HFS+ catalog node ID (CNID) constants ──────────────────────────────────

/// CNID of the root folder. Children of the volume root have parent_cnid = 2.
const HFS_ROOT_FOLDER_CNID: u32 = 2;

// ── B-tree node kinds ──────────────────────────────────────────────────────

const BTREE_LEAF_NODE: u8 = 0xFF;
const BTREE_HEADER_NODE: u8 = 0x01;

// ── Catalog record types ───────────────────────────────────────────────────

const RECORD_TYPE_FOLDER: u16 = 0x0001;
const RECORD_TYPE_FILE: u16 = 0x0002;
const RECORD_TYPE_FOLDER_THREAD: u16 = 0x0003;
const RECORD_TYPE_FILE_THREAD: u16 = 0x0004;

// ── Error type ─────────────────────────────────────────────────────────────

/// Errors that can arise while detecting or parsing an HFS+ volume.
#[derive(Debug)]
pub enum Error {
    /// The image is too short to contain a valid HFS+ volume header.
    TooShort,
    /// The 2-byte signature at offset 1024 was not 0x482B or 0x4858.
    BadMagic,
    /// The volume header version field was not 4 (HFS+) or 5 (HFSX).
    BadVersion,
    /// The catalog B-tree structure is invalid (e.g. node 0 is not a
    /// header node, or the node size is below the minimum of 512 bytes).
    BadCatalog,
    /// The B-tree or catalog structure is too deeply nested for our
    /// recursion guard.
    TooDeep,
    /// An underlying I/O error occurred.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image too short for HFS+ volume header"),
            Error::BadMagic => write!(
                f,
                "HFS+ magic 0x482B or HFSX magic 0x4858 not found at offset 1024"
            ),
            Error::BadVersion => write!(f, "HFS+ version field is not 4 (HFS+) or 5 (HFSX)"),
            Error::BadCatalog => write!(f, "HFS+ catalog B-tree structure is invalid"),
            Error::TooDeep => write!(f, "HFS+ B-tree too deep to traverse"),
            Error::Io(e) => write!(f, "HFS+ I/O error: {e}"),
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

// ── On-disk structs (all fields big-endian) ────────────────────────────────

/// HFSPlusForkData (80 bytes, §2.4).
///
/// Describes one fork (data or resource) of a file, or one of the special
/// B-tree files embedded in the volume.
#[derive(Debug, Clone, Copy)]
pub struct ForkData {
    /// Logical byte length of the fork.
    pub logical_size: u64,
    /// Allocation block count.
    pub total_blocks: u32,
    /// Up to 8 extent descriptors: (start_block, block_count).
    pub extents: [(u32, u32); 8],
}

impl ForkData {
    /// Parse 80 bytes of ForkData from a buffer slice.
    fn from_bytes(b: &[u8]) -> Self {
        let logical_size = u64::from_be_bytes(b[0..8].try_into().unwrap());
        // clump_size at b[8..12] — not needed
        let total_blocks = u32::from_be_bytes(b[12..16].try_into().unwrap());
        let mut extents = [(0u32, 0u32); 8];
        for (i, ext) in extents.iter_mut().enumerate() {
            let off = 16 + i * 8;
            ext.0 = u32::from_be_bytes(b[off..off + 4].try_into().unwrap());
            ext.1 = u32::from_be_bytes(b[off + 4..off + 8].try_into().unwrap());
        }
        Self {
            logical_size,
            total_blocks,
            extents,
        }
    }

    /// Returns the byte offset of the first extent on-disk, given the
    /// allocation block size. Returns `None` if the fork is empty.
    fn first_extent_offset(&self, block_size: u64) -> Option<u64> {
        if self.extents[0].1 == 0 {
            return None;
        }
        Some(self.extents[0].0 as u64 * block_size)
    }

    /// Returns `true` if the entire fork fits in a single contiguous extent
    /// (i.e. only extents[0] is non-zero and covers all total_blocks).
    fn is_single_extent(&self) -> bool {
        if self.total_blocks == 0 {
            return false;
        }
        // extents[0] must cover all blocks, everything else must be zero.
        self.extents[0].1 == self.total_blocks && self.extents[1..].iter().all(|&(_, c)| c == 0)
    }
}

/// Parsed volume header fields we actually need (§4.2).
#[derive(Debug)]
pub struct VolumeHeader {
    /// HFS+ or HFSX.
    pub signature: u16,
    /// 4 = HFS+, 5 = HFSX.
    pub version: u16,
    /// Number of regular files on the volume (excluding directories).
    pub file_count: u32,
    /// Number of directories on the volume.
    pub folder_count: u32,
    /// Size of one allocation block in bytes.
    pub block_size: u32,
    /// The catalog B-tree's fork data — tells us where the catalog lives.
    pub cat_file: ForkData,
}

impl VolumeHeader {
    /// Parse a 512-byte volume header.
    fn from_bytes(b: &[u8]) -> Result<Self, Error> {
        if b.len() < VOLUME_HEADER_SIZE {
            return Err(Error::TooShort);
        }
        let signature = u16::from_be_bytes([b[0], b[1]]);
        if signature != HFS_PLUS_MAGIC && signature != HFSX_MAGIC {
            return Err(Error::BadMagic);
        }
        let version = u16::from_be_bytes([b[2], b[3]]);
        if version != 4 && version != 5 {
            return Err(Error::BadVersion);
        }
        let file_count = u32::from_be_bytes(b[32..36].try_into().unwrap());
        let folder_count = u32::from_be_bytes(b[36..40].try_into().unwrap());
        let block_size = u32::from_be_bytes(b[40..44].try_into().unwrap());
        // Catalog file fork: offset 272 in the header (§4.2).
        let cat_file = ForkData::from_bytes(&b[272..352]);
        Ok(Self {
            signature,
            version,
            file_count,
            folder_count,
            block_size,
            cat_file,
        })
    }
}

// ── B-tree header record (§4.3.1) ─────────────────────────────────────────

/// Subset of the B-tree header record we need to navigate the leaf chain.
struct BTreeHeader {
    node_size: u16,
    first_leaf_node: u32,
}

impl BTreeHeader {
    /// Parse from the 106-byte header record (first record in the header node).
    fn from_bytes(b: &[u8]) -> Self {
        // tree_depth at [0..2] — not needed
        // root_node  at [2..6] — not needed for leaf-chain walk
        // leaf_records at [6..10] — not needed
        let first_leaf_node = u32::from_be_bytes(b[10..14].try_into().unwrap());
        // last_leaf_node at [14..18] — not needed
        let node_size = u16::from_be_bytes(b[18..20].try_into().unwrap());
        Self {
            node_size,
            first_leaf_node,
        }
    }
}

// ── Catalog record types ───────────────────────────────────────────────────

/// One catalog leaf record, as we need it for tree construction.
#[derive(Debug)]
enum CatalogRecord {
    Folder {
        parent_cnid: u32,
        name: String,
        cnid: u32,
    },
    File {
        parent_cnid: u32,
        name: String,
        #[allow(dead_code)]
        cnid: u32,
        /// Logical length of the data fork in bytes.
        file_length: u64,
        /// Byte offset of the data fork, if it fits in one extent.
        file_location: Option<u64>,
    },
    /// Thread record — gives us name+parent for a CNID we already know.
    Thread {
        #[allow(dead_code)]
        cnid_key: u32, // the CNID this thread is for (from the key parent field)
        #[allow(dead_code)]
        record_type: u16,
    },
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Detect whether the reader contains an HFS+ or HFSX volume.
///
/// Seeks to offset 1024, reads the 2-byte signature, then **restores the
/// cursor** to its position before the call. Returns `Ok(())` on a match.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let saved = r.stream_position()?;
    let result = do_detect(r);
    let _ = r.seek(SeekFrom::Start(saved));
    result
}

fn do_detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    r.seek(SeekFrom::Start(VOLUME_HEADER_OFFSET))?;
    let mut sig = [0u8; 2];
    r.read_exact(&mut sig).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;
    let sig_val = u16::from_be_bytes([sig[0], sig[1]]);
    if sig_val != HFS_PLUS_MAGIC && sig_val != HFSX_MAGIC {
        return Err(Error::BadMagic);
    }
    Ok(())
}

/// Read the volume header only (does not walk the catalog B-tree).
///
/// Exposed for unit tests and callers that only need top-level volume metadata.
pub fn parse_volume_header<R: Read + Seek>(r: &mut R) -> Result<VolumeHeader, Error> {
    r.seek(SeekFrom::Start(VOLUME_HEADER_OFFSET))?;
    let mut buf = [0u8; VOLUME_HEADER_SIZE];
    r.read_exact(&mut buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;
    VolumeHeader::from_bytes(&buf)
}

/// Detect an HFS+ or HFSX volume, then parse its catalog B-tree into a
/// [`TreeNode`] tree.
///
/// The tree root is `"/"` (a directory), with one child per entry directly
/// under the volume root (CNID 2). Subdirectories are populated recursively.
/// Files get `file_location = Some(offset)` only when their data fork fits in
/// a single contiguous allocation-block extent; multi-extent files fall back
/// to `None`.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let header = parse_volume_header(r)?;
    let records = read_catalog_leaf_records(r, &header)?;
    Ok(build_tree(&records, header.block_size as u64))
}

// ── Catalog B-tree leaf-chain walker ──────────────────────────────────────

/// Read all leaf records from the catalog B-tree by following the forward
/// link chain rather than traversing the full tree top-down.
///
/// Strategy (§4.3.4):
/// 1. The catalog file extents in the volume header tell us the on-disk
///    position of the catalog B-tree data.
/// 2. Node 0 is always the B-tree header node; we read the B-tree header
///    record from it to get `node_size` and `first_leaf_node`.
/// 3. Leaf nodes form a doubly-linked list via their `f_link` field; we
///    follow `f_link` until it is 0.
/// 4. For each leaf node we parse every record: catalog key (parent_cnid,
///    name) + catalog data (record_type, CNID, fork data for files).
fn read_catalog_leaf_records<R: Read + Seek>(
    r: &mut R,
    header: &VolumeHeader,
) -> Result<Vec<CatalogRecord>, Error> {
    let block_size = header.block_size as u64;
    let cat = &header.cat_file;

    // If the catalog fork is empty (logical_size == 0 or no extents), there
    // are no records to read — return an empty list rather than an error.
    let cat_offset = match cat.first_extent_offset(block_size) {
        Some(off) => off,
        None => return Ok(Vec::new()),
    };

    // ── Step 1: Read the B-tree header node (node 0) ──
    // We read exactly 120 bytes (14-byte node descriptor + 106-byte BTHeaderRec).
    // Per the HFS+ spec (TN1150 §2.2), the B-tree header record ALWAYS begins
    // at byte 14 of the header node, immediately after the node descriptor.
    // We do not use the offset table to locate record 0 here because the offset
    // table lives at the END of the node — and we don't know node_size yet.
    r.seek(SeekFrom::Start(cat_offset))?;
    let mut header_node_buf = vec![0u8; 120]; // 14 (descriptor) + 106 (BTHeaderRec)
    r.read_exact(&mut header_node_buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;

    // B-tree node descriptor is 14 bytes at the start of every node.
    let node_kind = header_node_buf[8];
    if node_kind != BTREE_HEADER_NODE {
        // Node 0 must be the header node.
        return Err(Error::BadCatalog);
    }
    let num_records_in_header = u16::from_be_bytes([header_node_buf[10], header_node_buf[11]]);
    if num_records_in_header < 1 {
        return Err(Error::TooShort);
    }

    // The BTHeaderRec starts at byte 14 (immediately after the node descriptor).
    let btree_header = BTreeHeader::from_bytes(&header_node_buf[14..]);

    let node_size = btree_header.node_size as u64;
    if node_size < 512 {
        return Err(Error::BadCatalog);
    }

    let mut first_leaf = btree_header.first_leaf_node;
    if first_leaf == 0 {
        // Empty catalog — no leaf nodes.
        return Ok(Vec::new());
    }

    // ── Step 2: Walk the leaf-node chain ──
    let mut records: Vec<CatalogRecord> = Vec::new();
    // Guard against corrupted or circular f_link chains.
    let max_nodes: u32 = (cat.logical_size / node_size).min(u32::MAX as u64) as u32 + 1;
    let mut visited = 0u32;

    loop {
        if visited > max_nodes {
            return Err(Error::TooDeep);
        }
        visited += 1;

        // Seek to the start of this leaf node.
        let node_offset = cat_offset + first_leaf as u64 * node_size;
        r.seek(SeekFrom::Start(node_offset))?;
        let mut node_buf = vec![0u8; node_size as usize];
        r.read_exact(&mut node_buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::TooShort
            } else {
                Error::Io(e)
            }
        })?;

        // Node descriptor (14 bytes).
        let f_link = u32::from_be_bytes(node_buf[0..4].try_into().unwrap());
        let kind = node_buf[8];
        let num_records = u16::from_be_bytes([node_buf[10], node_buf[11]]);

        if kind != BTREE_LEAF_NODE {
            // Should never happen in a healthy volume; tolerate it by skipping.
            if f_link == 0 {
                break;
            }
            first_leaf = f_link;
            continue;
        }

        // ── Parse each record in this leaf node ──
        parse_leaf_node_records(&node_buf, num_records, block_size, &mut records)?;

        if f_link == 0 {
            break;
        }
        first_leaf = f_link;
    }

    Ok(records)
}

/// Parse all catalog records from one leaf node's byte buffer.
fn parse_leaf_node_records(
    node_buf: &[u8],
    num_records: u16,
    block_size: u64,
    out: &mut Vec<CatalogRecord>,
) -> Result<(), Error> {
    let node_size = node_buf.len();

    for i in 0..num_records as usize {
        // The offset table is at the END of the node, one u16 per entry,
        // going backwards (§4.3.3). offset[i] is at:
        //   node_size - 2*(i+1)
        let off_idx = node_size.saturating_sub(2 * (i + 1));
        if off_idx + 2 > node_size {
            break;
        }
        let rec_start = u16::from_be_bytes([node_buf[off_idx], node_buf[off_idx + 1]]) as usize;
        if rec_start >= node_size {
            continue;
        }
        let rec_data = &node_buf[rec_start..];

        // ── Catalog key (§4.3.5) ──
        // [0..2] u16 key_length
        // [2..6] u32 parent_cnid (BE)
        // [6..8] u16 name.length (BE)
        // [8..8+2*name.length] UTF-16 BE code units
        if rec_data.len() < 6 {
            continue;
        }
        let key_length = u16::from_be_bytes([rec_data[0], rec_data[1]]) as usize;
        if key_length < 6 || rec_data.len() < 2 + key_length {
            continue;
        }
        let parent_cnid = u32::from_be_bytes(rec_data[2..6].try_into().unwrap());
        let name_len = u16::from_be_bytes([rec_data[6], rec_data[7]]) as usize;
        let name_bytes_len = name_len * 2;
        if rec_data.len() < 8 + name_bytes_len {
            continue;
        }
        let name = decode_utf16_be(&rec_data[8..8 + name_bytes_len]);

        // The catalog data starts immediately after the key, rounded up to
        // the next 2-byte boundary from the record start (§4.3.5).
        let raw_data_off = 2 + key_length; // key_length does NOT include the 2-byte key_length field itself
        let data_off = (raw_data_off + 1) & !1; // align to 2 bytes
        if rec_data.len() < data_off + 2 {
            continue;
        }
        let data = &rec_data[data_off..];
        let record_type = u16::from_be_bytes([data[0], data[1]]);

        match record_type {
            RECORD_TYPE_FOLDER => {
                // Folder record (248 bytes total, §4.3.6).
                // [8..12] u32 cnid
                if data.len() < 12 {
                    continue;
                }
                let cnid = u32::from_be_bytes(data[8..12].try_into().unwrap());
                out.push(CatalogRecord::Folder {
                    parent_cnid,
                    name,
                    cnid,
                });
            }
            RECORD_TYPE_FILE => {
                // File record (248 bytes total, §4.3.7).
                // [8..12]    u32 cnid
                // [88..168]  HFSPlusForkData data_fork (80 bytes)
                if data.len() < 168 {
                    continue;
                }
                let cnid = u32::from_be_bytes(data[8..12].try_into().unwrap());
                let data_fork = ForkData::from_bytes(&data[88..168]);
                let file_length = data_fork.logical_size;
                let file_location = if data_fork.is_single_extent() {
                    data_fork.first_extent_offset(block_size)
                } else {
                    None
                };
                out.push(CatalogRecord::File {
                    parent_cnid,
                    name,
                    cnid,
                    file_length,
                    file_location,
                });
            }
            RECORD_TYPE_FOLDER_THREAD | RECORD_TYPE_FILE_THREAD => {
                // Thread records are keyed by CNID; we don't need them for
                // tree construction since we already get names from the
                // corresponding folder/file records.
                out.push(CatalogRecord::Thread {
                    cnid_key: parent_cnid, // in thread records, the key parent field holds the CNID
                    record_type,
                });
            }
            _ => {
                // Unknown record type — skip.
            }
        }
    }

    Ok(())
}

// ── Tree construction ──────────────────────────────────────────────────────

/// Navigate a tree by a slice of directory-name segments and return a
/// mutable reference to the matching node, or `None` if not found.
fn find_by_path_mut<'a>(node: &'a mut TreeNode, path: &[String]) -> Option<&'a mut TreeNode> {
    if path.is_empty() {
        return Some(node);
    }
    for child in &mut node.children {
        if child.is_directory && child.name == path[0] {
            return find_by_path_mut(child, &path[1..]);
        }
    }
    None
}

/// Build the path (list of directory names from root) for a CNID by
/// following the `folder_map` chain.
fn cnid_path(cnid: u32, folder_map: &std::collections::HashMap<u32, (String, u32)>) -> Vec<String> {
    if cnid == HFS_ROOT_FOLDER_CNID {
        return vec![];
    }
    let mut path = Vec::new();
    let mut cur = cnid;
    let mut guard = 0usize;
    loop {
        guard += 1;
        if guard > 1000 {
            break; // cycle guard
        }
        match folder_map.get(&cur) {
            Some((name, parent)) => {
                path.push(name.clone());
                if *parent == HFS_ROOT_FOLDER_CNID || *parent == 0 {
                    break;
                }
                cur = *parent;
            }
            None => break,
        }
    }
    path.reverse();
    path
}

/// Build a [`TreeNode`] tree from the flat list of catalog records.
///
/// HFS+ uses catalog node IDs (CNIDs) as stable directory identifiers:
/// every record carries a `parent_cnid` linking it to its parent folder.
/// CNID 2 is the root folder; its children appear in the `"/"` node.
///
/// Algorithm:
/// 1. Index all folder CNIDs → mutable TreeNode placeholders.
/// 2. Walk records in order, attaching each file/folder to its parent.
/// 3. Return the root node (CNID 2) with `calculate_directory_size` applied.
fn build_tree(records: &[CatalogRecord], _block_size: u64) -> TreeNode {
    // ── Pass 1: collect all folder records ──
    // Map from CNID to (name, parent_cnid) for every folder.
    let mut folder_map: HashMap<u32, (String, u32)> = HashMap::new();
    folder_map.insert(HFS_ROOT_FOLDER_CNID, ("/".to_string(), 0));

    for rec in records {
        if let CatalogRecord::Folder {
            cnid,
            name,
            parent_cnid,
        } = rec
        {
            if *cnid != HFS_ROOT_FOLDER_CNID {
                folder_map.insert(*cnid, (name.clone(), *parent_cnid));
            }
        }
    }

    // ── Pass 2: build a flat map of TreeNodes, keyed by CNID ──
    let mut nodes: HashMap<u32, TreeNode> = HashMap::new();
    nodes.insert(
        HFS_ROOT_FOLDER_CNID,
        TreeNode::new_directory("/".to_string()),
    );
    for (&cnid, (name, _)) in &folder_map {
        if cnid != HFS_ROOT_FOLDER_CNID {
            nodes.insert(cnid, TreeNode::new_directory(name.clone()));
        }
    }

    // ── Pass 3: collect file nodes and their parent CNIDs ──
    let mut file_items: Vec<(u32, TreeNode)> = Vec::new();
    for rec in records {
        if let CatalogRecord::File {
            parent_cnid,
            name,
            file_length,
            file_location,
            ..
        } = rec
        {
            let node = if let Some(loc) = file_location {
                // Adjust file_location: the extent start_block is relative to
                // the beginning of the *volume*, which (for a bare HFS+ image)
                // starts at byte 0. block_size is already in bytes.
                // We pass it through as-is; the caller's image must be seekable
                // to this offset.
                TreeNode::new_file_with_location(name.clone(), *file_length, *loc, *file_length)
            } else {
                TreeNode::new_file(name.clone(), *file_length)
            };
            file_items.push((*parent_cnid, node));
        }
    }

    // ── Pass 4: attach children in parent-CNID order ──
    // We do this in two phases to avoid borrow-checker conflicts:
    // first collect (parent_cnid, child_cnid) pairs for dirs, then attach.
    let dir_children: Vec<(u32, u32)> = folder_map
        .iter()
        .filter(|(&cnid, _)| cnid != HFS_ROOT_FOLDER_CNID)
        .map(|(&cnid, (_, parent_cnid))| (*parent_cnid, cnid))
        .collect();

    // Build the tree bottom-up: repeatedly find directories whose parent
    // exists in `nodes` and whose children are all already there.
    // Because HFS+ catalogs are in key order (parent_cnid, name), the
    // simplest approach is a topological attachment by repeated passes.
    // For real-world volumes the depth is ≤ a few hundred at most.
    // We bound iterations at folder_map.len() + 1 to avoid infinite loops
    // on corrupted volumes.
    let mut remaining: Vec<(u32, u32)> = dir_children; // (parent_cnid, child_cnid)
    let max_iters = folder_map.len() + 1;
    for _ in 0..max_iters {
        if remaining.is_empty() {
            break;
        }
        let mut still_pending: Vec<(u32, u32)> = Vec::new();
        for (parent_cnid, child_cnid) in remaining.drain(..) {
            if nodes.contains_key(&child_cnid) && nodes.contains_key(&parent_cnid) {
                let child = nodes.remove(&child_cnid).unwrap();
                nodes.entry(parent_cnid).and_modify(|p| p.add_child(child));
            } else {
                still_pending.push((parent_cnid, child_cnid));
            }
        }
        remaining = still_pending;
    }

    // Attach files to their parents.  After Pass 4 only the root node
    // remains in `nodes`; subdirectory nodes were removed and nested
    // inside it.  Resolve each parent CNID to its path in the tree and
    // navigate there to attach the file.
    let root_node = nodes.get_mut(&HFS_ROOT_FOLDER_CNID).unwrap();
    for (parent_cnid, file_node) in file_items {
        let path = cnid_path(parent_cnid, &folder_map);
        if let Some(parent) = find_by_path_mut(root_node, &path) {
            parent.add_child(file_node);
        }
        // If path not found (orphan / corrupted image), silently drop.
    }

    // Sort children alphabetically to produce a stable output order.
    sort_children_recursive(nodes.get_mut(&HFS_ROOT_FOLDER_CNID).unwrap());

    let mut root = nodes
        .remove(&HFS_ROOT_FOLDER_CNID)
        .unwrap_or_else(|| TreeNode::new_directory("/".to_string()));
    root.calculate_directory_size();
    root
}

/// Sort a [`TreeNode`]'s children (and their children) alphabetically by name.
fn sort_children_recursive(node: &mut TreeNode) {
    node.children.sort_by(|a, b| a.name.cmp(&b.name));
    for child in &mut node.children {
        if child.is_directory {
            sort_children_recursive(child);
        }
    }
}

// ── UTF-16 BE decoding ─────────────────────────────────────────────────────

/// Decode a big-endian UTF-16 byte slice into a `String`.
///
/// HFS+ stores all filenames as UTF-16 BE (§2.1). Invalid surrogate pairs
/// and lone surrogates are replaced with U+FFFD to match the behaviour of
/// the existing GPT UTF-16LE decoder (`gpt::decode_utf16le`).
fn decode_utf16_be(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let unit = u16::from_be_bytes([chunk[0], chunk[1]]);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    String::from_utf16_lossy(&units)
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a minimal in-memory HFS+ image: 1024 bytes of zeros (boot
    /// blocks) followed by a valid volume header, with any remaining space
    /// zeroed. Enough to satisfy `detect` and `parse_volume_header`.
    fn make_volume_header_bytes(
        signature: u16,
        version: u16,
        file_count: u32,
        folder_count: u32,
        block_size: u32,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 1024 + VOLUME_HEADER_SIZE];
        let h = &mut buf[1024..1024 + VOLUME_HEADER_SIZE];
        h[0..2].copy_from_slice(&signature.to_be_bytes());
        h[2..4].copy_from_slice(&version.to_be_bytes());
        h[32..36].copy_from_slice(&file_count.to_be_bytes());
        h[36..40].copy_from_slice(&folder_count.to_be_bytes());
        h[40..44].copy_from_slice(&block_size.to_be_bytes());
        // cat_file at offset 272 — all zeros is fine for unit tests that
        // don't walk the B-tree.
        buf
    }

    #[test]
    fn detect_hfsplus_signature() {
        let buf = make_volume_header_bytes(HFS_PLUS_MAGIC, 4, 0, 0, 4096);
        let mut c = Cursor::new(&buf);
        assert!(detect(&mut c).is_ok(), "should detect HFS+ magic 0x482B");
    }

    #[test]
    fn detect_hfsx_signature() {
        let buf = make_volume_header_bytes(HFSX_MAGIC, 5, 0, 0, 4096);
        let mut c = Cursor::new(&buf);
        assert!(detect(&mut c).is_ok(), "should detect HFSX magic 0x4858");
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let buf = make_volume_header_bytes(0xDEAD, 4, 0, 0, 4096);
        let mut c = Cursor::new(&buf);
        assert!(
            matches!(detect(&mut c), Err(Error::BadMagic)),
            "should reject non-HFS+ signature"
        );
    }

    #[test]
    fn detect_restores_cursor() {
        let buf = make_volume_header_bytes(HFS_PLUS_MAGIC, 4, 0, 0, 4096);
        let mut c = Cursor::new(&buf);
        c.seek(SeekFrom::Start(42)).unwrap();
        let _ = detect(&mut c);
        assert_eq!(c.position(), 42, "detect must restore the cursor position");
    }

    #[test]
    fn parse_volume_header_fields() {
        let buf = make_volume_header_bytes(HFS_PLUS_MAGIC, 4, 100, 20, 4096);
        let mut c = Cursor::new(&buf);
        let vh = parse_volume_header(&mut c).expect("parse volume header");
        assert_eq!(vh.signature, HFS_PLUS_MAGIC);
        assert_eq!(vh.version, 4);
        assert_eq!(vh.file_count, 100);
        assert_eq!(vh.folder_count, 20);
        assert_eq!(vh.block_size, 4096);
    }

    #[test]
    fn parse_volume_header_rejects_bad_version() {
        let buf = make_volume_header_bytes(HFS_PLUS_MAGIC, 99, 0, 0, 4096);
        let mut c = Cursor::new(&buf);
        assert!(
            matches!(parse_volume_header(&mut c), Err(Error::BadVersion)),
            "version 99 should be rejected"
        );
    }

    #[test]
    fn decode_utf16_be_ascii() {
        // "Hello" in big-endian UTF-16.
        let bytes: Vec<u8> = "Hello"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(decode_utf16_be(&bytes), "Hello");
    }

    #[test]
    fn decode_utf16_be_stops_at_nul() {
        let mut bytes: Vec<u8> = "AB".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        bytes.extend_from_slice(&[0x00, 0x00]); // NUL terminator
        bytes.extend_from_slice(&[0x00, 0x43]); // 'C' after NUL — should be ignored
        assert_eq!(decode_utf16_be(&bytes), "AB");
    }

    #[test]
    fn fork_data_single_extent_detection() {
        let mut b = [0u8; 80];
        // logical_size = 1024, total_blocks = 2, extent[0] = (start=10, count=2)
        b[0..8].copy_from_slice(&1024u64.to_be_bytes());
        b[12..16].copy_from_slice(&2u32.to_be_bytes());
        b[16..20].copy_from_slice(&10u32.to_be_bytes()); // start_block
        b[20..24].copy_from_slice(&2u32.to_be_bytes()); // block_count
        let fork = ForkData::from_bytes(&b);
        assert!(fork.is_single_extent(), "should be single-extent fork");
        assert_eq!(
            fork.first_extent_offset(512),
            Some(10 * 512),
            "offset = start_block * block_size"
        );
    }

    #[test]
    fn fork_data_multi_extent_not_single() {
        let mut b = [0u8; 80];
        // total_blocks = 4, two extents of 2 each
        b[12..16].copy_from_slice(&4u32.to_be_bytes());
        b[16..20].copy_from_slice(&10u32.to_be_bytes());
        b[20..24].copy_from_slice(&2u32.to_be_bytes());
        b[24..28].copy_from_slice(&20u32.to_be_bytes());
        b[28..32].copy_from_slice(&2u32.to_be_bytes());
        let fork = ForkData::from_bytes(&b);
        assert!(
            !fork.is_single_extent(),
            "two-extent fork should not be single-extent"
        );
    }

    #[test]
    fn detect_and_parse_empty_catalog() {
        // Build an image where the catalog file's logical_size is 0 (no records).
        // detect_and_parse should succeed and return an empty root.
        let buf = make_volume_header_bytes(HFS_PLUS_MAGIC, 4, 0, 0, 4096);
        let mut c = Cursor::new(&buf);
        let tree = detect_and_parse(&mut c).expect("parse empty HFS+ volume");
        assert_eq!(tree.name, "/");
        assert!(tree.is_directory);
        assert!(tree.children.is_empty(), "empty catalog → no children");
    }

    // ── build_tree ────────────────────────────────────────────────────────────

    #[test]
    fn build_tree_single_file_in_root() {
        let records = vec![CatalogRecord::File {
            parent_cnid: HFS_ROOT_FOLDER_CNID, // 2
            name: "hello.txt".to_string(),
            cnid: 10,
            file_length: 42,
            file_location: Some(4096),
        }];
        let root = build_tree(&records, 4096);
        assert_eq!(root.name, "/");
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "hello.txt");
        assert_eq!(root.children[0].size, 42);
    }

    #[test]
    fn build_tree_nested_directory() {
        let records = vec![
            CatalogRecord::Folder {
                parent_cnid: HFS_ROOT_FOLDER_CNID,
                name: "docs".to_string(),
                cnid: 20,
            },
            CatalogRecord::File {
                parent_cnid: 20, // child of "docs"
                name: "readme.txt".to_string(),
                cnid: 21,
                file_length: 100,
                file_location: None,
            },
        ];
        let root = build_tree(&records, 4096);
        assert_eq!(root.children.len(), 1);
        let docs = &root.children[0];
        assert_eq!(docs.name, "docs");
        assert!(docs.is_directory);
        assert_eq!(docs.children.len(), 1);
        assert_eq!(docs.children[0].name, "readme.txt");
    }

    #[test]
    fn build_tree_thread_record_ignored() {
        let records = vec![
            CatalogRecord::Thread {
                cnid_key: 5,
                record_type: 3,
            },
            CatalogRecord::File {
                parent_cnid: HFS_ROOT_FOLDER_CNID,
                name: "f.bin".to_string(),
                cnid: 5,
                file_length: 0,
                file_location: None,
            },
        ];
        let root = build_tree(&records, 4096);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "f.bin");
    }

    #[test]
    fn build_tree_file_without_location() {
        let records = vec![CatalogRecord::File {
            parent_cnid: HFS_ROOT_FOLDER_CNID,
            name: "sparse.dat".to_string(),
            cnid: 30,
            file_length: 200,
            file_location: None,
        }];
        let root = build_tree(&records, 512);
        let node = &root.children[0];
        assert_eq!(node.name, "sparse.dat");
        assert!(node.file_location.is_none());
    }

    // ── parse_leaf_node_records ───────────────────────────────────────────────

    /// Build a minimal leaf node buffer with one catalog record at a given
    /// offset, plus the offset table at the end.
    fn make_leaf_node(key_data: &[u8], record_data: &[u8]) -> Vec<u8> {
        // Node size = 512 bytes (one sector).
        let node_size = 512usize;
        let mut node = vec![0u8; node_size];

        // Record starts at offset 14 (after the 14-byte node descriptor).
        let rec_start: u16 = 14;
        let rec_end = rec_start as usize + key_data.len() + record_data.len();

        node[rec_start as usize..rec_start as usize + key_data.len()].copy_from_slice(key_data);
        node[rec_start as usize + key_data.len()..rec_end].copy_from_slice(record_data);

        // Offset table: [node_size - 2] = rec_start (one record → one entry)
        let ot_idx = node_size - 2; // offset for record 0
        node[ot_idx..ot_idx + 2].copy_from_slice(&rec_start.to_be_bytes());

        node
    }

    /// Build a catalog key with the given parent_cnid and name (UTF-16 BE).
    fn make_catalog_key(parent_cnid: u32, name: &str) -> Vec<u8> {
        let name_utf16: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let name_len = name.encode_utf16().count() as u16;
        let key_length = (6 + name_utf16.len()) as u16; // parent(4) + name.length(2) + name
        let mut key = Vec::new();
        key.extend_from_slice(&key_length.to_be_bytes()); // key_length (2 bytes)
        key.extend_from_slice(&parent_cnid.to_be_bytes()); // parent_cnid (4 bytes)
        key.extend_from_slice(&name_len.to_be_bytes()); // name.length (2 bytes)
        key.extend_from_slice(&name_utf16); // name bytes
                                            // Pad key to even length if needed (data_off = (2 + key_length + 1) & !1)
        if key.len() % 2 != 0 {
            key.push(0);
        }
        key
    }

    #[test]
    fn parse_leaf_node_folder_record() {
        let key = make_catalog_key(HFS_ROOT_FOLDER_CNID, "subdir");
        // Folder record: type=0x0001, valence(ignored), cnid at offset 8
        let cnid: u32 = 100;
        let mut rec_data = vec![0u8; 248];
        rec_data[0..2].copy_from_slice(&RECORD_TYPE_FOLDER.to_be_bytes());
        rec_data[8..12].copy_from_slice(&cnid.to_be_bytes());

        let node = make_leaf_node(&key, &rec_data);
        let mut out = Vec::new();
        parse_leaf_node_records(&node, 1, 4096, &mut out).unwrap();

        assert_eq!(out.len(), 1);
        if let CatalogRecord::Folder {
            parent_cnid,
            name,
            cnid: c,
        } = &out[0]
        {
            assert_eq!(*parent_cnid, HFS_ROOT_FOLDER_CNID);
            assert_eq!(name, "subdir");
            assert_eq!(*c, 100);
        } else {
            panic!("expected Folder record");
        }
    }

    #[test]
    fn parse_leaf_node_file_record() {
        let key = make_catalog_key(HFS_ROOT_FOLDER_CNID, "file.txt");
        let mut rec_data = vec![0u8; 248];
        rec_data[0..2].copy_from_slice(&RECORD_TYPE_FILE.to_be_bytes());
        // cnid at offset 8
        rec_data[8..12].copy_from_slice(&55u32.to_be_bytes());
        // data_fork at offset 88: logical_size=1024, total_blocks=1, extent[0]=(10,1)
        let fork_off = 88;
        rec_data[fork_off..fork_off + 8].copy_from_slice(&1024u64.to_be_bytes()); // logical_size
        rec_data[fork_off + 12..fork_off + 16].copy_from_slice(&1u32.to_be_bytes()); // total_blocks
        rec_data[fork_off + 16..fork_off + 20].copy_from_slice(&10u32.to_be_bytes()); // start_block
        rec_data[fork_off + 20..fork_off + 24].copy_from_slice(&1u32.to_be_bytes()); // block_count

        let node = make_leaf_node(&key, &rec_data);
        let mut out = Vec::new();
        parse_leaf_node_records(&node, 1, 4096, &mut out).unwrap();

        assert_eq!(out.len(), 1);
        if let CatalogRecord::File {
            name,
            file_length,
            file_location,
            ..
        } = &out[0]
        {
            assert_eq!(name, "file.txt");
            assert_eq!(*file_length, 1024);
            // single extent → location = start_block * block_size = 10 * 4096
            assert_eq!(*file_location, Some(10 * 4096));
        } else {
            panic!("expected File record");
        }
    }

    #[test]
    fn parse_leaf_node_thread_record() {
        let key = make_catalog_key(HFS_ROOT_FOLDER_CNID, "");
        let mut rec_data = vec![0u8; 10];
        rec_data[0..2].copy_from_slice(&RECORD_TYPE_FOLDER_THREAD.to_be_bytes());

        let node = make_leaf_node(&key, &rec_data);
        let mut out = Vec::new();
        parse_leaf_node_records(&node, 1, 4096, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], CatalogRecord::Thread { .. }));
    }
}
