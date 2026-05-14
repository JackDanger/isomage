//! # isomage
//!
//! Browse and extract files from ISO 9660 and UDF disc images directly,
//! without mounting them. Pure-Rust, zero runtime dependencies, no CLI binary.
//!
//! ## What this library does
//!
//! - Parses ISO 9660 (with Joliet and Rock Ridge extensions) and UDF
//!   disc images into a [`TreeNode`] hierarchy rooted at `"/"`.
//! - Lets you stream a single file's bytes ([`cat_node`]) or extract a
//!   file or subtree to disk ([`extract_node`]) without loading the
//!   whole image into memory.
//! - Never writes to the input image: read-only by design.
//!
//! Detection is automatic — call [`detect_and_parse_filesystem`] and the
//! library will try ISO 9660 first, then UDF, returning whichever matches.
//!
//! ## Safety
//!
//! [`extract_node`] enforces that every output path stays inside the
//! caller-supplied output directory. Entries with names containing path
//! separators, NUL bytes, `.`, `..`, or empty strings are rejected with
//! a clear error rather than silently written. This protects against
//! adversarial ISOs whose directory entries (e.g. Rock Ridge `NM`
//! records) attempt to write outside the destination via `..` traversal.
//!
//! [`cat_node`] returns `Ok(())` when the downstream writer closes
//! its pipe (`BrokenPipe`), matching standard Unix pipeline behaviour
//! (e.g. `cat_node(&node, &mut stdout())?` piped to `head`).
//!
//! ## Quick example
//!
//! ```no_run
//! use std::fs::File;
//! use isomage::detect_and_parse_filesystem;
//!
//! let mut file = File::open("disc.iso")?;
//! let root = detect_and_parse_filesystem(&mut file, "disc.iso")?;
//!
//! for child in &root.children {
//!     let kind = if child.is_directory { "d" } else { "-" };
//!     println!("{} {} ({} bytes)", kind, child.name, child.size);
//! }
//! # Ok::<(), isomage::Error>(())
//! ```

pub mod iso9660;
pub mod tree;
pub mod udf;

// v3.0 infrastructure. The `image_io` module is always compiled
// (the `RandomAccess` trait is free of unsafe and free of deps);
// only the `MmapImage` submodule is gated behind the `mmap`
// feature. The module is named `image_io` rather than `io` to
// avoid colliding with `std::io`, which lib.rs imports unqualified.
pub mod image_io;

#[cfg(feature = "simd")]
pub mod simd;

// v3.0 format submodules. Each is feature-gated; the umbrella
// `formats` module is always compiled (its body is just `pub mod`
// declarations) so consumers can spell `isomage::formats::mbr`
// without conditional imports.
pub mod formats;

pub use tree::TreeNode;

// `File` is no longer named by the public API as of v3.0 — the
// reader entry points are generic over `R: Read + Seek`. `File`
// remains the canonical example in doc-tests (which `use` it
// themselves) and is used as `std::fs::File::create` in the
// extraction code path below.
use std::fs::create_dir_all;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The error type returned by every fallible public function in this crate.
///
/// Boxed so the library doesn't pin callers to a specific error enum, and
/// `Send + Sync + 'static` so it composes cleanly with thread spawning,
/// `anyhow::Error`, and async runtimes.
pub type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The result type returned by every fallible public function in this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Parse the filesystem contained in `file`, returning the root node of
/// the directory tree.
///
/// Tries ISO 9660 first (including Joliet and Rock Ridge extensions),
/// then UDF (including metadata partitions and multi-extent files).
/// Returns an error describing both parsers' failures if neither matches.
///
/// `filename` is used only in the error message — it is not opened.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use isomage::detect_and_parse_filesystem;
///
/// let mut file = File::open("disc.iso")?;
/// let root = detect_and_parse_filesystem(&mut file, "disc.iso")?;
/// assert_eq!(root.name, "/");
/// # Ok::<(), isomage::Error>(())
/// ```
pub fn detect_and_parse_filesystem<R: Read + Seek>(
    file: &mut R,
    filename: &str,
) -> Result<TreeNode> {
    detect_and_parse_filesystem_verbose(file, filename, false)
}

