# v3.0 implementation handoff

This document captures the state of the v3.0 work as of the most
recent commit on `chore/v3.0-foundation`. It exists so the next
agent (Sonnet / Composer 2) can resume without re-deriving
context.

> The complete plan is in the prompt logs:
> `prompts/20260512-193754-v3-scope-expansion.md` (substrate),
> `prompts/20260512-210223-round-trip-harness.md` (PR A1).
> Read both before starting work.

## What landed across the v3.0 foundation branch

- **Phase 0** — `CLAUDE.md` rewritten for v3.0 invariants; `Cargo.toml`
  feature matrix scaffolded (`mmap`, `simd`, `write`, plus one per
  planned format and codec). Zero new runtime deps in `default`.
- **Phase 1** — `benches/seqread.rs` runs `isomage` and `7zz` over
  the corpus in `test_data/` via criterion. Skip-if-7zz-missing.
  Compiled (not executed) in CI on every push so bench regressions
  surface at build time.
- **Phase 2** — `image_io::RandomAccess` trait + `MmapImage`
  (`mmap` feature, with contained `unsafe`); `simd::crc16_ccitt`
  (`simd` feature, table-based scalar with `TODO: PMULL`).
- **Phase 3 start** — `formats::mbr`, `formats::gpt`,
  `formats::raw`. Each emits a `TreeNode` whose children are the
  partition byte ranges; `cat_node` extracts partition contents.
- **PR A1 — round-trip harness** — `tests/common/` infrastructure:
  Tool detection with alias chains, `RoundTrip` builder with
  `$IMAGE`/`$SRC_DIR` substitution, golden-file snapshots,
  `require_or_skip` pattern. Self-test (28 tests, universal POSIX
  tools) runs everywhere; per-format `*_round_trip.rs` tests run
  against real `sfdisk`/`sgdisk`/etc. Verified end-to-end against
  `sgdisk 1.0.10` on macOS; committed snapshot files give CI a
  pinned reference.
- 54 lib tests + 7 doc tests + 28 self-test + 10 MBR + 9 GPT =
  **108 tests** pass with `--features mbr,gpt,raw,mmap,simd`.
  `cargo clippy --all-targets -- -D warnings` clean on every
  feature combo.

## What did NOT land, and why

| Plan item | Status | Why |
|-----------|--------|-----|
| `TreeNode.name → Cow<'_, str>` | Not started | Requires rewriting `iso9660.rs` + `udf.rs` to take `&[u8]` (RandomAccess) instead of `Read + Seek`. ~50 internal callsites. High risk of subtle regression that the existing 35-test suite won't catch. PR A2 (parser-entry generalization) lands first as a stepping stone. |
| Parser entry-point generalization (`&mut File` → `&mut (impl Read+Seek)`) | **Next PR (A2)** | Mechanically straightforward, source-compatible, unblocks `MmapImage` for v2 callers. |
| GitHub issue for v3.0 milestone | Not opened | Outward-facing; user can open it manually. The prompt logs capture the same content. |
| Real SIMD CRC (PMULL/CLMUL) | Stubbed | Setup cost only amortizes above ~1 KiB; UDF descriptors are 16–512 B. The scalar table is the right baseline. Add intrinsics when SquashFS or VHDX log lands. |
| Bench corpus | Trivially small | Existing test ISOs are ~400 KB each — overhead-dominated. Real numbers need ≥100 MB images. `benches/seqread.rs` doc comment has the curl recipe; a `make bench-corpus` Makefile target would be a one-PR improvement. |
| **`gzippy` crate identity** | UNRESOLVED | User named this crate; `cargo search` returns no match. Phase 0 reserves the `deflate-gzippy` feature flag but adds no dep. **The first DEFLATE-bearing format (qcow2/dmg/squashfs/wim) needs the user to re-confirm or pick from `flate2`/`libdeflater`/`miniz_oxide`/`libflate`. Block on this answer.** PR A3 (codec policy) is the gate. |
| Fuzz harness for ISO/UDF | Not yet | MBR + GPT fuzz target stubs land with PR A1 polish; ISO/UDF parsers don't have one yet. Phase 6 of the plan; can pull forward at low cost. |
| Cross-tool cross-validation | Not yet | Round-trips trust the reference tool. Parsing the same image with `fdisk -l` AND our parser AND asserting agreement would catch "we and sfdisk are wrong the same way" cases. Phase 6. |

## Where the API stands

