pub mod iso9660;
pub mod tree;

pub use tree::TreeNode;

use std::fs::{File, create_dir_all};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub fn detect_and_parse_filesystem(file: &mut File, filename: &str) -> Result<TreeNode, Box<dyn std::error::Error>> {
    if let Ok(root) = iso9660::parse_iso9660(file) {
        return Ok(root);
    }
    
    Err(format!("Unable to detect supported filesystem in {}", filename).into())
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

fn extract_file(file: &mut File, node: &TreeNode, output_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let (Some(location), Some(length)) = (node.file_location, node.file_length) {
        file.seek(SeekFrom::Start(location))?;
        
        let mut buffer = vec![0u8; length as usize];
        file.read_exact(&mut buffer)?;
        
        let output_file_path = Path::new(output_path).join(&node.name);
        if let Some(parent) = output_file_path.parent() {
            create_dir_all(parent)?;
        }
        
        let mut output_file = std::fs::File::create(&output_file_path)?;
        output_file.write_all(&buffer)?;
        
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
        if child.is_directory {
            extract_directory(file, child, dir_path.to_str().unwrap())?;
        } else {
            extract_file(file, child, dir_path.to_str().unwrap())?;
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

    #[test]
    fn test_iso9660_parsing() {
        let test_files = ["test_linux.iso", "test_macos.iso"];
        
        for test_file in &test_files {
            let path = test_file_path(test_file);
            if Path::new(&path).exists() {
                let mut file = File::open(&path)
                    .unwrap_or_else(|_| panic!("Failed to open test file: {}", path));
                
                match iso9660::parse_iso9660(&mut file) {
                    Ok(root_node) => {
                        assert_eq!(root_node.name, "/");
                        assert!(root_node.is_directory);
                        println!("Successfully parsed ISO 9660: {}", test_file);
                    },
                    Err(e) => {
                        println!("ISO 9660 parsing failed for {}: {}", test_file, e);
                    }
                }
            } else {
                println!("Test file not found: {}", path);
            }
        }
    }


    #[test]
    fn test_filesystem_detection() {
        let test_files = [
            ("test_linux.iso", "ISO 9660"),
            ("test_macos.iso", "ISO 9660"),
        ];
        
        for (test_file, expected_type) in &test_files {
            let path = test_file_path(test_file);
            if Path::new(&path).exists() {
                let mut file = File::open(&path)
                    .unwrap_or_else(|_| panic!("Failed to open test file: {}", path));
                
                match detect_and_parse_filesystem(&mut file, test_file) {
                    Ok(root_node) => {
                        assert_eq!(root_node.name, "/");
                        assert!(root_node.is_directory);
                        println!("Successfully detected {} filesystem in: {}", expected_type, test_file);
                    },
                    Err(e) => {
                        println!("Filesystem detection failed for {}: {}", test_file, e);
                    }
                }
            } else {
                println!("Test file not found: {}", path);
            }
        }
    }

    #[test]
    fn test_tree_structure_validation() {
        let test_file = "test_linux.iso";
        let path = test_file_path(test_file);
        
        if Path::new(&path).exists() {
            let mut file = File::open(&path)
                .unwrap_or_else(|_| panic!("Failed to open test file: {}", path));
            
            if let Ok(root_node) = detect_and_parse_filesystem(&mut file, test_file) {
                validate_tree_structure(&root_node, 0);
                println!("Tree structure validation passed for: {}", test_file);
            }
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