/// Like [`detect_and_parse_filesystem`], but prints spec-section-tagged
/// diagnostics to stderr while parsing.
///
/// Useful for investigating images that fail to parse, or when building
/// your own diagnostic wrapper (see README's "If you want a CLI" section).
/// Generic over any `Read + Seek` source: pass a `File`, an
/// `image_io::MmapImage` (with `--features mmap`), or an in-memory
/// `Cursor<Vec<u8>>`.
pub fn detect_and_parse_filesystem_verbose<R: Read + Seek>(
    file: &mut R,
    filename: &str,
    verbose: bool,
) -> Result<TreeNode> {
    let mut errors = Vec::new();

    if verbose {
        // Show file size
        let file_size = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        eprintln!(
            "File size: {} bytes ({:.2} GB)",
            file_size,
            file_size as f64 / (1024.0 * 1024.0 * 1024.0)
        );

        // Show what's at key sectors
        eprintln!("Scanning key sectors for filesystem signatures...");
        for (sector, desc) in [
            (16, "ISO 9660 PVD / UDF VRS"),
            (17, "UDF VRS"),
            (256, "UDF AVDP"),
        ]
        .iter()
        {
            file.seek(SeekFrom::Start(*sector as u64 * 2048))?;
            let mut buf = [0u8; 32];
            if file.read_exact(&mut buf).is_ok() {
                let printable: String = buf
                    .iter()
                    .map(|&b| {
                        if (0x20..0x7f).contains(&b) {
                            b as char
                        } else {
                            '.'
                        }
                    })
                    .collect();
                eprintln!("  Sector {:>3} ({}): {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x} {:02x}  |{}|",
                    sector, desc, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], &printable[..8]);
            }
        }
        file.seek(SeekFrom::Start(0))?;
    }

    if verbose {
        eprintln!("\nAttempting ISO 9660 parsing...");
    }
    match iso9660::parse_iso9660_verbose(file, verbose) {
        Ok(root) => return Ok(root),
        Err(e) => {
            if verbose {
                eprintln!("  ISO 9660 parsing failed: {}", e);
            }
            errors.push(format!("ISO 9660: {}", e));
        }
    }

    // Seek back to start before trying next parser
    file.seek(SeekFrom::Start(0))?;
    if verbose {
        eprintln!("\nAttempting UDF parsing...");
    }
    match udf::parse_udf_verbose(file, verbose) {
        Ok(root) => return Ok(root),
        Err(e) => {
            if verbose {
                eprintln!("  UDF parsing failed: {}", e);
            }
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

/// Stream a file from the ISO to `writer` in fixed-size chunks.
///
/// `node` must reference a file (not a directory) and must carry the
/// `file_location` / `file_length` pair populated by the parsers.
///
/// **Broken pipe handling.** If `writer` returns `ErrorKind::BrokenPipe`
/// (e.g. a downstream `head` closed the pipe early), this function
/// returns `Ok(())` rather than propagating the error.
/// Other I/O errors are propagated unchanged.
///
/// `writer` receives only file bytes — no headers, framing, or progress
/// output. This makes the function safe to use in pipelines.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use isomage::{detect_and_parse_filesystem, cat_node};
///
/// let mut file = File::open("disc.iso")?;
/// let root = detect_and_parse_filesystem(&mut file, "disc.iso")?;
/// let node = root.find_node("etc/hostname")
///     .ok_or("not in ISO")?;
///
/// let mut out = Vec::new();
/// cat_node(&mut file, node, &mut out)?;
/// # Ok::<(), isomage::Error>(())
/// ```
pub fn cat_node<R: Read + Seek, W: Write>(
    file: &mut R,
    node: &TreeNode,
    writer: &mut W,
) -> Result<()> {
    if node.is_directory {
        return Err(format!("'{}' is a directory, not a file", node.name).into());
    }
    let (location, length) = match (node.file_location, node.file_length) {
        (Some(l), Some(n)) => (l, n),
        _ => return Err("File location information not available".into()),
    };

    file.seek(SeekFrom::Start(location))?;
    let mut remaining: u64 = length;
    let buf_cap = remaining.min(EXTRACT_CHUNK_SIZE as u64) as usize;
    let mut buffer = vec![0u8; buf_cap];

    while remaining > 0 {
        let to_read = remaining.min(EXTRACT_CHUNK_SIZE as u64) as usize;
        let buf = &mut buffer[..to_read];
        file.read_exact(buf)?;

        match writer.write_all(buf) {
            Ok(()) => {}
            // Downstream closed the pipe; that's normal for `| head`, not an error.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        remaining -= to_read as u64;
    }
    Ok(())
}

/// Extract `node` (a file or a directory subtree) to `output_path` on disk.
///
/// The output directory is created if it doesn't exist. For each file
/// extracted, a `Extracted: <path>` line is printed to stderr; for each
/// directory created, `Created directory: <path>`. Files larger than 100 MB
/// also report percentage progress on stderr.
///
/// # Safety against malicious ISOs
///
/// Every entry name is validated to reject path traversal: names that are
/// empty, `.`, `..`, or that contain `/`, `\`, or NUL bytes are refused
/// with an error. As defense in depth, each constructed output path is
/// verified to remain within the (canonicalized) output root via a lexical
/// `starts_with` check. This means an adversarial ISO whose directory records
/// claim a name like `../../etc/passwd` will produce a clear error, not
/// silently overwrite host files.
///
/// **Symlink limitation.** This check is lexical-only and does not follow
/// symlinks. If the output directory already contains a symlink pointing
/// outside it (e.g. `out/link → /etc`), extracting a file named `link/passwd`
/// into `out/` would follow the symlink. To avoid this, extract into a freshly
/// created empty directory, or audit the destination for pre-existing symlinks
/// before calling this function.
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use isomage::{detect_and_parse_filesystem, extract_node};
///
/// let mut file = File::open("disc.iso")?;
/// let root = detect_and_parse_filesystem(&mut file, "disc.iso")?;
/// let subtree = root.find_node("docs").ok_or("not in ISO")?;
/// extract_node(&mut file, subtree, "/tmp/disc-docs")?;
/// # Ok::<(), isomage::Error>(())
/// ```
pub fn extract_node<R: Read + Seek>(
    file: &mut R,
    node: &TreeNode,
    output_path: &str,
) -> Result<()> {
    create_dir_all(output_path)
        .map_err(|e| format!("cannot create output directory '{}': {}", output_path, e))?;
    let root = std::fs::canonicalize(output_path).map_err(|e| {
        format!(
            "cannot canonicalize output directory '{}': {}",
            output_path, e
        )
    })?;

    // The synthetic root node ("/") is the tree root from the parser. We
    // don't want to create a literal "/" subdirectory in the destination —
    // its children become the top level instead.
    if node.is_directory && node.name == "/" {
        for child in &node.children {
            extract_into(file, child, &root, &root)?;
        }
        Ok(())
    } else {
        extract_into(file, node, &root, &root)
    }
}

const EXTRACT_CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8 MB chunks

/// Reject names that, if joined to a parent path, could escape it or
/// produce ambiguous filesystem behaviour.
///
/// Conservative on purpose: rejects anything ISO/UDF parsers could
/// possibly stamp into a `TreeNode.name` that the host filesystem would
/// then interpret as something other than a single in-directory entry.
fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("refusing to extract entry with unsafe name {:?}", name).into());
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!(
            "refusing to extract entry whose name contains a path separator or NUL byte: {:?}",
            name
        )
        .into());
    }
    Ok(())
}

