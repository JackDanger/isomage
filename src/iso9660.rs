type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use crate::tree::TreeNode;

const SECTOR_SIZE: u64 = 2048;
const PRIMARY_VOLUME_DESCRIPTOR_SECTOR: u64 = 16;


#[derive(Debug, Clone)]
pub struct DirectoryRecord {
    pub extent_location: u32,
    pub data_length: u32,
    pub is_directory: bool,
    pub filename: String,
}

pub fn parse_iso9660(file: &mut File) -> Result<TreeNode> {
    parse_iso9660_verbose(file, false)
}

pub fn parse_iso9660_verbose(file: &mut File, verbose: bool) -> Result<TreeNode> {
    // Read Primary Volume Descriptor
    if let Err(e) = file.seek(SeekFrom::Start(PRIMARY_VOLUME_DESCRIPTOR_SECTOR * SECTOR_SIZE)) {
        return Err(format!("Failed to seek to PVD: {}", e).into());
    }
    
    let mut buffer = vec![0u8; SECTOR_SIZE as usize];
    if let Err(e) = file.read_exact(&mut buffer) {
        return Err(format!("Failed to read PVD: {}", e).into());
    }
    
    // Check for ISO 9660 signature
    if &buffer[1..6] != b"CD001" {
        if verbose {
            println!("  ISO 9660 signature 'CD001' not found at sector {}. Found: {:?}", 
                     PRIMARY_VOLUME_DESCRIPTOR_SECTOR, String::from_utf8_lossy(&buffer[1..6]));
        }
        return Err("Not a valid ISO 9660 filesystem".into());
    }
    
    if verbose { println!("  Found ISO 9660 signature at sector {}", PRIMARY_VOLUME_DESCRIPTOR_SECTOR); }
    
    // Parse root directory record (starts at offset 156)
    let root_record = parse_directory_record(&buffer[156..])?;
    if verbose { println!("  Root directory at sector {}, size {} bytes", root_record.extent_location, root_record.data_length); }
    
    // Parse the root directory
    let mut root_node = TreeNode::new_directory("/".to_string());
    parse_directory(file, &root_record, &mut root_node, verbose)?;
    
    root_node.calculate_directory_size();
    Ok(root_node)
}

fn parse_directory_record(data: &[u8]) -> Result<DirectoryRecord> {
    if data.len() < 33 {
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
    
    let is_directory = (file_flags & 0x02) != 0;
    
    let filename = if filename_length == 0 {
        ".".to_string()
    } else if filename_length == 1 && data[33] == 0 {
        ".".to_string()
    } else if filename_length == 1 && data[33] == 1 {
        "..".to_string()
    } else {
        let raw_name = String::from_utf8_lossy(&data[33..33 + filename_length]);
        // Remove ISO 9660 version suffix (;1, ;2, etc.) and trailing periods
        let cleaned_name = if let Some(semicolon_pos) = raw_name.find(';') {
            &raw_name[..semicolon_pos]
        } else {
            &raw_name
        };
        cleaned_name.trim_end_matches('.').to_lowercase()
    };
    
    Ok(DirectoryRecord {
        extent_location,
        data_length,
        is_directory,
        filename,
    })
}

fn parse_directory(file: &mut File, dir_record: &DirectoryRecord, parent_node: &mut TreeNode, verbose: bool) -> Result<()> {
    if !dir_record.is_directory || dir_record.data_length == 0 {
        return Ok(());
    }
    
    // Seek to the directory's data
    file.seek(SeekFrom::Start(dir_record.extent_location as u64 * SECTOR_SIZE))?;
    
    let mut buffer = vec![0u8; dir_record.data_length as usize];
    file.read_exact(&mut buffer)?;
    
    let mut offset = 0;
    while offset < buffer.len() {
        if buffer[offset] == 0 {
            // Skip padding
            offset += 1;
            continue;
        }
        
        let record_length = buffer[offset] as usize;
        if record_length == 0 || offset + record_length > buffer.len() {
            break;
        }
        
        if let Ok(record) = parse_directory_record(&buffer[offset..]) {
            // Skip "." and ".." entries
            if record.filename != "." && record.filename != ".." {
                if verbose { println!("    Found {}: {}", if record.is_directory { "dir" } else { "file" }, record.filename); }
                if record.is_directory {
                    let mut dir_node = TreeNode::new_directory(record.filename.clone());
                    parse_directory(file, &record, &mut dir_node, verbose)?;
                    parent_node.add_child(dir_node);
                } else {
                    let file_node = TreeNode::new_file_with_location(
                        record.filename.clone(), 
                        record.data_length as u64,
                        record.extent_location as u64 * SECTOR_SIZE,
                        record.data_length as u64
                    );
                    parent_node.add_child(file_node);
                }
            }
        }
        
        offset += record_length;
    }
    
    Ok(())
}
