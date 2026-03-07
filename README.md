# isomage

Browse and extract files from ISO images without mounting them.

No root. No FUSE. No mount points. Just read the bytes.

<p align="center">
  <img src="doc/demo.svg" width="640" alt="isomage demo showing listing, cat, and extraction">
</p>

## Install

**Homebrew** (macOS and Linux):

```sh
brew install jackdanger/tap/isomage
```

**Cargo** (any platform with Rust):

```sh
cargo install isomage
```

**Binary** — grab a prebuilt binary from [releases](../../releases):

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

PATH is a path inside the ISO. Leading slash is optional — `etc/hostname`
and `/etc/hostname` are equivalent. Use `/` with `-x` to extract everything.

All diagnostic output (verbose, progress, errors) goes to **stderr**.
Only file data goes to **stdout**, so `-c` is binary-safe and pipe-friendly.

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

Output legend: `d` = directory, `-` = file. Size shown is the total including children for directories.

### Stream a file to stdout (`-c`)

Print any file's raw bytes to stdout — no extraction, no temp files:

```sh
# Inspect a text file
isomage -c etc/hostname linux.iso

# Pipe to other tools
isomage -c BDMV/PLAYLIST/00000.mpls movie.iso | hexdump -C

# Page through a large file
isomage -c readme.txt data.iso | less

# Play video directly from the ISO (verbose output stays on stderr)
isomage -c BDMV/STREAM/00000.m2ts movie.iso | mpv -

# Extract a single file to stdout and redirect to disk
isomage -c BDMV/STREAM/00000.m2ts movie.iso > output.m2ts
```

### Extract to disk (`-x`)

Extract a single file, a directory tree, or the whole disc:

```sh
# One file (written to current directory)
isomage -x BDMV/STREAM/00001.m2ts movie.iso

# A directory (recursively), into a specific output directory
isomage -x BDMV/STREAM -o ./streams movie.iso

# The entire disc
isomage -x / -o ./full_dump movie.iso
```

Extracted files are written to `--output` (default: current directory).
The output directory is created automatically if it doesn't exist.
Progress is reported on stderr; extracted file paths are printed to stderr as each file completes.

### Verbose / debug mode (`-v`)

See how the filesystem is being parsed — useful when a disc won't read:

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

Verbose output always goes to stderr, so it's safe to combine with `-c`:

```sh
isomage -v -c etc/hostname linux.iso | xxd   # file bytes on stdout, debug on stderr
```

## Output format

The list mode (`isomage IMAGE`) prints one entry per line:

```
d NAME (SIZE)
- NAME (SIZE)
```

- `d` = directory, `-` = file
- Entries are indented two spaces per level of depth
- SIZE is human-readable (B, KB, MB, GB, TB)
- Directory sizes include all descendants
- All output goes to stdout; nothing goes to stderr in list mode

## Exit codes

| Code | Meaning |
|------|---------|
| 0    | Success |
| 1    | Error (file not found, parse failure, path not found in ISO, I/O error) |

## Supported formats

- **ISO 9660** with Joliet (Unicode filenames) and Rock Ridge (POSIX long filenames) extensions
- **UDF** including metadata partitions and multi-extent files

Covers CDs, DVDs, and Blu-rays.

## Why

I got tired of leaving a container just to `mount` an image just to read one file. `isomage` runs entirely in userspace — it reads the raw bytes and reconstructs the filesystem tree itself.

## Cross-compile

```sh
make install-targets   # one-time: adds musl target
make build-linux       # static linux binary from macOS
```

## Limitations

- Read-only (by design)
- Some exotic UDF variations might not parse correctly

If you hit a disc that doesn't work, run with `-v` and open an issue.

## License

[MIT](LICENSE)
