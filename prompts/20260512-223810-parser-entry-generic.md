---
session: "v3-scope-2026-05-12"
timestamp: "2026-05-12T22:38:10Z"
model: claude-opus-4-7
sequence: 7
---

## Human

[After PR #10 merged to main, picked option (a): merge then start
PR A2 next. PR A2 is the parser-entry generalization from the plan.]

## Assistant

### The PR A2 lever

This is the smallest possible change that unlocks `MmapImage` for
the v2 entry points. After this lands, `cargo install isomage`
users can replace `File::open(path)` with `MmapImage::open(path)`
in one line and get kernel page-cache prefetch + zero-syscall
reads through `cat_node`/`extract_node`. It's also the prerequisite
for PR A4 (zero-copy parser core via `RandomAccess`), where the
existing parsers will be rewritten to borrow `&[u8]` slices directly
out of an mmap.

### What changed

Every public reader entry point's first parameter:

  &mut File  →  &mut R  where R: Read + Seek

Concretely in `src/lib.rs`:

  detect_and_parse_filesystem<R: Read + Seek>(file: &mut R, ...)
  detect_and_parse_filesystem_verbose<R: Read + Seek>(file: &mut R, ...)
  cat_node<R: Read + Seek, W: Write>(file: &mut R, ...)
  extract_node<R: Read + Seek>(file: &mut R, ...)

And the internal helpers they call:

  extract_into<R: Read + Seek>(file: &mut R, ...)
  extract_file_at<R: Read + Seek>(file: &mut R, ...)

Same pattern in `src/iso9660.rs`:

  parse_iso9660<R: Read + Seek>(file: &mut R)
  parse_iso9660_verbose<R: Read + Seek>(file: &mut R, verbose: bool)
  detect_rock_ridge<R: Read + Seek>(file: &mut R, ...)
  parse_directory<R: Read + Seek>(file: &mut R, ...)

And `src/udf.rs`:

  parse_udf<R: Read + Seek>(file: &mut R)
  parse_udf_verbose<R: Read + Seek>(file: &mut R, verbose: bool)
  parse_directory<R: Read + Seek>(file: &mut R, ...)
  get_file_info<R: Read + Seek>(file: &mut R, ...)

### Why explicit `<R: Read + Seek>` rather than `&mut (impl Read + Seek)`

Both are equivalent at the type level. I picked the explicit form
for three reasons:

1. **Reads cleaner with the second generic.** `cat_node` already
   had `<W: Write>`. Mixing `impl Trait` for `R` and explicit
   `<W>` for the writer felt asymmetric.

2. **Easier to deprecate / re-rename later.** Named generics are
   findable; `impl Trait` arguments anonymise the parameter.

3. **Single source of truth for the bound.** Where helpers chain
   the generic through (e.g., `extract_node` calls
   `extract_into`), having both signatures spell `R: Read + Seek`
   makes the propagation obvious to a reader.

### Source compatibility

Every existing caller passes `&mut File`. `File: Read + Seek`. So
the API change is binary-incompatible (it's generic now) but
source-compatible — no existing user code needs to change. This
PR therefore doesn't bump version (still 2.0.x); a follow-on
release-prep PR will cut a 2.1.0 minor when there's a meaningful
shipping target.

The compile-time monomorphisation does mean the released binary
size grows by ~N copies of each parser fn body (one per concrete
`R`). In practice consumers will pass exactly one type (`File`,
or `MmapImage`, never both in the same binary unless they're
benchmarking), so the cost is one extra copy. Acceptable.

### Parity test

`tests/mmap_parity.rs` proves the change is semantics-preserving:

- `parse_via_file(path)` and `parse_via_mmap(path)` produce trees
  that compare structurally-equal (name + size + is_directory +
  file_location + file_length, recursively).
- `cat_node` through `MmapImage` returns identical bytes to
  `cat_node` through `File` for the same file in the same image.
- Both checked-in test ISOs (`test_linux.iso`, `test_macos.iso`)
  exercise the parity path. Tests skip cleanly if `make
  test-data` hasn't been run.

This is the strongest evidence the generalization didn't quietly
break either path. If a future refactor accidentally diverges the
File and Mmap codepaths, this test catches it.

### Cleanup beneficiary: `tests/common/assertions.rs`

`assert_file_contents` previously had to materialize image bytes
to a tempfile before calling `cat_node`, because `cat_node`
required `&mut File`. With the generalization, the helper now
hands `Cursor::new(image)` straight to `cat_node` — no tempfile,
no IO syscalls, same assertion. Removed the dead `tempfile`
import path and the `_silence_cursor_warning` shim.

This is exactly the kind of downstream win the harness was built
to receive. Every future format that adds a `<format>_round_trip.rs`
gets `assert_file_contents` for free; the cleanup made it cheaper.

### What I deliberately didn't do

- **No version bump.** `Cargo.toml` stays at 2.0.0. Cutting 2.1.0
  is a separate decision driven by an actual release intent.
- **No deprecation of the existing entries.** They're not
  going away. The generalization is additive in capability.
- **No `RandomAccess`-path parsing for ISO/UDF.** That's PR A4.
  The internals here still go through `Read + Seek` and allocate
  per-sector buffers. Zero-copy is a separate, more invasive
  refactor.
- **No `Box<dyn Read + Seek>` shorthand type alias** (e.g.
  `pub type ImageSource = dyn Read + Seek + 'static`). Considered
  it; decided the type sigs are already short and a type alias
  would obscure the bound at the call site.

### Test results

- `cargo test` (default features) — 33 lib + 5 doc = 38 tests pass.
- `cargo test --features mbr,gpt,raw,mmap,simd` — 54 + 14 + 33 +
  15 + 15 + 8 = **139 tests pass** (was 123; +16 from
  `mmap_parity.rs` and the new round-trip-test compilations).
- `ISOMAGE_REQUIRE_TOOLS=1 cargo test --features gpt --test gpt_round_trip`
  — still green; the snapshot file's body matches.
- `cargo clippy --all-targets --features mbr,gpt,raw,mmap,simd -- -D warnings`
  — clean.
- `cargo fmt --check` — clean.
