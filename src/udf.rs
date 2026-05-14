//! UDF (ECMA-167) parser. Supports metadata partitions and multi-extent
//! files — enough for typical CD/DVD/Blu-ray media.
//!
//! The entry points are [`parse_udf`] and [`parse_udf_verbose`]. Both
//! return a [`crate::TreeNode`] tree rooted at `"/"` on success.

use crate::tree::TreeNode;
use crate::Result;
// `File` is no longer mentioned by the parser; entry points are
// generic over `R: Read + Seek` as of v3.0.
use std::io::{Read, Seek, SeekFrom};

const SECTOR_SIZE: u64 = 2048;

#[derive(Debug, Clone, Copy)]
struct ExtentAd {
    length: u32,
    location: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
struct LongAd {
    length: u32,
    location: u32,
    partition: u16,
}

#[derive(Debug, Clone)]
struct PartitionInfo {
    number: u16,
    start_sector: u64,
}

#[derive(Debug, Clone)]
struct MetadataPartitionInfo {
    file_location: u32,
    partition_ref: u16,
}

/// Represents a file's allocation — possibly spanning multiple extents.
#[derive(Debug, Clone)]
struct FileAllocation {
    extents: Vec<ExtentAd>,
    total_length: u64,
    /// For inline data (ad_type 3), the raw data is stored here.
    inline_data: Option<Vec<u8>>,
}

fn read_extent_ad(buffer: &[u8]) -> ExtentAd {
    ExtentAd {
        length: u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]),
        location: u32::from_le_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]),
    }
}

fn read_long_ad(buffer: &[u8]) -> LongAd {
    LongAd {
        length: u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]),
        location: u32::from_le_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]),
        partition: u16::from_le_bytes([buffer[8], buffer[9]]),
    }
}

/// Parse a UDF image, returning the root of the directory tree.
///
/// Equivalent to `parse_udf_verbose(file, false)`. Errors out cleanly
/// (returns `Err`, never panics) on images whose anchor or partition
/// descriptors don't validate.
pub fn parse_udf<R: Read + Seek>(file: &mut R) -> Result<TreeNode> {
    parse_udf_verbose(file, false)
}