```text
v2.x API — unchanged, still works:
  detect_and_parse_filesystem(&mut File, &str) -> Result<TreeNode>
  cat_node(&mut File, &TreeNode, &mut impl Write) -> Result<()>
  extract_node(&mut File, &TreeNode, &Path) -> Result<()>
  TreeNode { name: String, .. }

v3.0 additions — additive, all behind features:
  image_io::RandomAccess (trait)
  image_io::RandomAccessMut (trait)
  image_io::MmapImage (struct, --features mmap)
  simd::crc16_ccitt (--features simd)
  formats::mbr::{parse, detect_and_parse, to_tree, Partition, Error}
  formats::gpt::{parse, detect_and_parse, to_tree, Partition, Error, Header}
  formats::raw::detect_and_parse
```

The breaking changes that **will** make this v3.0 (not v2.1) are
the `TreeNode.name → Cow` refactor and the parser-entry
generalization. Until those land, the existing v2.x API is intact;
the version in `Cargo.toml` stays at `2.0.0`.

## Test infrastructure available to follow-on agents

Every new format PR should use these. The shape is exercised end
to end (mbr + gpt) and committed; pattern docs live in
`tests/README.md`.

- **`tests/common/tool.rs`** — `Tool::with_aliases("primary",
  &["alias1", "alias2"])` declares a binary; `require_or_skip()`
  is the test-guard pattern. Strict mode via env var.
- **`tests/common/round_trip.rs`** — `RoundTrip::new("<name>")`
  builder; `.with(tool)`, `.args(["...$IMAGE..."])`, `.stdin(...)`,
  `.source_file(...)`, `.image_size(...)`, `.build_bytes()`.
- **`tests/common/snapshot.rs`** — `assert_snapshot_with_tool(name,
  tree, tool_version)`. Snapshots live at `tests/snapshots/<name>.snap`
  with reference-tool version in the header.
- **`tests/common/assertions.rs`** — `assert_path_exists`,
  `assert_partition_at`, `assert_file_contents`,
  `assert_tree_invariants`.
- **`tests/common/binaries.rs`** — pre-declared `Tool`s for every
  format we plan to support. Add new entries here when a new
  reference tool enters the matrix; also update the apt/brew
  install list in `.github/workflows/ci.yml`.

When a new format lands, the agent writes one
`tests/<format>_round_trip.rs` file using these helpers, adds
the format's feature flag to `Cargo.toml` plus a
`required-features` declaration for the test binary, and the
existing `round-trip` CI job picks it up.

## Worktree-agent task list (Phase 3 cont. + Phase 4)

Each row is intended as a single `Agent` worktree task. The agent
gets `subagent_type: claude` (or your project equivalent),
`isolation: worktree`, and the prompt template below. Every row
**requires** a round-trip test against the named reference tool
in the same PR — that is the merge gate.

### Phase 3 — read parity

| Format | Module | Reference tool for tests | Notes |
|--------|--------|--------------------------|-------|
| VHD (fixed + dynamic) | `formats::vhd` | `qemu-img info` | Well-documented Microsoft spec. Start here — easiest. |
| VHDX | `formats::vhdx` | `qemu-img info` | Has a log structure; replay only on read if log is dirty. |
| VMDK (flat, sparse, stream-optimized) | `formats::vmdk` | `qemu-img info` | Descriptor file + grain table. |
| QCOW2 (v2 + v3) | `formats::qcow2` | `qemu-img info`, `qemu-img check` | **First DEFLATE-bearing format.** Block on `gzippy` resolution. |
| WIM (XPRESS + LZX) | `formats::wim` | `wimlib-imagex info` | LZX is the hard one — partial spec. Allocate research week. |
| DMG (UDIF: zlib/bzip2/LZMA) | `formats::dmg` | `hdiutil verify` | Multi-codec. |
| FAT12/16/32 | `formats::fat` | `fsck.vfat`, `mtools` | Pragmatic + common. |
| exFAT | `formats::exfat` | `fsck.exfat` | Like FAT but with checksum table; well-documented. |
| ext2/3/4 | `formats::ext` | `e2fsck -fn`, `debugfs` | Skip journal replay; treat journal as advisory. |
| HFS+ | `formats::hfsplus` | `fsck_hfs` | B-trees: catalog + extents. |
| SquashFS | `formats::squashfs` | `unsquashfs -ll` | Multi-codec (xz, zstd, lz4, gzip). |
| NTFS | `formats::ntfs` | `ntfsfix`, `ntfsls` | **Scope-cut**: MFT + $DATA + $I30 only. Don't chase resident/non-resident edge cases beyond what real images use. |
| APFS | `formats::apfs` | `fsck_apfs` | **Defer to v3.2** — partial open spec, encrypted by default. |

