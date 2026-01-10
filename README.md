# isomage

Browse and extract files from ISO images without mounting them.
I got tired of having to exit a container just to mount an image just to read a file. This tool runs entirely in userspace—no root, no fuse, no mount points.

## Install

Grab a binary from releases, or build it yourself:

```
cargo build --release
```

Cross-compile for Linux (from macOS):

```
make build-linux
```

## Usage

List contents:

```
isomage movie.iso
```

Extract a file:

```
isomage -x BDMV/STREAM/00000.m2ts movie.iso
```

Extract a directory:

```
isomage -x BDMV/STREAM movie.iso
```

Extract everything:

```
isomage -x / movie.iso
```

Extract to a specific location:

```
isomage -x BDMV -o ./output movie.iso
```

Debug a weird disc:

```
isomage -v movie.iso
```

## Supported formats

- **ISO 9660** — standard CD/DVD images
- **UDF** — DVDs and Blu-rays, including discs with metadata partitions

## Limitations

- Read-only (by design)
- No multi-extent file support yet (very large files split across multiple extents)
- Some exotic UDF variations might not parse correctly

If you hit a disc that doesn't work, run with `-v` and open an issue.
