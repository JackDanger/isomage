//! TAR archive reader (`tar` feature).
//!
//! Reads a POSIX.1-2001 (`ustar`) or GNU TAR archive and produces a
//! [`TreeNode`] tree compatible with `cat_node` / `extract_node`.
//!
//! Reference: POSIX.1-2001 pax interchange format; GNU tar internals.
//!
//! ## What is implemented
//!
//! - Magic detection: `ustar\0` (POSIX) and `ustar  \0` (GNU).
//! - Header parsing: name, size, typeflag (regular file, hard link,
//!   symbolic link, directory).
//! - GNU long-name extension (type `L`): up to 64 KiB long filenames.
//! - GNU long-link extension (type `K`): long symlink targets (parsed but
//!   symlinks appear as zero-byte files in the tree).
//! - PAX extended header (type `x` / `g`): `path` and `size` overrides
//!   applied before the next entry. Other PAX keys are ignored.
//! - `file_location` is set for regular files so `cat_node` can read them
//!   without understanding the TAR framing.
//! - Directory structure reconstructed from slash-delimited names.
//!
//! ## What is NOT implemented
//!
//! - Compressed TAR (`.tar.gz`, `.tar.bz2`, `.tar.xz`): compression is
//!   handled by a separate wrapper (planned). This reader requires an
//!   already-decompressed byte stream.
//! - Sparse files (GNU `S` and `0S` entries).
//! - Multi-volume TAR archives.

