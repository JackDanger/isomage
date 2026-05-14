//! FAT12/FAT16/FAT32 filesystem reader (`fat` feature).
//!
//! Parses the BPB (BIOS Parameter Block), walks the FAT cluster chains, and
//! builds a [`TreeNode`] tree from the directory hierarchy. VFAT long-filename
//! (LFN) entries are reassembled from their component chunks and used in
//! preference to the 8.3 short name.
//!
//! The `file_location` / `file_length` fields on returned [`TreeNode`]s point
//! at the file's bytes in the *original reader* relative to whatever byte
//! offset `detect_and_parse` was called at. For files whose clusters are
//! physically contiguous (the typical case for freshly-written images), this
//! means `cat_node` works directly. For fragmented files, `file_location` is
//! `None` and `cat_node` returns an error.
//!
//! References — Microsoft FAT Specification, 2004 ("fatgen103.doc"):
//!   § 2   BPB layout
//!   § 3   FAT12/16 extended BPB
//!   § 4   FAT32 extended BPB
//!   § 5   FAT Data Structure
//!   § 6   Directory Data Structure (8.3 + VFAT LFN)

use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Error {
    /// Fewer than 512 bytes are available at the scan position.
    TooShort,
    /// Boot-sector signature absent or BPB fields are out of spec.
    BadBootSector,
    /// Underlying I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image too short for a FAT boot sector"),
            Error::BadBootSector => write!(f, "invalid FAT BPB / boot sector"),
            Error::Io(e) => write!(f, "FAT I/O: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Error::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

// ---------------------------------------------------------------------------
// FAT type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

// End-of-chain test per § 5.
fn is_eoc(fat_type: FatType, cluster: u32) -> bool {
    match fat_type {
        FatType::Fat12 => cluster >= 0x0FF8,
        FatType::Fat16 => cluster >= 0xFFF8,
        FatType::Fat32 => (cluster & 0x0FFF_FFFF) >= 0x0FFF_FFF8,
    }
}

fn is_bad_cluster(fat_type: FatType, cluster: u32) -> bool {
    match fat_type {
        FatType::Fat12 => cluster == 0x0FF7,
        FatType::Fat16 => cluster == 0xFFF7,
        FatType::Fat32 => (cluster & 0x0FFF_FFFF) == 0x0FFF_FFF7,
    }
}

// ---------------------------------------------------------------------------
// Parsed BPB + derived geometry (§ 2–4)
// ---------------------------------------------------------------------------

struct Context {
    /// Absolute byte offset of the FAT filesystem's boot sector in the reader.
    base_offset: u64,
    fat_type: FatType,
    bytes_per_cluster: u64,
    /// Byte offset of the first FAT copy, relative to `base_offset`.
    fat_start_rel: u64,
    /// Byte offset of the FAT12/16 fixed root-directory region, relative to
    /// `base_offset`. Not used for FAT32 (root is in a cluster chain).
    root_dir_rel: u64,
    /// Byte length of the FAT12/16 root-directory region.
    root_dir_size_bytes: u64,
    /// Byte offset of cluster 2's first byte, relative to `base_offset`.
    data_start_rel: u64,
    /// First cluster of the FAT32 root directory (unused for FAT12/16).
    root_cluster: u32,
    /// Total data clusters, used to cap cluster-chain walks.
    total_clusters: u32,
}

impl Context {
    fn cluster_abs(&self, cluster: u32) -> u64 {
        self.base_offset + self.data_start_rel + (cluster as u64 - 2) * self.bytes_per_cluster
    }

    /// Read a single FAT entry for `cluster` (§ 5).
    fn fat_entry<R: Read + Seek>(&self, file: &mut R, cluster: u32) -> Result<u32, Error> {
        let fat_abs = self.base_offset + self.fat_start_rel;
        match self.fat_type {
            FatType::Fat12 => {
                // Two 12-bit entries share three bytes. Entry n occupies byte
                // ⌊n*3/2⌋ and the even/odd half of the 16-bit word there.
                let byte_off = cluster as u64 + cluster as u64 / 2;
                file.seek(SeekFrom::Start(fat_abs + byte_off))?;
                let mut buf = [0u8; 2];
                file.read_exact(&mut buf)?;
                let word = u16::from_le_bytes(buf) as u32;
                Ok(if cluster & 1 == 0 {
                    word & 0x0FFF
                } else {
                    word >> 4
                })
            }
            FatType::Fat16 => {
                file.seek(SeekFrom::Start(fat_abs + cluster as u64 * 2))?;
                let mut buf = [0u8; 2];
                file.read_exact(&mut buf)?;
                Ok(u16::from_le_bytes(buf) as u32)
            }
            FatType::Fat32 => {
                file.seek(SeekFrom::Start(fat_abs + cluster as u64 * 4))?;
                let mut buf = [0u8; 4];
                file.read_exact(&mut buf)?;
                // Top 4 bits are reserved; mask them out (§ 5.1).
                Ok(u32::from_le_bytes(buf) & 0x0FFF_FFFF)
            }
        }
    }

    /// Walk the FAT from `start_cluster`, returning the ordered chain of
    /// cluster numbers. Stops at EOC, bad-cluster markers, out-of-range
    /// cluster numbers, and cycles (chain length exceeds total_clusters).
    fn cluster_chain<R: Read + Seek>(&self, file: &mut R, start: u32) -> Result<Vec<u32>, Error> {
        let mut chain = Vec::new();
        let mut cluster = start;
        // Valid data clusters are 2..=total_clusters+1.
        let max_valid = self.total_clusters.saturating_add(1);
        loop {
            if cluster < 2 || cluster > max_valid {
                break;
            }
            if is_eoc(self.fat_type, cluster) || is_bad_cluster(self.fat_type, cluster) {
                break;
            }
            chain.push(cluster);
            if chain.len() > self.total_clusters as usize {
                // Cycle or corrupt chain — stop.
                break;
            }
            cluster = self.fat_entry(file, cluster)?;
        }
        Ok(chain)
    }
}

// ---------------------------------------------------------------------------
// BPB parsing
// ---------------------------------------------------------------------------

fn read_bpb<R: Read + Seek>(file: &mut R) -> Result<Context, Error> {
    let base_offset = file.stream_position()?;

    let mut sector = [0u8; 512];
    let mut filled = 0usize;
    while filled < 512 {
        match file.read(&mut sector[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    if filled < 512 {
        return Err(Error::TooShort);
    }

    // Signature check (§ 2.2).
    if sector[510] != 0x55 || sector[511] != 0xAA {
        return Err(Error::BadBootSector);
    }

    let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
    let sectors_per_cluster = sector[13];
    let reserved_sectors = u16::from_le_bytes([sector[14], sector[15]]);
    let num_fats = sector[16];
    let root_entry_count = u16::from_le_bytes([sector[17], sector[18]]);
    let total_sectors_16 = u16::from_le_bytes([sector[19], sector[20]]);
    let fat_size_16 = u16::from_le_bytes([sector[22], sector[23]]);
    let total_sectors_32 = u32::from_le_bytes([sector[32], sector[33], sector[34], sector[35]]);
    let fat_size_32 = u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]);
    let root_cluster_32 = u32::from_le_bytes([sector[44], sector[45], sector[46], sector[47]]);

    // Field-validity checks that together are a reliable FAT fingerprint.
    if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
        return Err(Error::BadBootSector);
    }
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
        return Err(Error::BadBootSector);
    }
    if num_fats == 0 || num_fats > 2 {
        return Err(Error::BadBootSector);
    }

    let bps = bytes_per_sector as u64;
    let spc = sectors_per_cluster as u64;
    let fat_size = if fat_size_16 != 0 {
        fat_size_16 as u64
    } else {
        fat_size_32 as u64
    };
    let total_sectors = if total_sectors_16 != 0 {
        total_sectors_16 as u64
    } else {
        total_sectors_32 as u64
    };
    if fat_size == 0 || total_sectors == 0 {
        return Err(Error::BadBootSector);
    }

    let fat_start_rel = reserved_sectors as u64 * bps;
    let root_dir_rel = fat_start_rel + num_fats as u64 * fat_size * bps;
    let root_dir_entry_bytes = root_entry_count as u64 * 32;
    let root_dir_sectors = root_dir_entry_bytes.div_ceil(bps);
    let data_start_rel = root_dir_rel + root_dir_sectors * bps;

    let data_sectors = total_sectors
        .saturating_sub(reserved_sectors as u64 + num_fats as u64 * fat_size + root_dir_sectors);
    let total_clusters = (data_sectors / spc) as u32;

    // FAT32 is uniquely identified by root_entry_count == 0 AND fat_size_16 == 0.
    // The cluster-count heuristic alone misclassifies small images formatted with
    // `mkfs.fat -F 32` (e.g. 16 MiB with 4 KiB clusters → only ~4000 clusters,
    // which is below the 65525 FAT32 threshold). Use the BPB fields first.
    let fat_type = if root_entry_count == 0 && fat_size_16 == 0 && root_cluster_32 >= 2 {
        FatType::Fat32
    } else if total_clusters < 4085 {
        FatType::Fat12
    } else if total_clusters < 65525 {
        FatType::Fat16
    } else {
        FatType::Fat32
    };

    let root_cluster = if fat_type == FatType::Fat32 {
        if root_cluster_32 < 2 {
            return Err(Error::BadBootSector);
        }
        root_cluster_32
    } else {
        0 // FAT12/16 root is at the fixed region
    };

    Ok(Context {
        base_offset,
        fat_type,
        bytes_per_cluster: spc * bps,
        fat_start_rel,
        root_dir_rel,
        root_dir_size_bytes: root_dir_entry_bytes,
        data_start_rel,
        root_cluster,
        total_clusters,
    })
}

// ---------------------------------------------------------------------------
// Directory parsing
// ---------------------------------------------------------------------------

/// Read raw directory bytes from either the FAT12/16 fixed root-directory
/// region (`start_cluster == 0`) or a cluster chain.
fn read_dir_bytes<R: Read + Seek>(
    ctx: &Context,
    file: &mut R,
    start_cluster: u32,
) -> Result<Vec<u8>, Error> {
    if start_cluster == 0 {
        file.seek(SeekFrom::Start(ctx.base_offset + ctx.root_dir_rel))?;
        let mut buf = vec![0u8; ctx.root_dir_size_bytes as usize];
        file.read_exact(&mut buf)?;
        return Ok(buf);
    }
    let chain = ctx.cluster_chain(file, start_cluster)?;
    let mut buf = Vec::with_capacity(chain.len() * ctx.bytes_per_cluster as usize);
    for &cluster in &chain {
        file.seek(SeekFrom::Start(ctx.cluster_abs(cluster)))?;
        let start = buf.len();
        buf.resize(start + ctx.bytes_per_cluster as usize, 0);
        file.read_exact(&mut buf[start..])?;
    }
    Ok(buf)
}

/// Extract the 13 UTF-16LE code units from an LFN entry (§ 6.3).
/// Fields: bytes 1–10 (5 chars), 14–25 (6 chars), 28–31 (2 chars).
fn lfn_chars(entry: &[u8]) -> [u16; 13] {
    let mut chars = [0xFFFF_u16; 13];
    let fields: &[(usize, usize)] = &[(1, 5), (14, 6), (28, 2)];
    let mut idx = 0;
    for &(start, count) in fields {
        for j in 0..count {
            let off = start + j * 2;
            chars[idx] = u16::from_le_bytes([entry[off], entry[off + 1]]);
            idx += 1;
        }
    }
    chars
}

/// Reassemble collected LFN pieces into a `String`.
///
/// The pieces arrive in forward-parse order (highest sequence number first
/// in the directory, so the vector starts with the last chunk). Sort
/// ascending by sequence number to get chunks 1 → … → N.
fn reassemble_lfn(pieces: &[(u8, [u16; 13])]) -> String {
    let mut sorted: Vec<_> = pieces.to_vec();
    sorted.sort_by_key(|(seq, _)| *seq);
    let chars: Vec<u16> = sorted
        .iter()
        .flat_map(|(_, c)| c.iter().copied())
        .take_while(|&c| c != 0x0000)
        .filter(|&c| c != 0xFFFF)
        .collect();
    String::from_utf16_lossy(&chars).to_string()
}

/// Format an 8.3 entry's name + extension into a display string.
///
/// FAT names are OEM code page 437 in the spec, but in practice they're
/// plain ASCII for every disk image we're likely to encounter. Uses
/// `from_utf8_lossy` for graceful degradation.
fn short_name_83(entry: &[u8]) -> String {
    let mut name_bytes = [0u8; 8];
    name_bytes.copy_from_slice(&entry[..8]);
    // 0x05 in position 0 means the actual first character is 0xE5 (§ 6.1).
    if name_bytes[0] == 0x05 {
        name_bytes[0] = 0xE5;
    }
    let name = String::from_utf8_lossy(&name_bytes);
    let name = name.trim_end_matches(' ');
    let ext = String::from_utf8_lossy(&entry[8..11]);
    let ext = ext.trim_end_matches(' ');
    if ext.is_empty() {
        name.to_string()
    } else {
        format!("{name}.{ext}")
    }
}

struct RawEntry {
    name: String,
    is_dir: bool,
    file_size: u32,
    start_cluster: u32,
}

/// Parse all valid (non-deleted, non-dot) 32-byte directory entries from
/// `dir_bytes`. Handles VFAT LFN continuation entries transparently.
fn parse_dir_entries(dir_bytes: &[u8]) -> Vec<RawEntry> {
    // Directory attribute flag values (§ 6.1).
    const ATTR_VOLUME_ID: u8 = 0x08;
    const ATTR_DIRECTORY: u8 = 0x10;
    const ATTR_LONG_NAME: u8 = 0x0F; // READ_ONLY|HIDDEN|SYSTEM|VOLUME_ID

    let mut out = Vec::new();
    let mut lfn_pieces: Vec<(u8, [u16; 13])> = Vec::new();

    for chunk in dir_bytes.chunks_exact(32) {
        let first = chunk[0];
        if first == 0x00 {
            break; // end-of-directory marker
        }
        if first == 0xE5 {
            lfn_pieces.clear(); // deleted; discard any pending LFN
            continue;
        }

        let attr = chunk[11];

        if attr & ATTR_LONG_NAME == ATTR_LONG_NAME && attr & ATTR_DIRECTORY == 0 {
            // LFN entry: collect for reassembly.
            let seq = chunk[0] & 0x3F;
            lfn_pieces.push((seq, lfn_chars(chunk)));
            continue;
        }

        // Volume label — disk name, not a real entry.
        if attr & ATTR_VOLUME_ID != 0 && attr & ATTR_DIRECTORY == 0 {
            lfn_pieces.clear();
            continue;
        }

        let name = if !lfn_pieces.is_empty() {
            let n = reassemble_lfn(&lfn_pieces);
            lfn_pieces.clear();
            n
        } else {
            short_name_83(chunk)
        };

        if name == "." || name == ".." {
            continue;
        }

        let is_dir = attr & ATTR_DIRECTORY != 0;
        let file_size = u32::from_le_bytes([chunk[28], chunk[29], chunk[30], chunk[31]]);
        let cluster_hi = u16::from_le_bytes([chunk[20], chunk[21]]) as u32;
        let cluster_lo = u16::from_le_bytes([chunk[26], chunk[27]]) as u32;
        let start_cluster = (cluster_hi << 16) | cluster_lo;

        out.push(RawEntry {
            name,
            is_dir,
            file_size,
            start_cluster,
        });
    }
    out
}

/// True when every consecutive pair of clusters is adjacent in physical
/// cluster number (i.e. the file's bytes are one contiguous run on disk).
fn is_contiguous(chain: &[u32]) -> bool {
    chain.windows(2).all(|w| w[1] == w[0] + 1)
}

/// Recursively build a [`TreeNode`] subtree rooted at `start_cluster`
/// (pass `0` for the FAT12/16 root directory, or the actual root-cluster
/// for FAT32 and subdirectories).
fn build_tree<R: Read + Seek>(
    ctx: &Context,
    file: &mut R,
    start_cluster: u32,
    depth: u32,
) -> Result<Vec<TreeNode>, Error> {
    if depth > 32 {
        return Ok(Vec::new());
    }

    let dir_bytes = read_dir_bytes(ctx, file, start_cluster)?;
    let entries = parse_dir_entries(&dir_bytes);
    let mut nodes = Vec::with_capacity(entries.len());

    for entry in entries {
        if entry.is_dir {
            let mut dir_node = TreeNode::new_directory(entry.name);
            let children = if entry.start_cluster >= 2 {
                build_tree(ctx, file, entry.start_cluster, depth + 1)?
            } else {
                Vec::new()
            };
            for child in children {
                dir_node.add_child(child);
            }
            nodes.push(dir_node);
        } else {
            let node = if entry.start_cluster >= 2 && entry.file_size > 0 {
                let chain = ctx.cluster_chain(file, entry.start_cluster)?;
                let required_clusters =
                    (entry.file_size as u64).div_ceil(ctx.bytes_per_cluster) as usize;
                if !chain.is_empty() && is_contiguous(&chain) && chain.len() >= required_clusters {
                    TreeNode::new_file_with_location(
                        entry.name,
                        entry.file_size as u64,
                        ctx.cluster_abs(chain[0]),
                        entry.file_size as u64,
                    )
                } else {
                    // Fragmented or truncated chain: tree entry exists but cat_node won't work.
                    TreeNode::new_file(entry.name, entry.file_size as u64)
                }
            } else {
                // Zero-length or no-cluster file.
                TreeNode::new_file(entry.name, entry.file_size as u64)
            };
            nodes.push(node);
        }
    }
    Ok(nodes)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Probe whether `file` (at its current position) looks like a FAT
/// filesystem. Restores the stream position regardless of outcome.
pub fn detect<R: Read + Seek>(file: &mut R) -> bool {
    let saved = match file.stream_position() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let ok = read_bpb(file).is_ok();
    let _ = file.seek(SeekFrom::Start(saved));
    ok
}

/// Parse a FAT12/16/32 filesystem starting at the current stream position,
/// returning a [`TreeNode`] tree rooted at `"/"`.
///
/// The caller must position `file` at the first byte of the FAT filesystem
/// (the BPB sector). For a raw single-filesystem image that is byte 0; for
/// a partitioned image the caller must seek to the partition start first.
pub fn detect_and_parse<R: Read + Seek>(file: &mut R) -> Result<TreeNode, Error> {
    let ctx = read_bpb(file)?;

    let root_cluster = match ctx.fat_type {
        FatType::Fat12 | FatType::Fat16 => 0, // fixed root-dir region
        FatType::Fat32 => ctx.root_cluster,
    };

    let mut root = TreeNode::new_directory("/".to_string());
    let children = build_tree(&ctx, file, root_cluster, 0)?;
    for child in children {
        root.add_child(child);
    }
    root.calculate_directory_size();
    Ok(root)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a minimal FAT12 image entirely in memory.
    ///
    /// Layout (512-byte sectors):
    ///   0   boot sector (BPB)
    ///   1   FAT copy 1
    ///   2   FAT copy 2
    ///   3   root directory  (16 entries × 32 B = 512 B)
    ///   4   cluster 2 data  → "hello world\n"
    ///   5   cluster 3 data  → free
    ///
    /// Six sectors = 3072 bytes. total_clusters = 2 → FAT12.
    fn make_fat12_image() -> Vec<u8> {
        let mut img = vec![0u8; 512 * 6];

        // BPB (§ 2.2)
        img[11..13].copy_from_slice(&512u16.to_le_bytes()); // bytes_per_sector
        img[13] = 1; // sectors_per_cluster
        img[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved_sectors
        img[16] = 2; // num_fats
        img[17..19].copy_from_slice(&16u16.to_le_bytes()); // root_entry_count
        img[19..21].copy_from_slice(&6u16.to_le_bytes()); // total_sectors_16
        img[21] = 0xF8; // media_type (fixed disk)
        img[22..24].copy_from_slice(&1u16.to_le_bytes()); // fat_size_16
        img[510] = 0x55;
        img[511] = 0xAA;

        // FAT1 at sector 1.
        // Clusters 0+1 (reserved): bytes [0xF8, 0xFF, 0xFF] (entries 0=0xFF8, 1=0xFFF)
        // Cluster 2 (EOC=0xFFF): bytes 3-5: entry2 even→ lower word = 0x0FFF
        //   bytes[3]=0xFF, bytes[4] = (0xF) | (entry3_low4=0x0)<<4 = 0x0F
        let f1 = 512usize;
        img[f1] = 0xF8; // cluster 0 low
        img[f1 + 1] = 0xFF; // cluster 0 hi + cluster 1 low
        img[f1 + 2] = 0xFF; // cluster 1 hi
        img[f1 + 3] = 0xFF; // cluster 2 low  (EOC)
        img[f1 + 4] = 0x0F; // cluster 2 hi + cluster 3 lo (cluster 3 = 0x000, free)

        // FAT2 — identical copy.
        let f2 = 512 * 2;
        let fat_copy = img[f1..f1 + 5].to_vec();
        img[f2..f2 + 5].copy_from_slice(&fat_copy);

        // Root directory at sector 3: one file entry "README  TXT".
        let rd = 512 * 3;
        img[rd..rd + 8].copy_from_slice(b"README  "); // 8.3 name
        img[rd + 8..rd + 11].copy_from_slice(b"TXT");
        img[rd + 11] = 0x20; // ATTR_ARCHIVE
        img[rd + 26..rd + 28].copy_from_slice(&2u16.to_le_bytes()); // first cluster low
        img[rd + 28..rd + 32].copy_from_slice(&12u32.to_le_bytes()); // file size

        // File data at cluster 2 = sector 4.
        let data = 512 * 4;
        img[data..data + 12].copy_from_slice(b"hello world\n");

        img
    }

    /// Minimal FAT32 image: 1 MiB, 512-byte sectors, 1 sector/cluster.
    ///
    /// The FAT32 BPB requires fat_size_16 = 0 and fills fat_size_32 instead.
    /// root_entry_count must be 0. root_cluster = 2.
    ///
    /// Geometry (all in sectors):
    ///   reserved = 32  (FAT32 standard minimum)
    ///   FATs      = 2 × 4 = 8  (4 sectors per FAT: enough for 512 clusters)
    ///   root dir  = 0 sectors fixed (FAT32 uses a cluster chain)
    ///   data      = 2048 - 32 - 8 = 2008 sectors → 2008 clusters
    ///   FAT type  : 2008 ≥ 65525? No (2008 < 65525), so this is actually
    ///               FAT16 by the cluster-count rule. Push to FAT32 by
    ///               using total_sectors = 66000 (fits in total_sectors_32)
    ///               and a correspondingly larger image. For simplicity,
    ///               just write a FAT32 signature string at offset 82 to
    ///               bypass the cluster-count heuristic? No — the spec
    ///               says the FS type string is informational only; the
    ///               cluster count determines FAT type.
    ///
    /// To get FAT32 by cluster count we need ≥ 65525 clusters. At 512 B
    /// per sector and 1 sector per cluster that means ≥ 65525 sectors of
    /// data. Use total_sectors_32 = 65600, reserved = 32, num_fats = 2,
    /// fat_size_32 = 512 sectors per FAT (covers 131072 clusters at 4 B
    /// each), root_dir_sectors = 0. Then:
    ///   data_sectors = 65600 - 32 - 2*512 - 0 = 64544 ≥ 65525? No again.
    ///
    /// FAT32 boundary is 65525. We need > 65525 data clusters. With
    /// sectors_per_cluster = 1 that's > 65525 data sectors.
    /// total = 65600, reserved = 32, 2 FATs × 512 = 1024 total FAT sectors.
    /// data = 65600 - 32 - 1024 = 64544. Still < 65525. No good.
    ///
    /// Easiest fix: sectors_per_cluster = 1, total_sectors_32 = 135000,
    /// reserved = 32, num_fats = 2, fat_size_32 = 512.
    ///   data = 135000 - 32 - 1024 = 133944 clusters → FAT32. ✓
    ///
    /// We don't actually allocate 135000 sectors of memory. The image bytes
    /// beyond what we write are irrelevant as long as we don't try to read
    /// cluster data. For pure detection + empty-tree tests this is fine.
    fn make_fat32_bpb_only() -> Vec<u8> {
        let mut img = vec![0u8; 512 * 64]; // 32 KiB — just enough for BPB + FATs + root

        // BPB common fields
        img[11..13].copy_from_slice(&512u16.to_le_bytes()); // bytes_per_sector
        img[13] = 1; // sectors_per_cluster
        img[14..16].copy_from_slice(&32u16.to_le_bytes()); // reserved_sectors
        img[16] = 2; // num_fats
                     // root_entry_count = 0 for FAT32 (bytes 17-18 stay 0)
                     // total_sectors_16 = 0 (use 32-bit field)
        img[22..24].copy_from_slice(&0u16.to_le_bytes()); // fat_size_16 = 0
        img[32..36].copy_from_slice(&135_000u32.to_le_bytes()); // total_sectors_32
                                                                // FAT32 extended BPB at offset 36
        img[36..40].copy_from_slice(&512u32.to_le_bytes()); // fat_size_32 (512 sectors)
        img[44..48].copy_from_slice(&2u32.to_le_bytes()); // root_cluster = 2
        img[510] = 0x55;
        img[511] = 0xAA;

        // FAT1 at sector 32 (offset 32*512 = 16384).
        let f1 = 32 * 512usize;
        img[f1] = 0xF8; // cluster 0 reserved
        img[f1 + 1] = 0xFF;
        img[f1 + 2] = 0xFF;
        img[f1 + 3] = 0x0F; // cluster 0 high nibble (FAT32 = 4 bytes)
        img[f1 + 4] = 0xFF; // cluster 1
        img[f1 + 5] = 0xFF;
        img[f1 + 6] = 0xFF;
        img[f1 + 7] = 0x0F;
        // Cluster 2 (root dir) = EOC
        img[f1 + 8] = 0xFF;
        img[f1 + 9] = 0xFF;
        img[f1 + 10] = 0xFF;
        img[f1 + 11] = 0x0F;

        // Root directory at cluster 2.
        // data_start = (32 + 2*512) * 512 = (32 + 1024) * 512 = 1056 * 512 = 540672
        // But that's beyond our 32 KiB image. For the empty-root test we
        // just need detect() to pass — no files to parse so build_tree
        // will read 1 cluster of zeros and return an empty vec.
        // read_dir_bytes will seek to cluster_abs(2) and try to read; we let
        // it return zeros (img is zero-initialized).

        img
    }

    #[test]
    fn detect_fat12_returns_true() {
        let img = make_fat12_image();
        let mut cursor = Cursor::new(&img);
        assert!(detect(&mut cursor), "FAT12 image should be detected");
    }

    #[test]
    fn detect_restores_position_on_success() {
        let img = make_fat12_image();
        let mut cursor = Cursor::new(&img);
        cursor.seek(SeekFrom::Start(0)).unwrap();
        detect(&mut cursor);
        assert_eq!(cursor.position(), 0, "detect must restore position");
    }

    #[test]
    fn detect_restores_position_on_failure() {
        let img = vec![0u8; 512];
        let mut cursor = Cursor::new(&img);
        detect(&mut cursor);
        assert_eq!(
            cursor.position(),
            0,
            "detect must restore position on failure"
        );
    }

    #[test]
    fn detect_rejects_non_fat() {
        let img = vec![0u8; 512]; // no boot signature, no valid BPB
        let mut cursor = Cursor::new(&img);
        assert!(!detect(&mut cursor));
    }

    #[test]
    fn parse_fat12_tree_shape() {
        let img = make_fat12_image();
        let mut cursor = Cursor::new(&img);
        let tree = detect_and_parse(&mut cursor).unwrap();

        assert_eq!(tree.name, "/");
        assert!(tree.is_directory);
        assert_eq!(tree.children.len(), 1);

        let file = &tree.children[0];
        assert_eq!(file.name, "README.TXT");
        assert_eq!(file.size, 12);
        assert_eq!(file.file_length, Some(12));
        assert!(!file.is_directory);
    }

    #[test]
    fn fat12_file_location_points_at_cluster_data() {
        let img = make_fat12_image();
        let mut cursor = Cursor::new(&img);
        let tree = detect_and_parse(&mut cursor).unwrap();

        let file = &tree.children[0];
        // Cluster 2 starts at sector 4 (BPB=0, FAT1=1, FAT2=2, rootdir=3).
        assert_eq!(file.file_location, Some(512 * 4));

        // Verify we can seek there and read the expected bytes.
        let loc = file.file_location.unwrap();
        let len = file.file_length.unwrap() as usize;
        cursor.seek(SeekFrom::Start(loc)).unwrap();
        let mut buf = vec![0u8; len];
        cursor.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello world\n");
    }

    #[test]
    fn empty_root_dir_parses_ok() {
        // Same image but with the root directory zeroed out → no files.
        let mut img = make_fat12_image();
        let rd = 512 * 3;
        img[rd..rd + 512].fill(0);
        let mut cursor = Cursor::new(&img);
        let tree = detect_and_parse(&mut cursor).unwrap();
        assert_eq!(tree.children.len(), 0);
        assert_eq!(tree.size, 0);
    }

    #[test]
    fn deleted_entry_skipped() {
        // Mark the single root-directory entry as deleted (first byte 0xE5).
        let mut img = make_fat12_image();
        img[512 * 3] = 0xE5;
        let mut cursor = Cursor::new(&img);
        let tree = detect_and_parse(&mut cursor).unwrap();
        assert_eq!(tree.children.len(), 0, "deleted entry must not appear");
    }

    #[test]
    fn parse_fat32_bpb_detect() {
        let img = make_fat32_bpb_only();
        let mut cursor = Cursor::new(&img);
        assert!(detect(&mut cursor), "FAT32 BPB should be detected");
    }

    #[test]
    fn fat12_with_lfn_entry() {
        // Build a root directory with a VFAT LFN entry followed by the 8.3
        // entry. The LFN carries "LongFile.txt" (12 chars → fits in 1 LFN entry
        // since each holds 13 UTF-16 code units).
        let mut img = make_fat12_image();
        let rd = 512 * 3;

        // LFN entry (sequence 0x41 = last + seq 1): "LongFile.txt" (12 chars + null).
        let lfn_name_chars: Vec<u16> = "LongFile.txt"
            .encode_utf16()
            .chain(std::iter::once(0x0000)) // null terminator
            .collect();
        let lfn = &mut img[rd..rd + 32];
        lfn[0] = 0x41; // last LFN entry, sequence 1
        lfn[11] = 0x0F; // ATTR_LONG_NAME
                        // Pack chars: bytes 1-10 (5 chars), 14-25 (6 chars), 28-31 (2 chars)
        let fields = [(1usize, 5usize), (14, 6), (28, 2)];
        let mut ci = 0;
        for (start, count) in fields {
            for j in 0..count {
                let ch = if ci < lfn_name_chars.len() {
                    lfn_name_chars[ci]
                } else {
                    0xFFFF
                };
                lfn[start + j * 2] = (ch & 0xFF) as u8;
                lfn[start + j * 2 + 1] = (ch >> 8) as u8;
                ci += 1;
            }
        }

        // 8.3 entry at rd+32: "LONGFI~1TXT"
        let e83 = &mut img[rd + 32..rd + 64];
        e83[..8].copy_from_slice(b"LONGFI~1");
        e83[8..11].copy_from_slice(b"TXT");
        e83[11] = 0x20; // ATTR_ARCHIVE
        e83[26..28].copy_from_slice(&2u16.to_le_bytes()); // cluster 2
        e83[28..32].copy_from_slice(&12u32.to_le_bytes()); // size 12

        // Zero out the old first entry (was README.TXT at rd+0, now LFN).
        // Actually the README entry is now the LFN — overwrite completed above.
        // We moved README to rd+0→LFN, rd+32→8.3. The old rd+0 was README.TXT
        // but we replaced it. The result should have ONE file with the LFN name.

        let mut cursor = Cursor::new(&img);
        let tree = detect_and_parse(&mut cursor).unwrap();
        assert_eq!(tree.children.len(), 1);
        // The LFN name should be used, not the 8.3 name "LONGFI~1.TXT".
        assert_eq!(
            tree.children[0].name, "LongFile.txt",
            "LFN reassembly failed, got: {}",
            tree.children[0].name
        );
    }

    #[test]
    fn too_short_image_errors() {
        let img = vec![0u8; 256]; // < 512 bytes
        let mut cursor = Cursor::new(&img);
        assert!(matches!(
            detect_and_parse(&mut cursor),
            Err(Error::TooShort)
        ));
    }

    #[test]
    fn bad_bytes_per_sector_errors() {
        let mut img = make_fat12_image();
        // Set bytes_per_sector to 300 (not a power of two, not in {512,1024,2048,4096}).
        img[11..13].copy_from_slice(&300u16.to_le_bytes());
        let mut cursor = Cursor::new(&img);
        assert!(matches!(
            detect_and_parse(&mut cursor),
            Err(Error::BadBootSector)
        ));
    }

    #[test]
    fn bad_sectors_per_cluster_errors() {
        let mut img = make_fat12_image();
        img[13] = 3; // not a power of two
        let mut cursor = Cursor::new(&img);
        assert!(matches!(
            detect_and_parse(&mut cursor),
            Err(Error::BadBootSector)
        ));
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        let msg = format!("{}", Error::TooShort);
        assert!(msg.contains("short") || msg.contains("FAT"), "got: {msg}");
    }

    #[test]
    fn error_display_bad_boot_sector() {
        let msg = format!("{}", Error::BadBootSector);
        assert!(
            msg.contains("BPB") || msg.contains("boot") || msg.contains("FAT"),
            "got: {msg}"
        );
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
        assert!(Error::BadBootSector.source().is_none());
    }

    // ── is_eoc / is_bad_cluster for FAT16 and FAT32 ───────────────────────────

    #[test]
    fn is_eoc_fat16_and_fat32() {
        assert!(is_eoc(FatType::Fat16, 0xFFF8));
        assert!(is_eoc(FatType::Fat16, 0xFFFF));
        assert!(!is_eoc(FatType::Fat16, 0xFFF7));
        assert!(is_eoc(FatType::Fat32, 0x0FFF_FFF8));
        assert!(is_eoc(FatType::Fat32, 0x0FFF_FFFF));
        assert!(!is_eoc(FatType::Fat32, 0x0FFF_FFF7));
    }

    #[test]
    fn is_bad_cluster_fat16_and_fat32() {
        assert!(is_bad_cluster(FatType::Fat16, 0xFFF7));
        assert!(!is_bad_cluster(FatType::Fat16, 0xFFF8));
        assert!(is_bad_cluster(FatType::Fat32, 0x0FFF_FFF7));
        assert!(!is_bad_cluster(FatType::Fat32, 0x0FFF_FFF8));
    }
}
