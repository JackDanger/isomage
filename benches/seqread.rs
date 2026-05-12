//! Sequential-read throughput benchmark.
//!
//! Compares `isomage`'s full-tree extract against `7zz x -so` on the
//! same image, both sinking the bytes to `/dev/null` so we measure
//! the parser + I/O path rather than disk-write speed.
//!
//! `cargo bench --bench seqread`
//!
//! ## Corpus
//!
//! Reads every `*.iso`, `*.img`, `*.udf` file under `test_data/`. The
//! checked-in test ISOs are tiny (~400 KB each) and L2-resident; they
//! exist mostly to validate the harness compiles and runs. For real
//! throughput numbers, populate `test_data/` with larger images:
//!
//! ```sh
//! # Debian netinst, ~700 MB
//! curl -L -o test_data/debian.iso \
//!   https://cdimage.debian.org/.../debian-XX.iso
//! # Multi-GB UDF blob via mkudffs
//! ```
//!
//! ## 7-Zip baseline
//!
//! If `7zz` (or `7z`) is not on `$PATH`, the `7zz_*` benches are
//! silently skipped. The build-system installs `p7zip` in CI.
//!
//! ## Why criterion
//!
//! The default `cargo bench` Harness has no statistical handling and
//! is not stable. Criterion (dev-dep only — never published to
//! crates.io as a runtime dep) gives us confidence intervals and
//! regression detection so the `benches/baseline.json` saved in CI
//! is comparable across runs.

use std::ffi::OsStr;
use std::fs::{read_dir, File};
use std::io::{self, copy};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use criterion::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};

use isomage::{detect_and_parse_filesystem, TreeNode};

/// Walk the tree, calling `cat_node` on every file, writing into `io::sink()`.
/// This is what an "extract everything to /dev/null" workload looks like.
fn extract_all_to_sink(image_path: &Path) -> io::Result<u64> {
    let mut file = File::open(image_path)?;
    let root = detect_and_parse_filesystem(&mut file, image_path.to_str().unwrap_or("?"))
        .map_err(|e| io::Error::other(e.to_string()))?;

    let mut total: u64 = 0;
    walk(&root, &mut file, &mut total)?;
    Ok(total)
}

fn walk(node: &TreeNode, file: &mut File, total: &mut u64) -> io::Result<()> {
    if !node.is_directory {
        let mut sink = io::sink();
        // cat_node streams bytes — no allocation per file beyond
        // its internal sector buffer.
        isomage::cat_node(file, node, &mut sink)
            .map_err(|e| io::Error::other(e.to_string()))?;
        *total = total.saturating_add(node.size);
    }
    for child in &node.children {
        walk(child, file, total)?;
    }
    Ok(())
}

/// Run `7zz x -so <image>` (extract every file to stdout) and discard
/// the output. Mirrors what `extract_all_to_sink` does at the same
/// I/O layer.
fn seven_zip_extract_to_null(image_path: &Path) -> io::Result<()> {
    // Prefer 7zz (the official p7zip-zstd build); fall back to 7z.
    let bin = if which("7zz").is_some() {
        "7zz"
    } else if which("7z").is_some() {
        "7z"
    } else {
        return Err(io::Error::new(io::ErrorKind::NotFound, "7zz/7z not on PATH"));
    };

    // `x -so` writes extracted bytes to stdout. `-bd` disables the
    // progress indicator (slows it down at small sizes). `-y` says
    // "assume yes" so it never blocks on prompts.
    let mut child = Command::new(bin)
        .arg("x")
        .arg("-so")
        .arg("-bd")
        .arg("-y")
        .arg(image_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    // Drain stdout to a sink so 7zz isn't blocked on a full pipe.
    if let Some(mut out) = child.stdout.take() {
        let mut sink = io::sink();
        copy(&mut out, &mut sink)?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{} exited with {:?}",
            bin,
            status.code()
        )));
    }
    Ok(())
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new("test_data");
    let mut out = Vec::new();
    if let Ok(entries) = read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(OsStr::to_str) {
                Some("iso") | Some("img") | Some("udf") => out.push(p),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

fn bench_seqread(c: &mut Criterion) {
    let images = corpus();
    if images.is_empty() {
        eprintln!(
            "warning: no images in test_data/. Run `make test-data` or drop \
             larger .iso/.img files into test_data/ for meaningful numbers."
        );
        return;
    }

    let have_7z = which("7zz").or_else(|| which("7z")).is_some();
    if !have_7z {
        eprintln!(
            "warning: 7zz/7z not on PATH; baseline group skipped. Install \
             p7zip to get a comparable number."
        );
    }

    let mut group = c.benchmark_group("seqread");
    for img in &images {
        let size = std::fs::metadata(img).map(|m| m.len()).unwrap_or(0);
        let name = img
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("unknown")
            .to_string();

        group.throughput(Throughput::Bytes(size));

        group.bench_with_input(
            BenchmarkId::new("isomage", &name),
            img,
            |b, path| {
                b.iter(|| {
                    let n = extract_all_to_sink(path).expect("isomage extract");
                    black_box(n);
                });
            },
        );

        if have_7z {
            group.bench_with_input(
                BenchmarkId::new("7zz", &name),
                img,
                |b, path| {
                    b.iter(|| {
                        seven_zip_extract_to_null(path).expect("7zz extract");
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_seqread);
criterion_main!(benches);
