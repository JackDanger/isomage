//! ZIP archive reader (`zip` feature).
//!
//! Reads the central directory of a ZIP/ZIP64 archive and produces a
//! [`TreeNode`] tree compatible with `cat_node` / `extract_node`.
//!
//! Reference: APPNOTE.TXT (PKWARE ZIP specification, v6.3.10).
//!
//! ## What is implemented
//!
//! - End-of-central-directory (EOCD) record detection, including ZIP64
//!   EOCD locator + EOCD64 for archives > 4 GiB.
//! - Central directory entry parsing: file name, compression method,
//!   uncompressed/compressed size, local file header offset.
//! - Stored (method 0) files get a `file_location` pointing at their raw
//!   data so `cat_node` / `extract_node` can read them directly without
//!   decompression.
//! - Directory entries and path components are reconstructed from the
//!   `/`-delimited names in the central directory.
//! - ZIP file comments and extra-field extensions are skipped gracefully.
//!
//! ## What is NOT implemented
//!
//! - Deflate, Deflate64, LZMA, BZip2, ZStd decompression (planned, each
//!   behind its own Cargo feature). Compressed entries appear in the tree
//!   but `cat_node` returns an error for them until the feature lands.
//! - Encryption (traditional PKWARE or WinZip AES).
//! - Multi-volume / split archives.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Magic / signature constants ───────────────────────────────────────────────

const EOCD_SIG: u32 = 0x0605_4B50;
const EOCD64_SIG: u32 = 0x0606_4B50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4B50;
const CDR_SIG: u32 = 0x0201_4B50;
const LFH_SIG: u32 = 0x0403_4B50;

const EOCD_MIN_SIZE: u64 = 22;
const EOCD64_SIZE: u64 = 56;
const EOCD64_LOCATOR_SIZE: u64 = 20;

/// Max comment length (u16::MAX) + EOCD fixed fields.
const MAX_EOCD_SEARCH: u64 = 65535 + EOCD_MIN_SIZE;

const METHOD_STORED: u16 = 0;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can arise while detecting or parsing a ZIP archive.
#[derive(Debug)]
pub enum Error {
    /// Stream too short or no EOCD signature found.
    NotZip,
    /// Central directory offset or size is inconsistent with file length.
    BadCentralDirectory,
    /// Underlying I/O failure.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotZip => write!(f, "not a ZIP archive (EOCD signature not found)"),
            Error::BadCentralDirectory => {
                write!(f, "ZIP central directory is corrupt or truncated")
            }
            Error::Io(e) => write!(f, "ZIP I/O error: {e}"),
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

// ── EOCD location ─────────────────────────────────────────────────────────────

struct EocdInfo {
    cd_offset: u64,
    cd_size: u64,
}

fn find_eocd<R: Read + Seek>(r: &mut R) -> Result<EocdInfo, Error> {
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < EOCD_MIN_SIZE {
        return Err(Error::NotZip);
    }

    let search_start = file_len.saturating_sub(MAX_EOCD_SEARCH);
    let search_len = (file_len - search_start) as usize;
    r.seek(SeekFrom::Start(search_start))?;
    let mut buf = vec![0u8; search_len];
    r.read_exact(&mut buf)?;

    // Scan backwards for EOCD signature.
    let eocd_pos = buf
        .windows(4)
        .rposition(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) == EOCD_SIG)
        .ok_or(Error::NotZip)?;

    let eocd = &buf[eocd_pos..];
    if eocd.len() < 22 {
        return Err(Error::NotZip);
    }

    let total_entries = u16::from_le_bytes([eocd[10], eocd[11]]) as u64;
    let cd_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as u64;
    let cd_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as u64;

    let is_zip64 = total_entries == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF;

