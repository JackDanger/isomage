//! Golden-file snapshot testing for `TreeNode`.
//!
//! The pattern: render the tree as deterministic text, compare
//! against `tests/snapshots/<name>.snap`. When the snapshot file
//! is missing, the first run creates it (and prints a warning so
//! CI doesn't silently accept anything). When the snapshot exists
//! and differs, print a line-by-line diff and fail.
//!
//! Refresh stale snapshots with:
//!
//! ```sh
//! ISOMAGE_UPDATE_SNAPSHOTS=1 cargo test
//! ```
//!
//! # Why not `insta`?
//!
//! Two reasons. First, this crate prizes a tiny dev-dep tree;
//! `insta` pulls in a transitive `serde`, `regex`, and an inline
//! TOML parser, which is overkill for the format we need. Second,
//! the snapshot file format includes per-snapshot metadata
//! (generation timestamp, *reference tool* name and version) so
//! that golden files document not just the expected output but
//! *which version of the reference tool produced it*. That's hard
//! to bolt onto `insta`'s file shape.
//!
//! # File format
//!
//! ```text
//! # isomage tree snapshot v1
//! # generated: 2026-05-12T19:00:00Z
//! # reference: sgdisk 1.0.10
//! # ---
//! /
//!   partition-0-Linux-0          location=2048      size=204800
//!   partition-1-EFI System-1     location=206848    size=204800
//! ```
//!
//! The header lines (anything starting with `#` before the `# ---`
//! sentinel) are advisory: they're stripped before comparison so
//! re-running the test on the same input doesn't flap when, say,
//! the generation timestamp changes.

use std::fs;
use std::path::{Path, PathBuf};

use isomage::TreeNode;

/// Render `tree` as the deterministic text format described in the
/// module docs. Children are emitted in tree-order (parser order;
/// already deterministic per-parser).
pub fn render_tree(tree: &TreeNode) -> String {
    let mut out = String::new();
    render_node(tree, 0, &mut out);
    out
}

fn render_node(node: &TreeNode, depth: usize, out: &mut String) {
    let indent: String = "  ".repeat(depth);
    let kind = if node.is_directory { "d" } else { "-" };
    if node.is_directory {
        out.push_str(&format!("{}{}{}\n", indent, kind, node.name));
    } else {
        let loc = node
            .file_location
            .map(|l| l.to_string())
            .unwrap_or_else(|| "?".to_string());
        out.push_str(&format!(
            "{}{}{:<32} location={loc:<10} size={}\n",
            indent, kind, node.name, node.size
        ));
    }
    for child in &node.children {
        render_node(child, depth + 1, out);
    }
}

/// Compare `tree` against `tests/snapshots/<name>.snap`. Creates the
/// snapshot file on first run (printing a warning) or in update mode.
///
/// `tool_version` is opaque text recorded in the snapshot header for
/// provenance; pass the reference tool's `--version` line if any.
pub fn assert_snapshot(name: &str, tree: &TreeNode) {
    assert_snapshot_with_tool(name, tree, None);
}

/// Like [`assert_snapshot`], but records the reference tool's name
/// and version in the snapshot header. Future refreshes won't change
/// the body unless the actual tree changes, but the version line
/// updates so contributors can see what tool produced the golden.
pub fn assert_snapshot_with_tool(name: &str, tree: &TreeNode, tool_version: Option<&str>) {
    let body = render_tree(tree);
    let path = snapshot_path(name);

    let update = std::env::var_os("ISOMAGE_UPDATE_SNAPSHOTS").is_some_and(|v| {
        let v = v.to_string_lossy();
        let v = v.trim();
        !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
    });

    if !path.exists() || update {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create snapshots dir");
        }
        let header = build_header(tool_version);
        let full = format!("{header}{body}");
        fs::write(&path, &full).expect("write snapshot");
        if !update {
            eprintln!(
                "snapshot: created {} (rerun with ISOMAGE_UPDATE_SNAPSHOTS=1 to refresh later)",
                path.display()
            );
        } else {
            eprintln!("snapshot: refreshed {}", path.display());
        }
        return;
    }

    let on_disk = fs::read_to_string(&path).expect("read snapshot");
    let stored_body = strip_header(&on_disk);
    if stored_body == body {
        return;
    }

    let diff = unified_diff(stored_body, &body);
    panic!(
        "snapshot mismatch for {name}\n\
         snapshot file: {}\n\
         {diff}\n\
         (set ISOMAGE_UPDATE_SNAPSHOTS=1 to overwrite, after verifying the new output is right)",
        path.display(),
    );
}

fn snapshot_path(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is set by Cargo for every test binary; it's
    // the workspace root. Snapshots live under tests/snapshots/.
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set (snapshot needs cargo)");
    PathBuf::from(root)
        .join("tests")
        .join("snapshots")
        .join(format!("{name}.snap"))
}

fn build_header(tool_version: Option<&str>) -> String {
    let mut h = String::from("# isomage tree snapshot v1\n");
    if let Some(v) = tool_version {
        h.push_str("# reference: ");
        h.push_str(v.trim());
        h.push('\n');
    }
    h.push_str("# ---\n");
    h
}

fn strip_header(snap: &str) -> &str {
    // Find the `# ---` sentinel; if absent, treat the whole file as body
    // (compat with hand-written goldens).
    if let Some(pos) = snap.find("\n# ---\n") {
        &snap[pos + "\n# ---\n".len()..]
    } else if let Some(rest) = snap.strip_prefix("# ---\n") {
        rest
    } else {
        snap
    }
}

/// Minimal unified diff: line-by-line. Not as pretty as `similar`,
/// but no extra dep, and the test output is meant to be read by
/// humans during review, not parsed by tools.
fn unified_diff(a: &str, b: &str) -> String {
    let a_lines: Vec<&str> = a.lines().collect();
    let b_lines: Vec<&str> = b.lines().collect();
    let mut out = String::from("--- snapshot (on disk)\n+++ actual (this run)\n");
    let max = a_lines.len().max(b_lines.len());
    for i in 0..max {
        match (a_lines.get(i), b_lines.get(i)) {
            (Some(a), Some(b)) if a == b => {
                out.push(' ');
                out.push_str(a);
                out.push('\n');
            }
            (Some(a), Some(b)) => {
                out.push_str("- ");
                out.push_str(a);
                out.push('\n');
                out.push_str("+ ");
                out.push_str(b);
                out.push('\n');
            }
            (Some(a), None) => {
                out.push_str("- ");
                out.push_str(a);
                out.push('\n');
            }
            (None, Some(b)) => {
                out.push_str("+ ");
                out.push_str(b);
                out.push('\n');
            }
            (None, None) => break,
        }
    }
    out
}

/// Silence unused-warning in test binaries that don't use the snapshot machinery.
#[allow(dead_code)]
fn _silence(_p: &Path) {}