### Phase 4 — write parity (`--features write,experimental`)

7-Zip writes almost no disk-image formats, so "exceed 7z on writes"
is mostly "have writes at all." Order, easiest first:

1. `raw_img` write — trivial
2. `vhd` (fixed → dynamic) — well-specified footer/header
3. `vmdk` (flat → sparse) — descriptor + grain table
4. `qcow2` create — L1/L2 + refcount table
5. `vhdx` create — log + BAT
6. `iso 9660 + Joliet + Rock Ridge` (mkisofs-equivalent) — **largest single subtask**
7. `udf 2.50` write — **defer to v3.2** if it threatens timeline
8. `fat`/`exfat` create — well-documented
9. `ext4` create — **defer to v3.3**; mkfs.ext4 is ~10k LOC

### Phase 5 — optimization
Profile vs. `benches/seqread.rs` baseline. Hot-path inlining,
buffer reuse, lookahead prefetch. Goal: ≥1.3× 7-Zip on every
format both tools support.

### Phase 6 — hardening
`cargo-fuzz` per format. Miri pass on all `unsafe`. Corpus of
100+ real-world images. Rewrite README + lib.rs rustdoc around
the unified `Image::open` / `Image::create` facade.

## Worktree-agent prompt template

```
You are adding the `<FORMAT>` parser to the `isomage` crate. The
project has invariants documented in CLAUDE.md — read it first.
The v3.0 design rationale is in
prompts/20260512-193754-v3-scope-expansion.md. The handoff doc is
HANDOFF.md. Existing format submodules (`src/formats/mbr.rs`,
`src/formats/gpt.rs`) are the house style to match.

Your single deliverable:

1. Implement `src/formats/<format>.rs` behind feature `<format>`.
2. It must expose:
     pub fn detect_and_parse(file: &mut File) -> Result<TreeNode, Error>
     pub fn parse(...) -> Result<Vec<Entry>, Error>  // raw entries
     pub fn to_tree(entries: &[Entry]) -> TreeNode
3. Unit tests with synthetic headers covering: missing magic,
   minimal valid header, off-by-one boundary cases.
4. A round-trip integration test against `<reference-tool>` (e.g.
   "create a <format> image with <tool>, parse it with this code,
   assert file list matches `<tool> list`"). The test goes in
   `tests/<format>_roundtrip.rs` and may use `#[ignore]` if the
   tool is not on every CI runner — gate it on env var
   `ISOMAGE_HAVE_<TOOL>=1`.
5. A prompt-log file at prompts/YYYYMMDD-HHMMSS-add-<format>.md
   describing the key design decisions.

DO NOT:
- Touch iso9660.rs, udf.rs, tree.rs, or lib.rs's existing API.
- Add a runtime dep without updating the prompt log to justify it.
- Use `unsafe` outside the `mmap`/`simd` modules.
- Write to the input file under any circumstances.

When done, report:
- "Implemented <format>. Reference-tool round-trip passes.
   N tests, M doc-tests, clippy clean on --features <format>.
   No prompt log yet — the orchestrator will commit one."
```

## Open questions for the user

1. **`gzippy`** — what crate is this? Resolution blocks the first
   DEFLATE-bearing format (likely qcow2).
2. **CI runner provisioning** — round-trip tests against
   `mkisofs`, `qemu-img`, `mkfs.vfat`, `mkfs.exfat`, `unsquashfs`,
   `wimlib`, `mtools`, `debugfs`, etc. need those tools installed.
   Should we extend `.github/workflows/ci.yml` to apt-install them,
   or stand up a separate filesystem-tools job with a cached image?
3. **`Image::open` facade** — when do we cut over from the v2 API
   to the unified facade? Doing it incrementally (additive in
   v3.0, then deprecating in v3.1, removing in v4.0) is safer
   but doubles the doc surface for a release cycle.
4. **`apfs` and `ext4 write`** — recommend deferring both to v3.2
   so they don't gate v3.0 from shipping the read matrix.

## A note on what "exceed 7z" actually requires

After Phase 2, the levers we have for sequential read are:

- mmap + `MADV_SEQUENTIAL` (kernel prefetch beats stdio buffering)
- Zero-copy slices via `RandomAccess` (no per-sector memcpy)
- Fast codec choice for compressed formats (libdeflate-class for DEFLATE)
- Buffer reuse across files in extract loops

The `TreeNode.name → Cow` refactor + parser generalization is
what unlocks the second lever. Without it, the existing ISO/UDF
parsers can't go zero-copy. Plan that as the v3.0-final session.
