//! ISO 9660 / ECMA-119 parser, with the Joliet (Unicode filenames) and
//! Rock Ridge (POSIX long filenames) extensions.
//!
//! The entry points are [`parse_iso9660`] and [`parse_iso9660_verbose`].
//! Both return a [`crate::TreeNode`] tree rooted at `"/"` on success.

use crate::tree::TreeNode;
use crate::Result;
// `File` is no longer mentioned by the parser; entry points are
// generic over `R: Read + Seek` as of v3.0. Keeping the imports
// minimal matches the rest of the crate's style.
use std::io::{Read, Seek, SeekFrom};

const SECTOR_SIZE: u64 = 2048;
const PRIMARY_VOLUME_DESCRIPTOR_SECTOR: u64 = 16;

#[derive(Debug, Clone)]
pub struct DirectoryRecord {
    pub extent_location: u32,
    pub data_length: u32,
    pub is_directory: bool,
    pub filename: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VolumeDescriptorType {
    Primary,
    Joliet,
}

/// Parse an ISO 9660 image, returning the root of the directory tree.
///
/// Equivalent to `parse_iso9660_verbose(file, false)`. Errors out cleanly
/// (returns `Err`, never panics) on images whose volume descriptors don't
/// validate.
pub fn parse_iso9660<R: Read + Seek>(file: &mut R) -> Result<TreeNode> {
    parse_iso9660_verbose(file, false)
}

/// Like [`parse_iso9660`], but prints spec-section-tagged diagnostics to
/// stderr while parsing. Useful for investigating images that fail.
///
/// As of v3.0 this takes `&mut (impl Read + Seek)` rather than
/// `&mut File`, so consumers can feed it an `MmapImage`, a
/// `Cursor<Vec<u8>>`, or any other byte-source that implements the
/// trait pair.
pub fn parse_iso9660_verbose<R: Read + Seek>(file: &mut R, verbose: bool) -> Result<TreeNode> {
    // Scan all volume descriptors to find Primary and Joliet
    let mut primary_vd: Option<Vec<u8>> = None;
    let mut joliet_vd: Option<Vec<u8>> = None;

    let mut sector = PRIMARY_VOLUME_DESCRIPTOR_SECTOR;
    loop {
        file.seek(SeekFrom::Start(sector * SECTOR_SIZE))?;
        let mut buffer = vec![0u8; SECTOR_SIZE as usize];
        if file.read_exact(&mut buffer).is_err() {
            break;
        }

        // Check for ISO 9660 signature
        if &buffer[1..6] != b"CD001" {
            if sector == PRIMARY_VOLUME_DESCRIPTOR_SECTOR {
                if verbose {
                    eprintln!(
                        "  ISO 9660 signature 'CD001' not found at sector {}. Found: {:?}",
                        sector,
                        String::from_utf8_lossy(&buffer[1..6])
                    );
                }
                return Err("Not a valid ISO 9660 filesystem".into());
            }
            break;
        }

        let vd_type = buffer[0];
        match vd_type {
            1 => {
                if verbose {
                    eprintln!("  Found Primary Volume Descriptor at sector {}", sector);
                }
                primary_vd = Some(buffer);
            }
            2 => {
                // Supplementary Volume Descriptor — check if Joliet
                // Joliet is indicated by escape sequences in bytes 88-90
                let escape = &buffer[88..91];
                if escape == b"%/@" || escape == b"%/C" || escape == b"%/E" {
                    if verbose {
                        eprintln!("  Found Joliet Volume Descriptor at sector {}", sector);
                    }
                    joliet_vd = Some(buffer);
                }
            }
            255 => {
                if verbose {
                    eprintln!("  Volume Descriptor Set Terminator at sector {}", sector);
                }
                break;
            }
            _ => {}
        }
        sector += 1;
    }

    // Prefer Joliet (Unicode filenames) over Primary
    let (buffer, vd_type) = if let Some(buf) = joliet_vd {
        (buf, VolumeDescriptorType::Joliet)
    } else if let Some(buf) = primary_vd {
        (buf, VolumeDescriptorType::Primary)
    } else {
        return Err("Not a valid ISO 9660 filesystem".into());
    };

    if verbose {
        eprintln!(
            "  Using {} Volume Descriptor",
            if vd_type == VolumeDescriptorType::Joliet {
                "Joliet"
            } else {
                "Primary"
            }
        );
    }

    // Parse root directory record (starts at offset 156)
    let root_record = parse_directory_record(&buffer[156..], vd_type)?;
    if verbose {
        eprintln!(
            "  Root directory at sector {}, size {} bytes",
            root_record.extent_location, root_record.data_length
        );
    }

    // Check for Rock Ridge (we'll detect it when parsing the root directory)
    let mut root_node = TreeNode::new_directory("/".to_string());
    let use_rock_ridge = if vd_type == VolumeDescriptorType::Primary {
        detect_rock_ridge(file, &root_record)?
    } else {
        false
    };
    if verbose && use_rock_ridge {
        eprintln!("  Rock Ridge extensions detected");
    }

    parse_directory(
        file,
        &root_record,
        &mut root_node,
        vd_type,
        use_rock_ridge,
        verbose,
    )?;

    root_node.calculate_directory_size();
    Ok(root_node)
}

fn detect_rock_ridge<R: Read + Seek>(file: &mut R, dir_record: &DirectoryRecord) -> Result<bool> {
    file.seek(SeekFrom::Start(
        dir_record.extent_location as u64 * SECTOR_SIZE,
    ))?;
    let mut buffer = vec![0u8; dir_record.data_length.min(4096) as usize];
    file.read_exact(&mut buffer)?;

    // Look at the first directory record's system use area for Rock Ridge signatures
    if buffer.len() < 34 {
        return Ok(false);
    }
    let record_length = buffer[0] as usize;
    let filename_length = buffer[32] as usize;
    // System use area starts after filename + padding
    let su_start = 33 + filename_length + ((filename_length + 1) % 2);
    if su_start + 7 <= record_length && record_length <= buffer.len() {
        // Check for "SP" (SUSP indicator) or "RR" signature
        let sig = &buffer[su_start..su_start + 2];
        if sig == b"SP" || sig == b"RR" {
            return Ok(true);
        }
        // Also check for "NM" (alternate name) or "PX" (POSIX attributes)
        if sig == b"NM" || sig == b"PX" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn parse_directory_record(data: &[u8], vd_type: VolumeDescriptorType) -> Result<DirectoryRecord> {
    if data.len() < 34 {
        return Err("Directory record too short".into());
    }

    let length = data[0];
    if length == 0 {
        return Err("Zero-length directory record".into());
    }

    let extent_location = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let data_length = u32::from_le_bytes([data[10], data[11], data[12], data[13]]);
    let file_flags = data[25];
    let filename_length = data[32] as usize;

    // Ensure data[33..33+filename_length] is in bounds
    if 33 + filename_length > data.len() {
        return Err("Directory record filename extends past buffer".into());
    }

    let is_directory = (file_flags & 0x02) != 0;

    let filename = if filename_length == 0 || (filename_length == 1 && data[33] == 0) {
        // ECMA-119 7.6.12: a single 0x00 byte is the special "." (current)
        // directory entry; an empty filename is also treated as "." here.
        ".".to_string()
    } else if filename_length == 1 && data[33] == 1 {
        // ECMA-119 7.6.12: a single 0x01 byte is the special ".." (parent)
        // directory entry.
        "..".to_string()
    } else if vd_type == VolumeDescriptorType::Joliet {
        // Joliet uses UCS-2 big-endian encoding
        let utf16_data: Vec<u16> = data[33..33 + filename_length]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        let raw_name = String::from_utf16_lossy(&utf16_data);
        // Remove ISO 9660 version suffix (;1, ;2, etc.)
        if let Some(semicolon_pos) = raw_name.find(';') {
            raw_name[..semicolon_pos].to_string()
        } else {
            raw_name
        }
    } else {
        let raw_name = String::from_utf8_lossy(&data[33..33 + filename_length]);
        // Remove ISO 9660 version suffix (;1, ;2, etc.) and trailing periods
        let cleaned_name = if let Some(semicolon_pos) = raw_name.find(';') {
            &raw_name[..semicolon_pos]
        } else {
            &raw_name
        };
        cleaned_name.trim_end_matches('.').to_string()
    };

    Ok(DirectoryRecord {
        extent_location,
        data_length,
        is_directory,
        filename,
    })
}

fn extract_rock_ridge_name(
    data: &[u8],
    record_length: usize,
    filename_length: usize,
) -> Option<String> {
    // System use area starts after the filename + padding byte for even alignment
    let su_start = 33 + filename_length + ((filename_length + 1) % 2);
    if su_start >= record_length {
        return None;
    }

    let su_area = &data[su_start..record_length];
    let mut offset = 0;
    let mut name_parts: Vec<u8> = Vec::new();

    while offset + 4 <= su_area.len() {
        let sig = &su_area[offset..offset + 2];
        let entry_len = su_area[offset + 2] as usize;
        if entry_len < 4 || offset + entry_len > su_area.len() {
            break;
        }

        if sig == b"NM" {
            // Rock Ridge Alternate Name entry
            // byte 3 = version, byte 4 = flags
            let flags = su_area[offset + 4];
            if flags & 0x02 != 0 {
                // CURRENT (.) - skip
            } else if flags & 0x04 != 0 {
                // PARENT (..) - skip
            } else {
                name_parts.extend_from_slice(&su_area[offset + 5..offset + entry_len]);
            }
        }

        offset += entry_len;
    }

    if name_parts.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(&name_parts).to_string())
    }
}

fn parse_directory<R: Read + Seek>(
    file: &mut R,
    dir_record: &DirectoryRecord,
    parent_node: &mut TreeNode,
    vd_type: VolumeDescriptorType,
    use_rock_ridge: bool,
    verbose: bool,
) -> Result<()> {
    if !dir_record.is_directory || dir_record.data_length == 0 {
        return Ok(());
    }

    file.seek(SeekFrom::Start(
        dir_record.extent_location as u64 * SECTOR_SIZE,
    ))?;

    let mut buffer = vec![0u8; dir_record.data_length as usize];
    file.read_exact(&mut buffer)?;

    let mut offset = 0;
    while offset < buffer.len() {
        if buffer[offset] == 0 {
            // Skip to next sector boundary (padding at end of sector)
            let next_sector = (offset / SECTOR_SIZE as usize + 1) * SECTOR_SIZE as usize;
            if next_sector <= offset {
                offset += 1;
            } else {
                offset = next_sector;
            }
            continue;
        }

        let record_length = buffer[offset] as usize;
        if record_length == 0 || offset + record_length > buffer.len() {
            break;
        }

        if let Ok(mut record) = parse_directory_record(&buffer[offset..], vd_type) {
            // Try Rock Ridge alternate name
            if use_rock_ridge
                && vd_type == VolumeDescriptorType::Primary
                && record.filename != "."
                && record.filename != ".."
            {
                let filename_length = buffer[offset + 32] as usize;
                if let Some(rr_name) = extract_rock_ridge_name(
                    &buffer[offset..offset + record_length],
                    record_length,
                    filename_length,
                ) {
                    record.filename = rr_name;
                }
            }

            // Skip "." and ".." entries
            if record.filename != "." && record.filename != ".." {
                if verbose {
                    eprintln!(
                        "    Found {}: {}",
                        if record.is_directory { "dir" } else { "file" },
                        record.filename
                    );
                }
                if record.is_directory {
                    let mut dir_node = TreeNode::new_directory(record.filename.clone());
                    parse_directory(
                        file,
                        &record,
                        &mut dir_node,
                        vd_type,
                        use_rock_ridge,
                        verbose,
                    )?;
                    parent_node.add_child(dir_node);
                } else {
                    let file_node = TreeNode::new_file_with_location(
                        record.filename.clone(),
                        record.data_length as u64,
                        record.extent_location as u64 * SECTOR_SIZE,
                        record.data_length as u64,
                    );
                    parent_node.add_child(file_node);
                }
            }
        }

        offset += record_length;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const S: usize = 2048; // sector size

    /// Build a minimal ISO 9660 Primary-only image (no Joliet).
    /// Puts one file "HELLO.TXT" in the root directory.
    fn make_iso_primary_only() -> Vec<u8> {
        let mut img = vec![0u8; S * 20];

        // PVD at sector 16 (type=1, magic=CD001)
        let pvd = 16 * S;
        img[pvd] = 1; // VD type: Primary
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        img[pvd + 6] = 1; // version

        // Root directory record at PVD offset 156 (33 bytes minimum)
        // Points root dir to sector 18, size = 2048
        let root_off = pvd + 156;
        img[root_off] = 34; // record length
        img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes()); // extent LE
        img[root_off + 6..root_off + 10].copy_from_slice(&18u32.to_be_bytes()); // extent BE
        img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes()); // size LE
        img[root_off + 14..root_off + 18].copy_from_slice(&(S as u32).to_be_bytes()); // size BE
        img[root_off + 25] = 0x02; // file flags: directory
        img[root_off + 32] = 1; // filename length = 1
        img[root_off + 33] = 0; // filename = 0x00 → "." (current dir)

        // Volume Descriptor Set Terminator at sector 17
        let vdst = 17 * S;
        img[vdst] = 255; // VD type: terminator
        img[vdst + 1..vdst + 6].copy_from_slice(b"CD001");

        // Root directory data at sector 18
        let dir = 18 * S;
        // Entry 0: "." (self)
        img[dir] = 34;
        img[dir + 2..dir + 6].copy_from_slice(&18u32.to_le_bytes());
        img[dir + 10..dir + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[dir + 25] = 0x02; // directory
        img[dir + 32] = 1;
        img[dir + 33] = 0; // 0x00 = "."

        // Entry 1: ".." (parent)
        let e1 = dir + 34;
        img[e1] = 34;
        img[e1 + 2..e1 + 6].copy_from_slice(&18u32.to_le_bytes());
        img[e1 + 10..e1 + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[e1 + 25] = 0x02;
        img[e1 + 32] = 1;
        img[e1 + 33] = 1; // 0x01 = ".."

        // Entry 2: "HELLO.TXT;1" regular file at sector 19, size 11
        let e2 = e1 + 34;
        let name = b"HELLO.TXT;1";
        img[e2] = (33 + name.len()) as u8; // record length
        img[e2 + 2..e2 + 6].copy_from_slice(&19u32.to_le_bytes()); // extent
        img[e2 + 10..e2 + 14].copy_from_slice(&11u32.to_le_bytes()); // size
        img[e2 + 25] = 0x00; // file
        img[e2 + 32] = name.len() as u8;
        img[e2 + 33..e2 + 33 + name.len()].copy_from_slice(name);

        // File data at sector 19
        img[19 * S..19 * S + 11].copy_from_slice(b"Hello World");

        img
    }

    /// Minimal ISO with a Joliet SVD in addition to PVD.
    fn make_iso_joliet() -> Vec<u8> {
        let mut img = make_iso_primary_only();
        img.resize(S * 22, 0);

        // Joliet SVD at sector 17 (before VDST which we push to 18)
        let svd = 17 * S;
        img[svd] = 2; // VD type: Supplementary
        img[svd + 1..svd + 6].copy_from_slice(b"CD001");
        img[svd + 6] = 1;
        // Joliet escape: %/@ at bytes 88-90
        img[svd + 88..svd + 91].copy_from_slice(b"%/@");

        // Root record in Joliet SVD (pointing to sector 20)
        let jroot = svd + 156;
        img[jroot] = 34;
        img[jroot + 2..jroot + 6].copy_from_slice(&20u32.to_le_bytes());
        img[jroot + 10..jroot + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[jroot + 25] = 0x02; // directory
        img[jroot + 32] = 1;
        img[jroot + 33] = 0; // "."

        // Move VDST to sector 18
        let vdst = 18 * S;
        img[vdst] = 255;
        img[vdst + 1..vdst + 6].copy_from_slice(b"CD001");

        // Joliet directory at sector 20
        let jdir = 20 * S;
        // "." entry
        img[jdir] = 34;
        img[jdir + 2..jdir + 6].copy_from_slice(&20u32.to_le_bytes());
        img[jdir + 10..jdir + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[jdir + 25] = 0x02;
        img[jdir + 32] = 1;
        img[jdir + 33] = 0;
        // ".." entry
        let je1 = jdir + 34;
        img[je1] = 34;
        img[je1 + 2..je1 + 6].copy_from_slice(&20u32.to_le_bytes());
        img[je1 + 10..je1 + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[je1 + 25] = 0x02;
        img[je1 + 32] = 1;
        img[je1 + 33] = 1;
        // "hi.txt" in Joliet (UCS-2 BE): h=0x0068, i=0x0069, .=0x002E, t=0x0074, x=0x0078, t=0x0074
        let joliet_name: Vec<u8> = "hi.txt"
            .encode_utf16()
            .flat_map(|c| c.to_be_bytes())
            .collect();
        let je2 = je1 + 34;
        img[je2] = (33 + joliet_name.len()) as u8;
        img[je2 + 2..je2 + 6].copy_from_slice(&19u32.to_le_bytes()); // same file data
        img[je2 + 10..je2 + 14].copy_from_slice(&11u32.to_le_bytes());
        img[je2 + 25] = 0x00; // file
        img[je2 + 32] = joliet_name.len() as u8;
        img[je2 + 33..je2 + 33 + joliet_name.len()].copy_from_slice(&joliet_name);

        img
    }

    // ── parse_directory_record ────────────────────────────────────────────────

    #[test]
    fn directory_record_too_short_errors() {
        let buf = [0u8; 10]; // less than 34 bytes
        assert!(parse_directory_record(&buf, VolumeDescriptorType::Primary).is_err());
    }

    #[test]
    fn directory_record_zero_length_errors() {
        let mut buf = [0u8; 40];
        buf[0] = 0; // length = 0 → error
        assert!(parse_directory_record(&buf, VolumeDescriptorType::Primary).is_err());
    }

    #[test]
    fn directory_record_dot_entry() {
        let mut buf = [0u8; 40];
        buf[0] = 34;
        buf[32] = 1; // filename_length = 1
        buf[33] = 0; // filename = 0x00 → "."
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Primary).unwrap();
        assert_eq!(rec.filename, ".");
    }

    #[test]
    fn directory_record_dotdot_entry() {
        let mut buf = [0u8; 40];
        buf[0] = 34;
        buf[32] = 1;
        buf[33] = 1; // 0x01 → ".."
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Primary).unwrap();
        assert_eq!(rec.filename, "..");
    }

    #[test]
    fn directory_record_primary_strips_version() {
        let mut buf = [0u8; 50];
        let name = b"FILE.TXT;1";
        buf[0] = (33 + name.len()) as u8;
        buf[32] = name.len() as u8;
        buf[33..33 + name.len()].copy_from_slice(name);
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Primary).unwrap();
        assert_eq!(rec.filename, "FILE.TXT");
    }

    #[test]
    fn directory_record_joliet_unicode() {
        // Encode "hi" as UCS-2 BE
        let name: Vec<u8> = "hi".encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
        let mut buf = vec![0u8; 33 + name.len() + 2];
        buf[0] = (33 + name.len()) as u8;
        buf[32] = name.len() as u8;
        buf[33..33 + name.len()].copy_from_slice(&name);
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Joliet).unwrap();
        assert_eq!(rec.filename, "hi");
    }

    #[test]
    fn directory_record_is_directory_flag() {
        let mut buf = [0u8; 40];
        buf[0] = 34;
        buf[25] = 0x02; // directory flag
        buf[32] = 1;
        buf[33] = 0;
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Primary).unwrap();
        assert!(rec.is_directory);
    }

