//! SquashFS filesystem reader (`squashfs` feature).
//!
//! SquashFS is a compressed read-only filesystem commonly used in Linux
//! embedded systems, live CDs, and container base layers. This reader
//! supports **uncompressed inode and data tables only** (flags
//! `UNCOMPRESSED_INODES | UNCOMPRESSED_DATA`). Compressed images return
//! [`Error::Compressed`]; the caller can enable a codec feature later.
//!
//! Reference: Linux kernel `fs/squashfs/` and the squashfs-tools source
//! at <https://github.com/plougher/squashfs-tools>.
//!
//! ## Magic and endianness
//!
//! SquashFS images are either little-endian (magic `0x73717368`) or
//! big-endian (magic `0x68737173`). This reader only supports LE images;
//! BE images are detected and rejected with [`Error::BadMagic`].
//!
//! ## Metadata blocks
//!
//! Inodes and directory entries live in "metadata blocks": a 2-byte
//! header (always LE — bit 15 set means uncompressed, bits 14..0 are
//! the byte count) followed by the raw data. We refuse to decompress;
//! if bit 15 is clear we return [`Error::Compressed`].
//!
//! ## Depth limit
//!
//! The directory recursion is bounded at 64 levels to prevent
//! stack-exhaustion on malicious or corrupt images.
//!
//! ## Superblock layout (v4, 96 bytes, offsets from byte 0)
//!
//! ```text
//!  0: magic(u32)
//!  4: inode_count(u32)
//!  8: mtime(u32)
//! 12: block_size(u32)
//! 16: fragment_count(u32)
//! 20: compression_id(u16)
//! 22: block_log(u16)
//! 24: flags(u16)
//! 26: no_ids(u16)
//! 28: version_major(u16)
//! 30: version_minor(u16)
//! 32: root_inode(u64)
//! 40: bytes_used(u64)
//! 48: id_table_start(u64)
//! 56: xattr_id_table_start(u64)
//! 64: inode_table_start(u64)
//! 72: directory_table_start(u64)
//! 80: fragment_table_start(u64)
//! 88: lookup_table_start(u64)
//! ```

use std::io::{self, Read, Seek, SeekFrom};

use crate::tree::TreeNode;

const MAGIC_LE: u32 = 0x7371_7368;
const MAGIC_BE: u32 = 0x6873_7173;

const SUPERBLOCK_SIZE: u64 = 96;

const FLAG_UNCOMPRESSED_INODES: u16 = 0x0001;
const FLAG_UNCOMPRESSED_DATA: u16 = 0x0002;

// Used only in tests to build minimal valid images.
#[cfg(test)]
const FLAG_UNCOMPRESSED_FRAGS: u16 = 0x0008;
#[cfg(test)]
const FLAG_NO_FRAGMENTS: u16 = 0x0010;

const MAX_DEPTH: usize = 64;

// Inode type codes.
const INODE_DIR: u16 = 1;
const INODE_REG: u16 = 2;
const INODE_SYMLINK: u16 = 3;
const INODE_LDIR: u16 = 8;
const INODE_LREG: u16 = 9;
const INODE_LSYMLINK: u16 = 10;

/// Parse errors for the SquashFS reader.
#[derive(Debug)]
pub enum Error {
    /// Image shorter than the 96-byte superblock.
    TooShort,
    /// The magic bytes don't match LE or BE SquashFS.
    BadMagic,
    /// Version is not 4.0.
    BadVersion,
    /// A metadata block or data region requires decompression, which
    /// this build doesn't support. Enable a codec feature.
    Compressed,
    /// Underlying I/O error.
    Io(io::Error),
    /// Directory tree is nested deeper than `MAX_DEPTH` (64) levels.
    TooDeep,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::TooShort => write!(f, "image shorter than SquashFS superblock (96 bytes)"),
            Error::BadMagic => write!(f, "SquashFS magic bytes not found"),
            Error::BadVersion => write!(f, "SquashFS version is not 4.0"),
            Error::Compressed => {
                write!(
                    f,
                    "SquashFS uses compression; enable a codec feature to read it"
                )
            }
            Error::Io(e) => write!(f, "SquashFS I/O error: {e}"),
            Error::TooDeep => write!(
                f,
                "SquashFS directory tree exceeds {MAX_DEPTH}-level depth limit"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Parsed SquashFS superblock (96 bytes at offset 0).
struct Superblock {
    block_size: u32,
    flags: u16,
    /// Root inode reference: upper 48 bits = block index, lower 16 bits = byte offset.
    root_inode: u64,
    inode_table_start: u64,
    directory_table_start: u64,
}

impl Superblock {
    fn read<R: Read + Seek>(r: &mut R) -> Result<Self, Error> {
        r.seek(SeekFrom::Start(0))?;
        let mut buf = [0u8; SUPERBLOCK_SIZE as usize];
        r.read_exact(&mut buf).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                Error::TooShort
            } else {
                Error::Io(e)
            }
        })?;

        // Read bytes 0-3 as LE. LE squashfs images yield MAGIC_LE; BE squashfs
        // images (bytes reversed) yield MAGIC_BE. We only parse LE images.
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic == MAGIC_BE {
            // Big-endian squashfs — all subsequent LE field reads would be garbage.
            return Err(Error::BadMagic);
        }
        if magic != MAGIC_LE {
            return Err(Error::BadMagic);
        }

        let u16_at = |off: usize| -> u16 { u16::from_le_bytes([buf[off], buf[off + 1]]) };
        let u32_at = |off: usize| -> u32 {
            u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
        };
        let u64_at = |off: usize| -> u64 {
            u64::from_le_bytes([
                buf[off],
                buf[off + 1],
                buf[off + 2],
                buf[off + 3],
                buf[off + 4],
                buf[off + 5],
                buf[off + 6],
                buf[off + 7],
            ])
        };

        let block_size = u32_at(12);

        // block_size is a divisor in block_count_for; reject 0 to prevent panic.
        if block_size == 0 {
            return Err(Error::BadMagic);
        }

        let flags = u16_at(24);
        let version_major = u16_at(28);
        let version_minor = u16_at(30);
        let root_inode = u64_at(32);
        let inode_table_start = u64_at(64);
        let directory_table_start = u64_at(72);

        if version_major != 4 || version_minor != 0 {
            return Err(Error::BadVersion);
        }

        Ok(Superblock {
            block_size,
            flags,
            root_inode,
            inode_table_start,
            directory_table_start,
        })
    }

    fn is_inodes_uncompressed(&self) -> bool {
        self.flags & FLAG_UNCOMPRESSED_INODES != 0
    }

    fn is_data_uncompressed(&self) -> bool {
        self.flags & FLAG_UNCOMPRESSED_DATA != 0
    }
}