    if is_zip64 {
        let eocd_abs = search_start + eocd_pos as u64;
        if eocd_abs < EOCD64_LOCATOR_SIZE {
            return Err(Error::NotZip);
        }
        let locator_abs = eocd_abs - EOCD64_LOCATOR_SIZE;
        r.seek(SeekFrom::Start(locator_abs))?;
        let mut loc = [0u8; 20];
        r.read_exact(&mut loc)?;
        if u32::from_le_bytes([loc[0], loc[1], loc[2], loc[3]]) != EOCD64_LOCATOR_SIG {
            return Err(Error::NotZip);
        }
        let eocd64_abs = u64::from_le_bytes(loc[8..16].try_into().unwrap());
        r.seek(SeekFrom::Start(eocd64_abs))?;
        let mut e64 = [0u8; EOCD64_SIZE as usize];
        r.read_exact(&mut e64)?;
        if u32::from_le_bytes([e64[0], e64[1], e64[2], e64[3]]) != EOCD64_SIG {
            return Err(Error::NotZip);
        }
        let cd_size64 = u64::from_le_bytes(e64[40..48].try_into().unwrap());
        let cd_offset64 = u64::from_le_bytes(e64[48..56].try_into().unwrap());
        return Ok(EocdInfo {
            cd_offset: cd_offset64,
            cd_size: cd_size64,
        });
    }

    Ok(EocdInfo { cd_offset, cd_size })
}

// ── Central directory parsing ─────────────────────────────────────────────────

struct CdEntry {
    /// Slash-delimited name as stored in the CD (may end with `/` for dirs).
    name: String,
    method: u16,
    uncompressed_size: u64,
    /// Byte offset of the local file header for this entry.
    local_header_offset: u64,
}

fn parse_central_directory(buf: &[u8]) -> Result<Vec<CdEntry>, Error> {
    let mut entries = Vec::new();
    let mut pos = 0usize;

    while pos + 4 <= buf.len() {
        let sig = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        if sig != CDR_SIG {
            break;
        }
        if pos + 46 > buf.len() {
            return Err(Error::BadCentralDirectory);
        }

        let method = u16::from_le_bytes([buf[pos + 10], buf[pos + 11]]);
        let compressed_size =
            u32::from_le_bytes([buf[pos + 20], buf[pos + 21], buf[pos + 22], buf[pos + 23]]) as u64;
        let uncompressed_size =
            u32::from_le_bytes([buf[pos + 24], buf[pos + 25], buf[pos + 26], buf[pos + 27]]) as u64;
        let name_len = u16::from_le_bytes([buf[pos + 28], buf[pos + 29]]) as usize;
        let extra_len = u16::from_le_bytes([buf[pos + 30], buf[pos + 31]]) as usize;
        let comment_len = u16::from_le_bytes([buf[pos + 32], buf[pos + 33]]) as usize;
        let local_header_offset =
            u32::from_le_bytes([buf[pos + 42], buf[pos + 43], buf[pos + 44], buf[pos + 45]]) as u64;

        let name_start = pos + 46;
        let name_end = name_start + name_len;
        if name_end > buf.len() {
            return Err(Error::BadCentralDirectory);
        }

        let name = String::from_utf8_lossy(&buf[name_start..name_end]).into_owned();

        // Resolve ZIP64 extra field if any sentinel values are present.
        let extra_start = name_end;
        let extra_end = (extra_start + extra_len).min(buf.len());
        let (_, uncomp, lh_off) = if extra_start < extra_end {
            parse_zip64_extra(
                &buf[extra_start..extra_end],
                compressed_size,
                uncompressed_size,
                local_header_offset,
            )
        } else {
            (compressed_size, uncompressed_size, local_header_offset)
        };

        entries.push(CdEntry {
            name,
            method,
            uncompressed_size: uncomp,
            local_header_offset: lh_off,
        });

        pos = name_end + extra_len + comment_len;
    }

    Ok(entries)
}

fn parse_zip64_extra(extra: &[u8], comp: u64, uncomp: u64, offset: u64) -> (u64, u64, u64) {
    let mut pos = 0;
    let mut comp_out = comp;
    let mut uncomp_out = uncomp;
    let mut offset_out = offset;

    while pos + 4 <= extra.len() {
        let tag = u16::from_le_bytes([extra[pos], extra[pos + 1]]);
        let size = u16::from_le_bytes([extra[pos + 2], extra[pos + 3]]) as usize;
        pos += 4;
        if pos + size > extra.len() {
            break;
        }
        if tag == 0x0001 {
            let mut p = pos;
            if uncomp == 0xFFFF_FFFF && p + 8 <= pos + size {
                uncomp_out = u64::from_le_bytes(extra[p..p + 8].try_into().unwrap());
                p += 8;
            }
            if comp == 0xFFFF_FFFF && p + 8 <= pos + size {
                comp_out = u64::from_le_bytes(extra[p..p + 8].try_into().unwrap());
                p += 8;
            }
            if offset == 0xFFFF_FFFF && p + 8 <= pos + size {
                offset_out = u64::from_le_bytes(extra[p..p + 8].try_into().unwrap());
            }
        }
        pos += size;
    }

    (comp_out, uncomp_out, offset_out)
}

