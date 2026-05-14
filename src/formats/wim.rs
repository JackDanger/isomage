//! WIM (Windows Imaging Format) reader (`wim` feature).
//!
//! WIM is Microsoft's image format for Windows installation media and
//! backup. Each WIM file contains one or more "images" (complete
//! filesystem snapshots) plus XML metadata describing them.
//!
//! This reader does **not** decompress or enumerate the files inside
//! each image (that would require LZX/XPRESS codec deps). It reads:
//!
//! - The 208-byte header to detect the format and extract image count.
//! - The XML metadata blob (when uncompressed) to extract image names.
//! - Returns a [`TreeNode`] tree with one directory child per image.
//!
//! If the XML data resource is compressed (flags bit `0x04` set) we
//! return [`Error::Compressed`] rather than pulling in a codec dep.
//!
//! ## WIM Header layout (little-endian, 208 bytes at offset 0)
//!
//! ```text
//! [0]   u8[8]   image_tag          = b"MSWIM\0\0\0"
//! [8]   u32     cb_size            = 208 (header size in bytes)
//! [12]  u32     wim_version        = 0x00010900 for WIM 1.09
//! [16]  u32     flags              // compression type flags
//! [20]  u32     chunk_size         // default 32768
//! [24]  u8[16]  guid
//! [40]  u16     part_number        // 1-based part index
//! [42]  u16     total_parts
//! [44]  u32     image_count
//! [48]  RESHDR  offset_table       // 24 bytes each
//! [72]  RESHDR  xml_data           // 24 bytes
//! [96]  RESHDR  boot_metadata      // 24 bytes
//! [120] u32     boot_index
//! [124] RESHDR  integrity          // 24 bytes
//! [148] u8[60]  reserved
//! ```
//!
//! ## RESHDR_DISK layout (24 bytes, little-endian)  [MS-WIM §2.3]
//!
//! ```text
//! [0]  u8[7]+u8  CBDisk: compressed size in bytes 0–6 (7-byte LE integer),
//!                        flags in byte 7 (high byte of the 8-byte LE read)
//! [8]  u64       Offset: byte offset of resource from start of WIM file
//! [16] u64       CBOriginal: uncompressed size of resource
//! ```
//!
//! `flags & 0x04` indicates the resource is compressed.

use std::io::{self, Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Magic ──────────────────────────────────────────────────────────────────────

/// WIM file magic bytes at offset 0.
const WIM_MAGIC: &[u8; 8] = b"MSWIM\0\0\0";

/// Expected header size for WIM format version 1.
const HEADER_SIZE: usize = 208;

/// RESHDR_DISK flag indicating the resource is compressed.
const RESHDR_FLAG_COMPRESSED: u8 = 0x04;

// ── Error type ─────────────────────────────────────────────────────────────────

/// Reasons [`detect`] or [`detect_and_parse`] can fail.
#[derive(Debug)]
pub enum Error {
    /// File is shorter than the 208-byte WIM header.
    TooShort,
    /// The 8-byte magic at offset 0 is not `b"MSWIM\0\0\0"`.
    BadMagic,
    /// The XML data resource is compressed; reading it without a
    /// codec dependency is not supported. Enable the `lzms` or
    /// `deflate-gzippy` feature (when available) to decode it.
    Compressed,
    /// The XML data could not be decoded as UTF-16 LE.
    BadEncoding,
    /// Underlying I/O error.
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "WIM file is shorter than the 208-byte header"),
            Error::BadMagic => write!(f, "WIM magic b\"MSWIM\\0\\0\\0\" not found at offset 0"),
            Error::Compressed => write!(
                f,
                "WIM XML data resource is compressed; codec not available in this build"
            ),
            Error::BadEncoding => write!(f, "WIM XML data has invalid UTF-16 LE encoding"),
            Error::Io(e) => write!(f, "WIM I/O error: {e}"),
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

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

// ── Parsed header types ────────────────────────────────────────────────────────

/// Parsed RESHDR_DISK (resource header, 24 bytes).
#[derive(Debug, Clone, Copy)]
struct ResHdr {
    /// Byte offset of the resource within the WIM file (from bytes 8–15).
    offset: u64,
    /// Compressed size of the resource on disk (from low 56 bits of bytes 0–7).
    /// Reserved for future use when codec support is added.
    #[allow(dead_code)]
    size: u64,
    /// Original (uncompressed) size of the resource (from bytes 16–23).
    original_size: u64,
    /// RESHDR flags byte (high byte of bytes 0–7, i.e. byte 7 of the field).
    flags: u8,
}

impl ResHdr {
    /// Parse a 24-byte RESHDR_DISK slice per MS-WIM §2.3.
    fn from_bytes(b: &[u8]) -> Self {
        debug_assert_eq!(b.len(), 24);
        // Bytes 0–7: CBDisk — compressed size in low 56 bits, flags in high 8 bits.
        let size_and_flags = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
        // Bytes 8–15: Offset — byte offset of resource in WIM file.
        let offset = u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]);
        // Bytes 16–23: CBOriginal — uncompressed size.
        let original_size =
            u64::from_le_bytes([b[16], b[17], b[18], b[19], b[20], b[21], b[22], b[23]]);
        let flags = (size_and_flags >> 56) as u8;
        let size = size_and_flags & 0x00FF_FFFF_FFFF_FFFF;
        ResHdr {
            offset,
            size,
            original_size,
            flags,
        }
    }

    /// Returns `true` when the RESHDR_FLAG_COMPRESSED bit is set.
    fn is_compressed(&self) -> bool {
        (self.flags & RESHDR_FLAG_COMPRESSED) != 0
    }
}

