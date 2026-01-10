use std::env;
use std::fs::File;
use isomage::{detect_and_parse_filesystem_verbose, extract_node, TreeNode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_help(program: &str) {
    println!("isomage v{} - ISO/UDF filesystem browser and extractor", VERSION);
    println!();
    println!("USAGE:");
    println!("    {} [OPTIONS] <file.iso>", program);
    println!();
    println!("OPTIONS:");
    println!("    -h, --help       Show this help message");
    println!("    -v, --verbose    Show detailed parsing information");
    println!("    -x <PATH>        Extract file or directory at PATH");
    println!("    -o <DIR>         Output directory for extraction (default: current directory)");
    println!();
    println!("EXAMPLES:");
    println!("    # List contents of an ISO file");
    println!("    {} movie.iso", program);
    println!();
    println!("    # List with verbose parsing info");
    println!("    {} -v movie.iso", program);
    println!();
    println!("    # Extract the entire disc to current directory");
    println!("    {} -x / movie.iso", program);
    println!();
    println!("    # Extract a specific directory");
    println!("    {} -x BDMV/STREAM movie.iso", program);
    println!();
    println!("    # Extract a specific file");
    println!("    {} -x BDMV/STREAM/00000.m2ts movie.iso", program);
    println!();
    println!("    # Extract to a specific output directory");
    println!("    {} -x BDMV -o ./output movie.iso", program);
    println!();
    println!("SUPPORTED FORMATS:");
    println!("    - ISO 9660 (standard CD/DVD ISOs)");
    println!("    - UDF (Blu-ray discs, DVDs)");
    println!("    - UDF with Metadata Partition (Blu-ray)");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    
    let mut extract_path = None;
    let mut output_dir = ".".to_string();
    let mut filename = None;
    let mut verbose = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help(&args[0]);
                std::process::exit(0);
            }
            "-v" | "--verbose" => {
                verbose = true;
            }
            "-x" | "--extract" => {
                if i + 1 < args.len() {
                    extract_path = Some(args[i+1].clone());
                    i += 1;
                } else {
                    eprintln!("Error: -x requires a path argument");
                    eprintln!("Try '{} --help' for more information.", args[0]);
                    std::process::exit(1);
                }
            }
            "-o" | "--output" => {
                if i + 1 < args.len() {
                    output_dir = args[i+1].clone();
                    i += 1;
                } else {
                    eprintln!("Error: -o requires a directory argument");
                    eprintln!("Try '{} --help' for more information.", args[0]);
                    std::process::exit(1);
                }
            }
            arg if arg.starts_with('-') => {
                eprintln!("Error: Unknown option '{}'", arg);
                eprintln!("Try '{} --help' for more information.", args[0]);
                std::process::exit(1);
            }
            _ => {
                if filename.is_none() {
                    filename = Some(args[i].clone());
                } else {
                    eprintln!("Error: Unexpected argument '{}'", args[i]);
                    eprintln!("Try '{} --help' for more information.", args[0]);
                    std::process::exit(1);
                }
            }
        }
        i += 1;
    }

    let filename = match filename {
        Some(f) => f,
        None => {
            print_help(&args[0]);
            std::process::exit(1);
        }
    };
    
    if verbose {
        println!("Opening file: {}", filename);
    }

    let mut file = match File::open(&filename) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Failed to open file '{}': {}", filename, e);
            std::process::exit(1);
        }
    };
    
    // Detect filesystem type and parse accordingly
    let root_node = match detect_and_parse_filesystem_verbose(&mut file, &filename, verbose) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("Failed to parse filesystem: {}", e);
            std::process::exit(1);
        }
    };
    
    if let Some(extract_path) = extract_path {
        // Extract mode
        let search_path = extract_path.trim_start_matches('/');
        
        let node_to_extract = if search_path.is_empty() || extract_path == "/" {
            Some(&root_node)
        } else {
            root_node.find_node(search_path)
        };
        
        if let Some(node) = node_to_extract {
            // Create output directory if it doesn't exist
            if output_dir != "." {
                if let Err(e) = std::fs::create_dir_all(&output_dir) {
                    eprintln!("Failed to create output directory '{}': {}", output_dir, e);
                    std::process::exit(1);
                }
            }
            
            match extract_node(&mut file, node, &output_dir) {
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
                let prefix = if child.is_directory { "  📁 " } else { "  📄 " };
                eprintln!("{}{}", prefix, child.name);
            }
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
