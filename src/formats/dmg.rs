//! DMG (Apple Disk Image / UDIF — Universal Disk Image Format) reader
//! (`dmg` feature).
//!
//! Apple's DMG format wraps one or more partitions (typically HFS+ or
//! APFS) inside a flat file. The format is identified by a 512-byte
//! "koly" trailer at the very end of the file, followed by an XML
//! Apple plist that describes the partition map ("blkx" entries).
//!
//! This reader does **not** decompress or decode the binary Mish/blkx
//! extent data that sits inside the `<data>` base64 blobs in the plist.
//! It reads:
//!
//! - The 512-byte koly trailer to detect the format and extract the
//!   sector count.
//! - The UTF-8 XML plist to extract partition names from `<key>CFName</key>`
//!   or `<key>Name</key>` entries inside the `blkx` array.
//! - Returns a [`TreeNode`] tree with one directory child per blkx entry.
//!
//! If the XML plist is missing or has no `blkx` entries we fall back to
//! a synthetic child `"disk.dmg"` with `file_length` set to
//! `sector_count * 512`.
//!
//! ## koly Trailer layout (big-endian, 512 bytes at file_end − 512)
//!
//! ```text
//! [0]   u8[4]   signature           = b"koly"
//! [4]   u32     version             = 4
//! [8]   u32     header_size         = 512
//! [12]  u32     flags
//! [16]  u64     running_data_fork_offset
//! [24]  u64     data_fork_offset
//! [32]  u64     data_fork_length
//! [40]  u64     rsrc_fork_offset
//! [48]  u64     rsrc_fork_length
//! [56]  u32     segment_number
//! [60]  u32     segment_count
//! [64]  u8[16]  segment_id          (UUID)
//! [80]  u32     data_fork_digest_type
//! [84]  u8[128] data_fork_digest
//! [212] u8[4]   reserved
//! [216] u64     xml_offset
//! [224] u64     xml_length
//! [232] u8[120] reserved1
//! [348] u32     checksum_type
//! [352] u8[136] master_checksum
//! [488] u32     image_variant       // 1=RO, 2=RW, 3=ROC
//! [492] u64     sector_count
//! [500] u8[12]  reserved2
//! ```
//!
//! All fields are big-endian.

use std::io::{self, Read, Seek, SeekFrom};

use crate::tree::TreeNode;

// ── Magic & constants ──────────────────────────────────────────────────────────

/// DMG koly trailer magic bytes.
const KOLY_MAGIC: &[u8; 4] = b"koly";

/// Size of the koly trailer in bytes.
const KOLY_SIZE: usize = 512;

/// Expected koly version.
const KOLY_VERSION: u32 = 4;

// ── Error type ─────────────────────────────────────────────────────────────────

/// Reasons [`detect`] or [`detect_and_parse`] can fail.
#[derive(Debug)]
pub enum Error {
    /// File is shorter than the 512-byte koly trailer.
    TooShort,
    /// The 4-byte magic at the start of the koly trailer is not `b"koly"`.
    BadMagic,
    /// The koly version field is not 4.
    BadVersion(u32),
    /// Underlying I/O error.
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "DMG file is shorter than the 512-byte koly trailer"),
            Error::BadMagic => write!(f, "DMG koly magic b\"koly\" not found at file end − 512"),
            Error::BadVersion(v) => write!(f, "DMG koly version {v} is not supported (expected 4)"),
            Error::Io(e) => write!(f, "DMG I/O error: {e}"),
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

// ── Parsed koly trailer ────────────────────────────────────────────────────────

/// Parsed fields from the 512-byte koly trailer.
struct Koly {
    /// Byte offset of the XML plist within the file.
    xml_offset: u64,
    /// Byte length of the XML plist.
    xml_length: u64,
    /// Total sector count of the disk image (512-byte sectors).
    sector_count: u64,
}

// ── koly reader ────────────────────────────────────────────────────────────────