use std::io::{Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Constants ─────────────────────────────────────────────────────────────────

const BLOCK: u64 = 512;
const USTAR_MAGIC_OFFSET: usize = 257;
const TYPEFLAG_OFFSET: usize = 156;

const TYPE_REGULAR: u8 = b'0';
const TYPE_REGULAR_ALT: u8 = b'\0'; // older archives
const TYPE_HARD_LINK: u8 = b'1';
const TYPE_SYMLINK: u8 = b'2';
const TYPE_DIR: u8 = b'5';
const TYPE_GNU_LONG_NAME: u8 = b'L';
const TYPE_GNU_LONG_LINK: u8 = b'K';
const TYPE_PAX_LOCAL: u8 = b'x';
const TYPE_PAX_GLOBAL: u8 = b'g';

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can arise while detecting or parsing a TAR archive.
#[derive(Debug)]
pub enum Error {
    /// Magic bytes not found; probably not a TAR file.
    NotTar,
    /// A TAR header field contains invalid data.
    BadHeader,
    /// Underlying I/O failure.
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotTar => write!(f, "not a TAR archive (ustar magic not found)"),
            Error::BadHeader => write!(f, "TAR header is corrupt or truncated"),
            Error::Io(e) => write!(f, "TAR I/O error: {e}"),
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

// ── Detection ─────────────────────────────────────────────────────────────────

fn has_ustar_magic(block: &[u8; 512]) -> bool {
    let magic = &block[USTAR_MAGIC_OFFSET..USTAR_MAGIC_OFFSET + 6];
    magic == b"ustar\0" || magic == b"ustar "
}

// ── Header parsing helpers ────────────────────────────────────────────────────

/// Read a NUL-terminated octal ASCII field from the header.
fn parse_octal(field: &[u8]) -> u64 {
    let s = field
        .iter()
        .take_while(|&&b| b != 0 && b != b' ')
        .copied()
        .collect::<Vec<u8>>();
    let s = std::str::from_utf8(&s).unwrap_or("0").trim();
    u64::from_str_radix(s, 8).unwrap_or(0)
}

/// Read a NUL-padded name field as a String.
fn parse_name(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Build the full entry name from the POSIX prefix + name fields.
fn entry_name(block: &[u8; 512]) -> String {
    let name = parse_name(&block[0..100]);
    // POSIX ustar prefix at offset 345 (155 bytes).
    let prefix = parse_name(&block[345..500]);
    if prefix.is_empty() {
        name
    } else {
        format!("{}/{}", prefix, name)
    }
}

// ── PAX extended header parsing ───────────────────────────────────────────────

/// Parse a PAX extended header body, returning overrides for `path` and `size`.
fn parse_pax(body: &[u8]) -> (Option<String>, Option<u64>) {
    let mut path = None;
    let mut size = None;

    // Each record is: "<length> <key>=<value>\n"
    let s = String::from_utf8_lossy(body);
    for line in s.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Strip the leading length field (digits + space).
        let rest = line.find(' ').and_then(|i| line.get(i + 1..)).unwrap_or("");
        if let Some(val) = rest.strip_prefix("path=") {
            path = Some(val.to_string());
        } else if let Some(val) = rest.strip_prefix("size=") {
            size = val.parse::<u64>().ok();
        }
    }

    (path, size)
}

// ── Archive scanning ──────────────────────────────────────────────────────────

struct TarEntry {
    name: String,
    size: u64,
    is_dir: bool,
    /// Byte offset of the entry's data (first byte after the 512-byte header).
    data_offset: u64,
}

fn scan_entries<R: Read + Seek>(r: &mut R) -> Result<Vec<TarEntry>, Error> {
    r.seek(SeekFrom::Start(0))?;
    let mut entries = Vec::new();

    // State for GNU long-name / PAX overrides that apply to the next entry.
    let mut pending_name: Option<String> = None;
    let mut pending_size: Option<u64> = None;

    let mut consecutive_zero = 0u32;

    loop {
        let header_pos = r.stream_position()?;
        let mut block = [0u8; 512];
        match r.read_exact(&mut block) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(Error::Io(e)),
        }

        // Two consecutive all-zero blocks = end-of-archive.
        if block.iter().all(|&b| b == 0) {
            consecutive_zero += 1;
            if consecutive_zero >= 2 {
                break;
            }
            continue;
        }
        consecutive_zero = 0;

        if !has_ustar_magic(&block) && entries.is_empty() && pending_name.is_none() {
            return Err(Error::NotTar);
        }

        let typeflag = block[TYPEFLAG_OFFSET];
        let raw_name = entry_name(&block);
        let raw_size = parse_octal(&block[124..136]);
        let data_offset = header_pos + BLOCK;

        // Number of 512-byte blocks to skip over the data.
        let data_blocks = raw_size.div_ceil(BLOCK);

        match typeflag {
            TYPE_GNU_LONG_NAME => {
                // Next 512*data_blocks bytes contain the long filename.
                let mut name_bytes = vec![0u8; raw_size as usize];
                r.read_exact(&mut name_bytes)?;
                let null_end = name_bytes
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(name_bytes.len());
                pending_name = Some(String::from_utf8_lossy(&name_bytes[..null_end]).into_owned());
                // Pad to block boundary.
                let leftover = data_blocks * BLOCK - raw_size;
                if leftover > 0 {
                    r.seek(SeekFrom::Current(leftover as i64))?;
                }
                continue;
            }
            TYPE_GNU_LONG_LINK => {
                // Long symlink target — consume and ignore (symlinks become
                // zero-byte files in the tree).
                r.seek(SeekFrom::Current((data_blocks * BLOCK) as i64))?;
                continue;
            }
            TYPE_PAX_LOCAL | TYPE_PAX_GLOBAL => {
                let mut pax_bytes = vec![0u8; raw_size as usize];
                r.read_exact(&mut pax_bytes)?;
                let leftover = data_blocks * BLOCK - raw_size;
                if leftover > 0 {
                    r.seek(SeekFrom::Current(leftover as i64))?;
                }
                let (p, s) = parse_pax(&pax_bytes);
                if p.is_some() {
                    pending_name = p;
                }
                if s.is_some() {
                    pending_size = s;
                }
                continue;
            }
            _ => {}
        }

        // Apply pending overrides.
        let raw = pending_name.take().unwrap_or(raw_name);
        let size = pending_size.take().unwrap_or(raw_size);
        // Strip the leading "./" that `tar -C dir -cf archive .` stores.
        let name = raw.trim_start_matches("./").to_string();

        let is_dir = typeflag == TYPE_DIR || name.ends_with('/');
        let is_file = typeflag == TYPE_REGULAR
            || typeflag == TYPE_REGULAR_ALT
            || typeflag == TYPE_HARD_LINK
            || typeflag == TYPE_SYMLINK;

        if is_file || is_dir {
            entries.push(TarEntry {
                name: name.trim_end_matches('/').to_string(),
                size,
                is_dir,
                data_offset,
            });
        }

        // Skip over data blocks.
        r.seek(SeekFrom::Current((data_blocks * BLOCK) as i64))?;
    }

    Ok(entries)
}

// ── Tree construction ─────────────────────────────────────────────────────────

fn build_tree(entries: Vec<TarEntry>) -> TreeNode {
    use std::collections::HashMap;

    let mut nodes: HashMap<String, TreeNode> = HashMap::new();

    for entry in &entries {
        let path = entry.name.trim_end_matches('/');
        if path.is_empty() {
            continue;
        }

        // Create ancestor directories.
        let mut acc = String::new();
        let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        for (i, component) in components.iter().enumerate() {
            if i > 0 {
                acc.push('/');
            }
            acc.push_str(component);
            nodes
                .entry(acc.clone())
                .or_insert_with(|| TreeNode::new_directory((*component).to_string()));
        }

        // Update leaf node.
        if let Some(node) = nodes.get_mut(path) {
            if entry.is_dir {
                node.is_directory = true;
            } else {
                node.is_directory = false;
                node.size = entry.size;
                node.file_length = Some(entry.size);
                node.file_location = Some(entry.data_offset);
            }
        }
    }

    // Wire parent → children.
    let mut paths: Vec<String> = nodes.keys().cloned().collect();
    paths.sort();

    let mut children_of: HashMap<String, Vec<String>> = HashMap::new();
    for path in &paths {
        let parent = match path.rfind('/') {
            Some(i) => path[..i].to_string(),
            None => String::new(),
        };
        children_of.entry(parent).or_default().push(path.clone());
    }

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

/// Returns `Ok(())` if `r` looks like a TAR archive (ustar magic at offset 257
/// of the first block). Stream position is restored.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let saved = r.stream_position().unwrap_or(0);
    let mut block = [0u8; 512];
    let result = r.read_exact(&mut block).map_err(Error::Io).and_then(|()| {
        if has_ustar_magic(&block) {
            Ok(())
        } else {
            Err(Error::NotTar)
        }
    });
    let _ = r.seek(SeekFrom::Start(saved));
    result
}

/// Parse a TAR archive from `r`, returning a [`TreeNode`] tree.
///
/// The root node is named `"/"`. Regular files have `file_location` set so
/// `cat_node` can read them directly from the TAR without extraction.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    // Verify magic first.
    detect(r)?;
    let entries = scan_entries(r)?;
    Ok(build_tree(entries))
}