/// Read one metadata block from the current reader position.
/// Returns the uncompressed content bytes.
///
/// The 2-byte header is always LE: bit 15 = 1 means uncompressed,
/// bits 14..0 = byte count of the data that follows.
fn read_metadata_block<R: Read>(r: &mut R) -> Result<Vec<u8>, Error> {
    let mut hdr = [0u8; 2];
    r.read_exact(&mut hdr)?;
    let header = u16::from_le_bytes(hdr);
    if header & 0x8000 == 0 {
        return Err(Error::Compressed);
    }
    let size = (header & 0x7FFF) as usize;
    let mut data = vec![0u8; size];
    r.read_exact(&mut data)?;
    Ok(data)
}

/// Seek to `table_start`, skip `block_count` metadata blocks (each has a
/// 2-byte header), then read and return the next block's content.
fn seek_to_metadata_block<R: Read + Seek>(
    r: &mut R,
    table_start: u64,
    block_count: u64,
) -> Result<Vec<u8>, Error> {
    r.seek(SeekFrom::Start(table_start))?;
    for _ in 0..block_count {
        let mut hdr = [0u8; 2];
        r.read_exact(&mut hdr)?;
        let header = u16::from_le_bytes(hdr);
        if header & 0x8000 == 0 {
            return Err(Error::Compressed);
        }
        let size = (header & 0x7FFF) as usize;
        r.seek(SeekFrom::Current(size as i64))?;
    }
    read_metadata_block(r)
}

/// Read the metadata block at index `block_idx` from the inode table.
fn read_inode_block<R: Read + Seek>(
    r: &mut R,
    inode_table_start: u64,
    block_idx: u64,
) -> Result<Vec<u8>, Error> {
    seek_to_metadata_block(r, inode_table_start, block_idx)
}

/// One parsed inode, normalized across basic and extended variants.
struct Inode {
    inode_type: u16,
    /// For directories: (start_block in dir table, offset within block, total byte count).
    dir_info: Option<(u32, u16, u32)>,
    /// For regular files: (absolute byte offset in image, file_size, block_sizes, fragment_idx).
    reg_info: Option<(u64, u64, Vec<u32>, u32)>,
}

/// Parse the type-specific body of an inode (the bytes after the 16-byte common header).
fn parse_inode_body(body: &[u8], inode_type: u16, block_size: u32) -> Result<Inode, Error> {
    let too_short = |needed: usize| -> Result<(), Error> {
        if body.len() < needed {
            Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "inode body truncated",
            )))
        } else {
            Ok(())
        }
    };

    let u16le = |off: usize| u16::from_le_bytes([body[off], body[off + 1]]);
    let u32le =
        |off: usize| u32::from_le_bytes([body[off], body[off + 1], body[off + 2], body[off + 3]]);
    let u64le = |off: usize| {
        u64::from_le_bytes([
            body[off],
            body[off + 1],
            body[off + 2],
            body[off + 3],
            body[off + 4],
            body[off + 5],
            body[off + 6],
            body[off + 7],
        ])
    };

    match inode_type {
        INODE_DIR => {
            // start_block(u32) nlink(u32) file_size(u16) offset(u16) parent_inode(u32) = 16 bytes
            too_short(16)?;
            let start_block = u32le(0);
            let file_size = u16le(8) as u32;
            let offset = u16le(10);
            Ok(Inode {
                inode_type,
                dir_info: Some((start_block, offset, file_size)),
                reg_info: None,
            })
        }
        INODE_LDIR => {
            // nlink(u32) file_size(u32) start_block(u32) parent_inode(u32)
            // i_count(u16) offset(u16) xattr_idx(u32) = 24 bytes
            too_short(24)?;
            let file_size = u32le(4);
            let start_block = u32le(8);
            let offset = u16le(20);
            Ok(Inode {
                inode_type,
                dir_info: Some((start_block, offset, file_size)),
                reg_info: None,
            })
        }
        INODE_REG => {
            // start_block(u32) fragment(u32) offset(u32) file_size(u32)
            // block_sizes[ceil(file_size/block_size)](u32) = 16 + n*4 bytes
            too_short(16)?;
            let start_block = u32le(0) as u64;
            let fragment = u32le(4);
            let file_size = u32le(12) as u64;
            let nblocks = block_count_for(file_size, block_size, fragment);
            too_short(16 + nblocks * 4)?;
            let block_sizes: Vec<u32> = (0..nblocks).map(|i| u32le(16 + i * 4)).collect();
            Ok(Inode {
                inode_type,
                dir_info: None,
                reg_info: Some((start_block, file_size, block_sizes, fragment)),
            })
        }
        INODE_LREG => {
            // start_block(u64) file_size(u64) sparse(u64) nlink(u32) fragment(u32)
            // offset(u32) xattr_idx(u32) block_sizes[](u32) = 40 + n*4 bytes
            too_short(40)?;
            let start_block = u64le(0);
            let file_size = u64le(8);
            let fragment = u32le(28);
            let nblocks = block_count_for(file_size, block_size, fragment);
            too_short(40 + nblocks * 4)?;
            let block_sizes: Vec<u32> = (0..nblocks).map(|i| u32le(40 + i * 4)).collect();
            Ok(Inode {
                inode_type,
                dir_info: None,
                reg_info: Some((start_block, file_size, block_sizes, fragment)),
            })
        }
        // Symlinks and everything else (device, fifo, socket) — no data location.
        _ => Ok(Inode {
            inode_type,
            dir_info: None,
            reg_info: None,
        }),
    }
}

/// Number of data blocks for a file: if a fragment is used, the last
/// partial block is stored in the fragment table, so we count one fewer.
fn block_count_for(file_size: u64, block_size: u32, fragment: u32) -> usize {
    if fragment == 0xFFFF_FFFF {
        file_size.div_ceil(block_size as u64) as usize
    } else {
        (file_size / block_size as u64) as usize
    }
}

/// Compute `file_location` for a regular file.
/// Set only when: no fragment, exactly one block, and that block is uncompressed (bit 24 set).
fn file_location_for_reg(start_block: u64, block_sizes: &[u32], fragment: u32) -> Option<u64> {
    if fragment != 0xFFFF_FFFF {
        return None;
    }
    if block_sizes.len() != 1 {
        return None;
    }
    // bit 24 of block_sizes entry: 1 = uncompressed.
    if block_sizes[0] & 0x0100_0000 == 0 {
        return None;
    }
    Some(start_block)
}