/// Like [`parse_udf`], but prints spec-section-tagged diagnostics to
/// stderr while parsing. Useful for investigating images that fail.
///
/// As of v3.0 this takes `&mut (impl Read + Seek)` rather than
/// `&mut File`, so consumers can feed it an `MmapImage`, a
/// `Cursor<Vec<u8>>`, or any other byte-source that implements
/// both traits.
pub fn parse_udf_verbose<R: Read + Seek>(file: &mut R, verbose: bool) -> Result<TreeNode> {
    // Check for UDF markers in the Volume Recognition Sequence (sectors 16-31)
    let mut found_udf_marker = false;
    if verbose {
        eprintln!("Scanning sectors 16-31 for UDF Volume Recognition Sequence...");
    }
    for sector in 16..32 {
        if file.seek(SeekFrom::Start(sector * SECTOR_SIZE)).is_err() {
            continue;
        }
        let mut buffer = [0u8; 16];
        if file.read_exact(&mut buffer).is_err() {
            continue;
        }

        let id = &buffer[1..6];
        if id == b"NSR02" || id == b"NSR03" || id == b"BEA01" || id == b"TEA01" {
            if verbose {
                eprintln!(
                    "  Found UDF marker '{:?}' at sector {}",
                    String::from_utf8_lossy(id),
                    sector
                );
            }
            found_udf_marker = true;
            break;
        }
    }

    if !found_udf_marker {
        return Err("Not a valid UDF filesystem (no VRS markers found)".into());
    }

    // Try to find the Anchor Volume Descriptor Pointer (AVDP).
    // ECMA-167 §8.4.2 mandates sector 256, and also sector N and N-256 for
    // multi-session discs.  Compact images (e.g. hdiutil on macOS) sometimes
    // place the AVDP earlier, so we scan a short candidate list.
    if verbose {
        eprintln!("Looking for Anchor Volume Descriptor Pointer...");
    }
    let image_size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let last_sector = image_size / SECTOR_SIZE;
    // Candidates: standard position 256, then last, last-256, and a compact
    // fallback scan for images smaller than 256 sectors.
    let mut candidates: Vec<u64> = vec![256];
    if last_sector > 0 && last_sector != 256 {
        candidates.push(last_sector);
    }
    if last_sector > 256 {
        candidates.push(last_sector - 256);
    }
    // Compact fallback: scan every sector from 32..min(256, last_sector).
    for s in 32..256.min(last_sector) {
        if !candidates.contains(&s) {
            candidates.push(s);
        }
    }

    let mut avdp_buffer = [0u8; 512];
    let mut found_avdp = false;
    for candidate in &candidates {
        if file.seek(SeekFrom::Start(candidate * SECTOR_SIZE)).is_err() {
            continue;
        }
        if file.read_exact(&mut avdp_buffer).is_err() {
            continue;
        }
        let tag_id = u16::from_le_bytes([avdp_buffer[0], avdp_buffer[1]]);
        if tag_id == 2 {
            if verbose {
                eprintln!("  Found AVDP at sector {}", candidate);
            }
            found_avdp = true;
            break;
        }
    }
    if !found_avdp {
        if verbose {
            eprintln!("  AVDP not found in any candidate sector");
        }
        return Err("UDF detected but no Anchor Volume Descriptor Pointer found.".into());
    }

    let main_vds_extent = read_extent_ad(&avdp_buffer[16..24]);
    if verbose {
        eprintln!(
            "  Found AVDP. Main VDS at sector {}, length {}",
            main_vds_extent.location, main_vds_extent.length
        );
    }

    // Collect partition info and parse LVD
    let mut partitions: Vec<PartitionInfo> = Vec::new();
    let mut root_fsd_long_ad = None;
    let mut metadata_partition: Option<MetadataPartitionInfo> = None;

    let mut sector = main_vds_extent.location as u64;
    let end_sector = sector + (main_vds_extent.length as u64).div_ceil(SECTOR_SIZE);

    if verbose {
        eprintln!(
            "Parsing Main Volume Descriptor Sequence (sectors {} to {})...",
            sector, end_sector
        );
    }
    while sector < end_sector {
        file.seek(SeekFrom::Start(sector * SECTOR_SIZE))?;
        let mut vds_buffer = vec![0u8; SECTOR_SIZE as usize];
        file.read_exact(&mut vds_buffer)?;

        let vds_tag_id = u16::from_le_bytes([vds_buffer[0], vds_buffer[1]]);

        match vds_tag_id {
            5 => {
                // Partition Descriptor
                let part_num = u16::from_le_bytes([vds_buffer[22], vds_buffer[23]]);
                let part_start = u32::from_le_bytes([
                    vds_buffer[188],
                    vds_buffer[189],
                    vds_buffer[190],
                    vds_buffer[191],
                ]) as u64;
                if verbose {
                    eprintln!(
                        "  Found Partition Descriptor #{}: starts at sector {}",
                        part_num, part_start
                    );
                }
                partitions.push(PartitionInfo {
                    number: part_num,
                    start_sector: part_start,
                });
            }
            6 => {
                // Logical Volume Descriptor
                // FSD location at offset 248
                root_fsd_long_ad = Some(read_long_ad(&vds_buffer[248..264]));
                if verbose {
                    let ad = root_fsd_long_ad.unwrap();
                    eprintln!(
                        "  Found Logical Volume Descriptor. FSD at location {} in partition {}",
                        ad.location, ad.partition
                    );
                }

                // Parse partition maps to find metadata partition
                let map_table_length = u32::from_le_bytes([
                    vds_buffer[264],
                    vds_buffer[265],
                    vds_buffer[266],
                    vds_buffer[267],
                ]) as usize;
                let num_partition_maps = u32::from_le_bytes([
                    vds_buffer[268],
                    vds_buffer[269],
                    vds_buffer[270],
                    vds_buffer[271],
                ]);
                if verbose {
                    eprintln!(
                        "    {} partition maps, table length {} bytes",
                        num_partition_maps, map_table_length
                    );
                }

                // Partition maps start at offset 440
                let mut map_offset = 440usize;
                for map_idx in 0..num_partition_maps {
                    if map_offset + 2 > vds_buffer.len() {
                        break;
                    }
                    let map_type = vds_buffer[map_offset];
                    let map_length = vds_buffer[map_offset + 1] as usize;
                    if map_length == 0 {
                        break;
                    } // malformed map: avoid infinite loop

                    if verbose {
                        eprintln!(
                            "    Partition map {}: type {}, length {}",
                            map_idx, map_type, map_length
                        );
                    }

                    if map_type == 2 && map_length >= 64 {
                        let id_string = &vds_buffer[map_offset + 5..map_offset + 28];

                        if verbose {
                            let id_printable: String = id_string
                                .iter()
                                .take_while(|&&b| b != 0)
                                .map(|&b| {
                                    if (0x20..0x7f).contains(&b) {
                                        b as char
                                    } else {
                                        '.'
                                    }
                                })
                                .collect();
                            eprintln!("      Type 2 identifier: '{}'", id_printable);
                        }

                        if id_string.starts_with(b"*UDF Metadata Partition") {
                            let meta_part_ref = u16::from_le_bytes([
                                vds_buffer[map_offset + 38],
                                vds_buffer[map_offset + 39],
                            ]);
                            let meta_file_loc = u32::from_le_bytes([
                                vds_buffer[map_offset + 40],
                                vds_buffer[map_offset + 41],
                                vds_buffer[map_offset + 42],
                                vds_buffer[map_offset + 43],
                            ]);
                            if verbose {
                                eprintln!(
                                    "      Metadata Partition: file at location {} in partition {}",
                                    meta_file_loc, meta_part_ref
                                );
                            }
                            metadata_partition = Some(MetadataPartitionInfo {
                                file_location: meta_file_loc,
                                partition_ref: meta_part_ref,
                            });
                        }
                    }

                    map_offset += map_length;
                }
            }
            8 => {
                if verbose {
                    eprintln!("  Found Terminating Descriptor at sector {}", sector);
                }
                break;
            }
            _ => {}
        }
        sector += 1;
    }

    let fsd_long_ad =
        root_fsd_long_ad.ok_or("Failed to find File Set Descriptor location in LVD")?;

    // Find the partition that the FSD references
    let fsd_partition_ref = fsd_long_ad.partition;

    // Determine where to read the FSD from
    let (fsd_sector, partition_start) = if let Some(ref meta_info) = metadata_partition {
        if verbose {
            eprintln!("FSD is in metadata partition, reading via metadata file...");
        }

        let meta_phys_partition = partitions
            .iter()
            .find(|p| p.number == meta_info.partition_ref)
            .ok_or("Cannot find physical partition for metadata file")?;

        let meta_fe_sector = meta_phys_partition.start_sector + meta_info.file_location as u64;
        if verbose {
            eprintln!("  Metadata File Entry at sector {}", meta_fe_sector);
        }

        file.seek(SeekFrom::Start(meta_fe_sector * SECTOR_SIZE))?;
        let mut meta_fe_buffer = vec![0u8; SECTOR_SIZE as usize];
        file.read_exact(&mut meta_fe_buffer)?;

        let meta_tag_id = u16::from_le_bytes([meta_fe_buffer[0], meta_fe_buffer[1]]);
        if verbose {
            eprintln!("  Metadata FE tag: {}", meta_tag_id);
        }

        // Read first extent of metadata file
        let meta_alloc = get_file_allocation(&meta_fe_buffer)?;
        let first_extent = meta_alloc
            .extents
            .first()
            .ok_or("Metadata file has no allocation extents")?;

        if verbose {
            eprintln!(
                "  Metadata file extent: location {}, length {}",
                first_extent.location, first_extent.length
            );
        }

        let metadata_data_sector = meta_phys_partition.start_sector + first_extent.location as u64;
        let fsd_offset_in_metadata = fsd_long_ad.location as u64;

        (
            metadata_data_sector + fsd_offset_in_metadata,
            metadata_data_sector,
        )
    } else {
        let partition = partitions
            .iter()
            .find(|p| p.number == fsd_partition_ref)
            .or_else(|| partitions.first())
            .ok_or("No partition found")?;

        (
            partition.start_sector + fsd_long_ad.location as u64,
            partition.start_sector,
        )
    };

    if verbose {
        eprintln!("Reading File Set Descriptor at sector {}...", fsd_sector);
    }
    file.seek(SeekFrom::Start(fsd_sector * SECTOR_SIZE))?;
    let mut fsd_buffer = [0u8; 512];
    file.read_exact(&mut fsd_buffer)?;

    let fsd_tag_id = u16::from_le_bytes([fsd_buffer[0], fsd_buffer[1]]);
    if fsd_tag_id != 256 {
        if verbose {
            eprintln!(
                "  Tag {} at expected FSD location, scanning nearby...",
                fsd_tag_id
            );
        }
        let mut found_fsd = false;
        for offset in 1..32 {
            file.seek(SeekFrom::Start((fsd_sector + offset) * SECTOR_SIZE))?;
            file.read_exact(&mut fsd_buffer)?;
            let tag = u16::from_le_bytes([fsd_buffer[0], fsd_buffer[1]]);
            if tag == 256 {
                if verbose {
                    eprintln!(
                        "  Found FSD at sector {} (offset +{})",
                        fsd_sector + offset,
                        offset
                    );
                }
                found_fsd = true;
                break;
            }
        }
        if !found_fsd {
            return Err(format!(
                "Invalid File Set Descriptor tag: expected 256, found {}",
                fsd_tag_id
            )
            .into());
        }
    }

    let root_icb_long_ad = read_long_ad(&fsd_buffer[400..416]);
    if verbose {
        eprintln!(
            "  Found FSD. Root ICB at location {} in partition {}",
            root_icb_long_ad.location, root_icb_long_ad.partition
        );
    }

    let mut root_node = TreeNode::new_directory("/".to_string());
    if verbose {
        eprintln!("Parsing root directory...");
    }
    parse_directory(
        file,
        partition_start,
        &root_icb_long_ad,
        &mut root_node,
        verbose,
    )?;

    root_node.calculate_directory_size();
    Ok(root_node)
}