/// Read and validate the 512-byte koly trailer from the end of `r`.
///
/// Does **not** restore the stream position; callers should save/restore.
fn read_koly<R: Read + Seek>(r: &mut R) -> Result<Koly, Error> {
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < KOLY_SIZE as u64 {
        return Err(Error::TooShort);
    }

    r.seek(SeekFrom::Start(file_len - KOLY_SIZE as u64))?;

    let mut buf = [0u8; KOLY_SIZE];
    match r.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    // Check magic.
    if &buf[0..4] != KOLY_MAGIC {
        return Err(Error::BadMagic);
    }

    // Check version (big-endian u32 at offset 4).
    let version = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if version != KOLY_VERSION {
        return Err(Error::BadVersion(version));
    }

    // xml_offset at koly offset 216, xml_length at koly offset 224 (big-endian u64).
    //
    // The koly layout through offset 216:
    //   [0]   magic      (4 bytes)
    //   [4]   version    (4 bytes)
    //   [8]   header_size (4 bytes)
    //   [12]  flags       (4 bytes)
    //   [16]  running_data_fork_offset (8 bytes)
    //   [24]  data_fork_offset         (8 bytes)
    //   [32]  data_fork_length         (8 bytes)
    //   [40]  rsrc_fork_offset         (8 bytes)
    //   [48]  rsrc_fork_length         (8 bytes)
    //   [56]  segment_number           (4 bytes)
    //   [60]  segment_count            (4 bytes)
    //   [64]  segment_id               (16 bytes)
    //   [80]  data_fork_digest_type    (4 bytes)
    //   [84]  data_fork_digest         (128 bytes) ← ends at 212
    //   [212] reserved                 (4 bytes)   ← NOTE: 4-byte reserved, not xml_offset
    //   [216] xml_offset               (8 bytes)
    //   [224] xml_length               (8 bytes)
    let xml_offset = u64::from_be_bytes([
        buf[216], buf[217], buf[218], buf[219], buf[220], buf[221], buf[222], buf[223],
    ]);
    let xml_length = u64::from_be_bytes([
        buf[224], buf[225], buf[226], buf[227], buf[228], buf[229], buf[230], buf[231],
    ]);

    // sector_count at offset 492 (big-endian u64).
    let sector_count = u64::from_be_bytes([
        buf[492], buf[493], buf[494], buf[495], buf[496], buf[497], buf[498], buf[499],
    ]);

    Ok(Koly {
        xml_offset,
        xml_length,
        sector_count,
    })
}

// ── Detection ──────────────────────────────────────────────────────────────────

/// Detect whether `r` is a DMG file by checking the koly trailer magic.
///
/// Seeks to `file_end − 512`, reads 4 bytes, checks they equal `b"koly"`,
/// then restores the stream position on both success and error paths.
///
/// Returns `Ok(())` on success.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let pos = r.stream_position()?;
    let result = detect_inner(r);
    let _ = r.seek(SeekFrom::Start(pos));
    result
}

fn detect_inner<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let file_len = r.seek(SeekFrom::End(0))?;
    if file_len < KOLY_SIZE as u64 {
        return Err(Error::TooShort);
    }

    r.seek(SeekFrom::Start(file_len - KOLY_SIZE as u64))?;
    let mut magic = [0u8; 4];
    match r.read_exact(&mut magic) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }

    if &magic != KOLY_MAGIC {
        return Err(Error::BadMagic);
    }
    Ok(())
}

// ── XML plist scanner ──────────────────────────────────────────────────────────

/// A single blkx partition entry extracted from the XML plist.
#[derive(Debug)]
struct BlkxEntry {
    /// Partition name from `<key>CFName</key>` or `<key>Name</key>`.
    name: String,
}

