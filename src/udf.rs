type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use crate::tree::TreeNode;

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
    file_location: u32,      // Location of metadata file in main partition
    partition_ref: u16,      // Which partition the metadata file is in
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

pub fn parse_udf(file: &mut File) -> Result<TreeNode> {
    parse_udf_verbose(file, false)
}

pub fn parse_udf_verbose(file: &mut File, verbose: bool) -> Result<TreeNode> {
    // Check for UDF markers in the Volume Recognition Sequence (sectors 16-31)
    let mut found_udf_marker = false;
    if verbose { println!("Scanning sectors 16-31 for UDF Volume Recognition Sequence..."); }
    for sector in 16..32 {
        if let Err(_) = file.seek(SeekFrom::Start(sector * SECTOR_SIZE)) {
            continue;
        }
        let mut buffer = [0u8; 16];
        if let Err(_) = file.read_exact(&mut buffer) {
            continue;
        }
        
        let id = &buffer[1..6];
        if id == b"NSR02" || id == b"NSR03" || id == b"BEA01" || id == b"TEA01" {
            if verbose { println!("  Found UDF marker '{:?}' at sector {}", String::from_utf8_lossy(id), sector); }
            found_udf_marker = true;
            break;
        }
    }

    if !found_udf_marker {
        return Err("Not a valid UDF filesystem (no VRS markers found)".into());
    }

    // Try to find the Anchor Volume Descriptor Pointer (AVDP) at sector 256
    if verbose { println!("Looking for Anchor Volume Descriptor Pointer at sector 256..."); }
    file.seek(SeekFrom::Start(256 * SECTOR_SIZE))?;
    let mut avdp_buffer = [0u8; 512];
    file.read_exact(&mut avdp_buffer)?;
    
    let tag_id = u16::from_le_bytes([avdp_buffer[0], avdp_buffer[1]]);
    if tag_id != 2 {
        if verbose { println!("  AVDP not found at sector 256 (tag id: {})", tag_id); }
        return Err("UDF detected but AVDP not found at sector 256.".into());
    }

    let main_vds_extent = read_extent_ad(&avdp_buffer[16..24]);
    if verbose { println!("  Found AVDP. Main VDS at sector {}, length {}", main_vds_extent.location, main_vds_extent.length); }
    
    // Collect partition info and parse LVD
    let mut partitions: Vec<PartitionInfo> = Vec::new();
    let mut root_fsd_long_ad = None;
    let mut metadata_partition: Option<MetadataPartitionInfo> = None;
    
    let mut sector = main_vds_extent.location as u64;
    let end_sector = sector + (main_vds_extent.length as u64 + SECTOR_SIZE - 1) / SECTOR_SIZE;
    
    if verbose { println!("Parsing Main Volume Descriptor Sequence (sectors {} to {})...", sector, end_sector); }
    while sector < end_sector {
        file.seek(SeekFrom::Start(sector * SECTOR_SIZE))?;
        let mut vds_buffer = vec![0u8; SECTOR_SIZE as usize];
        file.read_exact(&mut vds_buffer)?;
        
        let vds_tag_id = u16::from_le_bytes([vds_buffer[0], vds_buffer[1]]);
        
        match vds_tag_id {
            5 => { // Partition Descriptor
                let part_num = u16::from_le_bytes([vds_buffer[22], vds_buffer[23]]);
                let part_start = u32::from_le_bytes([vds_buffer[188], vds_buffer[189], vds_buffer[190], vds_buffer[191]]) as u64;
                if verbose { println!("  Found Partition Descriptor #{}: starts at sector {}", part_num, part_start); }
                partitions.push(PartitionInfo { number: part_num, start_sector: part_start });
            }
            6 => { // Logical Volume Descriptor
                // FSD location at offset 248
                root_fsd_long_ad = Some(read_long_ad(&vds_buffer[248..264]));
                if verbose { 
                    let ad = root_fsd_long_ad.unwrap();
                    println!("  Found Logical Volume Descriptor. FSD at location {} in partition {}", ad.location, ad.partition); 
                }
                
                // Parse partition maps to find metadata partition
                let map_table_length = u32::from_le_bytes([vds_buffer[264], vds_buffer[265], vds_buffer[266], vds_buffer[267]]) as usize;
                let num_partition_maps = u32::from_le_bytes([vds_buffer[268], vds_buffer[269], vds_buffer[270], vds_buffer[271]]);
                if verbose { println!("    {} partition maps, table length {} bytes", num_partition_maps, map_table_length); }
                
                // Partition maps start at offset 440
                let mut map_offset = 440usize;
                for map_idx in 0..num_partition_maps {
                    if map_offset >= vds_buffer.len() { break; }
                    let map_type = vds_buffer[map_offset];
                    let map_length = vds_buffer[map_offset + 1] as usize;
                    
                    if verbose { println!("    Partition map {}: type {}, length {}", map_idx, map_type, map_length); }
                    
                    if map_type == 2 && map_length >= 64 {
                        // Type 2 partition map - check if it's a Metadata Partition
                        // EntityIdentifier structure: Flags(1) + Identifier(23) + Suffix(8) = 32 bytes starting at offset +4
                        // The actual identifier string starts at offset +5 (after flags byte)
                        let id_string = &vds_buffer[map_offset + 5..map_offset + 28]; // 23-byte identifier
                        
                        if verbose {
                            let id_printable: String = id_string.iter()
                                .take_while(|&&b| b != 0)
                                .map(|&b| if b >= 0x20 && b < 0x7f { b as char } else { '.' })
                                .collect();
                            println!("      Type 2 identifier: '{}'", id_printable);
                        }
                        
                        if id_string.starts_with(b"*UDF Metadata Partition") {
                            // Metadata partition map structure:
                            // +36: Volume Sequence Number (2 bytes)
                            // +38: Partition Number (2 bytes) - which physical partition contains metadata file
                            // +40: Metadata File Location (4 bytes)
                            let meta_part_ref = u16::from_le_bytes([vds_buffer[map_offset + 38], vds_buffer[map_offset + 39]]);
                            let meta_file_loc = u32::from_le_bytes([vds_buffer[map_offset + 40], vds_buffer[map_offset + 41], 
                                                                    vds_buffer[map_offset + 42], vds_buffer[map_offset + 43]]);
                            if verbose { 
                                println!("      Metadata Partition: file at location {} in partition {}", meta_file_loc, meta_part_ref); 
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
                if verbose { println!("  Found Terminating Descriptor at sector {}", sector); }
                break;
            }
            _ => {}
        }
        sector += 1;
    }

    let fsd_long_ad = root_fsd_long_ad.ok_or("Failed to find File Set Descriptor location in LVD")?;
    
    // Find the partition that the FSD references
    let fsd_partition_ref = fsd_long_ad.partition;
    
    // Determine where to read the FSD from
    let (fsd_sector, partition_start) = if let Some(ref meta_info) = metadata_partition {
        // FSD is in metadata partition - need to read through metadata file
        if verbose { println!("FSD is in metadata partition, reading via metadata file..."); }
        
        // Find the physical partition that contains the metadata file
        let meta_phys_partition = partitions.iter()
            .find(|p| p.number == meta_info.partition_ref)
            .ok_or("Cannot find physical partition for metadata file")?;
        
        // Read the metadata file's File Entry to get its extent
        let meta_fe_sector = meta_phys_partition.start_sector + meta_info.file_location as u64;
        if verbose { println!("  Metadata File Entry at sector {}", meta_fe_sector); }
        
        file.seek(SeekFrom::Start(meta_fe_sector * SECTOR_SIZE))?;
        let mut meta_fe_buffer = vec![0u8; SECTOR_SIZE as usize];
        file.read_exact(&mut meta_fe_buffer)?;
        
        let meta_tag_id = u16::from_le_bytes([meta_fe_buffer[0], meta_fe_buffer[1]]);
        if verbose { println!("  Metadata FE tag: {}", meta_tag_id); }
        
        let meta_extent = if meta_tag_id == 261 { // File Entry
            read_extent_ad(&meta_fe_buffer[184..192])
        } else if meta_tag_id == 266 { // Extended File Entry
            read_extent_ad(&meta_fe_buffer[216..224])
        } else {
            return Err(format!("Unexpected metadata file entry tag: {}", meta_tag_id).into());
        };
        
        if verbose { println!("  Metadata file extent: location {}, length {}", meta_extent.location, meta_extent.length); }
        
        // The FSD is at offset (fsd_long_ad.location * block_size) within the metadata file
        // The metadata file's data starts at meta_extent.location in the physical partition
        let metadata_data_sector = meta_phys_partition.start_sector + meta_extent.location as u64;
        let fsd_offset_in_metadata = fsd_long_ad.location as u64; // In blocks (sectors)
        
        (metadata_data_sector + fsd_offset_in_metadata, metadata_data_sector)
    } else {
        // Direct partition access
        let partition = partitions.iter()
            .find(|p| p.number == fsd_partition_ref)
            .or_else(|| partitions.first())
            .ok_or("No partition found")?;
        
        (partition.start_sector + fsd_long_ad.location as u64, partition.start_sector)
    };
    
    if verbose { println!("Reading File Set Descriptor at sector {}...", fsd_sector); }
    file.seek(SeekFrom::Start(fsd_sector * SECTOR_SIZE))?;
    let mut fsd_buffer = [0u8; 512];
    file.read_exact(&mut fsd_buffer)?;
    
    let fsd_tag_id = u16::from_le_bytes([fsd_buffer[0], fsd_buffer[1]]);
    if fsd_tag_id != 256 {
        // Try scanning nearby sectors for FSD
        if verbose { println!("  Tag {} at expected FSD location, scanning nearby...", fsd_tag_id); }
        let mut found_fsd = false;
        for offset in 1..32 {
            file.seek(SeekFrom::Start((fsd_sector + offset) * SECTOR_SIZE))?;
            file.read_exact(&mut fsd_buffer)?;
            let tag = u16::from_le_bytes([fsd_buffer[0], fsd_buffer[1]]);
            if tag == 256 {
                if verbose { println!("  Found FSD at sector {} (offset +{})", fsd_sector + offset, offset); }
                found_fsd = true;
                break;
            }
        }
        if !found_fsd {
            return Err(format!("Invalid File Set Descriptor tag: expected 256, found {}", fsd_tag_id).into());
        }
    }
    
    let root_icb_long_ad = read_long_ad(&fsd_buffer[400..416]);
    if verbose { println!("  Found FSD. Root ICB at location {} in partition {}", root_icb_long_ad.location, root_icb_long_ad.partition); }
    
    let mut root_node = TreeNode::new_directory("/".to_string());
    if verbose { println!("Parsing root directory..."); }
    parse_directory(file, partition_start, &root_icb_long_ad, &mut root_node, verbose)?;
    
    root_node.calculate_directory_size();
    Ok(root_node)
}

fn parse_directory(file: &mut File, partition_start: u64, icb_long_ad: &LongAd, parent_node: &mut TreeNode, verbose: bool) -> Result<()> {
    let extent = get_file_extent(file, partition_start, icb_long_ad)?;
    
    if verbose { println!("  Directory extent: location {}, length {}", extent.location, extent.length); }
    
    file.seek(SeekFrom::Start((partition_start + extent.location as u64) * SECTOR_SIZE))?;
    let mut buffer = vec![0u8; extent.length as usize];
    file.read_exact(&mut buffer)?;
    
    let mut offset = 0;
    while offset < buffer.len() {
        if offset + 40 > buffer.len() { break; }
        
        let tag_id = u16::from_le_bytes([buffer[offset], buffer[offset+1]]);
        if tag_id == 0 {
            offset += 4; // Skip padding in 4-byte increments
            continue;
        }
        
        if tag_id != 257 { // File Identifier Descriptor
            if verbose { println!("    Warning: Expected FID (257) at offset {}, found {}", offset, tag_id); }
            break;
        }
        
        let file_characteristics = buffer[offset + 18];
        let length_of_fi = buffer[offset + 19] as usize;
        let icb = read_long_ad(&buffer[offset + 20..offset + 36]);
        let length_of_iu = u16::from_le_bytes([buffer[offset + 36], buffer[offset + 37]]) as usize;
        
        let name_offset = offset + 38 + length_of_iu;
        if name_offset + length_of_fi > buffer.len() {
             if verbose { println!("    Warning: FID name offset out of bounds at offset {}", offset); }
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
            if verbose { println!("    Found {}: {}", if is_directory { "dir" } else { "file" }, name); }
            if is_directory {
                let mut dir_node = TreeNode::new_directory(name);
                if let Err(e) = parse_directory(file, partition_start, &icb, &mut dir_node, verbose) {
                    if verbose { println!("      Warning: Failed to parse subdirectory: {}", e); }
                }
                parent_node.add_child(dir_node);
            } else {
                match get_file_extent(file, partition_start, &icb) {
                    Ok(file_extent) => {
                        let file_node = TreeNode::new_file_with_location(
                            name,
                            file_extent.length as u64,
                            (partition_start + file_extent.location as u64) * SECTOR_SIZE,
                            file_extent.length as u64
                        );
                        parent_node.add_child(file_node);
                    }
                    Err(e) => {
                        if verbose { println!("      Warning: Failed to get file extent: {}", e); }
                        // Add file with unknown size
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

fn get_file_extent(file: &mut File, partition_start: u64, icb_long_ad: &LongAd) -> Result<ExtentAd> {
    file.seek(SeekFrom::Start((partition_start + icb_long_ad.location as u64) * SECTOR_SIZE))?;
    let mut fe_buffer = vec![0u8; SECTOR_SIZE as usize];
    file.read_exact(&mut fe_buffer)?;
    
    let tag_id = u16::from_le_bytes([fe_buffer[0], fe_buffer[1]]);
    
    if tag_id == 261 { // File Entry
        // ICB Tag at offset 16, Allocation Descriptors Length at offset 176
        // Extended Attributes Length at offset 168
        let ea_length = u32::from_le_bytes([fe_buffer[168], fe_buffer[169], fe_buffer[170], fe_buffer[171]]) as usize;
        let ad_offset = 176 + ea_length;
        if ad_offset + 8 <= fe_buffer.len() {
            Ok(read_extent_ad(&fe_buffer[ad_offset..ad_offset + 8]))
        } else {
            // Fallback to fixed offset
            Ok(read_extent_ad(&fe_buffer[184..192]))
        }
    } else if tag_id == 266 { // Extended File Entry
        // Extended Attributes Length at offset 208, AD offset varies
        let ea_length = u32::from_le_bytes([fe_buffer[208], fe_buffer[209], fe_buffer[210], fe_buffer[211]]) as usize;
        let ad_offset = 216 + ea_length;
        if ad_offset + 8 <= fe_buffer.len() {
            Ok(read_extent_ad(&fe_buffer[ad_offset..ad_offset + 8]))
        } else {
            // Fallback to fixed offset
            Ok(read_extent_ad(&fe_buffer[216..224]))
        }
    } else {
        Err(format!("Unsupported ICB tag: {}", tag_id).into())
    }
}

fn parse_udf_name(data: &[u8]) -> String {
    if data.is_empty() { return String::new(); }
    
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
