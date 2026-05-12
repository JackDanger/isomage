# isomage

[![Crates.io](https://img.shields.io/crates/v/isomage.svg)](https://crates.io/crates/isomage)
[![docs.rs](https://img.shields.io/docsrs/isomage)](https://docs.rs/isomage)
[![CI](https://github.com/JackDanger/isomage/actions/workflows/ci.yml/badge.svg)](https://github.com/JackDanger/isomage/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.74-blue)](Cargo.toml)

Browse and extract files from ISO images without mounting them.

No root. No FUSE. No mount points. Just read the bytes.

```sh
curl -fsSL https://raw.githubusercontent.com/JackDanger/isomage/main/install.sh | sh
```

```console
$ isomage movie.iso
d / (24.8 GB)
  d BDMV (24.8 GB)
    d STREAM (24.7 GB)
      - 00000.m2ts (20.1 GB)
      - 00001.m2ts (4.6 GB)
    d CLIPINF (1.2 KB)
    d PLAYLIST (408 B)
  - CERTIFICATE (3.1 KB)

$ isomage -c BDMV/PLAYLIST/00000.mpls movie.iso | hexdump -C | head
00000000  4d 50 4c 53 30 32 30 30  00 00 00 ea 00 00 00
00000010  00 00 01 1a 00 00 00 00  00 01 00 00 00 01 00

$ isomage -x BDMV/STREAM/00001.m2ts -o ./extras movie.iso
Extracted: ./extras/00001.m2ts
Extraction completed successfully.
```

---

## Contents

- [What this is](#what-this-is)
- [Install](#install)
- [Quick reference](#quick-reference)
- [Usage](#usage)
- [Output contract](#output-contract)
- [Supported formats](#supported-formats)
- [Use as a library](#use-as-a-library)
- [Architecture](#architecture)
- [Invariants and extension points](#invariants-and-extension-points)
- [Build, test, release](#build-test-release)
- [Security](#security)
- [Contributing](#contributing)
- [Changelog](#changelog)
- [License](#license)

---

## What this is

`isomage` is a single-binary Rust CLI that reads ISO 9660 and UDF disc
images directly from disk and reconstructs their filesystem tree in
userspace. No kernel mount, no loopback device, no FUSE driver. The
binary opens the file, parses volume descriptors, walks directory
records, and resolves file extents — that's it.

Three modes:

| Mode | Flag | What it does |
|---|---|---|
| **List** | (none) | Print the directory tree to stdout |
| **Cat** | `-c PATH` | Stream a single file's raw bytes to stdout |
| **Extract** | `-x PATH` | Write a file or directory tree to disk |

Plus `-v` (verbose, parser diagnostics to stderr) and `-o DIR` (extract
output directory).

It is **read-only by design**: there is no code path that mutates an
ISO image. The CLI is the only entry point; the library crate is
re-exported so other Rust programs can embed the parser.

## Install

**Homebrew** (macOS and Linux):

```sh
brew install jackdanger/tap/isomage
```

**Cargo** (any platform with Rust):

```sh
cargo install isomage
```

**Binary** — grab a prebuilt from [releases](../../releases):

```sh
# macOS (Apple Silicon)
curl -L https://github.com/JackDanger/isomage/releases/latest/download/isomage-macos-arm64.tar.gz | tar xz
sudo mv isomage-macos-arm64 /usr/local/bin/isomage

# macOS (Intel)
curl -L https://github.com/JackDanger/isomage/releases/latest/download/isomage-macos-x86_64.tar.gz | tar xz
sudo mv isomage-macos-x86_64 /usr/local/bin/isomage

# Linux (x86_64, static musl)
curl -L https://github.com/JackDanger/isomage/releases/latest/download/isomage-linux-x86_64.tar.gz | tar xz
sudo mv isomage-linux-x86_64 /usr/local/bin/isomage

# Linux (ARM64, static musl)
curl -L https://github.com/JackDanger/isomage/releases/latest/download/isomage-linux-arm64.tar.gz | tar xz
sudo mv isomage-linux-arm64 /usr/local/bin/isomage
```

**From source**:

```sh
git clone https://github.com/JackDanger/isomage.git
cd isomage && cargo build --release
```

## Quick reference

```
isomage IMAGE                       # list all files and directories
isomage -c PATH IMAGE               # stream a file to stdout
isomage -x PATH IMAGE               # extract a file or directory to disk
isomage -x PATH -o DIR IMAGE        # extract into a specific directory
isomage -v IMAGE                    # verbose: show filesystem parsing details
```

`PATH` is a path inside the ISO. Leading slash is optional —
`etc/hostname` and `/etc/hostname` are equivalent. Use `/` with `-x` to
extract everything.

All diagnostic output (verbose, progress, errors) goes to **stderr**.
Only file data goes to **stdout**, so `-c` is binary-safe and
pipe-friendly.

## Usage

### List contents

```sh
isomage movie.iso
```
```
d / (24.8 GB)
  d BDMV (24.8 GB)
    d STREAM (24.7 GB)
      - 00000.m2ts (20.1 GB)
      - 00001.m2ts (4.6 GB)
    d CLIPINF (1.2 KB)
    d PLAYLIST (408 B)
  - CERTIFICATE (3.1 KB)
```

`d` = directory, `-` = file. Indentation is two spaces per level. Size
is human-readable (`B`, `KB`, `MB`, `GB`, `TB`) and includes
descendants for directories.

### Stream a file to stdout (`-c`)

```sh
# Inspect a text file
isomage -c etc/hostname linux.iso

# Pipe to other tools
isomage -c BDMV/PLAYLIST/00000.mpls movie.iso | hexdump -C

# Page through a large file
isomage -c readme.txt data.iso | less

# Play video directly from the ISO (verbose output stays on stderr)
isomage -c BDMV/STREAM/00000.m2ts movie.iso | mpv -

# Stream to disk
isomage -c BDMV/STREAM/00000.m2ts movie.iso > output.m2ts
```

`-c` does not buffer to a temp file; it `pread`s in 8 MB chunks and
writes straight to stdout.

### Extract to disk (`-x`)

```sh
# One file (into current directory)
isomage -x BDMV/STREAM/00001.m2ts movie.iso

# A directory tree, into a specific output directory
isomage -x BDMV/STREAM -o ./streams movie.iso

# The entire disc
isomage -x / -o ./full_dump movie.iso
```

The output directory is created if it doesn't exist. Per-file
extraction paths are printed to stderr as each file completes.
Files over 100 MB get a percentage progress meter on stderr.

### Verbose / debug (`-v`)

```sh
isomage -v movie.iso
```
```
File size: 26663725056 bytes (24.84 GB)
Scanning key sectors for filesystem signatures...
  Sector  16 (ISO 9660 PVD / UDF VRS): 01 43 44 30 30 31 01 00  |.CD001..|
  Sector 256 (UDF AVDP): 02 00 02 00 ...
Attempting ISO 9660 parsing...
  Found Primary Volume Descriptor at sector 16
  ...
```

`-v` is safe to combine with `-c` — diagnostics stay on stderr:

```sh
isomage -v -c etc/hostname linux.iso | xxd   # bytes on stdout, debug on stderr
```

## Output contract

This is the contract the CLI guarantees. Don't break it without a major
version bump.

| Mode | stdout | stderr |
|---|---|---|
| List | One line per entry: `[indent]{d\|-} NAME (SIZE)` | empty |
| Cat (`-c`) | The file's raw bytes, exactly | Errors only (and `-v` if set) |
| Extract (`-x`) | empty | Per-file `Extracted: <path>`, progress, errors |
| Any mode + `-v` | (same as above) | Parser diagnostics added |

Exit codes:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Any error (file not found, parse failure, path not in ISO, I/O error) |

## Supported formats

- **ISO 9660**, including the **Joliet** (Unicode filenames) and **Rock
  Ridge** (POSIX long filenames) extensions
- **UDF**, including metadata partitions and multi-extent files

Covers CDs, DVDs, and Blu-rays. Detection is automatic: the parser
tries ISO 9660 first, then UDF, and reports both errors if neither
succeeds.

---

## Use as a library

The same crate that ships the `isomage` binary is also a published
library on [crates.io](https://crates.io/crates/isomage). Both
audiences install from the same tag — there is no separate `-cli`
crate to keep in sync.

```toml
# Cargo.toml
[dependencies]
isomage = "1"
```

```rust
use std::fs::File;
use isomage::{detect_and_parse_filesystem, cat_node, extract_node};

let mut file = File::open("disc.iso")?;
let root = detect_and_parse_filesystem(&mut file, "disc.iso")?;

// Walk the tree.
for child in &root.children {
    println!("{} {} ({} bytes)",
        if child.is_directory { "d" } else { "-" },
        child.name, child.size);
}

// Stream one file to any `std::io::Write`.
let node = root.find_node("etc/hostname").ok_or("missing")?;
let mut out = Vec::new();
cat_node(&mut file, node, &mut out)?;

// Extract a subtree to disk; the library refuses names that would
// escape the output directory.
extract_node(&mut file, node, "/tmp/extracted")?;
# Ok::<(), isomage::Error>(())
```

Full API documentation lives at
[**docs.rs/isomage**](https://docs.rs/isomage). The public surface is:

| Item | What it does |
|---|---|
| [`detect_and_parse_filesystem`](https://docs.rs/isomage/latest/isomage/fn.detect_and_parse_filesystem.html) | Open and parse, trying ISO 9660 then UDF. |
| [`cat_node`](https://docs.rs/isomage/latest/isomage/fn.cat_node.html) | Stream a file's bytes to any `Write`. BrokenPipe-tolerant. |
| [`extract_node`](https://docs.rs/isomage/latest/isomage/fn.extract_node.html) | Extract a file or subtree to disk. Path-traversal-safe. |
| [`TreeNode`](https://docs.rs/isomage/latest/isomage/tree/struct.TreeNode.html) | The parsed-tree model: file or directory, with byte-range references into the original image. |
| [`isomage::iso9660`](https://docs.rs/isomage/latest/isomage/iso9660/index.html) and [`udf`](https://docs.rs/isomage/latest/isomage/udf/index.html) | The format-specific parsers, exposed for callers that already know which they have. |
| [`isomage::Error`](https://docs.rs/isomage/latest/isomage/type.Error.html) / [`Result`](https://docs.rs/isomage/latest/isomage/type.Result.html) | `Box<dyn Error + Send + Sync + 'static>` and its result alias — composes cleanly with `anyhow` and threads. |

MSRV is **1.74**.

---

## Architecture

isomage is small (~1.7k lines of Rust across five files). Read the
modules in this order if you want to understand the whole system:

```
src/
├── tree.rs       The TreeNode model used by every other module
├── iso9660.rs    ISO 9660 parser (incl. Joliet, Rock Ridge)
├── udf.rs        UDF parser (incl. metadata partitions, multi-extent)
├── lib.rs        Public API: detect_and_parse, cat_node, extract_node
└── main.rs       Clap CLI; orchestrates lib calls; owns stdout/stderr
```

### Data model: `TreeNode` (`src/tree.rs`)

Everything the rest of the codebase touches is a `TreeNode`:

```rust
pub struct TreeNode {
    pub name: String,
    pub size: u64,
    pub is_directory: bool,
    pub children: Vec<TreeNode>,
    pub file_location: Option<u64>,   // byte offset into the image
    pub file_length:   Option<u64>,   // file size in bytes
}
```

Parsers return a fully-built `TreeNode` tree rooted at `"/"`. Files
carry a `(file_location, file_length)` pair pointing into the original
image — there is no in-memory copy of the file bytes. `cat_node` and
`extract_node` seek to `file_location` and read `file_length` bytes.

Helpers on `TreeNode`:

- `find_node(path)` — slash-separated path lookup. Leading slash is
  tolerated. Returns the node or `None`.
- `calculate_directory_size()` — recursive sum of descendants. Called
  by the parser once after the tree is built.
- `new_file_with_location(name, size, location, length)` — the
  constructor parsers should use for real files.

### Public library API (`src/lib.rs`)

The crate is exposed both as a binary and as a library. Embedders use
four functions:

```rust
detect_and_parse_filesystem(&mut File, filename) -> Result<TreeNode, _>
detect_and_parse_filesystem_verbose(&mut File, filename, verbose) -> Result<TreeNode, _>
cat_node(&mut File, &TreeNode, &mut impl Write) -> Result<(), _>
extract_node(&mut File, &TreeNode, output_path: &str) -> Result<(), _>
```

`detect_and_parse_filesystem` is a thin wrapper that calls the
`_verbose` variant with `false`. The verbose variant prints a hex dump
of key sectors and the names of each parser it tries, all to stderr.

I/O is sequential `pread`-style reads using `Seek + Read`. There is no
mmap. The chunk size for both cat and extract is `EXTRACT_CHUNK_SIZE =
8 MB`.

### Parsers

Both parsers expose the same pair of entry points:

```rust
iso9660::parse_iso9660(&mut File) -> io::Result<TreeNode>
iso9660::parse_iso9660_verbose(&mut File, verbose: bool) -> io::Result<TreeNode>
udf::parse_udf(&mut File)        -> io::Result<TreeNode>
udf::parse_udf_verbose(&mut File, verbose: bool) -> io::Result<TreeNode>
```

The `_verbose` variants print spec-section-tagged diagnostics to
stderr. `lib.rs` always calls the `_verbose` variant and threads the
flag from the CLI.

Both parsers seek to sector 16 (the Volume Recognition Sequence) and
look for their respective signatures. Both fail gracefully — they
return an `io::Error` rather than panic on unrecognized input.

### CLI (`src/main.rs`)

The CLI is intentionally thin. It parses args with `clap`, opens the
file, calls `detect_and_parse_filesystem_verbose`, and dispatches to
one of three branches:

- **List** — recursive `print_tree` to stdout
- **Cat** — `find_node` then `cat_node` into a locked `stdout`
- **Extract** — `find_node` then `extract_node` to the output directory

`print_tree` is the only stdout-side renderer; everything else uses
`eprintln!`. `MAX_TREE_DEPTH = 100` guards against pathological inputs.

`format_size` is the human-readable size formatter (`1024.0`-based
binary units, despite the units being labelled `KB`/`MB`/`GB`/`TB`).

---

## Invariants and extension points

These are the rules the codebase relies on. Break them and something
in CI or downstream will notice.

### Invariants

1. **stdout is sacred in `-c` and `-x`.** Only `-c` writes file bytes
   to stdout; `-x` writes nothing to stdout. Everything else goes to
   stderr. Don't `println!` from `lib.rs` — use `eprintln!`.
2. **Read-only.** No code path writes to the input file. Open in
   read mode and never seek past EOF without bounds-checking first.
3. **No mmap.** isomage targets large Blu-ray images (50+ GB). Use
   `Seek` + `Read` with the existing chunk size (`EXTRACT_CHUNK_SIZE`).
4. **No panic on bad input.** Parsers return `io::Error`; the CLI maps
   errors to exit code 1. If you find a `.unwrap()` on parser-derived
   data, it's a bug.
5. **Paths normalize the same way everywhere.** `path.trim_start_matches('/')`
   is the canonical normalization. Use it; don't reinvent it.
6. **`TreeNode` is the wire format between parsers and the rest.**
   New parsers must produce a `TreeNode` tree; new consumers must
   accept one.
7. **Promptlog gate.** Every PR that changes `src/` or `Cargo.toml`
   commits a `prompts/YYYYMMDD-HHMMSS-<slug>.md` file. CI enforces this.

### Extension points

| You want to… | Touch this |
|---|---|
| Support a new on-disc filesystem (HFS+, exFAT, FAT) | Add `src/<fs>.rs` exposing `parse_<fs>{,_verbose}`. Register it in `detect_and_parse_filesystem_verbose` in `src/lib.rs` after the existing tries. |
| Add a new CLI subcommand | Extend `Cli` in `src/main.rs`. Keep the stdout/stderr contract above. |
| Add a new metadata field to entries (timestamps, permissions) | Add fields to `TreeNode` in `src/tree.rs`, populate from each parser, render where appropriate. |
| Make parsing faster | Look at `EXTRACT_CHUNK_SIZE` in `src/lib.rs` and the inner read loops in `iso9660.rs` / `udf.rs`. No mmap. |
| Add a new diagnostic in `-v` mode | `eprintln!` from inside the parser, gated on `verbose`. |

---

## Build, test, release

```sh
make test-data       # generate the synthetic ISOs under test_data/
cargo build          # debug build at target/debug/isomage
cargo test           # tests live as #[cfg(test)] mod tests inside src/*.rs
```

Cross-compile from macOS to Linux:

```sh
make install-targets   # one-time: adds the musl target
make build-linux       # static linux binary at releases/isomage-linux
```

Release flow:

1. Bump `version` in `Cargo.toml`.
2. Add a `prompts/` entry recording the bump (the CI gate fires on `Cargo.toml`).
3. Merge to `main`.
4. Tag `vX.Y.Z` and push the tag. `.github/workflows/release.yml`
   builds binaries for all four targets, creates a GitHub Release,
   publishes to crates.io, and updates the Homebrew tap.

---

## Security

`isomage` is a parser for untrusted binary data and an extractor that
writes to disk on the user's behalf. Vulnerability reports should go
to GitHub's [private security advisories](https://github.com/JackDanger/isomage/security/advisories/new),
not the public issue tracker — see [`SECURITY.md`](SECURITY.md) for
the full policy. The current hardening surface (path-traversal guards,
`BrokenPipe` tolerance, 64-bit-safe extract loops) is summarized there.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). The short checklist is in
[`.github/pull_request_template.md`](.github/pull_request_template.md),
which GitHub pre-fills for every new PR.

isomage follows the [**promptlog pattern**](https://jackdanger.com/promptlog/):
every PR that changes source code commits a sanitized log of the
prompts that led to the change. AI agents and humans both follow the
same rule. The spec is in [`prompts/PROMPTLOG.md`](prompts/PROMPTLOG.md);
agents can use the [`promptlog`](.claude/skills/promptlog.md) skill;
the CI gate in [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
enforces it.

If you're an AI agent reading this, also read [`CLAUDE.md`](CLAUDE.md) —
that's the short rulebook for this repo.

---

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for the human-curated list of
changes per release. Auto-generated release notes also live on each
[GitHub Release](https://github.com/JackDanger/isomage/releases).

---

## Why

I got tired of leaving a container just to `mount` an image just to
read one file. `isomage` runs entirely in userspace — it reads the
raw bytes and reconstructs the filesystem tree itself.

## Limitations

- Read-only (by design)
- Some exotic UDF variations might not parse correctly

If you hit a disc that doesn't work, run with `-v` and open an issue.

## License

[MIT](LICENSE)
