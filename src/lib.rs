pub mod iso9660;
pub mod udf;
pub mod tree;

pub use tree::TreeNode;

use std::fs::{File, create_dir_all};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub fn detect_and_parse_filesystem(file: &mut File, filename: &str) -> Result<TreeNode, Box<dyn std::error::Error>> {
    // For now, we'll use a simple environment variable or just internal logic
    // but the user wants a -v flag. Let's add it to the function signature.
    detect_and_parse_filesystem_verbose(file, filename, false)
}

pub fn detect_and_parse_filesystem_verbose(file: &mut File, filename: &str, verbose: bool) -> Result<TreeNode, Box<dyn std::error::Error>> {
    let mut errors = Vec::new();

    if verbose {
        // Show file size
        let file_size = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        println!("File size: {} bytes ({:.2} GB)", file_size, file_size as f64 / (1024.0 * 1024.0 * 1024.0));
        
        // Show what's at key sectors
        println!("Scanning key sectors for filesystem signatures...");
        for (sector, desc) in [(16, "ISO 9660 PVD / UDF VRS"), (17, "UDF VRS"), (256, "UDF AVDP")].iter() {
            file.seek(SeekFrom::Start(*sector as u64 * 2048))?;
            let mut buf = [0u8; 32];
            if file.read_exact(&mut buf).is_ok() {
                let printable: String = buf.iter().map(|&b| if b >= 0x20 && b < 0x7f { b as char } else { '.' }).collect();
                println!("  Sector {:>3} ({}): {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}  |{}|",
                    sector, desc, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], &printable[..8]);
            }
        }
        file.seek(SeekFrom::Start(0))?;
    }

    if verbose { println!("\nAttempting ISO 9660 parsing..."); }
    match iso9660::parse_iso9660_verbose(file, verbose) {
        Ok(root) => return Ok(root),
        Err(e) => {
            if verbose { println!("  ISO 9660 parsing failed: {}", e); }
            errors.push(format!("ISO 9660: {}", e));
        }
    }
    
    // Seek back to start before trying next parser
    file.seek(SeekFrom::Start(0))?;
    if verbose { println!("\nAttempting UDF parsing..."); }
    match udf::parse_udf_verbose(file, verbose) {
        Ok(root) => return Ok(root),
        Err(e) => {
            if verbose { println!("  UDF parsing failed: {}", e); }
            errors.push(format!("UDF: {}", e));
        }
    }
    
    let mut msg = format!("Unable to detect supported filesystem in {}", filename);
    if !errors.is_empty() {
        msg.push_str("\nDetails:\n  - ");
        msg.push_str(&errors.join("\n  - "));
    }
    
    Err(msg.into())
}

pub fn extract_node(file: &mut File, node: &TreeNode, output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    if node.is_directory {
        if node.name == "/" {
            // For root directory, extract children directly to output path
            for child in &node.children {
                if child.is_directory {
                    extract_directory(file, child, output_path)?;
                } else {
                    extract_file(file, child, output_path)?;
                }
            }
        } else {
            extract_directory(file, node, output_path)?;
        }
    } else {
        extract_file(file, node, output_path)?;
    }
    Ok(())
}

const EXTRACT_CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8 MB chunks

fn extract_file(file: &mut File, node: &TreeNode, output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let (Some(location), Some(length)) = (node.file_location, node.file_length) {
        file.seek(SeekFrom::Start(location))?;

        let output_file_path = Path::new(output_path).join(&node.name);
        if let Some(parent) = output_file_path.parent() {
            create_dir_all(parent)?;
        }

        let mut output_file = std::fs::File::create(&output_file_path)?;
        let mut remaining = length as usize;
        let mut buffer = vec![0u8; EXTRACT_CHUNK_SIZE.min(remaining)];

        while remaining > 0 {
            let to_read = EXTRACT_CHUNK_SIZE.min(remaining);
            let buf = &mut buffer[..to_read];
            file.read_exact(buf)?;
            output_file.write_all(buf)?;
            remaining -= to_read;

            // Print progress for large files (> 100 MB)
            if length > 100 * 1024 * 1024 {
                let done = length as usize - remaining;
                eprint!("\r  Extracting {}: {:.1}%", node.name, done as f64 / length as f64 * 100.0);
            }
        }
        if length > 100 * 1024 * 1024 {
            eprintln!();
        }

        println!("Extracted: {}", output_file_path.display());
    } else {
        return Err("File location information not available for extraction".into());
    }
    Ok(())
}

