use std::fs::File;
use clap::Parser;
use isomage::{detect_and_parse_filesystem_verbose, extract_node, TreeNode};

/// ISO/UDF filesystem browser and extractor
#[derive(Parser)]
#[command(name = "isomage", version, about)]
struct Cli {
    /// ISO file to open
    file: String,

    /// Show detailed parsing information
    #[arg(short, long)]
    verbose: bool,

    /// Extract file or directory at PATH
    #[arg(short = 'x', long = "extract", value_name = "PATH")]
    extract: Option<String>,

    /// Output directory for extraction (default: current directory)
    #[arg(short, long, default_value = ".")]
    output: String,
}

fn main() {
    let cli = Cli::parse();

    if cli.verbose {
        println!("Opening file: {}", cli.file);
    }

    let mut file = match File::open(&cli.file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Failed to open file '{}': {}", cli.file, e);
            std::process::exit(1);
        }
    };

    let root_node = match detect_and_parse_filesystem_verbose(&mut file, &cli.file, cli.verbose) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("Failed to parse filesystem: {}", e);
            std::process::exit(1);
        }
    };

    if let Some(extract_path) = cli.extract {
        let search_path = extract_path.trim_start_matches('/');

        let node_to_extract = if search_path.is_empty() || extract_path == "/" {
            Some(&root_node)
        } else {
            root_node.find_node(search_path)
        };

        if let Some(node) = node_to_extract {
            if cli.output != "." {
                if let Err(e) = std::fs::create_dir_all(&cli.output) {
                    eprintln!("Failed to create output directory '{}': {}", cli.output, e);
                    std::process::exit(1);
                }
            }

            match extract_node(&mut file, node, &cli.output) {
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
            eprintln!();
            eprintln!("Available top-level entries:");
            for child in &root_node.children {
                let prefix = if child.is_directory { "  d " } else { "  - " };
                eprintln!("{}{}", prefix, child.name);
            }
            std::process::exit(1);
        }
    } else {
        print_tree(&root_node, 0);
    }
}

fn print_tree(node: &TreeNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let prefix = if node.is_directory { "d " } else { "- " };
    println!("{}{}{} ({})", indent, prefix, node.name, format_size(node.size));

    for child in &node.children {
        print_tree(child, depth + 1);
    }
}

fn format_size(size: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
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
