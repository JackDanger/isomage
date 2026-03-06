# isomage

Browse and extract files from ISO images without mounting them.

No root. No FUSE. No mount points. Just read the bytes.

<p align="center">
  <img src="doc/demo.svg" width="640" alt="isomage demo showing listing, cat, and extraction">
</p>

## Install

Grab a binary from [releases](../../releases), or build from source:

```
cargo build --release
```

## Usage

**List** what's on a disc:

```sh
isomage movie.iso
```

**Cat** a file straight to stdout — no extraction, no temp files:

```sh
isomage -c BDMV/PLAYLIST/00000.mpls movie.iso | hexdump -C
isomage -c etc/hostname linux.iso
isomage -c readme.txt data.iso | less
```

**Extract** a file, a directory, or everything:

```sh
isomage -x BDMV/STREAM/00001.m2ts movie.iso
isomage -x BDMV/STREAM -o ./streams movie.iso
isomage -x / -o ./full_dump movie.iso
```

**Debug** a disc that won't parse:

```sh
isomage -v movie.iso
```

## Supported formats

- **ISO 9660** with Joliet and Rock Ridge extensions
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
