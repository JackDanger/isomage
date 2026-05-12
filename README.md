# isomage

[![Crates.io](https://img.shields.io/crates/v/isomage.svg)](https://crates.io/crates/isomage)
[![docs.rs](https://img.shields.io/docsrs/isomage)](https://docs.rs/isomage)
[![CI](https://github.com/JackDanger/isomage/actions/workflows/ci.yml/badge.svg)](https://github.com/JackDanger/isomage/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.74-blue)](Cargo.toml)
[![Zero deps](https://img.shields.io/badge/dependencies-0-success)](Cargo.toml)

> **A pure-Rust reader for ISO 9660 and UDF disc images. Zero
> dependencies. Read-only. No mount, no FUSE, no `unsafe`.**

```toml
[dependencies]
isomage = "2"
```

```rust
use std::fs::File;
use isomage::{detect_and_parse_filesystem, cat_node, extract_node};

let mut iso = File::open("disc.iso")?;
let root = detect_and_parse_filesystem(&mut iso, "disc.iso")?;

// Walk the tree.
for child in &root.children {
    println!("{} {} ({} bytes)",
        if child.is_directory { "d" } else { "-" },
        child.name, child.size);
}

// Stream one file into any std::io::Write.
let hostname = root.find_node("etc/hostname").ok_or("not in ISO")?;
let mut buf = Vec::new();
cat_node(&mut iso, hostname, &mut buf)?;

// Or extract a subtree to disk — names that try to escape via
// "../" or '/' are refused with a clear error, not silently written.
extract_node(&mut iso, hostname, "/tmp/extracted")?;
# Ok::<(), isomage::Error>(())
```

---

## What it parses

- **ISO 9660** (ECMA-119), including the **Joliet** Unicode-filenames
  extension and the **Rock Ridge** POSIX long-filenames extension.
- **UDF** (ECMA-167), including metadata partitions and multi-extent
  files — enough for typical CDs, DVDs, and Blu-rays.

Detection is automatic: `detect_and_parse_filesystem` tries ISO 9660,
then UDF, returning whichever matches and a tagged-error string
listing both attempts if neither does.

---

## Why a new crate

`7z` and friends already extract from ISO and UDF. `isomage` is for a
narrower audience:

- **A Rust program that wants to inspect an ISO without shelling out**
  or pulling in a C/C++ FFI dep. There was no pure-Rust crate doing
  ISO 9660 + UDF together when this was written.
- **Embedding into bigger systems**: indexers, server-side preview
  generators, build tooling that needs to read installer ISOs in
  CI without spawning child processes.
- **Investigating malformed discs.** Every parser entry point has a
  `_verbose` variant that prints spec-section-tagged diagnostics
  (volume descriptors at sector 16, AVDP at 256, partition maps,
  etc.) to stderr — useful for figuring out *why* a particular disc
  won't read.
- **Auditable code.** ~1.7k lines of safe Rust, zero `unsafe`, zero
  runtime dependencies. You can read the whole parser in an
  afternoon.

If you just want to extract a movie disc on the command line, `7z x
movie.iso` is faster than getting Rust set up. This crate isn't
trying to replace that.

---

## Contents

- [What it parses](#what-it-parses)
- [Why a new crate](#why-a-new-crate)
- [Public API](#public-api)
- [Safety guarantees](#safety-guarantees)
- [Examples](#examples)
- [Architecture](#architecture)
- [Invariants and extension points](#invariants-and-extension-points)
- [If you want a CLI](#if-you-want-a-cli)
- [Build and test](#build-and-test)
- [Security](#security)
- [Contributing](#contributing)
- [Changelog](#changelog)
- [License](#license)

---

## Public API

Everything is on [docs.rs/isomage](https://docs.rs/isomage); this is
the cheat sheet.

| Item | What it does |
|---|---|
| [`detect_and_parse_filesystem`](https://docs.rs/isomage/latest/isomage/fn.detect_and_parse_filesystem.html) | Try ISO 9660 then UDF; return the root `TreeNode`. |
| [`detect_and_parse_filesystem_verbose`](https://docs.rs/isomage/latest/isomage/fn.detect_and_parse_filesystem_verbose.html) | Same, with spec-tagged diagnostics to stderr. |
| [`cat_node`](https://docs.rs/isomage/latest/isomage/fn.cat_node.html) | Stream a file to any `std::io::Write`. BrokenPipe-tolerant. |
| [`extract_node`](https://docs.rs/isomage/latest/isomage/fn.extract_node.html) | Extract a file or subtree to disk. Path-traversal-safe. |
| [`TreeNode`](https://docs.rs/isomage/latest/isomage/tree/struct.TreeNode.html) | The parsed-tree model: file or directory, with byte-range references into the image. |
| [`TreeNode::find_node`](https://docs.rs/isomage/latest/isomage/tree/struct.TreeNode.html#method.find_node) | Slash-separated path lookup, leading `/` tolerated. |
| [`isomage::iso9660`](https://docs.rs/isomage/latest/isomage/iso9660/index.html) / [`isomage::udf`](https://docs.rs/isomage/latest/isomage/udf/index.html) | The format-specific parsers, exposed for callers that already know what they have. |
| [`isomage::Error`](https://docs.rs/isomage/latest/isomage/type.Error.html) / [`isomage::Result`](https://docs.rs/isomage/latest/isomage/type.Result.html) | `Box<dyn std::error::Error + Send + Sync + 'static>` and its `Result` alias — composes cleanly with `anyhow` and threads. |

MSRV is **1.74**. The crate has no runtime dependencies and uses no
`unsafe` blocks.

---

## Safety guarantees

The crate parses untrusted binary input. Two specific surfaces are
hardened:

1. **`extract_node` cannot write outside its output directory.**
   Every directory-entry name is validated to reject empty strings,
   `.`, `..`, and any name containing `/`, `\`, or NUL bytes. As
   defense in depth, the output directory is canonicalized once at
   entry and every resolved write path is checked to stay under it.
   An adversarial ISO whose directory records claim a name like
   `../../etc/passwd` produces a clear `Err` rather than silently
   writing to the host filesystem.

2. **`cat_node` does not panic on closed pipes.** If the downstream
   `Write` returns `ErrorKind::BrokenPipe`, `cat_node` returns
   `Ok(())` — matching standard Unix `| head` semantics. Useful when
   you're streaming a large extent into another process that might
   stop reading.

The parsers themselves return `Err` on invalid input rather than
panicking. If you find a crafted ISO that panics, that's a bug —
see [SECURITY.md](SECURITY.md).

---

## Examples

### List all directories and files

```rust
use std::fs::File;
use isomage::{detect_and_parse_filesystem, TreeNode};

fn walk(node: &TreeNode, depth: usize) {
    println!("{:width$}{} {}",
        "", if node.is_directory { "d" } else { "-" },
        node.name, width = depth * 2);
    for child in &node.children {
        walk(child, depth + 1);
    }
}

let mut iso = File::open("disc.iso")?;
let root = detect_and_parse_filesystem(&mut iso, "disc.iso")?;
walk(&root, 0);
# Ok::<(), isomage::Error>(())
```

### Stream one file to stdout

```rust
use std::fs::File;
use std::io;
use isomage::{detect_and_parse_filesystem, cat_node};

let mut iso = File::open("disc.iso")?;
let root = detect_and_parse_filesystem(&mut iso, "disc.iso")?;
let node = root.find_node("etc/hostname").ok_or("not in ISO")?;

let mut stdout = io::stdout().lock();
cat_node(&mut iso, node, &mut stdout)?;
# Ok::<(), isomage::Error>(())
```

### Extract a subtree to disk

```rust
use std::fs::File;
use isomage::{detect_and_parse_filesystem, extract_node};

let mut iso = File::open("disc.iso")?;
let root = detect_and_parse_filesystem(&mut iso, "disc.iso")?;
let docs = root.find_node("docs").ok_or("not in ISO")?;
extract_node(&mut iso, docs, "/tmp/disc-docs")?;
# Ok::<(), isomage::Error>(())
```

### Investigate a malformed disc

```rust
use std::fs::File;
use isomage::detect_and_parse_filesystem_verbose;

let mut iso = File::open("weird.iso")?;
// Prints to stderr: file size, signatures at key sectors, which
// parser tried what, where it gave up.
let _ = detect_and_parse_filesystem_verbose(&mut iso, "weird.iso", true);
# Ok::<(), isomage::Error>(())
```

---

## Architecture

The crate is small (~1.7k lines across four files). The natural reading
order is `tree.rs` → `iso9660.rs` → `udf.rs` → `lib.rs`:

```
src/
├── tree.rs       The TreeNode model used by every other module.
├── iso9660.rs    ISO 9660 parser (incl. Joliet, Rock Ridge).
├── udf.rs        UDF parser (incl. metadata partitions, multi-extent).
└── lib.rs        Public API: detect_and_parse, cat_node, extract_node;
                  re-exports TreeNode and exposes the Error/Result aliases.
```

### Data model: `TreeNode`

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
carry a `(file_location, file_length)` pair pointing into the
original image — there is no in-memory copy of the file bytes.
`cat_node` and `extract_node` seek to `file_location` and read
`file_length` bytes.

### Parsers

Both parsers expose `parse_<fmt>(file)` and
`parse_<fmt>_verbose(file, verbose)`. The verbose variants print
spec-section-tagged diagnostics to stderr. `lib.rs` always calls the
verbose variant and threads the flag from `detect_and_parse_filesystem_verbose`.

Both parsers seek to sector 16 (the Volume Recognition Sequence) and
look for their respective signatures. Both fail gracefully — they
return an `io::Error` rather than panic on unrecognized input.

I/O is sequential `Seek + Read` with an 8 MB chunk size for the
extract / cat paths. There is no mmap.

---

## Invariants and extension points

These are the rules the codebase relies on. Break them and something
in CI or downstream will notice.

### Invariants

1. **`extract_node` never escapes its output directory.** Names are
   validated *and* resolved paths are checked against the canonical
   root. New extract code paths must use `safe_join`, not `Path::join`.
2. **Read-only.** No code path writes to the input file. Open in
   read mode and never seek past EOF without bounds-checking first.
3. **No mmap, no panics.** Use `Seek + Read` with the existing chunk
   size constant. Parsers return `io::Error` on bad input; a
   `.unwrap()` on parser-derived data is a bug.
4. **`u64` for lengths, clamp before `usize` cast.** Anywhere a
   `u64` file length meets a `usize` buffer, clamp by the buffer
   size *first*, then cast. This is what keeps 32-bit targets safe
   on > 4 GB files.
5. **Paths normalize the same way everywhere.**
   `path.trim_start_matches('/')` is the canonical normalization
   inside `find_node`. Use it; don't reinvent it.
6. **`TreeNode` is the wire format between parsers and the rest.**
   New parsers must produce a `TreeNode` tree; new consumers must
   accept one.
7. **Zero runtime dependencies.** Adding a `[dependencies]` entry
   needs a real justification in the PR — see
   [`CONTRIBUTING.md`](CONTRIBUTING.md). The point of being
   pure-Rust-and-tiny is that downstream consumers can adopt the
   crate without auditing a tree.
8. **Promptlog gate.** Every PR that changes `src/` or `Cargo.toml`
   commits a `prompts/YYYYMMDD-HHMMSS-<slug>.md` file. CI enforces
   this.

### Extension points

| You want to… | Touch this |
|---|---|
| Support a new on-disc filesystem (HFS+, exFAT, FAT) | Add `src/<fs>.rs` exposing `parse_<fs>{,_verbose}`. Register it in `detect_and_parse_filesystem_verbose` in `src/lib.rs` after the existing tries. |
| Add a new metadata field to entries (timestamps, permissions) | Add fields to `TreeNode` in `src/tree.rs`, populate from each parser, render where appropriate. Adding *new* `pub` fields is non-breaking; reordering or removing existing ones is. |
| Make parsing faster | Look at `EXTRACT_CHUNK_SIZE` in `src/lib.rs` and the inner read loops in `iso9660.rs` / `udf.rs`. No mmap, no `unsafe`. |
| Add a new diagnostic in `-verbose` mode | `eprintln!` from inside the parser, gated on `verbose`. |
| Improve docs.rs landing | Crate-level `//!` doc at the top of `src/lib.rs` controls the docs.rs front page. |

---

## If you want a CLI

There isn't one (any more — see [CHANGELOG](CHANGELOG.md)). About
50 lines of `main.rs` on top of `isomage` reproduces the previous
`isomage IMAGE`, `isomage -c PATH IMAGE`, `isomage -x PATH IMAGE`
behaviour:

```rust
use std::env;
use std::fs::File;
use std::io;
use std::process::ExitCode;
use isomage::{detect_and_parse_filesystem, cat_node, extract_node, TreeNode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let usage = "usage: isomage [-c PATH | -x PATH [-o DIR]] IMAGE";
    let (mode, target, out, image) = match args.as_slice() {
        [_, image]                          => ("list", "",      ".", image.clone()),
        [_, flag, path, image] if flag=="-c"=> ("cat",  path.as_str(), ".", image.clone()),
        [_, flag, path, image] if flag=="-x"=> ("ext",  path.as_str(), ".", image.clone()),
        [_, "-x", path, "-o", dir, image]   => ("ext",  path.as_str(), dir, image.clone()),
        _ => { eprintln!("{usage}"); return ExitCode::from(2); }
    };
    let mut iso = match File::open(&image) {
        Ok(f) => f, Err(e) => { eprintln!("open {image}: {e}"); return ExitCode::from(1); }
    };
    let root = match detect_and_parse_filesystem(&mut iso, &image) {
        Ok(r) => r, Err(e) => { eprintln!("parse {image}: {e}"); return ExitCode::from(1); }
    };
    let result: isomage::Result<()> = match mode {
        "list" => { walk(&root, 0); Ok(()) }
        "cat"  => {
            let n = root.find_node(target).ok_or("not in ISO")?;
            cat_node(&mut iso, n, &mut io::stdout().lock())
        }
        "ext"  => {
            let n = root.find_node(target).ok_or("not in ISO")?;
            extract_node(&mut iso, n, out)
        }
        _ => unreachable!(),
    };
    match result {
        Ok(())  => ExitCode::SUCCESS,
        Err(e)  => { eprintln!("{e}"); ExitCode::from(1) }
    }
}

fn walk(n: &TreeNode, d: usize) {
    println!("{:w$}{} {} ({} B)", "",
        if n.is_directory { "d" } else { "-" }, n.name, n.size, w=d*2);
    for c in &n.children { walk(c, d+1); }
}
```

If you need a packaged CLI as a Cargo package, fork it; the
maintainer of `isomage` is intentionally not distributing one.

---

## Build and test

```sh
make test-data       # generate the synthetic ISOs under test_data/
cargo test           # 30 unit tests + 5 doc-tests
cargo doc --open     # browse the rustdoc locally
```

CI runs `test` (macOS + Ubuntu), `fmt`, `clippy --all-targets -D warnings`,
`doc --no-deps` with `RUSTDOCFLAGS="-D warnings"`, MSRV-build (Rust
1.74), `cargo audit` against `Cargo.lock`, and a `cargo package`
contents check so we don't accidentally ship `prompts/` or test data
to crates.io.

Release flow:

1. Bump `version` in `Cargo.toml` and add a `## [vX.Y.Z]` block to `CHANGELOG.md`.
2. Add a `prompts/` entry recording the bump (the CI gate watches `Cargo.toml`).
3. Merge to `main`.
4. Tag `vX.Y.Z` and push the tag. `.github/workflows/release.yml`
   creates a GitHub Release with auto-generated notes and runs
   `cargo publish`. That's it — no binaries, no Homebrew tap.

---

## Security

`isomage` parses untrusted binary data. Vulnerability reports should
go to GitHub's [private security advisories](https://github.com/JackDanger/isomage/security/advisories/new),
not the public issue tracker — see [`SECURITY.md`](SECURITY.md) for
the full policy. The current hardening surface (path-traversal
guards, `BrokenPipe` tolerance, 64-bit-safe extract loops) is
summarized there.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) and the
[PR template](.github/pull_request_template.md).

The repo follows the [**promptlog pattern**](https://jackdanger.com/promptlog/):
every PR that changes `src/` or `Cargo.toml` commits a sanitized log
of the prompts that led to the change. The spec is in
[`prompts/PROMPTLOG.md`](prompts/PROMPTLOG.md); agents can use the
[`promptlog`](.claude/skills/promptlog.md) skill; the CI gate
enforces it.

If you're an AI agent reading this, also read [`CLAUDE.md`](CLAUDE.md)
— that's the short rulebook for this repo.

---

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for the curated change list per
release. Auto-generated release notes also live on each
[GitHub Release](https://github.com/JackDanger/isomage/releases).

The previous CLI binary was discontinued in v2.0.0. If you installed
via `cargo install isomage` from a v1 release, that binary still
works locally; future `cargo install isomage` will fail (the package
no longer publishes a `[[bin]]`). The "If you want a CLI" snippet
above reproduces the previous behaviour in your own project in ~50
lines.

---

## License

[MIT](LICENSE).