/// Parse all allocation descriptors from a File Entry buffer, supporting multi-extent files.
fn get_file_allocation(fe_buffer: &[u8]) -> Result<FileAllocation> {
    let tag_id = u16::from_le_bytes([fe_buffer[0], fe_buffer[1]]);

    let (ad_length_offset, ea_length_offset, ad_data_offset_base) = match tag_id {
        261 => (172, 168, 176usize), // File Entry
        266 => (212, 208, 216usize), // Extended File Entry
        _ => return Err(format!("Unsupported ICB tag: {}", tag_id).into()),
    };

    // ICB tag flags (at offset 20-21 in ICB tag, which starts at offset 16)
    let icb_flags = u16::from_le_bytes([fe_buffer[18], fe_buffer[19]]);
    let ad_type = icb_flags & 0x07;

    let ea_length = u32::from_le_bytes([
        fe_buffer[ea_length_offset],
        fe_buffer[ea_length_offset + 1],
        fe_buffer[ea_length_offset + 2],
        fe_buffer[ea_length_offset + 3],
    ]) as usize;

    let ad_length = u32::from_le_bytes([
        fe_buffer[ad_length_offset],
        fe_buffer[ad_length_offset + 1],
        fe_buffer[ad_length_offset + 2],
        fe_buffer[ad_length_offset + 3],
    ]) as usize;

    let ad_offset = ad_data_offset_base + ea_length;

    let mut extents = Vec::new();
    let mut total_length: u64 = 0;
    let mut inline_data = None;

    match ad_type {
        0 => {
            // Short Allocation Descriptors (8 bytes each: length[4] + position[4])
            let mut pos = ad_offset;
            while pos + 8 <= fe_buffer.len() && pos < ad_offset + ad_length {
                let raw_length = u32::from_le_bytes([
                    fe_buffer[pos],
                    fe_buffer[pos + 1],
                    fe_buffer[pos + 2],
                    fe_buffer[pos + 3],
                ]);
                let extent_type = raw_length >> 30;
                let length = raw_length & 0x3FFFFFFF;
                let location = u32::from_le_bytes([
                    fe_buffer[pos + 4],
                    fe_buffer[pos + 5],
                    fe_buffer[pos + 6],
                    fe_buffer[pos + 7],
                ]);

                if length == 0 {
                    break;
                }
                if extent_type == 3 {
                    break;
                } // Next extent of allocation descriptors — not yet supported

                // Type 0 = recorded and allocated, Type 1 = allocated but not recorded (sparse)
                if extent_type == 0 {
                    extents.push(ExtentAd { length, location });
                }
                total_length += length as u64;
                pos += 8;
            }
        }
        1 => {
            // Long Allocation Descriptors (16 bytes each)
            let mut pos = ad_offset;
            while pos + 16 <= fe_buffer.len() && pos < ad_offset + ad_length {
                let raw_length = u32::from_le_bytes([
                    fe_buffer[pos],
                    fe_buffer[pos + 1],
                    fe_buffer[pos + 2],
                    fe_buffer[pos + 3],
                ]);
                let extent_type = raw_length >> 30;
                let length = raw_length & 0x3FFFFFFF;
                let location = u32::from_le_bytes([
                    fe_buffer[pos + 4],
                    fe_buffer[pos + 5],
                    fe_buffer[pos + 6],
                    fe_buffer[pos + 7],
                ]);

                if length == 0 {
                    break;
                }
                if extent_type == 3 {
                    break;
                }

                if extent_type == 0 {
                    extents.push(ExtentAd { length, location });
                }
                total_length += length as u64;
                pos += 16;
            }
        }
        3 => {
            // Inline data — embedded directly in the file entry at the AD area
            let end = (ad_offset + ad_length).min(fe_buffer.len());
            if ad_offset < end {
                inline_data = Some(fe_buffer[ad_offset..end].to_vec());
                total_length = (end - ad_offset) as u64;
            }
        }
        _ => {
            // Fallback: try reading a single extent at the expected offset
            if ad_offset + 8 <= fe_buffer.len() {
                let ext = read_extent_ad(&fe_buffer[ad_offset..ad_offset + 8]);
                if ext.length > 0 {
                    total_length = ext.length as u64;
                    extents.push(ext);
                }
            }
        }
    }

    if extents.is_empty() && inline_data.is_none() {
        return Err("No allocation extents found in file entry".into());
    }

    Ok(FileAllocation {
        extents,
        total_length,
        inline_data,
    })
}