/// Parsed WIM header fields we use.
struct Header {
    /// Number of images stored in this WIM.
    image_count: u32,
    /// Resource descriptor for the XML metadata blob.
    xml_data: ResHdr,
}

// ── Header reader ──────────────────────────────────────────────────────────────

/// Read and parse the 208-byte WIM header from offset 0.
///
/// Does **not** restore the stream position; callers should save/restore if
/// they need it.
fn read_header<R: Read + Seek>(r: &mut R) -> Result<Header, Error> {
    r.seek(SeekFrom::Start(0))?;

    let mut buf = [0u8; HEADER_SIZE];
    match r.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    // Check magic.
    if &buf[0..8] != WIM_MAGIC {
        return Err(Error::BadMagic);
    }

    // image_count is at offset 44, u32 LE.
    let image_count = u32::from_le_bytes([buf[44], buf[45], buf[46], buf[47]]);

    // xml_data RESHDR starts at offset 72.
    let xml_data = ResHdr::from_bytes(&buf[72..96]);

    Ok(Header {
        image_count,
        xml_data,
    })
}

// ── Detection ──────────────────────────────────────────────────────────────────

/// Detect whether `r` is a WIM file by checking the magic bytes.
///
/// Reads the first 8 bytes, verifies they are `b"MSWIM\0\0\0"`, then
/// restores the stream position on both success and error paths.
///
/// Returns `Ok(())` on success.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let pos = r.stream_position()?;
    let result = detect_inner(r);
    let _ = r.seek(SeekFrom::Start(pos));
    result
}

fn detect_inner<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    // Need at least 208 bytes for a valid WIM header.
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < HEADER_SIZE as u64 {
        return Err(Error::TooShort);
    }

    r.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 8];
    match r.read_exact(&mut magic) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    if &magic != WIM_MAGIC {
        return Err(Error::BadMagic);
    }
    Ok(())
}

// ── XML parser ─────────────────────────────────────────────────────────────────

/// An image entry extracted from the WIM XML metadata.
#[derive(Debug, Default)]
struct ImageEntry {
    /// 1-based image index from the `INDEX` attribute of `<IMAGE>`.
    index: u32,
    /// Value of the `<NAME>` element, if present.
    name: Option<String>,
    /// Value of the `<TOTALBYTES>` element, if present.
    total_bytes: Option<u64>,
}

/// Parse UTF-16 LE bytes into a Rust `String` (lossy).
///
/// Skips a BOM (`0xFEFF`) if present at the start.
fn utf16le_to_string(raw: &[u8]) -> Result<String, Error> {
    if raw.len() % 2 != 0 {
        return Err(Error::BadEncoding);
    }
    let mut units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();

    // Strip BOM if present.
    if units.first().copied() == Some(0xFEFF) {
        units.remove(0);
    }

    Ok(String::from_utf16_lossy(&units).to_string())
}

/// Extract the content of the first occurrence of `<TAG>...</TAG>` in `text`
/// that starts at or after byte offset `start`.
///
/// Returns `(content, end_position)` where `end_position` is the byte offset
/// just after the closing tag, suitable for further scanning.
fn extract_tag<'a>(text: &'a str, tag: &str, start: usize) -> Option<(&'a str, usize)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let begin = text[start..].find(&open)? + start + open.len();
    let end = text[begin..].find(&close)? + begin;
    Some((&text[begin..end], end + close.len()))
}

