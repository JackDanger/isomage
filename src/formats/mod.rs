//! Per-format submodules added in v3.0.
//!
//! Each module here owns the parser (and, in v3.1+, the writer) for
//! one disk-image or filesystem format. They share three conventions:
//!
//! 1. **Detection by magic bytes only.** Every public `detect_*`
//!    function reads a small header, returns quickly with a clear
//!    error if the magic doesn't match, and leaves the file cursor
//!    in a defined state.
//! 2. **`TreeNode` is the output type.** A successful parse yields
//!    the same shape the v2 `iso9660`/`udf` parsers produce — root
//!    directory at `"/"`, partitions or files as children, byte
//!    ranges populated for `cat_node` / `extract_node`.
//! 3. **Feature-gated.** Each module sits behind a Cargo feature so
//!    consumers who only want ISO/UDF aren't paying for everything.
//!    See `Cargo.toml` for the matrix.
//!
//! The Phase 3 starter set is below. Subsequent formats (`vhd`,
//! `vmdk`, `qcow2`, `fat`, `ntfs`, etc.) follow the same pattern.

#[cfg(feature = "mbr")]
pub mod mbr;

#[cfg(feature = "gpt")]
pub mod gpt;

#[cfg(feature = "raw")]
pub mod raw;

#[cfg(feature = "ext")]
pub mod ext;

#[cfg(feature = "squashfs")]
pub mod squashfs;

#[cfg(feature = "vhd")]
pub mod vhd;

#[cfg(feature = "vmdk")]
pub mod vmdk;