fn parse_directory<R: Read + Seek>(
    file: &mut R,
    partition_start: u64,
    icb_long_ad: &LongAd,
    parent_node: &mut TreeNode,
    verbose: bool,
) -> Result<()> {
    // Read the file entry to get allocation info
    file.seek(SeekFrom::Start(
        (partition_start + icb_long_ad.location as u64) * SECTOR_SIZE,
    ))?;
    let mut fe_buffer = vec![0u8; SECTOR_SIZE as usize];
    file.read_exact(&mut fe_buffer)?;

    let alloc = get_file_allocation(&fe_buffer)?;

    if verbose {
        if alloc.inline_data.is_some() {
            eprintln!("  Directory has inline data, {} bytes", alloc.total_length);
        } else {
            eprintln!(
                "  Directory has {} extent(s), total {} bytes",
                alloc.extents.len(),
                alloc.total_length
            );
        }
    }

    // Read directory data — either inline or from extents
    let buffer = if let Some(data) = alloc.inline_data {
        data
    } else {
        let cap = usize::try_from(alloc.total_length)
            .map_err(|_| format!("directory too large: {} bytes", alloc.total_length))?;
        let mut buf = Vec::with_capacity(cap);
        for extent in &alloc.extents {
            file.seek(SeekFrom::Start(
                (partition_start + extent.location as u64) * SECTOR_SIZE,
            ))?;
            let mut chunk = vec![0u8; extent.length as usize];
            file.read_exact(&mut chunk)?;
            buf.extend_from_slice(&chunk);
        }
        buf
    };

    let mut offset = 0;
    while offset < buffer.len() {
        if offset + 40 > buffer.len() {
            break;
        }

        let tag_id = u16::from_le_bytes([buffer[offset], buffer[offset + 1]]);
        if tag_id == 0 {
            offset += 4;
            continue;
        }

        if tag_id != 257 {
            // File Identifier Descriptor
            if verbose {
                eprintln!(
                    "    Warning: Expected FID (257) at offset {}, found {}",
                    offset, tag_id
                );
            }
            break;
        }

        let file_characteristics = buffer[offset + 18];
        let length_of_fi = buffer[offset + 19] as usize;
        let icb = read_long_ad(&buffer[offset + 20..offset + 36]);
        let length_of_iu = u16::from_le_bytes([buffer[offset + 36], buffer[offset + 37]]) as usize;

        let name_offset = offset + 38 + length_of_iu;
        if name_offset + length_of_fi > buffer.len() {
            if verbose {
                eprintln!(
                    "    Warning: FID name offset out of bounds at offset {}",
                    offset
                );
            }
            break;
        }

        let name = if length_of_fi == 0 {
            String::new()
        } else {
            parse_udf_name(&buffer[name_offset..name_offset + length_of_fi])
        };

        let is_directory = (file_characteristics & 0x02) != 0;
        let is_deleted = (file_characteristics & 0x04) != 0;
        let is_parent = (file_characteristics & 0x08) != 0;

        if !is_deleted && !is_parent && !name.is_empty() {
            if verbose {
                eprintln!(
                    "    Found {}: {}",
                    if is_directory { "dir" } else { "file" },
                    name
                );
            }
            if is_directory {
                let mut dir_node = TreeNode::new_directory(name);
                if let Err(e) = parse_directory(file, partition_start, &icb, &mut dir_node, verbose)
                {
                    if verbose {
                        eprintln!("      Warning: Failed to parse subdirectory: {}", e);
                    }
                }
                parent_node.add_child(dir_node);
            } else {
                match get_file_info(file, partition_start, &icb) {
                    Ok(alloc) => {
                        let file_node = if let Some(first) = alloc.extents.first() {
                            // Extent-based file: record location for extraction
                            TreeNode::new_file_with_location(
                                name,
                                alloc.total_length,
                                (partition_start + first.location as u64) * SECTOR_SIZE,
                                alloc.total_length,
                            )
                        } else {
                            // Inline data (ad_type 3): size known but no extent location
                            TreeNode::new_file(name, alloc.total_length)
                        };
                        parent_node.add_child(file_node);
                    }
                    Err(e) => {
                        if verbose {
                            eprintln!("      Warning: Failed to get file extent: {}", e);
                        }
                        let file_node = TreeNode::new_file(name, 0);
                        parent_node.add_child(file_node);
                    }
                }
            }
        }

        // FIDs are padded to 4-byte boundaries
        let fid_length = 38 + length_of_iu + length_of_fi;
        offset += (fid_length + 3) & !3;
    }

    Ok(())
}

fn get_file_info<R: Read + Seek>(
    file: &mut R,
    partition_start: u64,
    icb_long_ad: &LongAd,
) -> Result<FileAllocation> {
    file.seek(SeekFrom::Start(
        (partition_start + icb_long_ad.location as u64) * SECTOR_SIZE,
    ))?;
    let mut fe_buffer = vec![0u8; SECTOR_SIZE as usize];
    file.read_exact(&mut fe_buffer)?;

    get_file_allocation(&fe_buffer)
}