/// Read and parse the inode at `(block_idx, offset)` from the inode table.
fn read_and_parse_inode<R: Read + Seek>(
    r: &mut R,
    inode_table_start: u64,
    block_idx: u64,
    offset: u16,
    block_size: u32,
) -> Result<Inode, Error> {
    let block_data = read_inode_block(r, inode_table_start, block_idx)?;
    let off = offset as usize;
    if block_data.len() < off + 16 {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "inode common header extends past block boundary",
        )));
    }
    let inode_type = u16::from_le_bytes([block_data[off], block_data[off + 1]]);
    let body = &block_data[off + 16..];
    parse_inode_body(body, inode_type, block_size)
}

/// Parse a directory table region and return a list of
/// `(entry_name, inode_block_idx, inode_offset_within_block)`.
fn parse_directory<R: Read + Seek>(
    r: &mut R,
    directory_table_start: u64,
    dir_start_block: u32,
    dir_offset: u16,
    dir_file_size: u32,
) -> Result<Vec<(String, u64, u16)>, Error> {
    let block_data = seek_to_metadata_block(r, directory_table_start, dir_start_block as u64)?;

    let off = dir_offset as usize;
    if block_data.len() < off {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "directory offset past metadata block end",
        )));
    }

    // dir_file_size includes a 3-byte trailing overhead that tools add; cap to available.
    let available = block_data.len() - off;
    let total = (dir_file_size as usize).min(available);
    let dir_bytes = &block_data[off..off + total];

    let mut entries: Vec<(String, u64, u16)> = Vec::new();
    let mut pos = 0usize;

    // Directory listing: sequence of (12-byte header + (count+1) 8-byte-plus-name entries).
    while pos + 12 <= dir_bytes.len() {
        let count = u32::from_le_bytes([
            dir_bytes[pos],
            dir_bytes[pos + 1],
            dir_bytes[pos + 2],
            dir_bytes[pos + 3],
        ]) as usize;
        let header_start_block = u32::from_le_bytes([
            dir_bytes[pos + 4],
            dir_bytes[pos + 5],
            dir_bytes[pos + 6],
            dir_bytes[pos + 7],
        ]);
        // base inode_number at [8..12] — used only for relative inode_offset; we navigate
        // by block_idx + entry_offset directly.
        pos += 12;

        for _ in 0..=count {
            // Entry: offset(u16) inode_offset(s16) type(u16) name_size(u16) name[name_size+1]
            // = 8-byte fixed header followed by name_size+1 name bytes.
            // Despite some docs showing u8, the actual on-disk format uses u16 for name_size
            // (confirmed from the squashfs_dir_entry struct in the kernel squashfs source).
            if pos + 8 > dir_bytes.len() {
                break;
            }
            let entry_inode_offset = u16::from_le_bytes([dir_bytes[pos], dir_bytes[pos + 1]]);
            // s16 inode_offset at [2..4] — used only for relative inode number; we
            // navigate by block_idx + entry_offset directly so we skip it.
            // u16 type at [4..6] — we re-read the inode header for the authoritative type.
            let name_size =
                u16::from_le_bytes([dir_bytes[pos + 6], dir_bytes[pos + 7]]) as usize + 1;
            pos += 8;
            if pos + name_size > dir_bytes.len() {
                break;
            }
            let name_bytes = &dir_bytes[pos..pos + name_size];
            pos += name_size;

            let name = String::from_utf8_lossy(name_bytes).into_owned();
            if name == "." || name == ".." {
                continue;
            }

            entries.push((name, header_start_block as u64, entry_inode_offset));
        }
    }

    Ok(entries)
}

/// Recursively build the directory tree rooted at the inode at `(block_idx, offset)`.
fn build_tree<R: Read + Seek>(
    r: &mut R,
    sb: &Superblock,
    name: String,
    block_idx: u64,
    offset: u16,
    depth: usize,
) -> Result<TreeNode, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::TooDeep);
    }

    let inode = read_and_parse_inode(r, sb.inode_table_start, block_idx, offset, sb.block_size)?;

    match inode.inode_type {
        INODE_DIR | INODE_LDIR => {
            let (dir_start_block, dir_offset, dir_file_size) =
                inode.dir_info.expect("dir_info always set for dir inodes");

            let mut node = TreeNode::new_directory(name);
            let child_refs = parse_directory(
                r,
                sb.directory_table_start,
                dir_start_block,
                dir_offset,
                dir_file_size,
            )?;

            for (child_name, child_block_idx, child_inode_offset) in child_refs {
                let child = build_tree(
                    r,
                    sb,
                    child_name,
                    child_block_idx,
                    child_inode_offset,
                    depth + 1,
                )?;
                node.add_child(child);
            }
            Ok(node)
        }
        INODE_REG | INODE_LREG => {
            let (start_block, file_size, block_sizes, fragment) =
                inode.reg_info.expect("reg_info always set for reg inodes");

            let location = file_location_for_reg(start_block, &block_sizes, fragment);
            if let Some(loc) = location {
                Ok(TreeNode::new_file_with_location(
                    name, file_size, loc, file_size,
                ))
            } else {
                let mut node = TreeNode::new_file(name, file_size);
                node.file_length = Some(file_size);
                Ok(node)
            }
        }
        INODE_SYMLINK | INODE_LSYMLINK | 4..=7 | 11..=14 => {
            // Symlinks, device nodes, FIFOs, sockets — zero-size, no location.
            Ok(TreeNode::new_file(name, 0))
        }
        _ => Ok(TreeNode::new_file(name, 0)),
    }
}

/// Detect whether `r` is a SquashFS image by checking the magic bytes.
/// Restores the stream position on both success and error paths.
///
/// Returns `Ok(())` if the magic matches.
pub fn detect<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    let pos = r.stream_position()?;
    let result = detect_inner(r);
    // Best-effort restore; if the seek itself fails we return the inner result.
    let _ = r.seek(SeekFrom::Start(pos));
    result
}

fn detect_inner<R: Read + Seek>(r: &mut R) -> Result<(), Error> {
    r.seek(SeekFrom::Start(0))?;
    let mut buf = [0u8; 4];
    match r.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::TooShort),
        Err(e) => return Err(Error::Io(e)),
    }
    let magic = u32::from_le_bytes(buf);
    if magic == MAGIC_LE {
        Ok(())
    } else {
        Err(Error::BadMagic)
    }
}