/// Scan XML text for `<IMAGE INDEX="N">…</IMAGE>` blocks and extract
/// each image's index, name, and total bytes.
///
/// The XML is flat enough that a simple linear scan works. We do not
/// handle nested tags inside `<IMAGE>` other than the ones we care
/// about.
fn parse_xml(xml: &str) -> Vec<ImageEntry> {
    let mut images: Vec<ImageEntry> = Vec::new();
    let mut pos = 0;

    while let Some(tag_start) = xml[pos..].find("<IMAGE") {
        let abs_start = pos + tag_start;

        // Find the end of the opening tag so we can read the INDEX attribute.
        let tag_head_end = match xml[abs_start..].find('>') {
            Some(e) => abs_start + e + 1,
            None => break,
        };
        let tag_head = &xml[abs_start..tag_head_end];

        // Extract INDEX="N" from the tag head.
        let index = parse_attr_u32(tag_head, "INDEX").unwrap_or(0);

        // Locate </IMAGE>.
        let close_tag = "</IMAGE>";
        let image_end = match xml[tag_head_end..].find(close_tag) {
            Some(e) => tag_head_end + e + close_tag.len(),
            None => break,
        };
        let image_body = &xml[tag_head_end..image_end - close_tag.len()];

        // Extract <NAME> and <TOTALBYTES> from the body.
        let name = extract_tag(image_body, "NAME", 0).map(|(s, _)| s.trim().to_string());
        let total_bytes = extract_tag(image_body, "TOTALBYTES", 0)
            .and_then(|(s, _)| s.trim().parse::<u64>().ok());

        images.push(ImageEntry {
            index,
            name,
            total_bytes,
        });

        pos = image_end;
    }

    images
}

/// Parse `NAME="VALUE"` or `NAME='VALUE'` from an attribute string,
/// returning VALUE parsed as u32.
fn parse_attr_u32(text: &str, name: &str) -> Option<u32> {
    let key = format!("{name}=");
    let start = text.find(&key)? + key.len();
    let rest = &text[start..];
    let (quote, value_start) = if rest.starts_with('"') {
        ('"', 1)
    } else if rest.starts_with('\'') {
        ('\'', 1)
    } else {
        return None;
    };
    let value_end = rest[value_start..].find(quote)? + value_start;
    rest[value_start..value_end].parse().ok()
}

// ── Tree building ──────────────────────────────────────────────────────────────