fn parse_udf_name(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

    let compression_id = data[0];
    if compression_id == 8 {
        String::from_utf8_lossy(&data[1..]).to_string()
    } else if compression_id == 16 {
        let utf16_data: Vec<u16> = data[1..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        String::from_utf16_lossy(&utf16_data)
    } else {
        String::from_utf8_lossy(data).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Helpers ───────────────────────────────────────────────────────────────

    const S: usize = 2048; // sector size

    /// Write `val` as a little-endian u16 into `buf` at `offset`.
    fn w16(buf: &mut [u8], offset: usize, val: u16) {
        buf[offset..offset + 2].copy_from_slice(&val.to_le_bytes());
    }
    /// Write `val` as a little-endian u32 into `buf` at `offset`.
    fn w32(buf: &mut [u8], offset: usize, val: u32) {
        buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
    }

    /// Build a minimal synthetic UDF image (270 sectors) that parse_udf can
    /// successfully read.  The root directory contains one file "hello.txt"
    /// with inline content.
    fn make_udf_image() -> Vec<u8> {
        let mut img = vec![0u8; S * 270];

        // VRS (sectors 16-18)
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"BEA01");
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"NSR02");
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"TEA01");

        // AVDP at sector 256 (tag_id=2)
        // Main VDS: location=257, length=3*S (PD + LVD + TD)
        let avdp = 256 * S;
        w16(&mut img, avdp, 2);
        w32(&mut img, avdp + 16, (3 * S) as u32); // Main VDS length
        w32(&mut img, avdp + 20, 257); // Main VDS location

        // PD at sector 257 (tag_id=5): partition 0 starts at sector 260
        let pd = 257 * S;
        w16(&mut img, pd, 5);
        w16(&mut img, pd + 22, 0); // partition_number = 0
        w32(&mut img, pd + 188, 260); // start sector = 260

        // LVD at sector 258 (tag_id=6): FSD at location=0 in partition=0
        let lvd = 258 * S;
        w16(&mut img, lvd, 6);
        // FSD LongAD (bytes 248-263): {length, location, partition}
        w32(&mut img, lvd + 248, S as u32); // length = one sector
        w32(&mut img, lvd + 252, 0); // location = 0 (in partition 0)
        w16(&mut img, lvd + 256, 0); // partition = 0
                                     // MapTableLength + NumPartitionMaps stay 0 → no metadata partition

        // TD at sector 259 (tag_id=8)
        w16(&mut img, 259 * S, 8);

        // FSD at sector 260 (partition 0, location 0 → absolute sector 260)
        let fsd = 260 * S;
        w16(&mut img, fsd, 256); // tag_id = 256
                                 // Root ICB LongAD at bytes 400-415: {length, location=1, partition=0}
        w32(&mut img, fsd + 400, S as u32); // length
        w32(&mut img, fsd + 404, 1); // location = 1 (→ sector 261)
        w16(&mut img, fsd + 408, 0); // partition = 0

        // Root directory File Entry at sector 261 (tag_id=261)
        // Uses inline data (ICB flags bits 0-2 = 3).
        let rfe = 261 * S;
        w16(&mut img, rfe, 261); // tag_id = 261
        w16(&mut img, rfe + 18, 3); // ICB flags: ad_type = 3 (inline)
                                    // EA length (offset 168) = 0
                                    // Build FID data inline: parent FID + one file FID
        let fid_data = make_fid_data();
        w32(&mut img, rfe + 172, fid_data.len() as u32); // AD length = FID data size
        img[rfe + 176..rfe + 176 + fid_data.len()].copy_from_slice(&fid_data);

        // "hello.txt" File Entry at sector 262 (partition 0, location 2)
        // Inline data: the file content itself.
        let content = b"Hello UDF!";
        let hfe = 262 * S;
        w16(&mut img, hfe, 261); // tag_id = 261
        w16(&mut img, hfe + 18, 3); // ICB flags: inline
        w32(&mut img, hfe + 172, content.len() as u32); // AD length
        img[hfe + 176..hfe + 176 + content.len()].copy_from_slice(content);

        img
    }

    /// Build FID bytes for the root directory: a parent FID plus a "hello.txt" file FID.
    fn make_fid_data() -> Vec<u8> {
        let mut data = Vec::new();

        // Parent FID (file_characteristics = 0x08 = PARENT, no name)
        // Structure: tag(16) + file_char(1) + len_fi(1) + ICB(16) + len_iu(2) = 36 bytes
        // But parser reads: offset+18=file_char, offset+19=len_fi, offset+20..36=ICB, 36..38=len_iu
        // Full FID minimum = 38 bytes; padded to 4-byte boundary = 40.
        let mut parent = vec![0u8; 40];
        w16(&mut parent, 0, 257); // tag_id = 257 (FID)
        parent[18] = 0x08; // PARENT flag
        parent[19] = 0; // no file identifier
                        // ICB at offset 20: zeros (ignored for parent)
                        // len_iu at offset 36-37: 0
        data.extend_from_slice(&parent);

        // "hello.txt" file FID (file_characteristics=0, has a name)
        // Name encoded as OSTA CS0 (compression_id=8 followed by ASCII bytes)
        let name_raw: Vec<u8> = {
            let mut n = vec![8u8]; // CS0: 8-bit chars
            n.extend_from_slice(b"hello.txt");
            n
        };
        // FID structure: 38 bytes header + len_iu bytes + name bytes, padded to 4 bytes
        let len_fi = name_raw.len() as u8;
        let total_unpadded = 38 + len_fi as usize;
        let padded = (total_unpadded + 3) & !3;
        let mut file_fid = vec![0u8; padded];
        w16(&mut file_fid, 0, 257); // tag_id = 257 (FID)
        file_fid[18] = 0x00; // regular file
        file_fid[19] = len_fi; // length_of_fi
                               // ICB at offset 20: location=2 (→ sector 262 in partition 0), partition=0
        w32(&mut file_fid, 20, 512); // length (arbitrary, for the ICB)
        w32(&mut file_fid, 24, 2); // location = 2
        w16(&mut file_fid, 28, 0); // partition = 0
                                   // len_iu at offset 36-37: 0
                                   // name at offset 38:
        file_fid[38..38 + name_raw.len()].copy_from_slice(&name_raw);
        data.extend_from_slice(&file_fid);

        data
    }

    // ── read_extent_ad / read_long_ad ─────────────────────────────────────────

    #[test]
    fn read_extent_ad_little_endian() {
        let buf = [0x00, 0x10, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00];
        let ext = read_extent_ad(&buf);
        assert_eq!(ext.length, 0x1000);
        assert_eq!(ext.location, 5);
    }

    #[test]
    fn read_long_ad_parses_partition() {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&512u32.to_le_bytes()); // length
        buf[4..8].copy_from_slice(&42u32.to_le_bytes()); // location
        buf[8..10].copy_from_slice(&7u16.to_le_bytes()); // partition
        let ad = read_long_ad(&buf);
        assert_eq!(ad.length, 512);
        assert_eq!(ad.location, 42);
        assert_eq!(ad.partition, 7);
    }

    // ── get_file_allocation ───────────────────────────────────────────────────

    fn make_fe_buf(tag_id: u16, ad_type: u16, ad_bytes: &[u8]) -> Vec<u8> {
        // Use File Entry (tag_id=261) offsets: EA@168, AD@172, data@176
        let mut buf = vec![0u8; 2048];
        w16(&mut buf, 0, tag_id);
        w16(&mut buf, 18, ad_type); // ICB flags[0..2] = ad_type
        w32(&mut buf, 168, 0); // EA length = 0
        w32(&mut buf, 172, ad_bytes.len() as u32); // AD length
        buf[176..176 + ad_bytes.len()].copy_from_slice(ad_bytes);
        buf
    }

    #[test]
    fn file_alloc_short_ad() {
        // One 8-byte short AD: length=1024, location=5
        let mut ad = vec![0u8; 8];
        w32(&mut ad, 0, 1024); // raw_length = 1024 (type=0)
        w32(&mut ad, 4, 5); // location = 5
        let buf = make_fe_buf(261, 0, &ad);
        let alloc = get_file_allocation(&buf).unwrap();
        assert_eq!(alloc.total_length, 1024);
        assert_eq!(alloc.extents.len(), 1);
        assert_eq!(alloc.extents[0].location, 5);
    }

    #[test]
    fn file_alloc_short_ad_sparse_skipped() {
        // Type 1 (allocated but not recorded — sparse): only AD is sparse,
        // leaving extents empty → get_file_allocation returns Err.
        let mut ad = vec![0u8; 8];
        let raw = (1u32 << 30) | 512; // extent_type=1, length=512
        w32(&mut ad, 0, raw);
        w32(&mut ad, 4, 99);
        let buf = make_fe_buf(261, 0, &ad);
        assert!(get_file_allocation(&buf).is_err());
    }

    #[test]
    fn file_alloc_long_ad() {
        // One 16-byte long AD
        let mut ad = vec![0u8; 16];
        w32(&mut ad, 0, 4096); // length
        w32(&mut ad, 4, 10); // location
        w16(&mut ad, 8, 0); // partition
        let buf = make_fe_buf(261, 1, &ad);
        let alloc = get_file_allocation(&buf).unwrap();
        assert_eq!(alloc.total_length, 4096);
        assert_eq!(alloc.extents.len(), 1);
    }

    #[test]
    fn file_alloc_inline_data() {
        let content = b"hello inline";
        let buf = make_fe_buf(261, 3, content);
        let alloc = get_file_allocation(&buf).unwrap();
        assert_eq!(alloc.total_length, content.len() as u64);
        assert!(alloc.inline_data.is_some());
        assert_eq!(alloc.inline_data.unwrap(), content);
    }

    #[test]
    fn file_alloc_extended_file_entry() {
        // Extended File Entry (tag_id=266) uses EA@208, AD@212, data@216
        let content = b"efe data";
        let mut buf = vec![0u8; 2048];
        w16(&mut buf, 0, 266); // EFE tag
        w16(&mut buf, 18, 3); // inline ad_type
        w32(&mut buf, 208, 0); // EA length = 0
        w32(&mut buf, 212, content.len() as u32); // AD length
        buf[216..216 + content.len()].copy_from_slice(content);
        let alloc = get_file_allocation(&buf).unwrap();
        assert_eq!(alloc.total_length, content.len() as u64);
    }

    #[test]
    fn file_alloc_rejects_unknown_tag() {
        let buf = make_fe_buf(999, 0, &[]);
        assert!(get_file_allocation(&buf).is_err());
    }

    #[test]
    fn file_alloc_fallback_ad_type() {
        // ad_type=2 triggers the fallback branch
        let mut ad = [0u8; 8];
        w32(&mut ad, 0, 512); // length=512
        w32(&mut ad, 4, 7); // location=7
        let buf = make_fe_buf(261, 2, &ad);
        let alloc = get_file_allocation(&buf).unwrap();
        assert_eq!(alloc.total_length, 512);
    }

    // ── parse_udf_name ────────────────────────────────────────────────────────

    #[test]
    fn udf_name_cs0_8bit() {
        let data = [8u8, b'h', b'i'];
        assert_eq!(parse_udf_name(&data), "hi");
    }

    #[test]
    fn udf_name_cs0_16bit() {
        // compression_id=16, UTF-16 big-endian 'A' (0x0041)
        let data = [16u8, 0x00, 0x41];
        assert_eq!(parse_udf_name(&data), "A");
    }

    #[test]
    fn udf_name_fallback_raw() {
        // Unknown compression ID → raw UTF-8 lossy
        let data = [42u8, b'x', b'y'];
        let name = parse_udf_name(&data);
        assert!(!name.is_empty());
    }

    #[test]
    fn udf_name_empty() {
        assert_eq!(parse_udf_name(&[]), "");
    }

    // ── parse_udf error paths ─────────────────────────────────────────────────

    #[test]
    fn parse_udf_rejects_non_udf() {
        let mut c = Cursor::new(vec![0u8; 4096]);
        assert!(parse_udf(&mut c).is_err());
    }

    #[test]
    fn parse_udf_no_avdp_after_vrs() {
        // Image has VRS markers but no AVDP anywhere
        let mut img = vec![0u8; S * 270];
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"BEA01");
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"NSR03");
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"TEA01");
        // No AVDP written → all sector tag_ids remain 0
        let mut c = Cursor::new(img);
        let err = parse_udf(&mut c).unwrap_err();
        assert!(err.to_string().contains("Anchor") || err.to_string().contains("AVDP"));
    }

    #[test]
    fn parse_udf_no_lvd_returns_err() {
        // VRS + AVDP pointing to a VDS with only a PD, no LVD
        let mut img = vec![0u8; S * 270];
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"BEA01");
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"NSR02");
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"TEA01");
        // AVDP at sector 256
        let avdp = 256 * S;
        w16(&mut img, avdp, 2);
        w32(&mut img, avdp + 16, (2 * S) as u32);
        w32(&mut img, avdp + 20, 257);
        // PD only (no LVD)
        w16(&mut img, 257 * S, 5);
        w16(&mut img, 257 * S + 22, 0);
        w32(&mut img, 257 * S + 188, 260);
        // TD
        w16(&mut img, 258 * S, 8);
        let mut c = Cursor::new(img);
        assert!(parse_udf(&mut c).is_err());
    }

    // ── parse_udf verbose ─────────────────────────────────────────────────────

    #[test]
    fn parse_udf_verbose_non_udf() {
        let mut c = Cursor::new(vec![0u8; 4096]);
        // verbose=true exercises the eprintln! path
        assert!(parse_udf_verbose(&mut c, true).is_err());
    }

    // ── Full happy-path parse ─────────────────────────────────────────────────

    #[test]
    fn parse_udf_synthetic_image_root_found() {
        let img = make_udf_image();
        let mut c = Cursor::new(img);
        let root = parse_udf(&mut c).expect("parse should succeed");
        assert_eq!(root.name, "/");
        assert!(root.is_directory);
    }

    #[test]
    fn parse_udf_synthetic_finds_hello_txt() {
        let img = make_udf_image();
        let mut c = Cursor::new(img);
        let root = parse_udf(&mut c).expect("parse should succeed");
        let node = root.find_node("/hello.txt");
        assert!(node.is_some(), "hello.txt should be in root");
    }

    #[test]
    fn parse_udf_verbose_synthetic_image() {
        let img = make_udf_image();
        let mut c = Cursor::new(img);
        // verbose=true exercises all eprintln! branches in the happy path
        let root = parse_udf_verbose(&mut c, true).expect("parse should succeed");
        assert_eq!(root.name, "/");
    }

    // ── Additional FID / parse_directory coverage ─────────────────────────────

    /// Build a UDF image whose root directory FID buffer contains:
    ///  - a zero-tag entry (4 bytes of zeros → tag_id==0 → skip 4 bytes)
    ///  - a deleted file FID (file_characteristics & 0x04)
    ///  - a valid "hello.txt" FID
    fn make_udf_image_edge_fids() -> Vec<u8> {
        let mut img = make_udf_image();

        // Build the replacement FID buffer
        let mut fids: Vec<u8> = Vec::new();

        // 1) zero-tag entry: 4 zero bytes (tag_id=0 → skip path)
        fids.extend_from_slice(&[0u8; 4]);

        // 2) parent FID (file_characteristics=0x08)
        let mut parent = vec![0u8; 40];
        let mut tmp = parent.clone();
        w16(&mut tmp, 0, 257);
        tmp[18] = 0x08;
        parent.copy_from_slice(&tmp);
        fids.extend_from_slice(&parent);

        // 3) deleted file FID (file_characteristics=0x04)
        let del_name: Vec<u8> = {
            let mut n = vec![8u8];
            n.extend_from_slice(b"deleted.txt");
            n
        };
        let del_len = del_name.len() as u8;
        let del_total = 38 + del_len as usize;
        let del_padded = (del_total + 3) & !3;
        let mut del_fid = vec![0u8; del_padded];
        w16(&mut del_fid, 0, 257);
        del_fid[18] = 0x04; // DELETED
        del_fid[19] = del_len;
        del_fid[38..38 + del_name.len()].copy_from_slice(&del_name);
        fids.extend_from_slice(&del_fid);

        // 4) valid "hello.txt" FID → ICB points to sector 262 (location=2)
        let name_raw: Vec<u8> = {
            let mut n = vec![8u8];
            n.extend_from_slice(b"hello.txt");
            n
        };
        let len_fi = name_raw.len() as u8;
        let total_unpadded = 38 + len_fi as usize;
        let padded = (total_unpadded + 3) & !3;
        let mut file_fid = vec![0u8; padded];
        w16(&mut file_fid, 0, 257);
        file_fid[18] = 0x00;
        file_fid[19] = len_fi;
        w32(&mut file_fid, 20, 512);
        w32(&mut file_fid, 24, 2); // location = 2 → sector 262
        w16(&mut file_fid, 28, 0);
        file_fid[38..38 + name_raw.len()].copy_from_slice(&name_raw);
        fids.extend_from_slice(&file_fid);

        // Replace the root FE's inline FID data at sector 261
        let rfe = 261 * S;
        w16(&mut img, rfe, 261);
        w16(&mut img, rfe + 18, 3); // inline
        w32(&mut img, rfe + 172, fids.len() as u32);
        img[rfe + 176..rfe + 176 + fids.len()].copy_from_slice(&fids);

        img
    }

    #[test]
    fn parse_udf_deleted_fid_skipped() {
        let img = make_udf_image_edge_fids();
        let mut c = Cursor::new(img);
        let root = parse_udf(&mut c).expect("parse should succeed");
        // "deleted.txt" should NOT appear; "hello.txt" should
        assert!(root.find_node("/deleted.txt").is_none());
        assert!(root.find_node("/hello.txt").is_some());
    }

    #[test]
    fn parse_udf_deleted_fid_verbose() {
        let img = make_udf_image_edge_fids();
        let mut c = Cursor::new(img);
        // verbose=true hits the eprintln! paths for zero-tag skip and found-file
        let root = parse_udf_verbose(&mut c, true).expect("parse should succeed");
        assert_eq!(root.name, "/");
    }

    /// Build a UDF image where a file FID's ICB points to a sector whose
    /// tag_id is 0 (not a valid File Entry). This causes get_file_info to
    /// return Err → parse_directory emits a zero-size TreeNode.
    fn make_udf_image_bad_file_entry() -> Vec<u8> {
        let mut img = make_udf_image();

        // Build a FID buffer: parent + one file FID pointing to sector 265
        // (location=5 within partition 0, which starts at sector 260).
        // Sector 265 is all-zero → tag_id=0 → get_file_allocation returns Err.
        let mut fids: Vec<u8> = Vec::new();

        // parent FID
        let mut parent = vec![0u8; 40];
        w16(&mut parent, 0, 257);
        parent[18] = 0x08;
        fids.extend_from_slice(&parent);

        // file FID pointing to sector 265 (location=5 in partition starting at 260)
        let name_raw: Vec<u8> = {
            let mut n = vec![8u8];
            n.extend_from_slice(b"badfile.txt");
            n
        };
        let len_fi = name_raw.len() as u8;
        let total_unpadded = 38 + len_fi as usize;
        let padded = (total_unpadded + 3) & !3;
        let mut file_fid = vec![0u8; padded];
        w16(&mut file_fid, 0, 257);
        file_fid[18] = 0x00;
        file_fid[19] = len_fi;
        w32(&mut file_fid, 20, 512);
        w32(&mut file_fid, 24, 5); // location=5 → sector 265, which is zero
        w16(&mut file_fid, 28, 0);
        file_fid[38..38 + name_raw.len()].copy_from_slice(&name_raw);
        fids.extend_from_slice(&file_fid);

        // Replace root FE inline FID data
        let rfe = 261 * S;
        w16(&mut img, rfe, 261);
        w16(&mut img, rfe + 18, 3);
        w32(&mut img, rfe + 172, fids.len() as u32);
        img[rfe + 176..rfe + 176 + fids.len()].copy_from_slice(&fids);

        img
    }

    #[test]
    fn parse_udf_bad_file_entry_emits_zero_size_node() {
        let img = make_udf_image_bad_file_entry();
        let mut c = Cursor::new(img);
        let root = parse_udf(&mut c).expect("parse should succeed even with bad file entry");
        // "badfile.txt" should appear as a zero-size node (get_file_info error fallback)
        let node = root.find_node("/badfile.txt");
        assert!(node.is_some(), "bad file entry should still emit a node");
        assert_eq!(node.unwrap().size, 0);
    }

    #[test]
    fn parse_udf_bad_file_entry_verbose() {
        let img = make_udf_image_bad_file_entry();
        let mut c = Cursor::new(img);
        // verbose=true hits the "Warning: Failed to get file extent" eprintln!
        let root = parse_udf_verbose(&mut c, true).expect("parse should succeed");
        assert_eq!(root.name, "/");
    }

    /// Build a UDF image where the primary FSD location has tag_id=0
    /// but the FSD is found one sector later. This exercises the "scan nearby"
    /// branch in parse_udf_verbose (lines ~394-424).
    fn make_udf_image_fsd_at_offset() -> Vec<u8> {
        let mut img = vec![0u8; S * 280];

        // VRS
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"BEA01");
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"NSR02");
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"TEA01");

        // AVDP at sector 256
        let avdp = 256 * S;
        w16(&mut img, avdp, 2);
        w32(&mut img, avdp + 16, (3 * S) as u32);
        w32(&mut img, avdp + 20, 257);

        // PD at 257
        w16(&mut img, 257 * S, 5);
        w16(&mut img, 257 * S + 22, 0);
        w32(&mut img, 257 * S + 188, 260);

        // LVD at 258: FSD at location=0 in partition 0
        w16(&mut img, 258 * S, 6);
        w32(&mut img, 258 * S + 248, S as u32);
        w32(&mut img, 258 * S + 252, 0); // location=0 → sector 260
        w16(&mut img, 258 * S + 256, 0);

        // TD at 259
        w16(&mut img, 259 * S, 8);

        // Sector 260 (expected FSD): leave tag_id=0 (empty) → triggers scan
        // Sector 261: the actual FSD (tag_id=256)
        let fsd = 261 * S;
        w16(&mut img, fsd, 256);
        // Root ICB at LBA 262 (location=2 from partition start 260)
        w32(&mut img, fsd + 400, S as u32);
        w32(&mut img, fsd + 404, 2); // location=2 → sector 262
        w16(&mut img, fsd + 408, 0);

        // Root FE at sector 262: inline FID data (empty dir, just parent)
        let rfe = 262 * S;
        w16(&mut img, rfe, 261);
        w16(&mut img, rfe + 18, 3); // inline
        let mut parent = vec![0u8; 40];
        w16(&mut parent, 0, 257);
        parent[18] = 0x08;
        w32(&mut img, rfe + 172, parent.len() as u32);
        img[rfe + 176..rfe + 176 + parent.len()].copy_from_slice(&parent);

        img
    }

    #[test]
    fn parse_udf_fsd_found_via_nearby_scan() {
        let img = make_udf_image_fsd_at_offset();
        let mut c = Cursor::new(img);
        let root = parse_udf(&mut c).expect("FSD nearby scan should succeed");
        assert_eq!(root.name, "/");
    }

    #[test]
    fn parse_udf_fsd_nearby_scan_verbose() {
        let img = make_udf_image_fsd_at_offset();
        let mut c = Cursor::new(img);
        // verbose=true exercises "Tag X at expected FSD location, scanning nearby..." eprintln!
        let root = parse_udf_verbose(&mut c, true).expect("FSD nearby scan should succeed");
        assert_eq!(root.name, "/");
    }

    /// FSD nowhere to be found in the nearby scan → parse_udf returns Err.
    fn make_udf_image_no_fsd() -> Vec<u8> {
        let mut img = vec![0u8; S * 280];
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"BEA01");
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"NSR02");
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"TEA01");
        let avdp = 256 * S;
        w16(&mut img, avdp, 2);
        w32(&mut img, avdp + 16, (3 * S) as u32);
        w32(&mut img, avdp + 20, 257);
        w16(&mut img, 257 * S, 5);
        w16(&mut img, 257 * S + 22, 0);
        w32(&mut img, 257 * S + 188, 260);
        w16(&mut img, 258 * S, 6);
        w32(&mut img, 258 * S + 248, S as u32);
        w32(&mut img, 258 * S + 252, 0); // location=0 → sector 260
        w16(&mut img, 258 * S + 256, 0);
        w16(&mut img, 259 * S, 8);
        // Sectors 260..292 all tag_id=0 → scan fails
        img
    }

    #[test]
    fn parse_udf_no_fsd_returns_err() {
        let img = make_udf_image_no_fsd();
        let mut c = Cursor::new(img);
        assert!(parse_udf(&mut c).is_err());
    }

    /// Image with an extent-based file entry (ad_type=0, short ADs) rather
    /// than inline data. Exercises the `alloc.extents.first() → Some` branch
    /// in parse_directory, which emits a TreeNode with a file_location.
    fn make_udf_image_extent_file() -> Vec<u8> {
        let mut img = vec![0u8; S * 280];

        // VRS
        img[16 * S + 1..16 * S + 6].copy_from_slice(b"BEA01");
        img[17 * S + 1..17 * S + 6].copy_from_slice(b"NSR02");
        img[18 * S + 1..18 * S + 6].copy_from_slice(b"TEA01");

        // AVDP at sector 256
        let avdp = 256 * S;
        w16(&mut img, avdp, 2);
        w32(&mut img, avdp + 16, (3 * S) as u32);
        w32(&mut img, avdp + 20, 257);

        // PD: partition 0 starts at sector 260
        w16(&mut img, 257 * S, 5);
        w16(&mut img, 257 * S + 22, 0);
        w32(&mut img, 257 * S + 188, 260);

        // LVD: FSD at location=0 in partition 0
        w16(&mut img, 258 * S, 6);
        w32(&mut img, 258 * S + 248, S as u32);
        w32(&mut img, 258 * S + 252, 0);
        w16(&mut img, 258 * S + 256, 0);

        // TD
        w16(&mut img, 259 * S, 8);

        // FSD at sector 260 (location=0 in partition)
        let fsd = 260 * S;
        w16(&mut img, fsd, 256);
        // Root ICB: location=1 → sector 261
        w32(&mut img, fsd + 400, S as u32);
        w32(&mut img, fsd + 404, 1);
        w16(&mut img, fsd + 408, 0);

        // Root FE at sector 261: inline FID data
        let rfe = 261 * S;
        w16(&mut img, rfe, 261);
        w16(&mut img, rfe + 18, 3); // inline

        // Build FID: parent + one file FID pointing to location=3 → sector 263
        let mut fids: Vec<u8> = Vec::new();

        let mut parent = vec![0u8; 40];
        w16(&mut parent, 0, 257);
        parent[18] = 0x08;
        fids.extend_from_slice(&parent);

        let name_raw: Vec<u8> = {
            let mut n = vec![8u8];
            n.extend_from_slice(b"data.bin");
            n
        };
        let len_fi = name_raw.len() as u8;
        let total_unpadded = 38 + len_fi as usize;
        let padded = (total_unpadded + 3) & !3;
        let mut file_fid = vec![0u8; padded];
        w16(&mut file_fid, 0, 257);
        file_fid[18] = 0x00;
        file_fid[19] = len_fi;
        w32(&mut file_fid, 20, S as u32);
        w32(&mut file_fid, 24, 3); // location=3 → sector 263
        w16(&mut file_fid, 28, 0);
        file_fid[38..38 + name_raw.len()].copy_from_slice(&name_raw);
        fids.extend_from_slice(&file_fid);

        w32(&mut img, rfe + 172, fids.len() as u32);
        img[rfe + 176..rfe + 176 + fids.len()].copy_from_slice(&fids);

        // File FE at sector 263: short AD (ad_type=0) pointing to sector 264 (location=4)
        let fe = 263 * S;
        w16(&mut img, fe, 261); // tag_id = 261 File Entry
        w16(&mut img, fe + 18, 0); // ICB flags: ad_type=0 (short ADs)
                                   // EA length=0 (offset 168)
                                   // AD length=8 (one short AD), offset 172
                                   // Short AD at offset 176: length=512, location=4
        let file_len: u32 = 512;
        w32(&mut img, fe + 172, 8u32); // AD length = 8 bytes (one short AD)
        w32(&mut img, fe + 176, file_len); // raw_length = 512 (type=0, length=512)
        w32(&mut img, fe + 180, 4); // location = 4 → sector 264

        // Sector 264: the actual file data
        let data_sector = 264 * S;
        img[data_sector..data_sector + 512].fill(0xAB);

        img
    }

    #[test]
    fn parse_udf_extent_based_file_has_location() {
        let img = make_udf_image_extent_file();
        let mut c = Cursor::new(img);
        let root = parse_udf(&mut c).expect("parse should succeed");
        let node = root.find_node("/data.bin");
        assert!(node.is_some(), "data.bin should exist");
        let node = node.unwrap();
        assert_eq!(node.size, 512);
        // The file_location should be set (extent-based, not inline)
        assert!(
            node.file_location.is_some(),
            "extent-based file should have file_location"
        );
        // Byte offset = (partition_start=260 + location=4) * 2048 = 264 * 2048
        assert_eq!(node.file_location.unwrap(), 264 * 2048);
    }

    /// Test that `get_file_allocation` with inline data where `ad_offset >= end`
    /// (empty inline data) returns an error (no extents, no inline_data).
    #[test]
    fn file_alloc_empty_inline_data_returns_err() {
        // ad_type=3 (inline) but AD length=0 → end==ad_offset → inline_data=None
        let buf = make_fe_buf(261, 3, &[]);
        assert!(
            get_file_allocation(&buf).is_err(),
            "empty inline data should error (no extents, no inline_data)"
        );
    }

    /// Verify `file_alloc_short_ad_sparse_skipped` also exercises the
    /// `extent_type == 3` (next-extent-of-ADs) early break.
    #[test]
    fn file_alloc_short_ad_next_extent_type_breaks() {
        let mut ad = vec![0u8; 8];
        let raw = (3u32 << 30) | 512; // extent_type=3, length=512
        w32(&mut ad, 0, raw);
        w32(&mut ad, 4, 7);
        let buf = make_fe_buf(261, 0, &ad);
        // Type 3 means "next extent of ADs" — break immediately → no extents → Err
        assert!(get_file_allocation(&buf).is_err());
    }

    /// Same for long ADs (ad_type=1): extent_type=3 should break.
    #[test]
    fn file_alloc_long_ad_next_extent_type_breaks() {
        let mut ad = vec![0u8; 16];
        let raw = (3u32 << 30) | 512;
        w32(&mut ad, 0, raw);
        w32(&mut ad, 4, 7);
        let buf = make_fe_buf(261, 1, &ad);
        assert!(get_file_allocation(&buf).is_err());
    }

    /// Long AD with extent_type=1 (sparse): skipped → no extents → Err.
    #[test]
    fn file_alloc_long_ad_sparse_skipped() {
        let mut ad = vec![0u8; 16];
        let raw = (1u32 << 30) | 512;
        w32(&mut ad, 0, raw);
        w32(&mut ad, 4, 7);
        let buf = make_fe_buf(261, 1, &ad);
        assert!(get_file_allocation(&buf).is_err());
    }
}
