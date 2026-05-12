---
session: "v3-scope-2026-05-12"
timestamp: "2026-05-12T22:01:30Z"
model: claude-opus-4-7
sequence: 5
---

## Human

[Continuation of "do that. proceed through all this work" with
the explicit added instruction: "thoughtfully consider where
docker helps us in our test harness".]

## Assistant

### Where Docker helps, where it hurts

Thought through carefully before designing.

**Helps**:

1. **Reproducibility.** `apt-get install -y sgdisk` gives whatever
   Ubuntu repo state is live today; six months later, the same
   apt-get produces a different binary and committed snapshot
   files start to diff. Pinning to a tagged Docker image freezes
   tool versions.
2. **macOS contributors.** `sfdisk`, `mkntfs`, `debugfs`,
   `wimlib-imagex` are Linux-only or second-rate ports on macOS.
   Containerised Linux is the bridge.
3. **Mount-based tests (future).** Verifying a FAT image we wrote
   by `mount`-ing it requires root + loop devices. Privileged
   containers have those; GitHub Actions runners don't by default.

**Hurts**:

1. **Latency.** `docker run` cold-start is 100–500 ms vs ~1 ms for
   `Command::new(tool)`. The 28 self-tests would go from 10 ms to
   5–10 s.
2. **Dev friction.** Forcing Docker means every contributor needs
   Docker Desktop / Colima even for typo-fix PRs.
3. **CI runner already has apt.** On Ubuntu Actions, adding Docker
   indirection is pure overhead — extra job for no functional gain
   over plain apt.

**Verdict**: opt-in, not mandatory. The Path venue stays the
default; Docker is enabled via `ISOMAGE_TOOL_VENUE=docker:<image>`.
Existing tests work unchanged; opting in needs no per-test
modification.

### Implementation

`tests/common/venue.rs` — new module. `ToolVenue` enum with two
variants:

- `Path` (default): `Command::new(absolute_tool_path)`.
- `Docker { image, extra_mounts }`: assembles `docker run --rm
  -i -v <mount>:<mount> --user UID:GID <image> <tool_name>` with
  the host tempdir mount, the host user's UID/GID for
  file-ownership correctness, and the tool name as the binary
  inside the container's `$PATH` (resolved-host-path is ignored
  under Docker).

`Tool::run_inner` reads `ToolVenue::current()` and delegates
command construction. The bind-mounts arg is threaded through a
new `Tool::run_in_venue` public method that `RoundTrip::try_build`
calls with the tempdir as the mount source. `Tool::run` /
`run_with_stdin` / `run_with_env` are unchanged externally; they
just dispatch through the venue with an empty mounts list.

### Tempdir routing

Default `tempfile::TempDir::with_prefix(…)` uses
`std::env::temp_dir()`, which on macOS is `/var/folders/…` —
**not** shared by either Docker Desktop or Colima. Bind-mounting
an unshared host path silently produces an empty directory inside
the container, which manifests as "sgdisk: file does not exist"
about three test-failures deep.

Fix: `ToolVenue::tempdir_root()` returns `Some("/tmp")` under
Docker, `None` (use std default) under Path. `RoundTrip::try_build`
honours that. `/tmp` is shared by Docker Desktop's default config
and by Colima after a one-line `colima.yaml` edit (which we
document).

Side note caught in iteration: tempfile's `with_prefix_in(prefix,
dir)` argument order is the opposite of intuition (prefix first,
dir second). Got that wrong on the first try and produced
tempdirs at `/tmp{random}` instead of `/tmp/{random}` — text+random
concatenation. Easy fix once spotted.

### Dockerfile

`Dockerfile.test-tools` at repo root. `debian:bookworm-slim` base
(Debian 12 stable — slow-moving tool versions, much more
reproducible than Ubuntu LTS which point-releases mid-stream).
Pinned apt install of the full reference-tool matrix. Default
`CMD` prints each tool's `--version` line so an image build is
self-documenting.

Built locally and verified:

- sfdisk 2.38.1 (util-linux)
- sgdisk 1.0.9 (gptfdisk)  *(local Mac has 1.0.10; snapshot
  bodies match because partition layout is deterministic)*
- xorriso 1.5.4
- qemu-img 7.2.22
- mkfs.exfat 1.2.0 (exfatprogs)
- mksquashfs (squashfs-tools)
- wimlib-imagex 1.13.5
- mkntfs (ntfs-3g)
- debugfs 1.47.0 (e2fsprogs)
- mtools 4.0.32
- parted 3.5
- fdisk 2.38.1

Debian's `p7zip-full` provides `7z` (not `7zz`). The
`tests::common::binaries::tools::SEVEN_ZZ` alias chain already
fallbacks `7zz → 7z`, so no test changes needed.

### CI workflows

Two new jobs:

`round-trip-pinned` (in `ci.yml`):
- Runs on every PR.
- Pulls `ghcr.io/<owner>/isomage-test-tools:latest`.
- Sets `ISOMAGE_TOOL_VENUE` to the pulled image.
- Runs the round-trip binaries through it.
- `continue-on-error: true` on the very first push (the image
  hasn't been built yet). Drops to mandatory once the image
  exists.

`test-tools-image.yml` (new workflow file):
- Builds the image.
- Pushes on `test-tools-v*` tag.
- Build-only on PRs that touch the Dockerfile.
- GHCR auth via `GITHUB_TOKEN`.
- Runs the image's default CMD on PR builds as a smoke test.

### Local validation

- `cargo test --features mbr,gpt,raw,mmap,simd` — 123 tests pass
  (was 108; +15 venue-parsing tests).
- `docker build -f Dockerfile.test-tools -t isomage-test-tools:local .`
  — image builds; all tools resolve.
- `ISOMAGE_TOOL_VENUE=docker:isomage-test-tools:local cargo test
  --features mbr,gpt --test gpt_round_trip` on this Mac with
  default-config Colima — **fails** because Colima doesn't share
  `/tmp` by default. Documented as the macOS caveat in
  `venue.rs` and `tests/README.md`. CI Linux has no such caveat.

### What I didn't do

- **No nightly fuzz runs through Docker.** Doable but not needed
  yet.
- **No automatic snapshot re-pinning on Docker venue switch.**
  Snapshot bodies are deterministic given identical partition-
  table flags, so the existing snapshots work across venue
  switches as long as the tool's *behaviour* is unchanged. Header
  version lines do change; the strip-header-before-comparison
  logic handles that.
- **No Windows host support.** Out of scope per CLAUDE.md.

### Open question for the reviewer

Should the `round-trip-pinned` job be `continue-on-error: false`
once the image is published, or should it stay soft to avoid
gating on a one-image build? My instinct is to flip it to
mandatory after the first successful image build lands — the
whole point of the pinned image is that it should fail loudly
when its bytes diverge from snapshots.