/// Compute the byte offset of the actual file data by reading the local
/// file header at `lh_offset`. Returns `None` if the header is invalid.
fn local_data_offset<R: Read + Seek>(r: &mut R, lh_offset: u64) -> Option<u64> {
    r.seek(SeekFrom::Start(lh_offset)).ok()?;
    let mut hdr = [0u8; 30];
    r.read_exact(&mut hdr).ok()?;
    if u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) != LFH_SIG {
        return None;
    }
    let name_len = u16::from_le_bytes([hdr[26], hdr[27]]) as u64;
    let extra_len = u16::from_le_bytes([hdr[28], hdr[29]]) as u64;
    Some(lh_offset + 30 + name_len + extra_len)
}

// ── Tree construction ─────────────────────────────────────────────────────────

/// Build a `TreeNode` tree from a flat list of CD entries.
///
/// The `HashMap` maps each slash-path (without leading slash, without trailing
/// slash) to its node. After all entries are inserted we do a single pass to
/// wire parent→child relationships.
fn build_tree<R: Read + Seek>(r: &mut R, entries: Vec<CdEntry>) -> TreeNode {
    // path (no leading slash) → node
    let mut nodes: HashMap<String, TreeNode> = HashMap::new();

    for entry in &entries {
        let raw = entry.name.trim_end_matches('/');
        if raw.is_empty() {
            continue;
        }

        // Ensure every ancestor directory exists.
        let mut acc = String::new();
        for (i, component) in raw.split('/').enumerate() {
            if component.is_empty() {
                continue;
            }
            if i > 0 {
                acc.push('/');
            }
            acc.push_str(component);
            nodes
                .entry(acc.clone())
                .or_insert_with(|| TreeNode::new_directory(component.to_string()));
        }

        // Update the leaf with file metadata.
        let is_dir = entry.name.ends_with('/') || entry.name.ends_with('\\');
        if !is_dir {
            if let Some(node) = nodes.get_mut(raw) {
                node.is_directory = false;
                node.size = entry.uncompressed_size;
                node.file_length = Some(entry.uncompressed_size);
                if entry.method == METHOD_STORED {
                    node.file_location = local_data_offset(r, entry.local_header_offset);
                }
            }
        }
    }

    // Wire children into parents. Sort so parent paths always come before
    // children in the iteration order.
    let mut paths: Vec<String> = nodes.keys().cloned().collect();
    paths.sort();

    // Build parent→[child paths] index.
    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    for path in &paths {
        let parent = match path.rfind('/') {
            Some(i) => path[..i].to_string(),
            None => String::new(), // root-level entry
        };
        children_of.entry(parent).or_default().push(path.clone());
    }

    // Recursive attachment using a helper that drains `nodes`.
    fn attach(
        node: &mut TreeNode,
        key: &str,
        nodes: &mut HashMap<String, TreeNode>,
        children_of: &HashMap<String, Vec<String>>,
    ) {
        if let Some(child_keys) = children_of.get(key) {
            let mut keys = child_keys.clone();
            keys.sort();
            for ck in keys {
                if let Some(mut child) = nodes.remove(&ck) {
                    attach(&mut child, &ck, nodes, children_of);
                    node.children.push(child);
                }
            }
        }
    }

    let mut root = TreeNode::new_directory("/".to_string());
    attach(&mut root, "", &mut nodes, &children_of);
    root.calculate_directory_size();
    root
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Returns `Ok(())` if `r` looks like a ZIP archive.
/// Stream position is restored on both success and failure.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let saved = r.stream_position().unwrap_or(0);
    let result = find_eocd(r).map(|_| ());
    let _ = r.seek(SeekFrom::Start(saved));
    result
}