    // ── parse_iso9660 error paths ─────────────────────────────────────────────

    #[test]
    fn parse_iso9660_rejects_non_iso() {
        let mut c = Cursor::new(vec![0u8; S * 20]);
        assert!(parse_iso9660(&mut c).is_err());
    }

    #[test]
    fn parse_iso9660_verbose_rejects_non_iso() {
        let mut c = Cursor::new(vec![0u8; S * 20]);
        assert!(parse_iso9660_verbose(&mut c, true).is_err());
    }

    #[test]
    fn parse_iso9660_no_vd_returns_err() {
        // Has CD001 signature but no PVD or Joliet → should error
        let mut img = vec![0u8; S * 20];
        img[16 * S] = 255; // VD type: terminator immediately
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"CD001");
        let mut c = Cursor::new(img);
        assert!(parse_iso9660(&mut c).is_err());
    }

    // ── parse_iso9660 happy paths ─────────────────────────────────────────────

    #[test]
    fn parse_iso9660_primary_root_has_file() {
        let img = make_iso_primary_only();
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("should parse");
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
        // "HELLO.TXT;1" → stripped to "HELLO.TXT"
        let node = root.find_node("/HELLO.TXT");
        assert!(node.is_some(), "HELLO.TXT not found in root");
    }

    #[test]
    fn parse_iso9660_primary_file_size() {
        let img = make_iso_primary_only();
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).unwrap();
        let node = root.find_node("/HELLO.TXT").unwrap();
        assert_eq!(node.size, 11);
    }

    #[test]
    fn parse_iso9660_joliet_prefers_joliet() {
        let img = make_iso_joliet();
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("should parse");
        // Joliet path should have "hi.txt" (not "HELLO.TXT")
        assert!(
            root.find_node("/hi.txt").is_some(),
            "Joliet entry not found"
        );
    }

    #[test]
    fn parse_iso9660_verbose_primary_works() {
        let img = make_iso_primary_only();
        let mut c = Cursor::new(img);
        let root = parse_iso9660_verbose(&mut c, true).unwrap();
        assert_eq!(root.name, "/");
    }

    // ── extract_rock_ridge_name ───────────────────────────────────────────────

    #[test]
    fn rock_ridge_nm_entry_extracted() {
        // Build a directory record data buffer with an NM system-use entry
        // record_length=50, filename_length=1 (filename=0x00, ".")
        // su_start = 33 + 1 + ((1+1)%2) = 33 + 1 + 0 = 34
        // NM entry at offset 34: sig="NM", entry_len=12, version=1, flags=0, name="longname"
        let mut data = vec![0u8; 60];
        data[0] = 60; // record_length
        data[32] = 1; // filename_length
        data[33] = 0; // filename = "."
        let su_off = 34;
        data[su_off] = b'N';
        data[su_off + 1] = b'M';
        data[su_off + 2] = 13; // entry_len = 5+8 = 13
        data[su_off + 3] = 1; // version
        data[su_off + 4] = 0; // flags = 0 (normal name)
        data[su_off + 5..su_off + 13].copy_from_slice(b"longname");

        let result = extract_rock_ridge_name(&data, 60, 1);
        assert_eq!(result, Some("longname".to_string()));
    }

    #[test]
    fn rock_ridge_nm_parent_flag_skipped() {
        // flags=0x04 → PARENT flag → name should be skipped
        let mut data = vec![0u8; 60];
        data[0] = 60;
        data[32] = 1;
        data[33] = 0;
        let su_off = 34;
        data[su_off] = b'N';
        data[su_off + 1] = b'M';
        data[su_off + 2] = 13;
        data[su_off + 3] = 1;
        data[su_off + 4] = 0x04; // PARENT flag
        data[su_off + 5..su_off + 13].copy_from_slice(b"ignored!");
        let result = extract_rock_ridge_name(&data, 60, 1);
        assert_eq!(result, None);
    }

    // ── real images ───────────────────────────────────────────────────────────

    #[test]
    fn parse_linux_iso_succeeds() {
        let path = std::path::Path::new("test_data/test_linux.iso");
        if !path.exists() {
            return;
        }
        let mut f = std::fs::File::open(path).unwrap();
        let root = parse_iso9660(&mut f).expect("should parse test_linux.iso");
        assert_eq!(root.name, "/");
        assert!(!root.children.is_empty(), "root should have children");
    }

    #[test]
    fn parse_macos_iso_joliet() {
        let path = std::path::Path::new("test_data/test_macos.iso");
        if !path.exists() {
            return;
        }
        let mut f = std::fs::File::open(path).unwrap();
        let root = parse_iso9660(&mut f).expect("should parse test_macos.iso");
        assert_eq!(root.name, "/");
        assert!(!root.children.is_empty());
    }

    // ── parse_directory_record: additional edge cases ─────────────────────────

    #[test]
    fn directory_record_filename_extends_past_buffer() {
        // filename_length=100 but buffer only 50 bytes → 33+100 > 50 → Err (line 204).
        let mut buf = [0u8; 50];
        buf[0] = 50;
        buf[32] = 100; // filename_length = 100, overflows
        assert!(parse_directory_record(&buf, VolumeDescriptorType::Primary).is_err());
    }

    #[test]
    fn directory_record_primary_no_semicolon() {
        // Filename without ';' → else branch (line 236: `&raw_name`), no stripping.
        let name = b"HELLO";
        let mut buf = vec![0u8; 33 + name.len() + 2];
        buf[0] = (33 + name.len()) as u8;
        buf[32] = name.len() as u8;
        buf[33..33 + name.len()].copy_from_slice(name);
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Primary).unwrap();
        assert_eq!(rec.filename, "HELLO");
    }

    #[test]
    fn directory_record_joliet_with_version_suffix() {
        // Joliet filename with ';1' → strip suffix (line 226).
        let name: Vec<u8> = "hi.txt;1"
            .encode_utf16()
            .flat_map(|c| c.to_be_bytes())
            .collect();
        let mut buf = vec![0u8; 33 + name.len() + 2];
        buf[0] = (33 + name.len()) as u8;
        buf[32] = name.len() as u8;
        buf[33..33 + name.len()].copy_from_slice(&name);
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Joliet).unwrap();
        assert_eq!(rec.filename, "hi.txt");
    }

    #[test]
    fn directory_record_empty_filename_length_zero() {
        // filename_length=0 → treated as "." (line 209 `filename_length == 0`).
        let mut buf = [0u8; 40];
        buf[0] = 34;
        buf[32] = 0; // filename_length = 0
        let rec = parse_directory_record(&buf, VolumeDescriptorType::Primary).unwrap();
        assert_eq!(rec.filename, ".");
    }

    // ── extract_rock_ridge_name: additional paths ────────────────────────────

    #[test]
    fn rock_ridge_name_su_start_too_large() {
        // su_start >= record_length → return None (line 257).
        let data = vec![0u8; 40];
        // filename_length=10 → su_start = 33+10+((10+1)%2) = 33+10+1 = 44 > record_length=40
        let result = extract_rock_ridge_name(&data, 40, 10);
        assert_eq!(result, None);
    }

    #[test]
    fn rock_ridge_nm_current_flag_skipped() {
        // flags=0x02 → CURRENT flag → name bytes skipped (line 276 body entered).
        let mut data = vec![0u8; 60];
        let su_off = 34;
        data[su_off] = b'N';
        data[su_off + 1] = b'M';
        data[su_off + 2] = 13;
        data[su_off + 3] = 1;
        data[su_off + 4] = 0x02; // CURRENT flag
        data[su_off + 5..su_off + 13].copy_from_slice(b"ignored!");
        let result = extract_rock_ridge_name(&data, 60, 1);
        assert_eq!(result, None, "CURRENT-flagged NM entry should produce no name");
    }

    #[test]
    fn rock_ridge_entry_too_short_breaks() {
        // entry_len=2 < 4 → break on first iteration (entry_len < 4 guard).
        let mut data = vec![0u8; 60];
        let su_off = 34;
        data[su_off] = b'N';
        data[su_off + 1] = b'M';
        data[su_off + 2] = 2; // entry_len=2 < 4 → break
        let result = extract_rock_ridge_name(&data, 60, 1);
        assert_eq!(result, None);
    }

    #[test]
    fn rock_ridge_while_loop_terminates_normally() {
        // While loop exits via condition (not break), covering line 282.
        // su_start=34, entry_len=8 → one NM entry, offset advances to 8.
        // Then offset+4=12 > su_area.len()=8 → while condition false → line 282 covered.
        let mut data = vec![0u8; 50];
        // record_length=42, filename_length=1 → su_start=34, su_area.len()=42-34=8
        let su_off = 34;
        data[su_off] = b'N';
        data[su_off + 1] = b'M';
        data[su_off + 2] = 8; // entry_len=8
        data[su_off + 3] = 1; // version
        data[su_off + 4] = 0; // flags=0 (normal)
        data[su_off + 5] = b'h';
        data[su_off + 6] = b'i';
        data[su_off + 7] = b'!';
        // After processing this entry: offset=8, 8+4=12 > 8=su_area.len() → while exits normally.
        let result = extract_rock_ridge_name(&data, 42, 1);
        assert_eq!(result, Some("hi!".to_string()));
    }

    // ── parse_iso9660: additional parse paths ────────────────────────────────

    #[test]
    fn parse_iso9660_breaks_on_non_cd001_sector() {
        // Sector 16 = PVD with CD001. Sector 17 = all zeros (no CD001).
        // Loop reads sector 17, no CD001 + sector != 16 → break (lines 71-72).
        // Parser still succeeds with the PVD found at sector 16.
        let mut img = vec![0u8; S * 20];
        let pvd = 16 * S;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        // Root dir: sector 18, size 2048 (empty).
        let root_off = pvd + 156;
        img[root_off] = 34;
        img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[root_off + 25] = 0x02; // directory
        img[root_off + 32] = 1;
        img[root_off + 33] = 0; // "."
        // Sector 17 is left as all zeros — no CD001 → triggers break at line 72.
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("should succeed despite non-CD001 at sector 17");
        assert_eq!(root.name, "/");
    }

    #[test]
    fn parse_iso9660_unknown_vd_type_ignored() {
        // VD type=3 (unknown) at sector 17 → `_ => {}` (line 100).
        let mut img = make_iso_primary_only();
        // Overwrite sector 17 with a type=3 VD before the VDST.
        // (The VDST is still at 17 in make_iso_primary_only — we shift it to 18.)
        img.resize(S * 22, 0);
        // Type=3 at sector 17.
        img[17 * S] = 3; // unknown type
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        // VDST at sector 18.
        img[18 * S] = 255;
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"CD001");
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("unknown VD type should be ignored");
        assert_eq!(root.name, "/");
    }

    #[test]
    fn parse_iso9660_verbose_joliet_path() {
        // Covers verbose Joliet messages (lines 89, 118).
        let img = make_iso_joliet();
        let mut c = Cursor::new(img);
        let root = parse_iso9660_verbose(&mut c, true).expect("verbose Joliet parse");
        assert!(root.find_node("/hi.txt").is_some());
    }

    #[test]
    fn parse_iso9660_non_joliet_svd_ignored() {
        // SVD without Joliet escape → closing `}` of `if escape ==` (line 92 FALSE branch).
        // Build from scratch: PVD(16) + non-Joliet SVD(17) + VDST(18) + rootdir(19) + data(20)
        let mut img = vec![0u8; S * 22];
        // PVD at sector 16, root dir at sector 19
        let pvd = 16 * S;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        img[root_off] = 34;
        img[root_off + 2..root_off + 6].copy_from_slice(&19u32.to_le_bytes()); // root at 19
        img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[root_off + 25] = 0x02;
        img[root_off + 32] = 1;
        img[root_off + 33] = 0;
        // Non-Joliet SVD at sector 17 (no escape sequences → not Joliet)
        img[17 * S] = 2;
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        // VDST at sector 18
        img[18 * S] = 255;
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"CD001");
        // Root dir at sector 19
        let d = 19 * S;
        img[d] = 34;
        img[d + 2..d + 6].copy_from_slice(&19u32.to_le_bytes());
        img[d + 10..d + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[d + 25] = 0x02;
        img[d + 32] = 1;
        img[d + 33] = 0;
        let e1 = d + 34;
        img[e1] = 34;
        img[e1 + 2..e1 + 6].copy_from_slice(&19u32.to_le_bytes());
        img[e1 + 10..e1 + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[e1 + 25] = 0x02;
        img[e1 + 32] = 1;
        img[e1 + 33] = 1;
        let e2 = e1 + 34;
        let name = b"HELLO.TXT;1";
        img[e2] = (33 + name.len()) as u8;
        img[e2 + 2..e2 + 6].copy_from_slice(&20u32.to_le_bytes()); // data at sector 20
        img[e2 + 10..e2 + 14].copy_from_slice(&11u32.to_le_bytes());
        img[e2 + 32] = name.len() as u8;
        img[e2 + 33..e2 + 33 + name.len()].copy_from_slice(name);
        img[20 * S..20 * S + 11].copy_from_slice(b"Hello World");
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("non-Joliet SVD should be ignored");
        assert!(root.find_node("/HELLO.TXT").is_some());
    }

    // ── parse_directory: additional paths ────────────────────────────────────

    #[test]
    fn parse_directory_non_directory_returns_ok() {
        // Calling parse_directory on a non-directory record → immediate Ok (line 303).
        let img = make_iso_primary_only();
        let rec = DirectoryRecord {
            extent_location: 0,
            data_length: 100,
            is_directory: false,
            filename: "file.txt".to_string(),
        };
        let mut parent = crate::tree::TreeNode::new_directory("/".to_string());
        let mut c = Cursor::new(img);
        let result = parse_directory(&mut c, &rec, &mut parent, VolumeDescriptorType::Primary, false, false);
        assert!(result.is_ok());
        assert!(parent.children.is_empty());
    }

    #[test]
    fn parse_directory_zero_byte_padding_advances_to_sector() {
        // Zero byte mid-buffer → skip to next sector boundary (lines 315-322).
        let mut dir_img = vec![0u8; S * 20];
        let pvd = 16 * S;
        dir_img[pvd] = 1;
        dir_img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        dir_img[root_off] = 34;
        dir_img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes());
        dir_img[root_off + 25] = 0x02;
        dir_img[root_off + 32] = 1;
        dir_img[root_off + 33] = 0;
        // VDST
        dir_img[17 * S] = 255;
        dir_img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        // Root dir at sector 18: "." + ".." + zero byte at offset 68 + file entry at 2048.
        let d = 18 * S;
        // "." entry
        dir_img[d] = 34;
        dir_img[d + 2..d + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[d + 10..d + 14].copy_from_slice(&(S as u32).to_le_bytes());
        dir_img[d + 25] = 0x02;
        dir_img[d + 32] = 1;
        dir_img[d + 33] = 0;
        // ".." entry
        dir_img[d + 34] = 34;
        dir_img[d + 34 + 2..d + 34 + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[d + 34 + 10..d + 34 + 14].copy_from_slice(&(S as u32).to_le_bytes());
        dir_img[d + 34 + 25] = 0x02;
        dir_img[d + 34 + 32] = 1;
        dir_img[d + 34 + 33] = 1;
        // Offset 68: leave as 0x00 → zero byte → skip to next sector boundary.
        // (Next sector boundary from offset 68 in a 2048-byte buffer: (68/2048+1)*2048 = 2048)
        // After the skip, offset=2048 which equals buffer.len() → loop exits.
        // The parser should return an empty root (no files added since only "." and ".." were seen before).
        let mut c = Cursor::new(dir_img);
        let root = parse_iso9660(&mut c).expect("zero-padded dir should parse");
        assert_eq!(root.name, "/");
    }

    #[test]
    fn parse_directory_record_parse_error_skipped() {
        // When parse_directory_record returns Err within the parse_directory loop,
        // that record is skipped (line 378: `if let Ok(mut record)` FALSE branch).
        // Craft a directory where one entry has record_length > remaining buffer → parse fails.
        // Actually the outer check `record_length == 0 || offset + record_length > buffer.len()`
        // breaks the loop before calling parse_directory_record in that case.
        // Instead: craft a record where filename_length overflows → parse_directory_record Err.
        let mut dir_img = vec![0u8; S * 20];
        let pvd = 16 * S;
        dir_img[pvd] = 1;
        dir_img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        dir_img[root_off] = 34;
        dir_img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        // data_length=100: buffer is 100 bytes, entry at offset 68 gets 32-byte slice < 34
        dir_img[root_off + 10..root_off + 14].copy_from_slice(&100u32.to_le_bytes());
        dir_img[root_off + 25] = 0x02;
        dir_img[root_off + 32] = 1;
        dir_img[root_off + 33] = 0;
        dir_img[17 * S] = 255;
        dir_img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        let d = 18 * S;
        // "." entry
        dir_img[d] = 34;
        dir_img[d + 2..d + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[d + 10..d + 14].copy_from_slice(&100u32.to_le_bytes());
        dir_img[d + 25] = 0x02;
        dir_img[d + 32] = 1;
        dir_img[d + 33] = 0;
        // ".." entry
        dir_img[d + 34] = 34;
        dir_img[d + 34 + 2..d + 34 + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[d + 34 + 10..d + 34 + 14].copy_from_slice(&100u32.to_le_bytes());
        dir_img[d + 34 + 25] = 0x02;
        dir_img[d + 34 + 32] = 1;
        dir_img[d + 34 + 33] = 1;
        // Bad entry at offset 68: record_length=32 → buffer is data_length=100 bytes, so
        // parse_directory_record receives buffer[68..100] = 32 bytes < 34 → Err → if let Ok FALSE.
        dir_img[d + 68] = 32;
        let mut c = Cursor::new(dir_img);
        let root = parse_iso9660(&mut c).expect("bad dir entry should be skipped");
        assert_eq!(root.name, "/");
        assert!(root.children.is_empty());
    }

    #[test]
    fn parse_directory_record_length_overflow_breaks() {
        // record_length=33 at offset 68 in a 100-byte buffer → 68+33=101 > 100 → break (line 328).
        let mut dir_img = vec![0u8; S * 20];
        let pvd = 16 * S;
        dir_img[pvd] = 1;
        dir_img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        dir_img[root_off] = 34;
        dir_img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[root_off + 10..root_off + 14].copy_from_slice(&100u32.to_le_bytes());
        dir_img[root_off + 25] = 0x02;
        dir_img[root_off + 32] = 1;
        dir_img[root_off + 33] = 0;
        dir_img[17 * S] = 255;
        dir_img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        let d = 18 * S;
        dir_img[d] = 34;
        dir_img[d + 2..d + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[d + 10..d + 14].copy_from_slice(&100u32.to_le_bytes());
        dir_img[d + 25] = 0x02;
        dir_img[d + 32] = 1;
        dir_img[d + 33] = 0;
        dir_img[d + 34] = 34;
        dir_img[d + 34 + 2..d + 34 + 6].copy_from_slice(&18u32.to_le_bytes());
        dir_img[d + 34 + 10..d + 34 + 14].copy_from_slice(&100u32.to_le_bytes());
        dir_img[d + 34 + 25] = 0x02;
        dir_img[d + 34 + 32] = 1;
        dir_img[d + 34 + 33] = 1;
        // offset=68, record_length=33 → 68+33=101 > 100 → break
        dir_img[d + 68] = 33;
        let mut c = Cursor::new(dir_img);
        let root = parse_iso9660(&mut c).expect("overflow record should break loop cleanly");
        assert!(root.children.is_empty());
    }

    // ── detect_rock_ridge: additional paths ──────────────────────────────────

    #[test]
    fn detect_rock_ridge_short_buffer_returns_false() {
        // Root dir data_length < 34 → detect_rock_ridge returns Ok(false) (line 167).
        let mut img = vec![0u8; S * 20];
        let pvd = 16 * S;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        img[root_off] = 34;
        img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        img[root_off + 10..root_off + 14].copy_from_slice(&20u32.to_le_bytes()); // size=20 < 34
        img[root_off + 25] = 0x02;
        img[root_off + 32] = 1;
        img[root_off + 33] = 0;
        img[17 * S] = 255;
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        let mut c = Cursor::new(img);
        // parse_iso9660 on a Primary VD calls detect_rock_ridge;
        // root dir has size=20 < 34 → Ok(false) returned.
        let root = parse_iso9660(&mut c).expect("short root buffer should not error");
        assert_eq!(root.name, "/");
    }

    #[test]
    fn detect_rock_ridge_sp_signature_detected() {
        // Root dir first entry has SP signature in system use → detect_rock_ridge returns Ok(true).
        // Covers lines 175-178.
        let mut img = vec![0u8; S * 20];
        let pvd = 16 * S;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        img[root_off] = 34;
        img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[root_off + 25] = 0x02;
        img[root_off + 32] = 1;
        img[root_off + 33] = 0;
        img[17 * S] = 255;
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        // Root dir at sector 18: first record with SP signature in system use.
        let d = 18 * S;
        let record_len: u8 = 42;
        let filename_length: u8 = 1;
        // su_start = 33 + 1 + ((1+1)%2) = 34
        img[d] = record_len;
        img[d + 2..d + 6].copy_from_slice(&18u32.to_le_bytes());
        img[d + 10..d + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[d + 25] = 0x02;
        img[d + 32] = filename_length;
        img[d + 33] = 0; // "."
        // SP signature at su_start=34
        img[d + 34] = b'S';
        img[d + 35] = b'P';
        // detect_rock_ridge: buffer.len()=4096 (capped), record_length=42,
        // su_start=34, su_start+7=41 <= record_length=42 ✓ → sig="SP" → Ok(true)
        // → verbose message (line 142) if verbose=true
        let mut c = Cursor::new(img.clone());
        let root = parse_iso9660(&mut c).expect("SP rock ridge should parse");
        assert_eq!(root.name, "/");

        // Also test verbose path (line 142).
        let mut c2 = Cursor::new(img);
        let root2 = parse_iso9660_verbose(&mut c2, true).expect("verbose rock ridge");
        assert_eq!(root2.name, "/");
    }

    #[test]
    fn detect_rock_ridge_nm_px_signatures() {
        // NM or PX signature → lines 180-182 covered.
        let mut img = vec![0u8; S * 20];
        let pvd = 16 * S;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        let root_off = pvd + 156;
        img[root_off] = 34;
        img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[root_off + 25] = 0x02;
        img[root_off + 32] = 1;
        img[root_off + 33] = 0;
        img[17 * S] = 255;
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        let d = 18 * S;
        img[d] = 42;
        img[d + 2..d + 6].copy_from_slice(&18u32.to_le_bytes());
        img[d + 10..d + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[d + 25] = 0x02;
        img[d + 32] = 1;
        img[d + 33] = 0;
        // PX signature at su_start=34
        img[d + 34] = b'P';
        img[d + 35] = b'X';
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("PX rock ridge should parse");
        assert_eq!(root.name, "/");
    }

    #[test]
    fn parse_iso9660_rock_ridge_name_in_directory() {
        // Full Rock Ridge NM name replacement in parse_directory (lines 334-345, 282).
        let mut img = vec![0u8; S * 20];
        let pvd = 16 * S;
        img[pvd] = 1;
        img[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        // Root dir at sector 18.
        let root_off = pvd + 156;
        img[root_off] = 34;
        img[root_off + 2..root_off + 6].copy_from_slice(&18u32.to_le_bytes());
        img[root_off + 10..root_off + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[root_off + 25] = 0x02;
        img[root_off + 32] = 1;
        img[root_off + 33] = 0;
        // First record also has SP signature so Rock Ridge is detected.
        let d = 18 * S;
        img[d] = 42;
        img[d + 2..d + 6].copy_from_slice(&18u32.to_le_bytes());
        img[d + 10..d + 14].copy_from_slice(&(S as u32).to_le_bytes());
        img[d + 25] = 0x02;
        img[d + 32] = 1;
        img[d + 33] = 0; // "."
        img[d + 34] = b'S';
        img[d + 35] = b'P';
        // VDST at sector 17.
        img[17 * S] = 255;
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"CD001");
        // File entry at offset 42 in root dir: name="F;1" with NM entry "longname.txt"
        let e = d + 42;
        let iso_name = b"F;1";
        let rr_name = b"longname.txt";
        // su_start = 33 + 3 + ((3+1)%2) = 33 + 3 + 0 = 36
        // NM at su_start: sig="NM", entry_len=5+12=17, flags=0
        let entry_len = 33 + iso_name.len() + 17;
        img[e] = entry_len as u8;
        img[e + 2..e + 6].copy_from_slice(&19u32.to_le_bytes()); // extent=sector 19
        img[e + 10..e + 14].copy_from_slice(&11u32.to_le_bytes()); // size=11
        img[e + 25] = 0x00; // file
        img[e + 32] = iso_name.len() as u8;
        img[e + 33..e + 33 + iso_name.len()].copy_from_slice(iso_name);
        // su_start = 33 + 3 + ((3+1)%2) = 36
        let su = e + 36;
        img[su] = b'N';
        img[su + 1] = b'M';
        img[su + 2] = (5 + rr_name.len()) as u8;
        img[su + 3] = 1; // version
        img[su + 4] = 0; // flags = normal name
        img[su + 5..su + 5 + rr_name.len()].copy_from_slice(rr_name);
        // File data at sector 19.
        img[19 * S..19 * S + 11].copy_from_slice(b"Hello World");
        let mut c = Cursor::new(img);
        let root = parse_iso9660(&mut c).expect("rock ridge with NM entry");
        // The file should be named "longname.txt" (Rock Ridge NM), not "F".
        assert!(
            root.find_node("/longname.txt").is_some(),
            "Rock Ridge NM name not applied"
        );
    }
}
