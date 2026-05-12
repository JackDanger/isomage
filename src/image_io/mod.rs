//! Image-reader abstractions.
//!
//! v2.x parsers (`iso9660`, `udf`) read from anything implementing
//! `Read + Seek` and copy sector bytes into owned buffers. That's
//! fine for correctness and lets us read straight off a `File`, but
//! it leaves performance on the table for the case where the whole
//! image is mmap-able: the kernel already has the bytes in the page
//! cache, and we can hand a `&[u8]` slice straight to the writer
//! without an intermediate copy.
//!
//! [`RandomAccess`] is the additive trait that lets Phase 3 format
//! modules (raw, MBR, GPT, VHD, …) take advantage of zero-copy
//! reads when they're available. The existing parsers still work
//! over `Read + Seek`; nothing breaks. When a caller hands them a
//! [`MmapImage`] (`--features mmap`), the parser path is unchanged
//! but the underlying syscalls disappear into the page cache.
//!
//! The eventual `TreeNode.name → Cow<'a, str>` refactor (planned
//! for v3.0) will let parsers borrow names directly out of the
//! mmap slice. That's a separate, breaking change tracked in the
//! v3-scope-expansion prompt log.

use std::io;

#[cfg(feature = "mmap")]
pub mod mmap;

#[cfg(feature = "mmap")]
pub use mmap::MmapImage;

/// Read arbitrary byte ranges out of a disc image without copying.
///
/// Where `Read + Seek` returns bytes by filling a caller-provided
/// buffer (one syscall per read, copy through the kernel boundary),
/// `RandomAccess` returns a borrowed slice. Implementations that
/// back onto an mmap can satisfy reads with zero memcpy; implementations
/// that back onto a `File` will fill a scratch buffer internally.
///
/// All offsets and lengths are in bytes, not sectors. Implementations
/// must handle requests that span their internal block boundaries
/// transparently.
///
/// # Errors
///
/// Implementations return `io::ErrorKind::UnexpectedEof` if the
/// requested range extends past the image end, and propagate any
/// underlying I/O error otherwise. They do *not* return `Ok` with
/// a short slice — callers can rely on `slice.len() == len`.
pub trait RandomAccess {
    /// Total image size in bytes.
    fn len(&self) -> u64;

    /// `true` iff [`len`](Self::len) is `0`. The default impl is the
    /// idiomatic check; implementors don't typically override it.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow `len` bytes starting at `offset`. The returned slice is
    /// live for the lifetime of `&self`, which means the underlying
    /// buffer (mmap or scratch) must outlive every borrow.
    ///
    /// Implementations that need to read into a scratch buffer (e.g.
    /// `FileImage`) take `&mut self` via the [`RandomAccessMut`]
    /// extension trait. This trait is reserved for implementations
    /// where the bytes are already resident.
    fn read_at(&self, offset: u64, len: usize) -> io::Result<&[u8]>;
}

/// Mutable extension of [`RandomAccess`] for implementations that
/// can't satisfy reads without scratch space (e.g. an over-File
/// reader that fills a per-call buffer).
///
/// Splitting this from [`RandomAccess`] keeps mmap-backed images
/// usable behind a `&` reference, which matters when multiple
/// parsers walk the same image concurrently.
pub trait RandomAccessMut {
    /// Total image size in bytes.
    fn len(&self) -> u64;

    /// `true` iff the image is zero bytes long.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read `len` bytes at `offset` into the implementation's internal
    /// scratch buffer and return a borrowed slice into it. The slice
    /// is invalidated by the next call to `read_at_mut`.
    fn read_at_mut(&mut self, offset: u64, len: usize) -> io::Result<&[u8]>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // A trivial in-memory RandomAccess for testing.
    struct InMemory<'a>(&'a [u8]);

    impl<'a> RandomAccess for InMemory<'a> {
        fn len(&self) -> u64 {
            self.0.len() as u64
        }
        fn read_at(&self, offset: u64, len: usize) -> io::Result<&[u8]> {
            let off = offset as usize;
            let end = off
                .checked_add(len)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "offset overflow"))?;
            self.0.get(off..end).ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "read past image end")
            })
        }
    }

    #[test]
    fn in_memory_round_trip() {
        let buf = b"abcdefgh";
        let img = InMemory(buf);
        assert_eq!(img.len(), 8);
        assert!(!img.is_empty());
        assert_eq!(img.read_at(0, 4).unwrap(), b"abcd");
        assert_eq!(img.read_at(4, 4).unwrap(), b"efgh");
    }

    #[test]
    fn in_memory_eof() {
        let img = InMemory(b"abc");
        assert_eq!(
            img.read_at(0, 4).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn in_memory_offset_overflow() {
        let img = InMemory(b"abc");
        let err = img.read_at(u64::MAX, 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