/// Parse the SquashFS filesystem at `r` and return a [`TreeNode`] tree.
///
/// Only images with both `UNCOMPRESSED_INODES` and `UNCOMPRESSED_DATA`
/// flags set are supported. Compressed images return [`Error::Compressed`].
pub fn detect_and_parse<R: Read + Seek>(r: &mut R) -> Result<TreeNode, Error> {
    let sb = Superblock::read(r)?;

    if !sb.is_inodes_uncompressed() || !sb.is_data_uncompressed() {
        return Err(Error::Compressed);
    }

    // The root inode reference encodes (block_idx << 16 | offset).
    let root_block_idx = sb.root_inode >> 16;
    let root_offset = (sb.root_inode & 0xFFFF) as u16;

    let mut root = build_tree(r, &sb, "/".to_string(), root_block_idx, root_offset, 0)?;
    root.calculate_directory_size();
    Ok(root)
}

// ---------------------------------------------------------------------------
// Unit tests using an in-memory SquashFS image builder
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // SquashFS v4 LE image builder.
    //
    // Layout:
    //   [0..96]          superblock
    //   [96..]           inode metadata block (uncompressed): root dir inode + file inode
    //   [inode_end..]    directory metadata block: one header + one entry
    //   [dir_end..]      file data bytes
    //
    // Root dir inode is at block_idx=0, offset=0 of the inode table.
    // File inode is at block_idx=0, offset=FILE_INODE_OFFSET of the inode table.

    const FILE_INODE_OFFSET: u16 = 32; // bytes into inode block where file inode starts

    fn build_image(file_name: &str, file_data: &[u8]) -> Vec<u8> {
        let block_size: u32 = 4096;
        let file_size = file_data.len() as u32;
        let name_bytes = file_name.as_bytes();

        // ---- Build inode block content ----
        // Root dir inode (type 1): common header (16 B) + dir body (16 B) = 32 B.
        // File inode (type 2): common header (16 B) + reg body (16 B + 4 B block entry) = 36 B.
        //
        // Dir body: start_block(u32) nlink(u32) file_size(u16) offset(u16) parent(u32)
        // Dir listing: 12 (dir header) + 8 (entry fixed header: 4×u16) + name_len (name bytes).
        let dir_listing_size: u16 = (12 + 8 + name_bytes.len()) as u16;

        let mut inode_block: Vec<u8> = Vec::new();

        // Root dir common header.
        inode_block.extend_from_slice(&INODE_DIR.to_le_bytes()); // inode_type
        inode_block.extend_from_slice(&0o755u16.to_le_bytes()); // mode
        inode_block.extend_from_slice(&0u16.to_le_bytes()); // uid_idx
        inode_block.extend_from_slice(&0u16.to_le_bytes()); // gid_idx
        inode_block.extend_from_slice(&0u32.to_le_bytes()); // mtime
        inode_block.extend_from_slice(&1u32.to_le_bytes()); // inode_number
                                                            // Root dir body: start_block=0 (first dir table block), nlink=2, file_size, offset=0, parent=1.
        inode_block.extend_from_slice(&0u32.to_le_bytes()); // dir start_block
        inode_block.extend_from_slice(&2u32.to_le_bytes()); // nlink
        inode_block.extend_from_slice(&dir_listing_size.to_le_bytes()); // file_size
        inode_block.extend_from_slice(&0u16.to_le_bytes()); // offset within dir block
        inode_block.extend_from_slice(&1u32.to_le_bytes()); // parent_inode
                                                            // = 32 bytes so far (FILE_INODE_OFFSET).
        assert_eq!(inode_block.len(), FILE_INODE_OFFSET as usize);

        // We need to know file_data_start to fill start_block in the file inode.
        // Compute layout: inode_table_start=96, inode_block total = 2 + inode_block_len.
        // File inode body will be 20 bytes (4 u32s + 1 block_sizes entry).
        let file_inode_body_len = 20usize;
        let inode_block_content_len = FILE_INODE_OFFSET as usize + 16 + file_inode_body_len;
        let inode_table_start: u64 = 96;
        let inode_meta_total: u64 = 2 + inode_block_content_len as u64;
        let dir_table_start: u64 = inode_table_start + inode_meta_total;
        let dir_block_content_len: usize = 12 + 8 + name_bytes.len();
        let dir_meta_total: u64 = 2 + dir_block_content_len as u64;
        let file_data_start: u64 = dir_table_start + dir_meta_total;

        // File inode common header.
        inode_block.extend_from_slice(&INODE_REG.to_le_bytes()); // inode_type
        inode_block.extend_from_slice(&0o644u16.to_le_bytes()); // mode
        inode_block.extend_from_slice(&0u16.to_le_bytes()); // uid_idx
        inode_block.extend_from_slice(&0u16.to_le_bytes()); // gid_idx
        inode_block.extend_from_slice(&0u32.to_le_bytes()); // mtime
        inode_block.extend_from_slice(&2u32.to_le_bytes()); // inode_number
                                                            // File inode body: start_block(u32) fragment(u32) offset(u32) file_size(u32) block_sizes[1](u32).
        inode_block.extend_from_slice(&(file_data_start as u32).to_le_bytes()); // start_block
        inode_block.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // fragment = none
        inode_block.extend_from_slice(&0u32.to_le_bytes()); // offset
        inode_block.extend_from_slice(&file_size.to_le_bytes()); // file_size
                                                                 // block_sizes[0]: bit 24 = uncompressed flag, low bits = size on disk.
        let block_entry = file_size | 0x0100_0000;
        inode_block.extend_from_slice(&block_entry.to_le_bytes()); // block_sizes[0]

        assert_eq!(inode_block.len(), inode_block_content_len);

        // ---- Build directory block content ----
        // Dir header: count(u32)=0 (means 1 entry), start_block(u32)=0 (inode block 0),
        //             base_inode(u32)=1.
        let mut dir_block: Vec<u8> = Vec::new();
        dir_block.extend_from_slice(&0u32.to_le_bytes()); // count (0 = 1 entry)
        dir_block.extend_from_slice(&0u32.to_le_bytes()); // inode block index
        dir_block.extend_from_slice(&1u32.to_le_bytes()); // base inode number
                                                          // Dir entry: offset(u16) inode_offset(s16) type(u16) name_size(u16) name[name_size+1].
                                                          // name_size stores name_length - 1 (so name_size+1 bytes follow).
        dir_block.extend_from_slice(&FILE_INODE_OFFSET.to_le_bytes()); // offset in inode block
        dir_block.extend_from_slice(&1i16.to_le_bytes()); // inode_offset (relative)
        dir_block.extend_from_slice(&INODE_REG.to_le_bytes()); // type
        dir_block.extend_from_slice(&((name_bytes.len() - 1) as u16).to_le_bytes()); // name_size
        dir_block.extend_from_slice(name_bytes); // name_size+1 bytes
        assert_eq!(dir_block.len(), dir_block_content_len);

        // ---- Build superblock ----
        let flags = FLAG_UNCOMPRESSED_INODES
            | FLAG_UNCOMPRESSED_DATA
            | FLAG_UNCOMPRESSED_FRAGS
            | FLAG_NO_FRAGMENTS;
        // Root inode reference: block_idx=0, offset=0 → packed = 0.
        let root_inode_ref: u64 = 0;

        let mut sb = vec![0u8; 96];
        sb[0..4].copy_from_slice(&MAGIC_LE.to_le_bytes()); // magic
        sb[4..8].copy_from_slice(&2u32.to_le_bytes()); // inode_count
        sb[8..12].copy_from_slice(&0u32.to_le_bytes()); // mtime
        sb[12..16].copy_from_slice(&block_size.to_le_bytes()); // block_size
        sb[16..20].copy_from_slice(&0u32.to_le_bytes()); // fragment_count
        sb[20..22].copy_from_slice(&1u16.to_le_bytes()); // compression_id (gzip nominal)
        sb[22..24].copy_from_slice(&12u16.to_le_bytes()); // block_log = log2(4096)
        sb[24..26].copy_from_slice(&flags.to_le_bytes()); // flags
        sb[26..28].copy_from_slice(&1u16.to_le_bytes()); // no_ids
        sb[28..30].copy_from_slice(&4u16.to_le_bytes()); // version_major
        sb[30..32].copy_from_slice(&0u16.to_le_bytes()); // version_minor
        sb[32..40].copy_from_slice(&root_inode_ref.to_le_bytes()); // root_inode
        sb[40..48].copy_from_slice(&0u64.to_le_bytes()); // bytes_used
        sb[48..56].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()); // id_table
        sb[56..64].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()); // xattr
        sb[64..72].copy_from_slice(&inode_table_start.to_le_bytes()); // inode_table_start
        sb[72..80].copy_from_slice(&dir_table_start.to_le_bytes()); // dir_table_start
        sb[80..88].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()); // fragment_table
        sb[88..96].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes()); // lookup_table

        // ---- Assemble final image ----
        let mut image: Vec<u8> = Vec::new();
        image.extend_from_slice(&sb);

        // Inode metadata block (uncompressed bit set in header).
        let inode_hdr = 0x8000u16 | (inode_block.len() as u16);
        image.extend_from_slice(&inode_hdr.to_le_bytes());
        image.extend_from_slice(&inode_block);

        // Directory metadata block.
        let dir_hdr = 0x8000u16 | (dir_block.len() as u16);
        image.extend_from_slice(&dir_hdr.to_le_bytes());
        image.extend_from_slice(&dir_block);

        // File data.
        image.extend_from_slice(file_data);

        image
    }

    fn parse_image(image: &[u8]) -> TreeNode {
        let mut c = Cursor::new(image);
        detect_and_parse(&mut c).expect("detect_and_parse failed")
    }

    #[test]
    fn detect_le_magic_ok() {
        let img = build_image("hello.txt", b"hello");
        let mut c = Cursor::new(&img);
        assert!(detect(&mut c).is_ok());
    }

    #[test]
    fn detect_restores_position() {
        let img = build_image("f.txt", b"data");
        let mut c = Cursor::new(&img);
        c.seek(SeekFrom::Start(10)).unwrap();
        detect(&mut c).unwrap();
        // Position must be restored to 10.
        assert_eq!(c.stream_position().unwrap(), 10);
    }

    #[test]
    fn detect_rejects_bad_magic() {
        let img = vec![0u8; 128];
        let mut c = Cursor::new(&img);
        assert!(matches!(detect(&mut c), Err(Error::BadMagic)));
    }

    #[test]
    fn detect_rejects_too_short() {
        let img = vec![0u8; 3];
        let mut c = Cursor::new(&img);
        assert!(matches!(detect(&mut c), Err(Error::TooShort)));
    }

    #[test]
    fn root_is_slash_directory() {
        let img = build_image("file.txt", b"content");
        let tree = parse_image(&img);
        assert_eq!(tree.name, "/");
        assert!(tree.is_directory);
    }

    #[test]
    fn single_file_child_name_and_type() {
        let img = build_image("readme.txt", b"hello world");
        let tree = parse_image(&img);
        assert_eq!(tree.children.len(), 1);
        let child = &tree.children[0];
        assert_eq!(child.name, "readme.txt");
        assert!(!child.is_directory);
    }

    #[test]
    fn file_size_matches() {
        let data = b"the quick brown fox";
        let img = build_image("fox.txt", data);
        let tree = parse_image(&img);
        let child = &tree.children[0];
        assert_eq!(child.size, data.len() as u64);
    }

    #[test]
    fn file_location_set_for_uncompressed_single_block() {
        let img = build_image("data.bin", b"some bytes");
        let tree = parse_image(&img);
        let child = &tree.children[0];
        assert!(
            child.file_location.is_some(),
            "uncompressed single-block file should have file_location set"
        );
    }

    #[test]
    fn directory_size_is_sum_of_children() {
        let data = b"twelve bytes";
        let img = build_image("f.txt", data);
        let tree = parse_image(&img);
        let total: u64 = tree.children.iter().map(|c| c.size).sum();
        assert_eq!(tree.size, total);
    }

    #[test]
    fn reject_compressed_inodes_flag() {
        let img = build_image("f.txt", b"x");
        let mut patched = img.clone();
        // flags is at offset 24 (LE u16); clear UNCOMPRESSED_INODES bit.
        let flags = u16::from_le_bytes([patched[24], patched[25]]);
        let new_flags = flags & !FLAG_UNCOMPRESSED_INODES;
        patched[24..26].copy_from_slice(&new_flags.to_le_bytes());
        let mut c = Cursor::new(&patched);
        assert!(matches!(detect_and_parse(&mut c), Err(Error::Compressed)));
    }

    #[test]
    fn reject_wrong_version() {
        let img = build_image("f.txt", b"x");
        let mut patched = img.clone();
        // version_major at offset 28; patch to 3.
        patched[28..30].copy_from_slice(&3u16.to_le_bytes());
        let mut c = Cursor::new(&patched);
        assert!(matches!(detect_and_parse(&mut c), Err(Error::BadVersion)));
    }

    // ── Error Display / source ────────────────────────────────────────────────

    #[test]
    fn error_display_too_short() {
        let msg = format!("{}", Error::TooShort);
        assert!(
            msg.contains("SquashFS") || msg.contains("short"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_bad_magic() {
        let msg = format!("{}", Error::BadMagic);
        assert!(
            msg.contains("magic") || msg.contains("SquashFS"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_bad_version() {
        let msg = format!("{}", Error::BadVersion);
        assert!(
            msg.contains("version") || msg.contains("4.0"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_compressed() {
        let msg = format!("{}", Error::Compressed);
        assert!(
            msg.contains("compress") || msg.contains("codec"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_display_io() {
        let io = io::Error::other("disk");
        let msg = format!("{}", Error::Io(io));
        assert!(msg.contains("disk"), "unexpected: {msg}");
    }

    #[test]
    fn error_display_too_deep() {
        let msg = format!("{}", Error::TooDeep);
        assert!(
            msg.contains("depth") || msg.contains("deep") || msg.contains("64"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn error_source_io() {
        use std::error::Error as StdError;
        let io = io::Error::other("src");
        assert!(Error::Io(io).source().is_some());
    }

    #[test]
    fn error_source_non_io() {
        use std::error::Error as StdError;
        assert!(Error::TooShort.source().is_none());
        assert!(Error::BadMagic.source().is_none());
        assert!(Error::TooDeep.source().is_none());
    }

    // ── parse_inode_body variants ─────────────────────────────────────────────

    #[test]
    fn parse_inode_body_ldir() {
        // INODE_LDIR body: nlink(u32) file_size(u32) start_block(u32) parent(u32)
        //                  i_count(u16) offset(u16) xattr(u32) = 24 bytes
        let mut body = vec![0u8; 24];
        body[4..8].copy_from_slice(&99u32.to_le_bytes()); // file_size
        body[8..12].copy_from_slice(&7u32.to_le_bytes()); // start_block
        body[20..22].copy_from_slice(&5u16.to_le_bytes()); // offset
        let inode = parse_inode_body(&body, INODE_LDIR, 4096).expect("LDIR parse failed");
        assert_eq!(inode.inode_type, INODE_LDIR);
        assert!(inode.dir_info.is_some());
        let (start, off, size) = inode.dir_info.unwrap();
        assert_eq!(start, 7);
        assert_eq!(off, 5);
        assert_eq!(size, 99);
    }

    #[test]
    fn parse_inode_body_lreg() {
        // INODE_LREG body (no blocks needed when fragment != 0xFFFF_FFFF):
        // start_block(u64) file_size(u64) sparse(u64) nlink(u32) fragment(u32)
        // offset(u32) xattr(u32) = 40 bytes
        let mut body = vec![0u8; 40];
        body[..8].copy_from_slice(&42u64.to_le_bytes()); // start_block
        body[8..16].copy_from_slice(&100u64.to_le_bytes()); // file_size
                                                            // fragment = 0 (uses fragment table) → nblocks = 100/4096 = 0 → no block_sizes
        body[28..32].copy_from_slice(&0u32.to_le_bytes()); // fragment = 0 (used, not sentinel)
        let inode = parse_inode_body(&body, INODE_LREG, 4096).expect("LREG parse failed");
        assert_eq!(inode.inode_type, INODE_LREG);
        assert!(inode.reg_info.is_some());
        let (start, size, _, frag) = inode.reg_info.unwrap();
        assert_eq!(start, 42);
        assert_eq!(size, 100);
        assert_eq!(frag, 0);
    }

    #[test]
    fn parse_inode_body_symlink() {
        // INODE_SYMLINK (type 3): symlink_size(u32) symlink(bytes) — but our
        // parser ignores symlinks and returns a no-info inode.
        let body = vec![0u8; 8]; // minimal buffer
        let inode = parse_inode_body(&body, INODE_SYMLINK, 4096).expect("symlink parse failed");
        assert_eq!(inode.inode_type, INODE_SYMLINK);
        assert!(inode.dir_info.is_none());
        assert!(inode.reg_info.is_none());
    }

    #[test]
    fn parse_inode_body_lsymlink() {
        let body = vec![0u8; 8];
        let inode = parse_inode_body(&body, INODE_LSYMLINK, 4096).expect("lsymlink parse failed");
        assert!(inode.dir_info.is_none());
        assert!(inode.reg_info.is_none());
    }

    #[test]
    fn parse_inode_body_unknown_type() {
        let body = vec![0u8; 4];
        let inode = parse_inode_body(&body, 99, 4096).expect("unknown type should not error");
        assert!(inode.dir_info.is_none());
        assert!(inode.reg_info.is_none());
    }

    // ── block_count_for ───────────────────────────────────────────────────────

    #[test]
    fn block_count_for_with_fragment() {
        // When fragment != 0xFFFF_FFFF, the last partial block is in the fragment
        // table → nblocks = file_size / block_size (not ceil).
        let bs = 4096u32;
        // 8000 bytes: 8000/4096 = 1 (floor); ceil would be 2.
        assert_eq!(block_count_for(8000, bs, 0), 1);
        // Exactly one full block: 4096/4096 = 1.
        assert_eq!(block_count_for(4096, bs, 0), 1);
        // Zero: 0/4096 = 0.
        assert_eq!(block_count_for(0, bs, 0), 0);
    }

    #[test]
    fn block_count_for_no_fragment() {
        let bs = 4096u32;
        // Without fragment (0xFFFF_FFFF), use ceil.
        assert_eq!(block_count_for(8000, bs, 0xFFFF_FFFF), 2);
        assert_eq!(block_count_for(4096, bs, 0xFFFF_FFFF), 1);
        assert_eq!(block_count_for(1, bs, 0xFFFF_FFFF), 1);
    }

    // ── file_location_for_reg ────────────────────────────────────────────────

    #[test]
    fn file_location_for_reg_with_fragment_returns_none() {
        // Fragment used → no file_location.
        assert!(file_location_for_reg(100, &[0x0100_0000], 0).is_none());
    }

    #[test]
    fn file_location_for_reg_multiple_blocks_returns_none() {
        // Two blocks → no single contiguous location.
        assert!(file_location_for_reg(100, &[0x0100_0010, 0x0100_0010], 0xFFFF_FFFF).is_none());
    }

    #[test]
    fn file_location_for_reg_compressed_block_returns_none() {
        // Bit 24 of block_sizes not set → compressed → no file_location.
        assert!(file_location_for_reg(100, &[0x0000_0010], 0xFFFF_FFFF).is_none());
    }

    #[test]
    fn file_location_for_reg_success() {
        // Single uncompressed block with no fragment → returns start_block.
        let loc = file_location_for_reg(200, &[0x0100_0020], 0xFFFF_FFFF);
        assert_eq!(loc, Some(200));
    }

    // ── Big-endian magic bytes ────────────────────────────────────────────────

    #[test]
    fn be_magic_constant_is_byte_swap_of_le() {
        // Verify the constants are consistent: MAGIC_BE is the byte-swap of MAGIC_LE.
        assert_eq!(MAGIC_LE.swap_bytes(), MAGIC_BE);
    }

    // ── From<io::Error> ───────────────────────────────────────────────────────

    #[test]
    fn error_from_io_error() {
        let io_err = io::Error::other("disk error");
        let err: Error = Error::from(io_err);
        assert!(matches!(err, Error::Io(_)));
    }

    // ── Superblock::read edge cases ───────────────────────────────────────────

    #[test]
    fn detect_and_parse_too_short_returns_too_short() {
        let mut img = vec![0u8; 10];
        img[0..4].copy_from_slice(&MAGIC_LE.to_le_bytes());
        let mut c = Cursor::new(&img);
        assert!(matches!(detect_and_parse(&mut c), Err(Error::TooShort)));
    }

    #[test]
    fn detect_and_parse_bad_magic_returns_bad_magic() {
        let img = vec![0u8; 96]; // all zeros → no valid magic
        let mut c = Cursor::new(&img);
        assert!(matches!(detect_and_parse(&mut c), Err(Error::BadMagic)));
    }

    #[test]
    fn detect_and_parse_block_size_zero_returns_bad_magic() {
        let mut img = vec![0u8; 96];
        img[0..4].copy_from_slice(&MAGIC_LE.to_le_bytes());
        img[28..30].copy_from_slice(&4u16.to_le_bytes()); // version_major = 4
                                                          // block_size at [12..16] stays 0 → BadMagic
        let mut c = Cursor::new(&img);
        assert!(matches!(detect_and_parse(&mut c), Err(Error::BadMagic)));
    }

    // ── read_metadata_block compressed rejection ───────────────────────────────

    #[test]
    fn read_metadata_block_compressed_returns_error() {
        // Header 0x0005 has bit 15 clear → compressed
        let data = vec![0x05u8, 0x00];
        let mut c = Cursor::new(&data);
        assert!(matches!(
            read_metadata_block(&mut c),
            Err(Error::Compressed)
        ));
    }

    #[test]
    fn seek_to_metadata_block_compressed_in_skip_returns_error() {
        // Block at offset 0 has compressed header (bit 15 = 0) → error while skipping
        let data = vec![0x05u8, 0x00, 0xFF, 0xFF];
        let mut c = Cursor::new(&data);
        // block_count=1 means skip 1 block before reading; first block is compressed
        assert!(matches!(
            seek_to_metadata_block(&mut c, 0, 1),
            Err(Error::Compressed)
        ));
    }

    // ── read_and_parse_inode: block too short ─────────────────────────────────

    #[test]
    fn read_and_parse_inode_offset_past_block_returns_error() {
        // Metadata block with 8 bytes of content. offset=0 → need 16 bytes → error.
        let mut data = vec![0x08u8, 0x80]; // header: uncompressed, size=8
        data.extend_from_slice(&[0u8; 8]); // 8-byte content (< 16 needed for common header)
        let mut c = Cursor::new(&data);
        let result = read_and_parse_inode(&mut c, 0, 0, 0, 4096);
        assert!(matches!(result, Err(Error::Io(_))));
    }

    // ── parse_directory edge cases ────────────────────────────────────────────

    #[test]
    fn parse_directory_offset_past_block_returns_error() {
        // Block with 4 bytes of content; dir_offset=10 > 4 → UnexpectedEof.
        let mut data = vec![0x04u8, 0x80]; // header: uncompressed, size=4
        data.extend_from_slice(&[0u8; 4]);
        let mut c = Cursor::new(&data);
        let result = parse_directory(&mut c, 0, 0, 10, 100);
        assert!(matches!(result, Err(Error::Io(_))));
    }

    #[test]
    fn parse_directory_entry_header_truncated_breaks_loop() {
        // Dir block: 12-byte dir-header (count=0 → 1 entry) + 4 bytes of partial entry (< 8).
        // pos=12 after dir header; pos+8=20 > dir_bytes.len()=16 → break.
        let mut dir_block = vec![0u8; 12]; // count=0, start_block=0, base_inode=0
        dir_block.extend_from_slice(&[0u8; 4]); // partial entry (4 bytes, need 8)
        let sz = dir_block.len(); // 16
        let hdr = (sz | 0x8000) as u16;
        let mut data = hdr.to_le_bytes().to_vec();
        data.extend_from_slice(&dir_block);
        let mut c = Cursor::new(&data);
        let result = parse_directory(&mut c, 0, 0, 0, 100).unwrap();
        assert!(
            result.is_empty(),
            "truncated entry header should produce no entries"
        );
    }

    #[test]
    fn parse_directory_name_overflow_breaks_loop() {
        // Dir block: 12-byte dir-header + 8-byte entry with name_size=200.
        // After reading 8-byte header, pos=20; name needs 201 bytes but dir_bytes.len()=20 → break.
        let mut dir_block = vec![0u8; 12]; // dir header
        dir_block.extend_from_slice(&0u16.to_le_bytes()); // offset
        dir_block.extend_from_slice(&0i16.to_le_bytes()); // inode_offset
        dir_block.extend_from_slice(&2u16.to_le_bytes()); // type
        dir_block.extend_from_slice(&200u16.to_le_bytes()); // name_size=200 → need 201 bytes
        let sz = dir_block.len(); // 20
        let hdr = (sz | 0x8000) as u16;
        let mut data = hdr.to_le_bytes().to_vec();
        data.extend_from_slice(&dir_block);
        let mut c = Cursor::new(&data);
        let result = parse_directory(&mut c, 0, 0, 0, 100).unwrap();
        assert!(
            result.is_empty(),
            "overflowing name should produce no entries"
        );
    }

    #[test]
    fn parse_directory_dot_dotdot_entries_skipped() {
        // Build a directory block containing "." and ".." entries which should be skipped.
        let make_entry = |name: &[u8]| -> Vec<u8> {
            let mut e = Vec::new();
            e.extend_from_slice(&0u16.to_le_bytes()); // offset
            e.extend_from_slice(&0i16.to_le_bytes()); // inode_offset
            e.extend_from_slice(&1u16.to_le_bytes()); // type
            e.extend_from_slice(&((name.len() - 1) as u16).to_le_bytes()); // name_size
            e.extend_from_slice(name);
            e
        };
        let dot = make_entry(b".");
        let dotdot = make_entry(b"..");
        let count = 1u32; // count=1 → 2 entries (0..=1)
        let mut dir_block = Vec::new();
        dir_block.extend_from_slice(&count.to_le_bytes());
        dir_block.extend_from_slice(&0u32.to_le_bytes()); // start_block
        dir_block.extend_from_slice(&0u32.to_le_bytes()); // base_inode
        dir_block.extend_from_slice(&dot);
        dir_block.extend_from_slice(&dotdot);
        let sz = dir_block.len();
        let hdr = (sz | 0x8000) as u16;
        let mut data = hdr.to_le_bytes().to_vec();
        data.extend_from_slice(&dir_block);
        let mut c = Cursor::new(&data);
        let result = parse_directory(&mut c, 0, 0, 0, 1000).unwrap();
        assert!(result.is_empty(), "dot and dotdot entries must be skipped");
    }

    // ── build_tree: depth limit ───────────────────────────────────────────────

    #[test]
    fn build_tree_too_deep_returns_error() {
        let img = build_image("f.txt", b"x");
        let mut c = Cursor::new(&img);
        let sb = Superblock::read(&mut c).unwrap();
        let result = build_tree(&mut c, &sb, "x".to_string(), 0, 0, MAX_DEPTH + 1);
        assert!(matches!(result, Err(Error::TooDeep)));
    }

    // ── build_tree: REG/LREG no-location path ────────────────────────────────

    #[test]
    fn file_with_fragment_has_no_file_location() {
        // Patch the file inode's fragment field from 0xFFFFFFFF to 0 → location = None.
        // File inode common header starts at byte 130 in build_image layout:
        //   superblock(96) + block_hdr(2) + root_dir_inode(32 bytes = FILE_INODE_OFFSET)
        //   → 96 + 2 + 32 = 130. Body starts at 130 + 16 = 146. Fragment at body+4 = 150.
        let mut img = build_image("file.txt", b"hello");
        img[150..154].copy_from_slice(&0u32.to_le_bytes()); // fragment = 0 (not sentinel)
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).unwrap();
        let child = &tree.children[0];
        assert!(
            child.file_location.is_none(),
            "file with fragment must have no location"
        );
        assert_eq!(child.file_length, Some(5));
    }

    // ── build_tree: symlink and unknown inode types ───────────────────────────

    #[test]
    fn symlink_inode_returns_zero_size_file() {
        // Patch file inode type from INODE_REG(2) to INODE_SYMLINK(3).
        // File inode common header at byte 130 in build_image layout.
        let mut img = build_image("link", b"target");
        img[130..132].copy_from_slice(&INODE_SYMLINK.to_le_bytes());
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).unwrap();
        let child = &tree.children[0];
        assert_eq!(child.name, "link");
        assert_eq!(child.size, 0);
    }

    #[test]
    fn unknown_inode_type_returns_zero_size_file() {
        // Patch file inode type to 20 → falls into catch-all → zero-size file.
        let mut img = build_image("node", b"x");
        img[130..132].copy_from_slice(&20u16.to_le_bytes());
        let mut c = Cursor::new(&img);
        let tree = detect_and_parse(&mut c).unwrap();
        let child = &tree.children[0];
        assert_eq!(child.size, 0);
    }

    // ── seek_to_metadata_block: uncompressed-block skip covers lines 285-287 ──

    #[test]
    fn seek_to_metadata_block_uncompressed_skip() {
        // block_idx=1 → skip one block before reading.
        // First block header: uncompressed (bit 15 set), size=4 → skip 4 bytes.
        // Then the second block header + content = the actual target block.
        let mut data: Vec<u8> = Vec::new();
        // First (skip) block: header = size=4 | 0x8000 = 0x8004.
        data.extend_from_slice(&0x8004u16.to_le_bytes()); // uncompressed, size=4
        data.extend_from_slice(&[0xAAu8; 4]); // 4 bytes of skipped content
                                              // Target block: uncompressed, size=8, content = 8 zeros.
        data.extend_from_slice(&0x8008u16.to_le_bytes()); // uncompressed, size=8
        data.extend_from_slice(&[0xBBu8; 8]);
        let mut c = Cursor::new(&data);
        // seek_to_metadata_block(r, table_start=0, block_idx=1) should skip the
        // first block and return the content of the second block.
        let result = seek_to_metadata_block(&mut c, 0, 1).unwrap();
        assert_eq!(result, vec![0xBBu8; 8]);
    }

    // ── parse_inode_body: too_short path covers lines 313-317 ────────────────

    #[test]
    fn parse_inode_body_dir_too_short_returns_error() {
        // INODE_DIR requires 16 bytes; provide only 10 → too_short(16) fires (lines 314-317).
        let body = vec![0u8; 10];
        let result = parse_inode_body(&body, INODE_DIR, 4096);
        assert!(matches!(result, Err(Error::Io(_))));
    }

    // ── Big-endian images rejected ────────────────────────────────────────────

    #[test]
    fn superblock_read_big_endian_image_returns_bad_magic() {
        // BE squashfs on-disk bytes are [0x73, 0x71, 0x73, 0x68] = MAGIC_BE.to_le_bytes().
        // from_le_bytes of those bytes == MAGIC_BE → rejected before any field reads.
        let mut data = vec![0u8; 200];
        data[0..4].copy_from_slice(&MAGIC_BE.to_le_bytes());
        let mut c = Cursor::new(data);
        assert!(matches!(Superblock::read(&mut c), Err(Error::BadMagic)));
    }

    #[test]
    fn detect_inner_le_magic_accepted() {
        // LE squashfs on-disk bytes are [0x68, 0x73, 0x71, 0x73] = MAGIC_LE.to_le_bytes().
        let mut data = vec![0u8; 4];
        data[0..4].copy_from_slice(&MAGIC_LE.to_le_bytes());
        let mut c = Cursor::new(data);
        assert!(matches!(detect_inner(&mut c), Ok(())));
    }

    #[test]
    fn detect_inner_big_endian_image_rejected() {
        // BE squashfs bytes yield from_le_bytes == MAGIC_BE ≠ MAGIC_LE → BadMagic.
        let mut data = vec![0u8; 4];
        data[0..4].copy_from_slice(&MAGIC_BE.to_le_bytes());
        let mut c = Cursor::new(data);
        assert!(matches!(detect_inner(&mut c), Err(Error::BadMagic)));
    }
}