/// Parse a ZIP archive from `r`, returning a [`TreeNode`] tree.
///
/// The root node is named `"/"`. Stored (uncompressed) files have
/// `file_location` set so `cat_node` can read them directly.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let eocd = find_eocd(r)?;

    let file_len = r.seek(SeekFrom::End(0))?;
    let cd_end = eocd
        .cd_offset
        .checked_add(eocd.cd_size)
        .ok_or(Error::BadCentralDirectory)?;
    if cd_end > file_len {
        return Err(Error::BadCentralDirectory);
    }

    r.seek(SeekFrom::Start(eocd.cd_offset))?;
    let mut cd_buf = vec![0u8; eocd.cd_size as usize];
    r.read_exact(&mut cd_buf)?;

    let entries = parse_central_directory(&cd_buf)?;
    Ok(build_tree(r, entries))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn make_stored_zip(name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut z = Vec::new();

        let lh_offset = z.len() as u32;
        z.extend_from_slice(&LFH_SIG.to_le_bytes());
        z.extend_from_slice(&20u16.to_le_bytes()); // version needed
        z.extend_from_slice(&0u16.to_le_bytes()); // flags
        z.extend_from_slice(&METHOD_STORED.to_le_bytes());
        z.extend_from_slice(&0u32.to_le_bytes()); // mod time + date
        z.extend_from_slice(&0u32.to_le_bytes()); // CRC-32
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // extra len
        z.extend_from_slice(name);
        z.extend_from_slice(data);

        let cd_offset = z.len() as u32;
        z.extend_from_slice(&CDR_SIG.to_le_bytes());
        z.extend_from_slice(&20u16.to_le_bytes()); // version made by
        z.extend_from_slice(&20u16.to_le_bytes()); // version needed
        z.extend_from_slice(&0u16.to_le_bytes()); // flags
        z.extend_from_slice(&METHOD_STORED.to_le_bytes());
        z.extend_from_slice(&0u32.to_le_bytes()); // mod time + date
        z.extend_from_slice(&0u32.to_le_bytes()); // CRC-32
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(data.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // extra len
        z.extend_from_slice(&0u16.to_le_bytes()); // comment len
        z.extend_from_slice(&0u16.to_le_bytes()); // disk start
        z.extend_from_slice(&0u16.to_le_bytes()); // internal attr
        z.extend_from_slice(&0u32.to_le_bytes()); // external attr
        z.extend_from_slice(&lh_offset.to_le_bytes());
        z.extend_from_slice(name);

        let cd_size = z.len() as u32 - cd_offset;
        z.extend_from_slice(&EOCD_SIG.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // disk number
        z.extend_from_slice(&0u16.to_le_bytes()); // cd disk
        z.extend_from_slice(&1u16.to_le_bytes()); // entries on disk
        z.extend_from_slice(&1u16.to_le_bytes()); // total entries
        z.extend_from_slice(&cd_size.to_le_bytes());
        z.extend_from_slice(&cd_offset.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // comment len
        z
    }

    #[test]
    fn detect_stored_zip() {
        let zip = make_stored_zip(b"hello.txt", b"hi");
        let mut c = Cursor::new(&zip);
        assert!(detect(&mut c).is_ok());
    }

    #[test]
    fn parse_stored_zip_single_file() {
        let zip = make_stored_zip(b"hello.txt", b"hi");
        let mut c = Cursor::new(&zip);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert_eq!(root.name, "/");
        assert_eq!(root.children.len(), 1);
        let file = &root.children[0];
        assert_eq!(file.name, "hello.txt");
        assert_eq!(file.size, 2);
        assert!(
            file.file_location.is_some(),
            "stored file must have file_location"
        );
    }

    #[test]
    fn detect_rejects_non_zip() {
        let mut c = Cursor::new(b"this is not a zip file at all");
        assert!(detect(&mut c).is_err());
    }

    #[test]
    fn nested_directory_path() {
        let zip = make_stored_zip(b"a/b/c.txt", b"nested");
        let mut c = Cursor::new(&zip);
        let root = detect_and_parse(&mut c).expect("parse failed");
        // root → a → b → c.txt
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "a");
        assert_eq!(root.children[0].children.len(), 1);
        assert_eq!(root.children[0].children[0].name, "b");
        assert_eq!(root.children[0].children[0].children.len(), 1);
        assert_eq!(root.children[0].children[0].children[0].name, "c.txt");
    }

    #[test]
    fn directory_size_roll_up() {
        let zip = make_stored_zip(b"docs/readme.txt", b"hello world");
        let mut c = Cursor::new(&zip);
        let root = detect_and_parse(&mut c).expect("parse failed");
        let docs = &root.children[0];
        assert_eq!(docs.name, "docs");
        assert!(docs.is_directory);
        assert_eq!(docs.size, 11); // rolled up from readme.txt
    }
}