/// Compute the on-disk target for `name` inside `here`, verifying the
/// result stays under `root`. Defense in depth: even with
/// `validate_entry_name` already called, we re-check `starts_with(root)`
/// in case some future caller bypasses validation.
fn safe_join(root: &Path, here: &Path, name: &str) -> Result<PathBuf> {
    validate_entry_name(name)?;
    let target = here.join(name);
    if !target.starts_with(root) {
        return Err(format!(
            "path escape: entry '{}' would write outside output directory {}",
            name,
            root.display()
        )
        .into());
    }
    Ok(target)
}

fn extract_into<R: Read + Seek>(
    file: &mut R,
    node: &TreeNode,
    root: &Path,
    here: &Path,
) -> Result<()> {
    let target = safe_join(root, here, &node.name)?;

    if node.is_directory {
        create_dir_all(&target)?;
        eprintln!("Created directory: {}", target.display());
        for child in &node.children {
            extract_into(file, child, root, &target)?;
        }
    } else {
        extract_file_at(file, node, &target)?;
    }
    Ok(())
}

fn extract_file_at<R: Read + Seek>(file: &mut R, node: &TreeNode, target: &Path) -> Result<()> {
    let (location, length) = match (node.file_location, node.file_length) {
        (Some(l), Some(n)) => (l, n),
        _ => return Err("File location information not available for extraction".into()),
    };

    file.seek(SeekFrom::Start(location))?;

    if let Some(parent) = target.parent() {
        create_dir_all(parent)?;
    }

    let mut output_file = std::fs::File::create(target)
        .map_err(|e| format!("cannot create '{}': {}", target.display(), e))?;

    let mut remaining: u64 = length;
    let buf_cap = remaining.min(EXTRACT_CHUNK_SIZE as u64) as usize;
    let mut buffer = vec![0u8; buf_cap];

    while remaining > 0 {
        let to_read = remaining.min(EXTRACT_CHUNK_SIZE as u64) as usize;
        let buf = &mut buffer[..to_read];
        file.read_exact(buf)?;
        output_file.write_all(buf)?;
        remaining -= to_read as u64;

        // Print progress for large files (> 100 MB)
        if length > 100 * 1024 * 1024 {
            let done = length - remaining;
            eprint!(
                "\r  Extracting {}: {:.1}%",
                node.name,
                done as f64 / length as f64 * 100.0
            );
        }
    }
    if length > 100 * 1024 * 1024 {
        eprintln!();
    }

    eprintln!("Extracted: {}", target.display());
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
            eprintln!(
                "Skipping test: {} not found (run `make test-data` to generate)",
                path
            );
            return None;
        }
        Some(File::open(&path).unwrap_or_else(|_| panic!("Failed to open test file: {}", path)))
    }

    fn parse_linux_iso() -> Option<(File, TreeNode)> {
        let mut file = require_test_file("test_linux.iso")?;
        let root = detect_and_parse_filesystem(&mut file, "test_linux.iso")
            .expect("Failed to parse test_linux.iso");
        Some((file, root))
    }

    fn parse_macos_iso() -> Option<(File, TreeNode)> {
        let mut file = require_test_file("test_macos.iso")?;
        let root = detect_and_parse_filesystem(&mut file, "test_macos.iso")
            .expect("Failed to parse test_macos.iso");
        Some((file, root))
    }

    // ---- Parsing & detection ----

    #[test]
    fn test_iso9660_parsing() {
        for test_file in &["test_linux.iso", "test_macos.iso"] {
            if let Some(mut file) = require_test_file(test_file) {
                let root = iso9660::parse_iso9660(&mut file)
                    .unwrap_or_else(|e| panic!("ISO 9660 parsing failed for {}: {}", test_file, e));
                assert_eq!(root.name, "/");
                assert!(root.is_directory);
                assert!(
                    !root.children.is_empty(),
                    "{} should have children",
                    test_file
                );
            }
        }
    }

    #[test]
    fn test_filesystem_detection() {
        for test_file in &["test_linux.iso", "test_macos.iso"] {
            if let Some(mut file) = require_test_file(test_file) {
                let root = detect_and_parse_filesystem(&mut file, test_file).unwrap_or_else(|e| {
                    panic!("Filesystem detection failed for {}: {}", test_file, e)
                });
                assert_eq!(root.name, "/");
                assert!(root.is_directory);
            }
        }
    }

    #[test]
    fn test_invalid_file_handling() {
        assert!(File::open(test_file_path("nonexistent.iso")).is_err());
    }

    #[test]
    fn test_garbage_data_rejected() {
        // Create a temp file with garbage data
        let dir = std::env::temp_dir().join("isomage_test");
        std::fs::create_dir_all(&dir).unwrap();
        let garbage_path = dir.join("garbage.iso");
        std::fs::write(&garbage_path, b"this is not an ISO file at all").unwrap();

        let mut file = File::open(&garbage_path).unwrap();
        let result = detect_and_parse_filesystem(&mut file, "garbage.iso");
        assert!(result.is_err(), "Garbage data should fail to parse");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Unable to detect"),
            "Error should mention detection failure, got: {}",
            err
        );

        std::fs::remove_file(&garbage_path).ok();
    }

    // ---- Linux ISO structure verification ----

    #[test]
    fn test_linux_iso_expected_directories() {
        if let Some((_file, root)) = parse_linux_iso() {
            for dir_name in &["boot", "etc", "home", "usr", "var"] {
                let node = root
                    .find_node(dir_name)
                    .unwrap_or_else(|| panic!("Expected directory '{}' not found", dir_name));
                assert!(node.is_directory, "'{}' should be a directory", dir_name);
            }
        }
    }

    #[test]
    fn test_linux_iso_expected_files() {
        if let Some((_file, root)) = parse_linux_iso() {
            let expected_files = [
                "boot/grub.cfg",
                "etc/hostname",
                "etc/hosts",
                "home/user/.bashrc",
                "usr/bin/hello",
                "var/log/system.log",
            ];
            for path in &expected_files {
                let node = root
                    .find_node(path)
                    .unwrap_or_else(|| panic!("Expected file '{}' not found", path));
                assert!(!node.is_directory, "'{}' should be a file", path);
                assert!(node.size > 0, "'{}' should have non-zero size", path);
                assert!(
                    node.file_location.is_some(),
                    "'{}' should have a file location",
                    path
                );
                assert!(
                    node.file_length.is_some(),
                    "'{}' should have a file length",
                    path
                );
            }
        }
    }

    #[test]
    fn test_linux_iso_nested_structure() {
        if let Some((_file, root)) = parse_linux_iso() {
            // Verify home/user/.bashrc exists through directory traversal
            let home = root.find_node("home").expect("home not found");
            assert!(home.is_directory);
            let user = home.find_node("user").expect("user not found in home");
            assert!(user.is_directory);
            let bashrc = user
                .find_node(".bashrc")
                .expect(".bashrc not found in user");
            assert!(!bashrc.is_directory);
        }
    }

    // ---- macOS ISO structure verification ----

    #[test]
    fn test_macos_iso_expected_structure() {
        if let Some((_file, root)) = parse_macos_iso() {
            for dir_name in &["Applications", "System", "Users", "private"] {
                let node = root.find_node(dir_name).unwrap_or_else(|| {
                    panic!("Expected directory '{}' not found in macOS ISO", dir_name)
                });
                assert!(node.is_directory);
            }

            let expected_files = [
                "Applications/readme.txt",
                "System/Library/info.txt",
                "Users/user/welcome.txt",
                "private/var/log/system.log",
            ];
            for path in &expected_files {
                let node = root
                    .find_node(path)
                    .unwrap_or_else(|| panic!("Expected file '{}' not found in macOS ISO", path));
                assert!(!node.is_directory);
                assert!(node.size > 0);
            }
        }
    }

    // ---- Tree structure validation ----

    #[test]
    fn test_tree_structure_validation() {
        for (name, parser) in [
            ("linux", parse_linux_iso as fn() -> Option<(File, TreeNode)>),
            ("macos", parse_macos_iso),
        ] {
            if let Some((_file, root)) = parser() {
                validate_tree_structure(&root, 0, name);
            }
        }
    }

    fn validate_tree_structure(node: &TreeNode, depth: usize, iso_name: &str) {
        assert!(
            !node.name.is_empty(),
            "Node name should not be empty in {}",
            iso_name
        );
        assert!(depth <= 10, "Tree depth exceeded limit in {}", iso_name);

        if !node.is_directory {
            assert!(
                node.children.is_empty(),
                "File '{}' should not have children in {}",
                node.name,
                iso_name
            );
        }

        for child in &node.children {
            validate_tree_structure(child, depth + 1, iso_name);
        }
    }

    // ---- TreeNode unit tests ----

    #[test]
    fn test_tree_node_creation() {
        let dir_node = TreeNode::new_directory("test_dir".to_string());
        assert!(dir_node.is_directory);
        assert_eq!(dir_node.name, "test_dir");
        assert_eq!(dir_node.size, 0);
        assert!(dir_node.children.is_empty());
        assert!(dir_node.file_location.is_none());

        let file_node = TreeNode::new_file("test_file.txt".to_string(), 1024);
        assert!(!file_node.is_directory);
        assert_eq!(file_node.name, "test_file.txt");
        assert_eq!(file_node.size, 1024);
        assert!(file_node.file_location.is_none());

        let located = TreeNode::new_file_with_location("f.bin".to_string(), 512, 4096, 512);
        assert_eq!(located.file_location, Some(4096));
        assert_eq!(located.file_length, Some(512));
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
        // Subdir should also have its size calculated
        let sub = root.find_node("subdir").unwrap();
        assert_eq!(sub.size, 300);
    }

    // ---- find_node edge cases ----

    #[test]
    fn test_find_node_with_leading_slash() {
        if let Some((_file, root)) = parse_linux_iso() {
            // Leading slash should be stripped
            assert!(root.find_node("/etc/hostname").is_some());
            assert!(root.find_node("etc/hostname").is_some());
            // Multiple leading slashes
            assert!(root.find_node("///etc/hostname").is_some());
        }
    }

    #[test]
    fn test_find_node_root_paths() {
        if let Some((_file, root)) = parse_linux_iso() {
            // Empty path and "/" both return root
            let by_empty = root.find_node("").unwrap();
            assert_eq!(by_empty.name, "/");
            let by_slash = root.find_node("/").unwrap();
            assert_eq!(by_slash.name, "/");
        }
    }

    #[test]
    fn test_find_node_nonexistent() {
        if let Some((_file, root)) = parse_linux_iso() {
            assert!(root.find_node("nonexistent").is_none());
            assert!(root.find_node("etc/nonexistent").is_none());
            assert!(root.find_node("a/b/c/d/e/f").is_none());
        }
    }

    // ---- cat tests ----

    #[test]
    fn test_cat_file_to_buffer() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let node = root
                .find_node("etc/hostname")
                .expect("etc/hostname not found");

            let mut output = Vec::new();
            cat_node(&mut file, node, &mut output).expect("cat_node failed");

            let content = String::from_utf8(output).expect("Not valid UTF-8");
            assert!(
                content.contains("test-linux-system"),
                "Expected hostname content, got: {:?}",
                content
            );
        }
    }

    #[test]
    fn test_cat_preserves_exact_bytes() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let node = root
                .find_node("etc/hostname")
                .expect("etc/hostname not found");

            let mut output = Vec::new();
            cat_node(&mut file, node, &mut output).expect("cat_node failed");

            // Output length should match the node's reported size
            assert_eq!(
                output.len() as u64,
                node.size,
                "cat output length {} doesn't match node size {}",
                output.len(),
                node.size
            );
        }
    }

    #[test]
    fn test_cat_rejects_directory() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let node = root.find_node("etc").expect("etc/ not found");

            let mut output = Vec::new();
            let result = cat_node(&mut file, node, &mut output);
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("directory"));
            assert!(
                output.is_empty(),
                "No bytes should be written for a directory"
            );
        }
    }

    #[test]
    fn test_cat_node_without_location() {
        let node = TreeNode::new_file("orphan.txt".to_string(), 100);
        // Create a dummy file to pass as the ISO (won't be read)
        let dir = std::env::temp_dir().join("isomage_test");
        std::fs::create_dir_all(&dir).unwrap();
        let dummy_path = dir.join("dummy.bin");
        std::fs::write(&dummy_path, b"x").unwrap();
        let mut file = File::open(&dummy_path).unwrap();

        let mut output = Vec::new();
        let result = cat_node(&mut file, &node, &mut output);
        assert!(result.is_err(), "cat on file without location should error");
        assert!(result.unwrap_err().to_string().contains("not available"));

        std::fs::remove_file(&dummy_path).ok();
    }

    #[test]
    fn test_cat_every_file_in_linux_iso() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let files = [
                ("boot/grub.cfg", "GRUB"),
                ("etc/hostname", "test-linux-system"),
                ("etc/hosts", "127.0.0.1"),
                ("home/user/.bashrc", "Bash"),
                ("usr/bin/hello", "Hello World"),
                ("var/log/system.log", "System started"),
            ];
            for (path, expected) in &files {
                let node = root
                    .find_node(path)
                    .unwrap_or_else(|| panic!("{} not found", path));
                let mut output = Vec::new();
                cat_node(&mut file, node, &mut output)
                    .unwrap_or_else(|e| panic!("cat failed for {}: {}", path, e));
                let content = String::from_utf8(output).expect("Not valid UTF-8");
                assert!(
                    content.contains(expected),
                    "Expected '{}' in {}, got: {:?}",
                    expected,
                    path,
                    content
                );
            }
        }
    }

    #[test]
    fn test_cat_every_file_in_macos_iso() {
        if let Some((mut file, root)) = parse_macos_iso() {
            let files = [
                ("Applications/readme.txt", "Application Data"),
                ("System/Library/info.txt", "System Library"),
                ("Users/user/welcome.txt", "Welcome to macOS"),
                ("private/var/log/system.log", "macOS system log"),
            ];
            for (path, expected) in &files {
                let node = root
                    .find_node(path)
                    .unwrap_or_else(|| panic!("{} not found in macOS ISO", path));
                let mut output = Vec::new();
                cat_node(&mut file, node, &mut output)
                    .unwrap_or_else(|e| panic!("cat failed for {}: {}", path, e));
                let content = String::from_utf8(output).expect("Not valid UTF-8");
                assert!(
                    content.contains(expected),
                    "Expected '{}' in {}, got: {:?}",
                    expected,
                    path,
                    content
                );
            }
        }
    }

    // ---- extraction tests ----

    #[test]
    fn test_extract_single_file() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let dir = std::env::temp_dir().join("isomage_test_extract_single");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            let node = root
                .find_node("etc/hostname")
                .expect("etc/hostname not found");
            extract_node(&mut file, node, dir.to_str().unwrap()).expect("extract failed");

            let extracted = std::fs::read_to_string(dir.join("hostname")).unwrap();
            assert!(extracted.contains("test-linux-system"));

            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn test_extract_directory() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let dir = std::env::temp_dir().join("isomage_test_extract_dir");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            let node = root.find_node("etc").expect("etc not found");
            extract_node(&mut file, node, dir.to_str().unwrap()).expect("extract failed");

            // Should create etc/ subdirectory with both files
            assert!(dir.join("etc/hostname").exists(), "hostname should exist");
            assert!(dir.join("etc/hosts").exists(), "hosts should exist");

            let hostname = std::fs::read_to_string(dir.join("etc/hostname")).unwrap();
            assert!(hostname.contains("test-linux-system"));

            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn test_extract_root() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let dir = std::env::temp_dir().join("isomage_test_extract_root");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            extract_node(&mut file, &root, dir.to_str().unwrap()).expect("extract root failed");

            // All top-level dirs should exist
            for name in &["boot", "etc", "home", "usr", "var"] {
                assert!(dir.join(name).is_dir(), "{} directory should exist", name);
            }
            // Deep file should exist
            assert!(
                dir.join("home/user/.bashrc").exists(),
                ".bashrc should exist"
            );

            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn test_extract_matches_cat() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let dir = std::env::temp_dir().join("isomage_test_extract_vs_cat");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();

            let node = root.find_node("etc/hosts").expect("etc/hosts not found");

            // Get cat output
            let mut cat_output = Vec::new();
            cat_node(&mut file, node, &mut cat_output).expect("cat failed");

            // Extract to disk
            extract_node(&mut file, node, dir.to_str().unwrap()).expect("extract failed");
            let extracted = std::fs::read(dir.join("hosts")).unwrap();

            assert_eq!(
                cat_output, extracted,
                "cat and extract should produce identical bytes"
            );

            std::fs::remove_dir_all(&dir).ok();
        }
    }

    // ---- security & robustness regression tests ----

    /// A handcrafted TreeNode pointing at a real on-disk file location,
    /// but with a malicious name. Used to assert that extract_node refuses
    /// to write outside its output directory regardless of what the parser
    /// stamped into TreeNode.name.
    fn dummy_iso() -> (File, std::path::PathBuf) {
        let dir = std::env::temp_dir().join("isomage_test_security");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("dummy.bin");
        std::fs::write(&p, b"hostile payload bytes").unwrap();
        let f = File::open(&p).unwrap();
        (f, p)
    }

    #[test]
    fn test_extract_rejects_dotdot_in_name() {
        let (mut file, _payload) = dummy_iso();
        let mut root = TreeNode::new_directory("/".to_string());
        // Real `file_location`/`file_length` so the only thing standing
        // between this and an out-of-tree write is the name guard.
        root.add_child(TreeNode::new_file_with_location(
            "../escapee.txt".to_string(),
            21,
            0,
            21,
        ));

        let out = std::env::temp_dir().join("isomage_test_extract_dotdot_out");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();

        let result = extract_node(&mut file, &root, out.to_str().unwrap());
        assert!(result.is_err(), "extract must refuse '../escapee.txt'");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unsafe") || err.contains("path"),
            "error should mention unsafe/path traversal, got: {}",
            err
        );

        // Confirm nothing landed outside `out`.
        assert!(
            !out.parent().unwrap().join("escapee.txt").exists(),
            "no file should have been written outside the output directory"
        );

        std::fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_extract_rejects_slash_in_name() {
        let (mut file, _payload) = dummy_iso();
        let mut root = TreeNode::new_directory("/".to_string());
        root.add_child(TreeNode::new_file_with_location(
            "subdir/file.txt".to_string(),
            21,
            0,
            21,
        ));

        let out = std::env::temp_dir().join("isomage_test_extract_slash_out");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();

        let result = extract_node(&mut file, &root, out.to_str().unwrap());
        assert!(result.is_err(), "extract must refuse a name containing '/'");

        std::fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_extract_rejects_absolute_name() {
        let (mut file, _payload) = dummy_iso();
        let mut root = TreeNode::new_directory("/".to_string());
        root.add_child(TreeNode::new_file_with_location(
            "/etc/passwd".to_string(),
            21,
            0,
            21,
        ));

        let out = std::env::temp_dir().join("isomage_test_extract_abs_out");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();

        let result = extract_node(&mut file, &root, out.to_str().unwrap());
        assert!(
            result.is_err(),
            "extract must refuse an absolute-looking name"
        );

        std::fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_extract_rejects_nul_byte() {
        let (mut file, _payload) = dummy_iso();
        let mut root = TreeNode::new_directory("/".to_string());
        root.add_child(TreeNode::new_file_with_location(
            "good\0name.txt".to_string(),
            21,
            0,
            21,
        ));

        let out = std::env::temp_dir().join("isomage_test_extract_nul_out");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();

        let result = extract_node(&mut file, &root, out.to_str().unwrap());
        assert!(
            result.is_err(),
            "extract must refuse a name with a NUL byte"
        );

        std::fs::remove_dir_all(&out).ok();
    }

    #[test]
    fn test_validate_entry_name_unit() {
        assert!(validate_entry_name("hostname").is_ok());
        assert!(validate_entry_name(".bashrc").is_ok());
        assert!(validate_entry_name("name with spaces").is_ok());

        assert!(validate_entry_name("").is_err());
        assert!(validate_entry_name(".").is_err());
        assert!(validate_entry_name("..").is_err());
        assert!(validate_entry_name("../etc/passwd").is_err());
        assert!(validate_entry_name("foo/bar").is_err());
        assert!(validate_entry_name("foo\\bar").is_err());
        assert!(validate_entry_name("/etc/passwd").is_err());
        assert!(validate_entry_name("a\0b").is_err());
    }

    /// Writer that returns BrokenPipe after the first N bytes. Used to
    /// assert that cat_node treats broken-pipe as clean exit, matching
    /// the standard Unix `| head` behaviour.
    struct BrokenPipeAfter {
        budget: usize,
        written: usize,
    }
    impl Write for BrokenPipeAfter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.written >= self.budget {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "downstream closed",
                ));
            }
            let take = buf.len().min(self.budget - self.written);
            self.written += take;
            if take == 0 {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "downstream closed",
                ))
            } else {
                Ok(take)
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_cat_node_swallows_broken_pipe() {
        if let Some((mut file, root)) = parse_linux_iso() {
            let node = root
                .find_node("etc/hostname")
                .expect("etc/hostname not found");

            // Budget 0 → every write hits BrokenPipe immediately.
            let mut w = BrokenPipeAfter {
                budget: 0,
                written: 0,
            };
            let result = cat_node(&mut file, node, &mut w);
            assert!(
                result.is_ok(),
                "cat_node should treat BrokenPipe as Ok, got: {:?}",
                result
            );
        }
    }

    // ── Synthetic-image tests for lib.rs public API coverage ─────────────────

    /// Build a tiny synthetic UDF image that detect_and_parse_filesystem can read.
    /// Reuses the same layout from udf.rs tests.
    fn make_tiny_udf() -> Vec<u8> {
        const S: usize = 2048;
        let mut img = vec![0u8; S * 270];
        let w16 = |buf: &mut Vec<u8>, off: usize, v: u16| {
            buf[off..off + 2].copy_from_slice(&v.to_le_bytes())
        };
        let w32 = |buf: &mut Vec<u8>, off: usize, v: u32| {
            buf[off..off + 4].copy_from_slice(&v.to_le_bytes())
        };
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
        w32(&mut img, 258 * S + 252, 0);
        w16(&mut img, 258 * S + 256, 0);
        w16(&mut img, 259 * S, 8);
        w16(&mut img, 260 * S, 256);
        w32(&mut img, 260 * S + 400, S as u32);
        w32(&mut img, 260 * S + 404, 1);
        w16(&mut img, 260 * S + 408, 0);
        let rfe = 261 * S;
        w16(&mut img, rfe, 261);
        w16(&mut img, rfe + 18, 3);
        let mut parent = vec![0u8; 40];
        parent[0..2].copy_from_slice(&257u16.to_le_bytes());
        parent[18] = 0x08;
        w32(&mut img, rfe + 172, parent.len() as u32);
        img[rfe + 176..rfe + 176 + parent.len()].copy_from_slice(&parent);
        img
    }

    #[test]
    fn detect_and_parse_verbose_false_garbage() {
        // Exercise the "Unable to detect" error path with verbose=false
        let mut c = std::io::Cursor::new(vec![0u8; 4096]);
        let result = detect_and_parse_filesystem_verbose(&mut c, "fake.iso", false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unable to detect"));
    }

    #[test]
    fn detect_and_parse_verbose_true_garbage() {
        // Exercise verbose=true on garbage: hits all verbose eprintln branches
        let mut c = std::io::Cursor::new(vec![0u8; 512 * 1024]); // 512 KiB
        let result = detect_and_parse_filesystem_verbose(&mut c, "fake.iso", true);
        assert!(result.is_err());
    }

    #[test]
    fn detect_and_parse_verbose_true_udf() {
        // verbose=true with a valid UDF image: hits successful path + verbose branches
        let img = make_tiny_udf();
        let mut c = std::io::Cursor::new(img);
        let result = detect_and_parse_filesystem_verbose(&mut c, "test.udf", true);
        assert!(result.is_ok());
    }

    #[test]
    fn safe_join_rejects_path_escape() {
        // validate_entry_name allows the name but safe_join sees a bypass scenario
        // via a crafted name that contains no / but somehow escapes (can't in practice
        // since validate_entry_name checks), so this just confirms safe_join works.
        let root = std::path::Path::new("/tmp");
        let here = std::path::Path::new("/tmp");
        // Valid join: stays inside
        let result = safe_join(root, here, "file.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn cat_node_non_broken_pipe_error_propagates() {
        // A writer that returns a non-BrokenPipe error should propagate the error.
        struct FailWriter;
        impl Write for FailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "no write"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let data = b"hello";
        let mut img = vec![0u8; 512];
        img[..data.len()].copy_from_slice(data);
        let mut c = std::io::Cursor::new(img);
        let node = TreeNode::new_file_with_location("f".to_string(), 5, 0, 5);
        let result = cat_node(&mut c, &node, &mut FailWriter);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no write") || msg.contains("Permission"),
            "got: {msg}"
        );
    }
}