/// Minimal UTF-8 XML scanner for the Apple plist blkx partition list.
///
/// We do not pull in an XML parser dep. The plist schema is regular
/// enough that a simple linear scan works:
///
/// 1. Find `<key>blkx</key>` then `<array>` to locate the partition list.
/// 2. Within each `<dict>` in the array, find `<key>CFName</key>` or
///    `<key>Name</key>` followed by `<string>…</string>`.
/// 3. Return one [`BlkxEntry`] per dict.
///
/// `CFName` is preferred over `Name` when both are present, matching
/// what macOS `hdiutil` reports.
fn parse_plist_xml(xml: &str) -> Vec<BlkxEntry> {
    let mut entries = Vec::new();

    // Find the blkx array.
    let blkx_key = "<key>blkx</key>";
    let blkx_pos = match xml.find(blkx_key) {
        Some(p) => p + blkx_key.len(),
        None => return entries,
    };

    let array_open = "<array>";
    let array_close = "</array>";
    let array_start = match xml[blkx_pos..].find(array_open) {
        Some(p) => blkx_pos + p + array_open.len(),
        None => return entries,
    };
    let array_end = match xml[array_start..].find(array_close) {
        Some(p) => array_start + p,
        None => return entries,
    };

    let array_body = &xml[array_start..array_end];

    // Iterate over <dict>…</dict> blocks within the array.
    let mut pos = 0;
    while let Some(dict_rel) = array_body[pos..].find("<dict>") {
        let dict_start = pos + dict_rel + "<dict>".len();
        let dict_end = match array_body[dict_start..].find("</dict>") {
            Some(e) => dict_start + e,
            None => break,
        };
        let dict_body = &array_body[dict_start..dict_end];

        // Extract CFName or Name from this dict.
        let cf_name = extract_keyed_string(dict_body, "CFName");
        let plain_name = extract_keyed_string(dict_body, "Name");
        let name = cf_name.or(plain_name).unwrap_or_default();

        if !name.is_empty() {
            entries.push(BlkxEntry { name });
        }

        pos = dict_end + "</dict>".len();
    }

    entries
}

/// Find `<key>KEY</key>` followed by `<string>VALUE</string>` and
/// return `VALUE`, or `None` if the pattern is absent.
fn extract_keyed_string(text: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let key_pos = text.find(&key_tag)? + key_tag.len();
    let rest = &text[key_pos..];
    let string_open = "<string>";
    let string_close = "</string>";
    let val_start = rest.find(string_open)? + string_open.len();
    let val_end = rest[val_start..].find(string_close)? + val_start;
    Some(rest[val_start..val_end].trim().to_string())
}

// ── Tree building ──────────────────────────────────────────────────────────────