// ── Write API (`write` feature) ───────────────────────────────────────────────

/// Write a ustar TAR archive to `w`.
///
/// `entries` is a slice of `(name, data)` pairs. Names may use `/` as a path
/// separator. The output is a valid POSIX ustar archive readable by any
/// modern `tar` implementation.
///
/// Requires `--features write`.
#[cfg(feature = "write")]
pub fn write<W: std::io::Write>(w: &mut W, entries: &[(&str, &[u8])]) -> std::io::Result<()> {
    for (name, data) in entries {
        write_entry(w, name, data)?;
    }
    // End-of-archive: two 512-byte zero blocks (POSIX.1-2001 §10.1.2).
    w.write_all(&[0u8; 1024])?;
    Ok(())
}

#[cfg(feature = "write")]
fn write_entry<W: std::io::Write>(w: &mut W, name: &str, data: &[u8]) -> std::io::Result<()> {
    let mut header = [0u8; 512];

    // Name field (offset 0, 100 bytes). Truncate silently; callers responsible
    // for keeping names under 100 bytes or using a GNU long-name pre-header.
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(100);
    header[..name_len].copy_from_slice(&name_bytes[..name_len]);

    // Mode: 0000644 (owner rw, group/other r)
    header[100..107].copy_from_slice(b"0000644");

    // UID + GID: 0000000
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");

    // Size (octal, 11 digits + NUL)
    let size_str = format!("{:011o}\0", data.len());
    header[124..136].copy_from_slice(size_str.as_bytes());

    // Modification time: zero epoch
    header[136..147].copy_from_slice(b"00000000000");
    header[147] = 0;

    // Type flag: regular file
    header[156] = b'0';

    // ustar magic + version
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    // Checksum: fill the checksum field with spaces, sum all 512 bytes,
    // write the octal sum back. The trailing space matches GNU tar convention.
    header[148..156].fill(b' ');
    let cksum: u32 = header.iter().map(|&b| b as u32).sum();
    let cksum_str = format!("{:06o}\0 ", cksum);
    header[148..156].copy_from_slice(cksum_str.as_bytes());

    w.write_all(&header)?;
    w.write_all(data)?;

    // Pad data to 512-byte block boundary.
    let remainder = data.len() % 512;
    if remainder != 0 {
        let padding = 512 - remainder;
        w.write_all(&vec![0u8; padding])?;
    }

    Ok(())
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Build a minimal ustar-format TAR containing one file.
    fn make_ustar(name: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 512 * 3]; // header + data block + EOF

        // Name at offset 0 (100 bytes).
        let name_bytes = name.as_bytes();
        buf[..name_bytes.len().min(100)].copy_from_slice(&name_bytes[..name_bytes.len().min(100)]);

        // Size at offset 124 (12 bytes, octal ASCII).
        let size_str = format!("{:011o}\0", data.len());
        buf[124..136].copy_from_slice(size_str.as_bytes());

        // Type flag at offset 156: regular file.
        buf[156] = TYPE_REGULAR;

        // ustar magic at offset 257.
        buf[257..263].copy_from_slice(b"ustar\0");
        buf[263..265].copy_from_slice(b"00");

        // Checksum at offset 148 (8 bytes). Fill with spaces first.
        buf[148..156].fill(b' ');
        let cksum: u32 = buf[..512].iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        buf[148..156].copy_from_slice(cksum_str.as_bytes());

        // Data in second block.
        buf[512..512 + data.len()].copy_from_slice(data);

        // Third block is all zeros (EOF marker; we'd need two but one is
        // enough for our parser which stops at EOF anyway).
        buf
    }

    #[test]
    fn detect_ustar() {
        let tar = make_ustar("hello.txt", b"hi");
        let mut c = Cursor::new(&tar);
        assert!(detect(&mut c).is_ok());
    }

    #[test]
    fn parse_single_file() {
        let tar = make_ustar("hello.txt", b"hi there");
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert_eq!(root.name, "/");
        assert_eq!(root.children.len(), 1);
        let file = &root.children[0];
        assert_eq!(file.name, "hello.txt");
        assert_eq!(file.size, 8);
        assert!(
            file.file_location.is_some(),
            "regular file must have file_location"
        );
        // Data starts at offset 512 (right after the header block).
        assert_eq!(file.file_location.unwrap(), 512);
    }

    #[test]
    fn detect_rejects_non_tar() {
        let mut c = Cursor::new(b"not a tar file, not 512 bytes long enough to be one either");
        assert!(detect(&mut c).is_err());
    }

    #[test]
    fn nested_path_from_name() {
        let tar = make_ustar("subdir/file.txt", b"data");
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "subdir");
        assert!(root.children[0].is_directory);
        assert_eq!(root.children[0].children.len(), 1);
        assert_eq!(root.children[0].children[0].name, "file.txt");
    }

    #[test]
    fn directory_size_roll_up() {
        let tar = make_ustar("docs/guide.txt", b"hello world");
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        let docs = &root.children[0];
        assert_eq!(docs.name, "docs");
        assert!(docs.is_directory);
        assert_eq!(docs.size, 11);
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_not_tar() {
        let msg = format!("{}", Error::NotTar);
        assert!(
            msg.contains("TAR") || msg.contains("ustar"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_bad_header() {
        let msg = format!("{}", Error::BadHeader);
        assert!(
            msg.contains("header") || msg.contains("TAR"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_io() {
        let io_err = std::io::Error::other("disk read");
        let msg = format!("{}", Error::Io(io_err));
        assert!(msg.contains("disk read"), "unexpected: {msg}");
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        let io_err = std::io::Error::other("src");
        assert!(Error::Io(io_err).source().is_some());
    }

    #[test]
    fn error_source_non_io() {
        use std::error::Error as StdError;
        assert!(Error::NotTar.source().is_none());
        assert!(Error::BadHeader.source().is_none());
    }

    // ── parse_octal ───────────────────────────────────────────────────────────

    #[test]
    fn parse_octal_basic() {
        assert_eq!(parse_octal(b"00000000755\0"), 0o755);
        assert_eq!(parse_octal(b"00000001234\0"), 0o1234);
        assert_eq!(parse_octal(b"\0"), 0);
    }

    #[test]
    fn parse_octal_space_terminated() {
        // Some TAR fields use space instead of NUL to terminate.
        assert_eq!(parse_octal(b"0000644 "), 0o644);
    }

    // ── parse_name / entry_name ───────────────────────────────────────────────

    #[test]
    fn parse_name_nul_terminated() {
        let mut field = [0u8; 100];
        field[..7].copy_from_slice(b"foo.txt");
        assert_eq!(parse_name(&field), "foo.txt");
    }

    #[test]
    fn entry_name_with_prefix() {
        // Build a block where prefix is non-empty; entry_name should concatenate.
        let mut block = [0u8; 512];
        block[..7].copy_from_slice(b"foo.txt");
        block[345..353].copy_from_slice(b"myprefix");
        block[257..263].copy_from_slice(b"ustar\0");
        let name = entry_name(&block);
        assert_eq!(name, "myprefix/foo.txt");
    }

    // ── parse_pax ─────────────────────────────────────────────────────────────

    #[test]
    fn parse_pax_path_key() {
        let body = b"22 path=/long/path/to/file\n";
        let (path, size) = parse_pax(body);
        assert_eq!(path.as_deref(), Some("/long/path/to/file"));
        assert!(size.is_none());
    }

    #[test]
    fn parse_pax_size_key() {
        let body = b"13 size=99999\n";
        let (path, size) = parse_pax(body);
        assert!(path.is_none());
        assert_eq!(size, Some(99999));
    }

    #[test]
    fn parse_pax_unknown_key_ignored() {
        let body = b"18 mtime=1234567890\n";
        let (path, size) = parse_pax(body);
        assert!(path.is_none());
        assert!(size.is_none());
    }

    #[test]
    fn parse_pax_empty_body() {
        let (path, size) = parse_pax(b"");
        assert!(path.is_none());
        assert!(size.is_none());
    }

    // ── GNU long name handling ────────────────────────────────────────────────

    fn make_gnu_long_name_tar(long_name: &str, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();

        // Block 1: type 'L' header carrying the long filename.
        let name_bytes = long_name.as_bytes();
        let name_padded_blocks = name_bytes.len().div_ceil(512) as u64;

        let mut hdr_l = [0u8; 512];
        // name field: "././@LongLink"
        hdr_l[..13].copy_from_slice(b"././@LongLink");
        // size = name_bytes.len()
        let size_str = format!("{:011o}\0", name_bytes.len());
        hdr_l[124..136].copy_from_slice(size_str.as_bytes());
        // typeflag = 'L'
        hdr_l[156] = TYPE_GNU_LONG_NAME;
        // ustar magic
        hdr_l[257..263].copy_from_slice(b"ustar\0");
        hdr_l[263..265].copy_from_slice(b"00");
        // checksum
        hdr_l[148..156].fill(b' ');
        let cksum: u32 = hdr_l.iter().map(|&b| b as u32).sum();
        hdr_l[148..156].copy_from_slice(format!("{:06o}\0 ", cksum).as_bytes());
        buf.extend_from_slice(&hdr_l);

        // Data blocks for the long name.
        let mut name_block = vec![0u8; name_padded_blocks as usize * 512];
        name_block[..name_bytes.len()].copy_from_slice(name_bytes);
        buf.extend_from_slice(&name_block);

        // Block 3: actual file header (short placeholder name).
        buf.extend_from_slice(&make_ustar("x", data));

        buf
    }

    #[test]
    fn gnu_long_name_overrides_short_name() {
        let long_name = "a_very_long_directory_name/with/many/components/file.txt";
        let data = b"content";
        let tar = make_gnu_long_name_tar(long_name, data);
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        // The file should appear under the long path.
        let f = root.find_node("/a_very_long_directory_name/with/many/components/file.txt");
        assert!(f.is_some(), "long-name file should be in tree");
    }

    // ── GNU long link (type K) ────────────────────────────────────────────────

    fn make_gnu_long_link_tar(target: &str, after_name: &str, after_data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();

        // Type 'K' header carrying the symlink target.
        let target_bytes = target.as_bytes();
        let target_blocks = target_bytes.len().div_ceil(512);

        let mut hdr_k = [0u8; 512];
        hdr_k[..13].copy_from_slice(b"././@LongLink");
        let size_str = format!("{:011o}\0", target_bytes.len());
        hdr_k[124..136].copy_from_slice(size_str.as_bytes());
        hdr_k[156] = TYPE_GNU_LONG_LINK;
        hdr_k[257..263].copy_from_slice(b"ustar\0");
        hdr_k[263..265].copy_from_slice(b"00");
        hdr_k[148..156].fill(b' ');
        let ck: u32 = hdr_k.iter().map(|&b| b as u32).sum();
        hdr_k[148..156].copy_from_slice(format!("{:06o}\0 ", ck).as_bytes());
        buf.extend_from_slice(&hdr_k);

        let mut target_block = vec![0u8; target_blocks * 512];
        target_block[..target_bytes.len()].copy_from_slice(target_bytes);
        buf.extend_from_slice(&target_block);

        // The regular file that follows.
        buf.extend_from_slice(&make_ustar(after_name, after_data));
        buf
    }

    #[test]
    fn gnu_long_link_consumed_and_next_entry_parsed() {
        let tar =
            make_gnu_long_link_tar("/very/long/symlink/target/path", "real_file.txt", b"data");
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        // real_file.txt should appear; the long-link target is silently dropped.
        assert!(root.find_node("/real_file.txt").is_some());
    }

    // ── PAX extended header (type x) ─────────────────────────────────────────

    fn make_pax_tar(pax_path: &str, actual_data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();

        // Build the PAX record body.
        let path_record = format!("{} path={}\n", 7 + pax_path.len(), pax_path);
        let pax_body = path_record.as_bytes();
        let pax_blocks = pax_body.len().div_ceil(512);

        let mut pax_hdr = [0u8; 512];
        let size_str = format!("{:011o}\0", pax_body.len());
        pax_hdr[124..136].copy_from_slice(size_str.as_bytes());
        pax_hdr[156] = TYPE_PAX_LOCAL;
        pax_hdr[257..263].copy_from_slice(b"ustar\0");
        pax_hdr[263..265].copy_from_slice(b"00");
        pax_hdr[148..156].fill(b' ');
        let ck: u32 = pax_hdr.iter().map(|&b| b as u32).sum();
        pax_hdr[148..156].copy_from_slice(format!("{:06o}\0 ", ck).as_bytes());
        buf.extend_from_slice(&pax_hdr);

        let mut pax_data = vec![0u8; pax_blocks * 512];
        pax_data[..pax_body.len()].copy_from_slice(pax_body);
        buf.extend_from_slice(&pax_data);

        // Regular file with a short placeholder name; PAX overrides it.
        buf.extend_from_slice(&make_ustar("x", actual_data));
        buf
    }

    #[test]
    fn pax_path_overrides_short_name() {
        let long_path = "pax/extended/path/to/file.rs";
        let tar = make_pax_tar(long_path, b"pax data");
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert!(
            root.find_node("/pax/extended/path/to/file.rs").is_some(),
            "PAX path override should place file at the correct path"
        );
    }

    // ── Hard link and symlink type flags ──────────────────────────────────────

    fn make_ustar_with_type(name: &str, typeflag: u8, size: usize) -> Vec<u8> {
        let mut buf = vec![0u8; 1024]; // one header + one data block
        let name_bytes = name.as_bytes();
        buf[..name_bytes.len().min(100)].copy_from_slice(&name_bytes[..name_bytes.len().min(100)]);
        let size_str = format!("{:011o}\0", size);
        buf[124..136].copy_from_slice(size_str.as_bytes());
        buf[156] = typeflag;
        buf[257..263].copy_from_slice(b"ustar\0");
        buf[263..265].copy_from_slice(b"00");
        buf[148..156].fill(b' ');
        let ck: u32 = buf[..512].iter().map(|&b| b as u32).sum();
        buf[148..156].copy_from_slice(format!("{:06o}\0 ", ck).as_bytes());
        buf
    }

    #[test]
    fn hard_link_appears_in_tree() {
        let tar = make_ustar_with_type("link.txt", TYPE_HARD_LINK, 0);
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert!(
            root.find_node("/link.txt").is_some(),
            "hard link should appear in tree"
        );
    }

    #[test]
    fn symlink_appears_as_zero_byte_file() {
        let tar = make_ustar_with_type("sym.txt", TYPE_SYMLINK, 0);
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        let n = root
            .find_node("/sym.txt")
            .expect("symlink should be in tree");
        assert!(!n.is_directory);
    }

    #[test]
    fn regular_alt_type_is_regular_file() {
        // TYPE_REGULAR_ALT ('\0') should be treated as a regular file.
        let tar = make_ustar_with_type("old.txt", TYPE_REGULAR_ALT, 3);
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        let n = root
            .find_node("/old.txt")
            .expect("old.txt should be in tree");
        assert_eq!(n.size, 3);
        assert!(n.file_location.is_some());
    }

    // ── Explicit directory type flag ──────────────────────────────────────────

    #[test]
    fn explicit_dir_entry_is_directory() {
        let tar = make_ustar_with_type("mydir/", TYPE_DIR, 0);
        let mut c = Cursor::new(&tar);
        let root = detect_and_parse(&mut c).expect("parse failed");
        let n = root.find_node("/mydir").expect("mydir should be in tree");
        assert!(n.is_directory, "TYPE_DIR entry should be a directory");
    }

    // ── Two consecutive zero blocks = end of archive ──────────────────────────

    #[test]
    fn two_zero_blocks_end_archive() {
        // Build: valid header + two zero blocks + another valid header.
        // The parser should stop at the two zero blocks; the second file is ignored.
        let file1 = make_ustar("first.txt", b"a");
        let mut buf = file1;
        buf.extend_from_slice(&[0u8; 1024]); // two zero blocks = EOF
        buf.extend_from_slice(&make_ustar("second.txt", b"b"));

        let mut c = Cursor::new(&buf);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert!(root.find_node("/first.txt").is_some());
        assert!(
            root.find_node("/second.txt").is_none(),
            "should stop at EOF marker"
        );
    }

    /// Build just the raw header+data blocks for one TAR entry (no trailing zero).
    fn make_ustar_raw(name: &str, data: &[u8]) -> Vec<u8> {
        let data_blocks = data.len().div_ceil(512);
        let total = 512 + data_blocks * 512;
        let mut buf = vec![0u8; total];
        let name_bytes = name.as_bytes();
        buf[..name_bytes.len().min(100)].copy_from_slice(&name_bytes[..name_bytes.len().min(100)]);
        let size_str = format!("{:011o}\0", data.len());
        buf[124..136].copy_from_slice(size_str.as_bytes());
        buf[156] = TYPE_REGULAR;
        buf[257..263].copy_from_slice(b"ustar\0");
        buf[263..265].copy_from_slice(b"00");
        buf[148..156].fill(b' ');
        let ck: u32 = buf[..512].iter().map(|&b| b as u32).sum();
        buf[148..156].copy_from_slice(format!("{:06o}\0 ", ck).as_bytes());
        buf[512..512 + data.len()].copy_from_slice(data);
        buf
    }

    #[test]
    fn single_zero_block_does_not_stop_parsing() {
        // One zero block between two entries: the parser keeps going.
        // Use raw builders (no trailing zero block) so consecutive_zero stays at 1.
        let mut buf = make_ustar_raw("first.txt", b"a");
        buf.extend_from_slice(&[0u8; 512]); // single zero block — not EOF (consecutive_zero=1)
        buf.extend_from_slice(&make_ustar_raw("second.txt", b"b"));
        buf.extend_from_slice(&[0u8; 1024]); // proper EOF (consecutive_zero reaches 2)

        let mut c = Cursor::new(&buf);
        let root = detect_and_parse(&mut c).expect("parse failed");
        assert!(root.find_node("/first.txt").is_some());
        assert!(root.find_node("/second.txt").is_some());
    }
}
