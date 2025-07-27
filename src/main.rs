use std::env;
use std::fs::File;
use isomage::{detect_and_parse_filesystem, extract_node, TreeNode};

fn main() {
    let args: Vec<String> = env::args().collect();
    
    // Parse command line arguments
    let (extract_path, filename) = if args.len() == 4 && args[1] == "-x" {
        (Some(args[2].clone()), args[3].clone())
    } else if args.len() == 2 {
        (None, args[1].clone())
    } else {
        eprintln!("Usage: {} [-x ROOT] <file.iso>", args[0]);
        eprintln!("  -x ROOT  Extract file or directory at ROOT path to current directory");
        eprintln!("Parses and displays the directory structure of ISO 9660 filesystems.");
        std::process::exit(1);
    };
    
    let mut file = match File::open(&filename) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Failed to open file '{}': {}", filename, e);
            std::process::exit(1);
        }
    };
    
    // Detect filesystem type and parse accordingly
    let root_node = match detect_and_parse_filesystem(&mut file, &filename) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("Failed to parse filesystem: {}", e);
            std::process::exit(1);
        }
    };
    
    if let Some(extract_path) = extract_path {
        // Extract mode
        if let Some(node_to_extract) = root_node.find_node(&extract_path) {
            match extract_node(&mut file, node_to_extract, ".") {
                Ok(()) => {
                    println!("Extraction completed successfully.");
                },
                Err(e) => {
                    eprintln!("Failed to extract '{}': {}", extract_path, e);
                    std::process::exit(1);
                }
            }
        } else {
            eprintln!("Path '{}' not found in filesystem", extract_path);
            std::process::exit(1);
        }
    } else {
        // List mode - print the tree structure
        print_tree(&root_node, 0);
    }
}


fn print_tree(node: &TreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let prefix = if node.is_directory { "📁 " } else { "📄 " };
    println!("{}{}{} ({})", indent, prefix, node.name, format_size(node.size));
    
    for child in &node.children {
        print_tree(child, depth + 1);
    }
}

fn format_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = size as f64;
    let mut unit_idx = 0;
    
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    
    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}