/// Parse the DMG at `r` and return a [`TreeNode`] tree.
///
/// The tree shape when blkx entries are present is:
///
/// ```text
/// / (dir)
/// ├─ "Apple_HFS : macOS"   (dir)
/// ├─ "Driver Descriptor Map" (dir)
/// └─ "Apple_partition_map" (dir)
/// ```
///
/// When the XML plist is empty or has no blkx entries a single
/// synthetic child is returned:
///
/// ```text
/// / (dir)
/// └─ "disk.dmg"  (file, file_length = sector_count × 512)
/// ```
///
/// `file_location` is always `None` for directory children — decoding
/// the binary Mish/blkx extent data is not done here.
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let koly = read_koly(r)?;

    // Attempt to read the XML plist.
    //
    // Guard: xml_length must be within the file and below a sanity cap
    // (32 MiB) so a corrupt koly cannot trigger a multi-GB allocation.
    let file_len = r.seek(SeekFrom::End(0))?;
    let xml_sane = koly.xml_length > 0
        && koly.xml_length <= 32 * 1024 * 1024
        && koly.xml_offset < file_len
        && koly.xml_offset + koly.xml_length <= file_len;

    let xml_text = if xml_sane {
        r.seek(SeekFrom::Start(koly.xml_offset))?;
        let read_len = koly.xml_length as usize;
        let mut raw = vec![0u8; read_len];
        match r.read_exact(&mut raw) {
            Ok(()) => String::from_utf8_lossy(&raw).into_owned(),
            Err(_) => String::new(),
        }
    } else {
        String::new()
    };

    let entries = parse_plist_xml(&xml_text);

    let mut root = TreeNode::new_directory("/".to_string());

    if entries.is_empty() {
        // Fallback: synthetic disk node with byte size.
        let disk_size = koly.sector_count.saturating_mul(512);
        let mut disk_node = TreeNode::new_file("disk.dmg".to_string(), disk_size);
        disk_node.file_length = Some(disk_size);
        root.add_child(disk_node);
    } else {
        for entry in &entries {
            let node = TreeNode::new_directory(entry.name.clone());
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

    // ── koly builder helper ────────────────────────────────────────────────

    /// Build a minimal buffer that ends with a valid koly trailer.
    ///
    /// `prefix_len` bytes of zeros are prepended so the total buffer
    /// length is `prefix_len + KOLY_SIZE`. The xml plist (if provided)
    /// is placed at `prefix_len` minus `xml.len()` so the xml sits
    /// immediately before the koly and the xml_offset/xml_length fields
    /// are set accordingly.
    ///
    /// If `xml` is empty the xml_offset and xml_length fields are both 0.
    fn build_dmg(prefix_len: usize, xml: &str, sector_count: u64) -> Vec<u8> {
        let xml_bytes = xml.as_bytes();
        let xml_len = xml_bytes.len();

        // Layout: [padding ... ] [xml?] [koly 512 bytes]
        // xml sits at offset prefix_len.
        let total = prefix_len + xml_len + KOLY_SIZE;
        let mut buf = vec![0u8; total];

        // Place xml immediately before koly.
        let xml_start = prefix_len;
        buf[xml_start..xml_start + xml_len].copy_from_slice(xml_bytes);

        // Build koly at the end.
        let koly_start = total - KOLY_SIZE;

        // magic
        buf[koly_start..koly_start + 4].copy_from_slice(KOLY_MAGIC);

        // version = 4 (big-endian u32)
        buf[koly_start + 4..koly_start + 8].copy_from_slice(&4u32.to_be_bytes());

        // header_size = 512 (big-endian u32)
        buf[koly_start + 8..koly_start + 12].copy_from_slice(&512u32.to_be_bytes());

        // xml_offset and xml_length (big-endian u64) at koly offsets 216 and 224.
        if xml_len > 0 {
            let xml_offset = xml_start as u64;
            buf[koly_start + 216..koly_start + 224].copy_from_slice(&xml_offset.to_be_bytes());
            buf[koly_start + 224..koly_start + 232]
                .copy_from_slice(&(xml_len as u64).to_be_bytes());
        }
        // else leave as zero (xml_length = 0 → skip reading).

        // sector_count (big-endian u64) at koly offset 492.
        buf[koly_start + 492..koly_start + 500].copy_from_slice(&sector_count.to_be_bytes());

        buf
    }

    // ── Detection tests ────────────────────────────────────────────────────

    #[test]
    fn detect_valid_dmg_ok() {
        let dmg = build_dmg(0, "", 2048);
        let mut c = Cursor::new(&dmg);
        assert!(
            detect(&mut c).is_ok(),
            "detect() should succeed on a valid koly trailer"
        );
    }

    #[test]
    fn detect_restores_position() {
        let dmg = build_dmg(0, "", 2048);
        let mut c = Cursor::new(&dmg);
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
        let data = vec![0u8; 1024];
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
            "64-byte buffer should fail with TooShort"
        );
    }

    #[test]
    fn detect_rejects_bad_version() {
        // Build a koly trailer with version=99.
        let mut buf = vec![0u8; KOLY_SIZE];
        buf[0..4].copy_from_slice(KOLY_MAGIC);
        buf[4..8].copy_from_slice(&99u32.to_be_bytes());
        let mut c = Cursor::new(&buf);
        // detect() only checks the magic, not the version.
        // read_koly() checks the version → detect_and_parse will return BadVersion.
        assert!(
            detect(&mut c).is_ok(),
            "detect() checks only the magic, not the version"
        );
        let mut c2 = Cursor::new(&buf);
        assert!(
            matches!(detect_and_parse(&mut c2), Err(Error::BadVersion(99))),
            "detect_and_parse() should return BadVersion(99) for version 99"
        );
    }

    // ── XML plist parser tests ─────────────────────────────────────────────

    #[test]
    fn parse_plist_no_blkx() {
        let xml = r#"<?xml version="1.0"?><plist version="1.0"><dict></dict></plist>"#;
        let entries = parse_plist_xml(xml);
        assert!(entries.is_empty(), "no blkx key → empty entries");
    }

    #[test]
    fn parse_plist_single_partition_cfname() {
        let xml = r#"<plist><dict>
  <key>resource-fork</key>
  <dict>
    <key>blkx</key>
    <array>
      <dict>
        <key>CFName</key><string>Apple_HFS : macOS</string>
        <key>Name</key><string>Driver Descriptor Map</string>
        <key>Data</key><data>AAAA</data>
      </dict>
    </array>
  </dict>
</dict></plist>"#;
        let entries = parse_plist_xml(xml);
        assert_eq!(entries.len(), 1);
        // CFName is preferred over Name.
        assert_eq!(entries[0].name, "Apple_HFS : macOS");
    }

    #[test]
    fn parse_plist_multiple_partitions() {
        let xml = r#"<plist><dict>
  <key>resource-fork</key>
  <dict>
    <key>blkx</key>
    <array>
      <dict><key>CFName</key><string>Driver Descriptor Map</string></dict>
      <dict><key>CFName</key><string>Apple_partition_map</string></dict>
      <dict><key>Name</key><string>Apple_HFS : macOS</string></dict>
    </array>
  </dict>
</dict></plist>"#;
        let entries = parse_plist_xml(xml);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "Driver Descriptor Map");
        assert_eq!(entries[1].name, "Apple_partition_map");
        assert_eq!(entries[2].name, "Apple_HFS : macOS");
    }

    #[test]
    fn parse_plist_falls_back_to_name_when_no_cfname() {
        let xml = r#"<plist><dict>
  <key>resource-fork</key><dict>
  <key>blkx</key><array>
    <dict><key>Name</key><string>free</string></dict>
  </array></dict></dict></plist>"#;
        let entries = parse_plist_xml(xml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "free");
    }

    // ── detect_and_parse tests ─────────────────────────────────────────────

    #[test]
    fn parse_no_xml_returns_synthetic_disk_node() {
        // sector_count = 4096 → file_length = 4096 * 512 = 2 097 152
        let dmg = build_dmg(0, "", 4096);
        let mut c = Cursor::new(&dmg);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");

        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "disk.dmg");
        assert!(!root.children[0].is_directory);
        assert_eq!(root.children[0].file_length, Some(4096 * 512));
    }

    #[test]
    fn parse_xml_with_partitions_returns_directory_children() {
        let xml = r#"<plist><dict>
  <key>resource-fork</key><dict>
  <key>blkx</key><array>
    <dict><key>CFName</key><string>Apple_HFS : macOS</string></dict>
    <dict><key>CFName</key><string>free</string></dict>
  </array></dict></dict></plist>"#;
        let dmg = build_dmg(512, xml, 8192);
        let mut c = Cursor::new(&dmg);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");

        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].name, "Apple_HFS : macOS");
        assert!(root.children[0].is_directory);
        assert_eq!(root.children[1].name, "free");
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut buf = vec![0u8; KOLY_SIZE];
        buf[0..4].copy_from_slice(b"XXXX");
        let mut c = Cursor::new(&buf);
        assert!(
            matches!(detect_and_parse(&mut c), Err(Error::BadMagic)),
            "bad magic should return Error::BadMagic"
        );
    }

    #[test]
    fn koly_sector_count_read_correctly() {
        let sector_count: u64 = 123_456;
        let dmg = build_dmg(0, "", sector_count);
        let mut c = Cursor::new(&dmg);
        let root = detect_and_parse(&mut c).expect("detect_and_parse should succeed");
        // With no XML, we get disk.dmg with file_length = sector_count * 512.
        assert_eq!(root.children[0].file_length, Some(sector_count * 512));
    }
}