/// Parse the WIM at `r` and return a [`TreeNode`] tree.
///
/// The tree shape is:
///
/// ```text
/// / (dir)
/// ├─ "Image 1"  (dir — name from XML, or "Image 1" if missing)
/// └─ "Image 2"  (dir)
/// ```
///
/// Each image node is a directory (we don't enumerate files within the
/// image — that requires codec support). `file_location` and
/// `file_length` are both `None`.
///
/// Returns [`Error::Compressed`] when the XML data resource is stored
/// compressed, and [`Error::BadMagic`] when the file is not a WIM.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let header = read_header(r)?;

    let xml_res = header.xml_data;

    // We can only read uncompressed XML without a codec dep.
    if xml_res.is_compressed() {
        return Err(Error::Compressed);
    }

    // Validate original_size before allocating: bounds-check against the
    // actual file size and impose a 64 MiB cap so a corrupt header can't
    // drive an OOM. WIM XML metadata is always well under 1 MiB in practice.
    let file_size = r.seek(io::SeekFrom::End(0))?;
    const MAX_XML_SIZE: u64 = 64 * 1024 * 1024;
    if xml_res.original_size > MAX_XML_SIZE
        || xml_res.offset > file_size
        || xml_res.original_size > file_size - xml_res.offset
    {
        return Err(Error::TooShort);
    }

    // Read the raw XML bytes.
    r.seek(SeekFrom::Start(xml_res.offset))?;
    let read_len = xml_res.original_size as usize;
    let mut raw = vec![0u8; read_len];
    r.read_exact(&mut raw).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::TooShort
        } else {
            Error::Io(e)
        }
    })?;

    // Decode UTF-16 LE.
    let xml_text = utf16le_to_string(&raw)?;

    // Parse image entries from XML.
    let mut entries = parse_xml(&xml_text);

    // Sort by index so the tree is stable.
    entries.sort_by_key(|e| e.index);

    // Build the tree.
    let mut root = TreeNode::new_directory("/".to_string());

    // If XML gave us no entries, fall back to image_count from the header.
    // Cap at 4096 to prevent a corrupt header from driving excessive allocation.
    if entries.is_empty() {
        let count = header.image_count.min(4096);
        for i in 1..=count {
            let node = TreeNode::new_directory(format!("Image {i}"));
            root.add_child(node);
        }
    } else {
        for entry in &entries {
            let name = match &entry.name {
                Some(n) if !n.is_empty() => n.clone(),
                _ => format!("Image {}", entry.index),
            };
            let mut node = TreeNode::new_directory(name);
            // Populate size from TOTALBYTES if available.
            if let Some(tb) = entry.total_bytes {
                node.size = tb;
            }
            root.add_child(node);
        }
    }

    root.calculate_directory_size();
    Ok(root)
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── WIM builder helpers ────────────────────────────────────────────────

    /// Encode a Rust string as UTF-16 LE bytes with a BOM prefix.
    fn encode_utf16le(s: &str) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        // BOM
        out.extend_from_slice(&0xFEFFu16.to_le_bytes());
        for c in s.encode_utf16() {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out
    }

    /// Build a minimal valid 208-byte WIM header + an uncompressed UTF-16 LE
    /// XML blob appended immediately after.
    ///
    /// The RESHDR for xml_data is set to point at offset 208 with
    /// `original_size = xml_bytes.len()` and `flags = 0` (uncompressed).
    fn build_wim(image_count: u32, xml: &str) -> Vec<u8> {
        let xml_bytes = encode_utf16le(xml);
        let xml_len = xml_bytes.len() as u64;

        let mut hdr = [0u8; HEADER_SIZE];

        // Magic.
        hdr[0..8].copy_from_slice(WIM_MAGIC);

        // cb_size = 208.
        hdr[8..12].copy_from_slice(&208u32.to_le_bytes());

        // wim_version = 0x00010900.
        hdr[12..16].copy_from_slice(&0x0001_0900u32.to_le_bytes());

        // flags = 0 (no compression).
        hdr[16..20].copy_from_slice(&0u32.to_le_bytes());

        // chunk_size = 32768.
        hdr[20..24].copy_from_slice(&32768u32.to_le_bytes());

        // guid = 16 zero bytes (already zeroed).

        // part_number = 1, total_parts = 1.
        hdr[40..42].copy_from_slice(&1u16.to_le_bytes());
        hdr[42..44].copy_from_slice(&1u16.to_le_bytes());

        // image_count.
        hdr[44..48].copy_from_slice(&image_count.to_le_bytes());

        // offset_table RESHDR at [48..72]: all zeros (not used in detection).

        // xml_data RESHDR at [72..96] (MS-WIM §2.3 layout):
        //   [72..80] CBDisk: compressed_size=xml_len, flags=0x00 (uncompressed)
        //   [80..88] Offset: 208 (XML blob starts immediately after 208-byte header)
        //   [88..96] CBOriginal: xml_len (same as compressed since uncompressed)
        let size_and_flags: u64 = xml_len; // flags = 0x00 in high byte
        hdr[72..80].copy_from_slice(&size_and_flags.to_le_bytes());
        hdr[80..88].copy_from_slice(&(HEADER_SIZE as u64).to_le_bytes()); // offset = 208
        hdr[88..96].copy_from_slice(&xml_len.to_le_bytes());

        // boot_metadata RESHDR at [96..120]: all zeros.
        // boot_index at [120..124]: 0.
        // integrity RESHDR at [124..148]: all zeros.
        // reserved [148..208]: all zeros.

        let mut out: Vec<u8> = hdr.to_vec();
        out.extend_from_slice(&xml_bytes);
        out
    }

    /// Build a WIM with a compressed XML resource (flags bit 0x04 set).
    fn build_wim_compressed_xml(image_count: u32) -> Vec<u8> {
        let mut wim = build_wim(image_count, "<WIM></WIM>");
        // Set flags byte (byte 7 of CBDisk field at hdr[72..80], i.e. hdr[79]) to 0x04.
        wim[79] = RESHDR_FLAG_COMPRESSED;
        wim
    }

    // ── XML for round-trip tests ───────────────────────────────────────────

    fn xml_one_image(name: &str, total_bytes: u64) -> String {
        format!(
            r#"<WIM>
  <IMAGE INDEX="1">
    <NAME>{name}</NAME>
    <TOTALBYTES>{total_bytes}</TOTALBYTES>
  </IMAGE>
</WIM>"#
        )
    }

    fn xml_two_images() -> String {
        r#"<WIM>
  <IMAGE INDEX="1">
    <NAME>Windows 10 Pro</NAME>
    <TOTALBYTES>5000000000</TOTALBYTES>
  </IMAGE>
  <IMAGE INDEX="2">
    <NAME>Windows Server 2022</NAME>
    <TOTALBYTES>8000000000</TOTALBYTES>
  </IMAGE>
</WIM>"#
            .to_string()
    }

    // ── Detection tests ───────────────────────────────────────────────────

    #[test]
    fn detect_valid_wim_ok() {
        let wim = build_wim(
            1,
            r#"<WIM><IMAGE INDEX="1"><NAME>Test</NAME></IMAGE></WIM>"#,
        );
        let mut c = Cursor::new(&wim);
        assert!(
            detect(&mut c).is_ok(),
            "detect() should succeed on a valid WIM"
        );
    }

    #[test]
    fn detect_restores_position() {
        let wim = build_wim(1, "<WIM></WIM>");
        let mut c = Cursor::new(&wim);
        c.seek(SeekFrom::Start(42)).unwrap();
        detect(&mut c).unwrap();
        assert_eq!(
            c.stream_position().unwrap(),
            42,
            "detect() must restore the stream position"
        );
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let data = vec![0u8; 256];
        let mut c = Cursor::new(&data);
        assert!(
            matches!(detect(&mut c), Err(Error::BadMagic)),
            "all-zeros buffer should fail with BadMagic"
        );
    }

    #[test]
    fn detect_rejects_too_short() {
        let data = vec![0u8; 64];
        let mut c = Cursor::new(&data);
        assert!(
            matches!(detect(&mut c), Err(Error::TooShort)),
            "64-byte image should fail with TooShort"
        );
    }

    // ── XML parser tests ──────────────────────────────────────────────────

    #[test]
    fn parse_xml_single_image() {
        let xml = xml_one_image("Windows 10 Pro", 1_234_567_890);
        let entries = parse_xml(&xml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].index, 1);
        assert_eq!(entries[0].name.as_deref(), Some("Windows 10 Pro"));
        assert_eq!(entries[0].total_bytes, Some(1_234_567_890));
    }

    #[test]
    fn parse_xml_two_images() {
        let xml = xml_two_images();
        let entries = parse_xml(&xml);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name.as_deref(), Some("Windows 10 Pro"));
        assert_eq!(entries[1].name.as_deref(), Some("Windows Server 2022"));
        assert_eq!(entries[0].total_bytes, Some(5_000_000_000));
        assert_eq!(entries[1].total_bytes, Some(8_000_000_000));
    }

    #[test]
    fn parse_xml_empty_wim() {
        let entries = parse_xml("<WIM></WIM>");
        assert!(
            entries.is_empty(),
            "empty WIM XML should produce no entries"
        );
    }

    // ── detect_and_parse tests ────────────────────────────────────────────

    #[test]
    fn parse_single_image_tree_shape() {
        let xml = xml_one_image("Windows 10 Pro", 5_000_000);
        let wim = build_wim(1, &xml);
        let mut c = Cursor::new(&wim);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");

        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "Windows 10 Pro");
        assert!(root.children[0].is_directory);
    }

    #[test]
    fn parse_two_image_tree_shape() {
        let xml = xml_two_images();
        let wim = build_wim(2, &xml);
        let mut c = Cursor::new(&wim);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");

        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "Windows 10 Pro");
        assert_eq!(root.children[1].name, "Windows Server 2022");
    }

    #[test]
    fn parse_fallback_name_when_no_name_tag() {
        let xml = r#"<WIM><IMAGE INDEX="1"><TOTALBYTES>100</TOTALBYTES></IMAGE></WIM>"#;
        let wim = build_wim(1, xml);
        let mut c = Cursor::new(&wim);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");
        assert_eq!(root.children[0].name, "Image 1");
    }

    #[test]
    fn parse_fallback_tree_when_xml_has_no_images() {
        // image_count=2 in header, but XML has no IMAGE elements.
        let wim = build_wim(2, "<WIM></WIM>");
        let mut c = Cursor::new(&wim);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "Image 1");
        assert_eq!(root.children[1].name, "Image 2");
    }

    #[test]
    fn parse_compressed_xml_returns_error() {
        let wim = build_wim_compressed_xml(1);
        let mut c = Cursor::new(&wim);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::Compressed)),
            "compressed XML resource should return Error::Compressed"
        );
    }

    #[test]
    fn utf16le_bom_stripped() {
        let s = "hello";
        let encoded = encode_utf16le(s);
        let decoded = utf16le_to_string(&encoded).unwrap();
        assert_eq!(decoded, s);
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        let msg = format!("{}", Error::TooShort);
        assert!(msg.contains("208") || msg.contains("short"), "got: {msg}");
    }

    #[test]
    fn error_display_bad_magic() {
        let msg = format!("{}", Error::BadMagic);
        assert!(msg.contains("MSWIM") || msg.contains("magic"), "got: {msg}");
    }

    #[test]
    fn error_display_compressed() {
        let msg = format!("{}", Error::Compressed);
        assert!(
            msg.contains("compress") || msg.contains("WIM"),
            "got: {msg}"
        );
    }

    #[test]
    fn error_display_bad_encoding() {
        let msg = format!("{}", Error::BadEncoding);
        assert!(
            msg.contains("UTF") || msg.contains("encoding"),
            "got: {msg}"
        );
    }

    #[test]
    fn error_display_io() {
        let io = io::Error::other("disk");
        let msg = format!("{}", Error::Io(io));
        assert!(msg.contains("disk"), "got: {msg}");
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        assert!(Error::Io(io::Error::other("s")).source().is_some());
    }

    #[test]
    fn error_source_non_io() {
        use std::error::Error as StdError;
        assert!(Error::TooShort.source().is_none());
        assert!(Error::BadMagic.source().is_none());
        assert!(Error::Compressed.source().is_none());
        assert!(Error::BadEncoding.source().is_none());
    }

    // ── From<io::Error> conversion ────────────────────────────────────────────

    #[test]
    fn error_from_io_error() {
        let io_err = io::Error::other("disk error");
        let err: Error = Error::from(io_err);
        assert!(matches!(err, Error::Io(_)));
    }

    // ── utf16le_to_string edge cases ──────────────────────────────────────────

    #[test]
    fn utf16le_to_string_odd_byte_count_returns_error() {
        let odd = vec![0x41u8, 0x00, 0x42]; // 3 bytes — not a multiple of 2
        assert!(matches!(utf16le_to_string(&odd), Err(Error::BadEncoding)));
    }

    // ── parse_attr_u32 edge cases ─────────────────────────────────────────────

    #[test]
    fn parse_attr_u32_single_quotes() {
        let text = "INDEX='3'";
        assert_eq!(parse_attr_u32(text, "INDEX"), Some(3));
    }

    #[test]
    fn parse_attr_u32_no_quote_returns_none() {
        let text = "INDEX=3"; // no surrounding quotes
        assert_eq!(parse_attr_u32(text, "INDEX"), None);
    }

    // ── parse_xml edge cases ──────────────────────────────────────────────────

    #[test]
    fn parse_xml_missing_close_image_tag() {
        // <IMAGE ...> with no </IMAGE> → parse_xml should return empty.
        let xml = r#"<WIM><IMAGE INDEX="1"><NAME>Orphan</NAME></WIM>"#;
        let entries = parse_xml(xml);
        assert!(
            entries.is_empty(),
            "unclosed IMAGE tag should yield no entries"
        );
    }

    // ── read_header edge cases ────────────────────────────────────────────────

    #[test]
    fn read_header_too_short_returns_too_short() {
        // A 10-byte buffer: read_exact will get UnexpectedEof.
        let data = vec![0u8; 10];
        let mut c = Cursor::new(data);
        assert!(matches!(read_header(&mut c), Err(Error::TooShort)));
    }

    #[test]
    fn read_header_wrong_magic_returns_bad_magic() {
        // Buffer is exactly HEADER_SIZE bytes but magic bytes are all zeros.
        let data = vec![0u8; HEADER_SIZE];
        let mut c = Cursor::new(data);
        assert!(matches!(read_header(&mut c), Err(Error::BadMagic)));
    }

    // ── detect_and_parse xml size guard ──────────────────────────────────────

    #[test]
    fn detect_and_parse_xml_original_size_exceeds_max() {
        // Build a WIM whose xml_data.original_size is absurdly large.
        let mut wim = build_wim(1, "<WIM></WIM>");
        // original_size is at bytes 88..96 of the header (CBOriginal in RESHDR).
        let huge: u64 = 128 * 1024 * 1024; // 128 MiB > MAX_XML_SIZE (64 MiB)
        wim[88..96].copy_from_slice(&huge.to_le_bytes());
        let mut c = Cursor::new(wim);
        assert!(matches!(detect_and_parse(&mut c), Err(Error::TooShort)));
    }
}
