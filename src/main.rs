use std::fs::File;
use std::io;
use clap::Parser;
use isomage::{detect_and_parse_filesystem_verbose, extract_node, cat_node, TreeNode};

/// Browse and extract files from ISO images without mounting them.
///
/// MODES
///
/// List all files and directories (no flags):
///
///   isomage IMAGE
///
/// Stream a file from the ISO to stdout (-c):
///
///   isomage -c PATH IMAGE
///
/// Extract a file or directory to disk (-x):
///
///   isomage -x PATH IMAGE
///
/// PATHS
///
/// Leading slash is optional: "etc/hostname" and "/etc/hostname" both work.
/// Use "/" with -x to extract the entire disc.
///
/// OUTPUT
///
/// All diagnostic output (verbose, progress, errors) goes to stderr.
/// Only file data goes to stdout — -c is binary-safe and pipe-friendly:
///
///   isomage -c BDMV/STREAM/00000.m2ts movie.iso | mpv -
#[derive(Parser)]
#[command(name = "isomage", version)]
struct Cli {
    /// Path to the ISO image (ISO 9660 or UDF)
    file: String,

    /// Print filesystem parsing details to stderr
    #[arg(short, long)]
    verbose: bool,

    /// Stream PATH from the ISO to stdout (raw bytes; pipe-safe; cannot combine with --extract)
    #[arg(short = 'c', long = "cat", value_name = "PATH", conflicts_with = "extract")]
    cat: Option<String>,

    /// Extract PATH to disk. Directories are extracted recursively. Use / for the entire disc.
    #[arg(short = 'x', long = "extract", value_name = "PATH")]
    extract: Option<String>,

    /// Write extracted files into DIR (created if needed)
    #[arg(short, long, default_value = ".", value_name = "DIR")]
    output: String,
}

fn main() {
    let cli = Cli::parse();

    if cli.verbose {
        eprintln!("Opening file: {}", cli.file);
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

    if let Some(cat_path) = cli.cat {
        // Cat mode — print file contents to stdout
        let search_path = cat_path.trim_start_matches('/');
        match root_node.find_node(search_path) {
            Some(node) => {
                let mut stdout = io::stdout().lock();
                if let Err(e) = cat_node(&mut file, node, &mut stdout) {
                    eprintln!("Failed to cat '{}': {}", cat_path, e);
                    std::process::exit(1);
                }
            }
            None => {
                eprintln!("Path '{}' not found in filesystem", cat_path);
                print_available_entries(&root_node);
                std::process::exit(1);
            }
        }
    } else if let Some(extract_path) = cli.extract {
        // Extract mode
        let search_path = extract_path.trim_start_matches('/');

        let node_to_extract = if search_path.is_empty() || extract_path == "/" {
            Some(&root_node)
        } else {
            root_node.find_node(search_path)
        };

        if let Some(node) = node_to_extract {
            if let Err(e) = std::fs::create_dir_all(&cli.output) {
                eprintln!("Failed to create output directory '{}': {}", cli.output, e);
                std::process::exit(1);
            }

            match extract_node(&mut file, node, &cli.output) {
                Ok(()) => {
                    eprintln!("Extraction completed successfully.");
                },
                Err(e) => {
                    eprintln!("Failed to extract '{}': {}", extract_path, e);
                    std::process::exit(1);
                }
            }
        } else {
            eprintln!("Path '{}' not found in filesystem", extract_path);
            print_available_entries(&root_node);
            std::process::exit(1);
        }
    } else {
        // List mode
        print_tree(&root_node, 0);
    }
}

fn print_available_entries(root: &TreeNode) {
    eprintln!();
    eprintln!("Available top-level entries:");
    for child in &root.children {
        let prefix = if child.is_directory { "  d " } else { "  - " };
        eprintln!("{}{}", prefix, child.name);
    }
}

const MAX_TREE_DEPTH: usize = 100;

fn print_tree(node: &TreeNode, depth: usize) {
    if depth > MAX_TREE_DEPTH {
        let indent = "  ".repeat(depth);
        println!("{}... (depth limit reached)", indent);
        return;
    }
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
