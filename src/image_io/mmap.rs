//! Memory-mapped image reader (`mmap` feature).
//!
//! [`MmapImage`] opens a disc image once, mmaps the whole file, and
//! satisfies both [`Read`] / [`Seek`] (so existing v2 parsers keep
//! working) and [`super::RandomAccess`] (so Phase 3 format modules
//! can do zero-copy reads).
//!
//! The win over a plain `File`:
//!
//! 1. **Page-cache prefetch** — Linux/macOS read-ahead is more
//!    aggressive than libc's stdio buffering, especially on cold
//!    images.
//! 2. **Zero-copy** — `RandomAccess::read_at` hands out `&[u8]`
//!    slices into the mapped region directly, no syscall, no memcpy.
//! 3. **`madvise(MADV_SEQUENTIAL)`** — tells the kernel "I'm going
//!    to scan this start-to-end" so it can drop pages eagerly behind
//!    the read head, keeping resident-set size modest on
//!    multi-gigabyte UDF images.
//!
//! The `unsafe` block is contained to the single `Mmap::map(...)`
//! call. The safety contract is that the underlying file mustn't be
//! truncated while the `Mmap` is live — `MmapImage` keeps the
//! `File` handle alive for its own lifetime to enforce that.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use memmap2::{Advice, Mmap};

use super::RandomAccess;

/// Memory-mapped, read-only view over a disc image.
///
/// Implements `Read + Seek` so v2 parsers like
/// [`crate::detect_and_parse_filesystem`] take an `MmapImage`
/// transparently, and [`RandomAccess`] so v3 format submodules can
/// borrow slices without copying.
///
/// # Example
///
/// ```no_run
/// use isomage::image_io::{MmapImage, RandomAccess};
///
/// let img = MmapImage::open("disc.iso")?;
/// // Read 16 bytes at offset 32768 (ISO 9660 PVD location) without
/// // any allocation or syscall: the slice points straight into the
/// // mapped region.
/// let pvd_magic = img.read_at(32768 + 1, 5)?;
/// // "CD001" — the ISO 9660 Standard Identifier (ECMA-119 §8.4.2).
/// assert_eq!(pvd_magic, b"CD001");
/// # Ok::<(), Box<dyn std::error::Error + Send + Sync + 'static>>(())
/// ```
///
/// # v2 parser compatibility
///
/// `MmapImage` implements `Read + Seek`, so in principle it could be
/// plugged into [`crate::detect_and_parse_filesystem`]. As of v2.x
/// that entry point pins its argument to `&mut File`, so feeding it
/// an `MmapImage` requires the planned v3.0 signature
/// generalization. Until then, drive parsing via the `iso9660` and
/// `udf` submodules directly, or stick with `File` and accept the
/// extra syscalls.
pub struct MmapImage {
    // Held only so the mmap can't outlive the file descriptor.
    // The map itself does not technically need the File alive after
    // creation on Unix, but holding it makes the intent clearer and
    // avoids surprises on platforms where it might.
    _file: File,
    mmap: Mmap,
    cursor: u64,
}

impl MmapImage {
    /// Open `path` read-only and mmap the whole file. Issues
    /// `madvise(MADV_SEQUENTIAL)` so the kernel prefetches ahead of
    /// the read head and drops pages behind it.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY: We hold `file` alive for `self`'s lifetime. The
        // file is opened read-only above, so the only failure modes
        // we don't guard against are truncation by another process
        // (which is a system-administration concern outside Rust's
        // memory-safety contract — same caveat as every mmap user)
        // and signal-handler-triggered SIGBUS on remote-mount EIO,
        // which the std library does not handle either.
        let mmap = unsafe { Mmap::map(&file)? };
        let _ = mmap.advise(Advice::Sequential);
        Ok(Self {
            _file: file,
            mmap,
            cursor: 0,
        })
    }

    /// Total image size in bytes.
    pub fn len(&self) -> u64 {
        self.mmap.len() as u64
    }

    /// `true` iff the image is zero bytes (an empty file).
    pub fn is_empty(&self) -> bool {
        self.mmap.is_empty()
    }

    /// Borrow the whole image as a slice. Available because mmap is
    /// the rare backing store that can satisfy "give me everything"
    /// in O(1) with no allocation.
    pub fn as_bytes(&self) -> &[u8] {
        &self.mmap
    }
}

impl Read for MmapImage {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.mmap.len() as u64 - self.cursor.min(self.mmap.len() as u64);
        if remaining == 0 {
            return Ok(0);
        }
        let n = buf.len().min(remaining as usize);
        let off = self.cursor as usize;
        buf[..n].copy_from_slice(&self.mmap[off..off + n]);
        self.cursor += n as u64;
        Ok(n)
    }
}

impl Seek for MmapImage {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => self.cursor as i64 + n,
            SeekFrom::End(n) => self.mmap.len() as i64 + n,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.cursor = new_pos as u64;
        Ok(self.cursor)
    }
}

impl RandomAccess for MmapImage {
    fn len(&self) -> u64 {
        self.mmap.len() as u64
    }

    fn read_at(&self, offset: u64, len: usize) -> io::Result<&[u8]> {
        let off = offset as usize;
        let end = off
            .checked_add(len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "offset overflow"))?;
        self.mmap.get(off..end).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "read past image end")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn scratch_image(bytes: &[u8], tag: &str) -> PathBuf {
        // `cargo test` runs tests in parallel by default — including
        // per-test, distinct filenames keeps siblings from racing on
        // the same scratch file.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("isomage-mmap-test-{}-{}.bin", std::process::id(), tag));
        let mut f = File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();
        path
    }

    #[test]
    fn opens_and_reads() {
        let path = scratch_image(b"0123456789", "opens_and_reads");
        let mut img = MmapImage::open(&path).unwrap();
        assert_eq!(<MmapImage as RandomAccess>::len(&img), 10);

        // Read + Seek path
        let mut buf = [0u8; 4];
        img.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"0123");
        img.seek(SeekFrom::Start(6)).unwrap();
        img.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"6789");

        // RandomAccess path — zero-copy slice
        assert_eq!(img.read_at(2, 4).unwrap(), b"2345");
        assert_eq!(img.as_bytes().len(), 10);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seek_before_start_rejected() {
        let path = scratch_image(b"abcd", "seek_before_start");
        let mut img = MmapImage::open(&path).unwrap();
        let err = img.seek(SeekFrom::Start(0)).and_then(|_| img.seek(SeekFrom::Current(-1)));
        assert!(err.is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_at_past_end_returns_eof() {
        let path = scratch_image(b"abc", "read_at_past_end");
        let img = MmapImage::open(&path).unwrap();
        assert_eq!(
            img.read_at(0, 4).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        std::fs::remove_file(&path).ok();
    }
}