fn extract_directory(file: &mut File, node: &TreeNode, output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir_path = Path::new(output_path).join(&node.name);
    create_dir_all(&dir_path)?;
    println!("Created directory: {}", dir_path.display());
    
    for child in &node.children {
        let dir_path_str = dir_path.to_str()
            .ok_or_else(|| format!("Non-UTF-8 path: {}", dir_path.display()))?;
        if child.is_directory {
            extract_directory(file, child, dir_path_str)?;
        } else {
            extract_file(file, child, dir_path_str)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::Path;

    fn test_file_path(filename: &str) -> String {
        format!("test_data/{}", filename)
    }

    fn require_test_file(name: &str) -> Option<File> {
        let path = test_file_path(name);
        if !Path::new(&path).exists() {
            eprintln!("Skipping test: {} not found (run `make test-data` to generate)", path);
            return None;
        }
        Some(File::open(&path).unwrap_or_else(|_| panic!("Failed to open test file: {}", path)))
    }

    #[test]
    fn test_iso9660_parsing() {
        for test_file in &["test_linux.iso", "test_macos.iso"] {
            if let Some(mut file) = require_test_file(test_file) {
                let root_node = iso9660::parse_iso9660(&mut file)
                    .unwrap_or_else(|e| panic!("ISO 9660 parsing failed for {}: {}", test_file, e));
                assert_eq!(root_node.name, "/");
                assert!(root_node.is_directory);
            }
        }
    }

    #[test]
    fn test_filesystem_detection() {
        for test_file in &["test_linux.iso", "test_macos.iso"] {
            if let Some(mut file) = require_test_file(test_file) {
                let root_node = detect_and_parse_filesystem(&mut file, test_file)
                    .unwrap_or_else(|e| panic!("Filesystem detection failed for {}: {}", test_file, e));
                assert_eq!(root_node.name, "/");
                assert!(root_node.is_directory);
            }
        }
    }

    #[test]
    fn test_tree_structure_validation() {
        if let Some(mut file) = require_test_file("test_linux.iso") {
            let root_node = detect_and_parse_filesystem(&mut file, "test_linux.iso")
                .expect("Failed to parse test_linux.iso");
            validate_tree_structure(&root_node, 0);
        }
    }

    fn validate_tree_structure(node: &TreeNode, depth: usize) {
        assert!(!node.name.is_empty(), "Node name should not be empty");
        
        if depth > 10 {
            panic!("Tree depth exceeded reasonable limit");
        }
        
        if !node.is_directory {
            assert!(node.children.is_empty(), "Files should not have children");
        }
        
        for child in &node.children {
            validate_tree_structure(child, depth + 1);
        }
    }

    #[test]
    fn test_invalid_file_handling() {
        let invalid_path = test_file_path("nonexistent.iso");
        
        assert!(File::open(&invalid_path).is_err(), "Should not be able to open nonexistent file");
    }

    #[test]
    fn test_tree_node_creation() {
        let dir_node = TreeNode::new_directory("test_dir".to_string());
        assert!(dir_node.is_directory);
        assert_eq!(dir_node.name, "test_dir");
        assert_eq!(dir_node.size, 0);
        assert!(dir_node.children.is_empty());

        let file_node = TreeNode::new_file("test_file.txt".to_string(), 1024);
        assert!(!file_node.is_directory);
        assert_eq!(file_node.name, "test_file.txt");
        assert_eq!(file_node.size, 1024);
        assert!(file_node.children.is_empty());
    }

    #[test]
    fn test_directory_size_calculation() {
        let mut root = TreeNode::new_directory("root".to_string());
        root.add_child(TreeNode::new_file("file1.txt".to_string(), 100));
        root.add_child(TreeNode::new_file("file2.txt".to_string(), 200));
        
        let mut subdir = TreeNode::new_directory("subdir".to_string());
        subdir.add_child(TreeNode::new_file("file3.txt".to_string(), 300));
        root.add_child(subdir);
        
        root.calculate_directory_size();
        
        assert_eq!(root.size, 600);
    }
}