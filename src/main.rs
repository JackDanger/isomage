use anyhow::{Context, Result};
use clap::Parser;
use std::fs::File;
use isomage::{detect_and_parse_filesystem, TreeNode};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the .img or .iso file
    file: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    
    let mut file = File::open(&args.file)
        .with_context(|| format!("Failed to open file: {}", args.file))?;
    
    // Detect filesystem type and parse accordingly
    let root_node = detect_and_parse_filesystem(&mut file, &args.file)?;
    
    // Print the tree structure
    print_tree(&root_node, 0);
    
    Ok(())
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
