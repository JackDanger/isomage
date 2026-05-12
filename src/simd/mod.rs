//! Fast checksum/CRC routines (`simd` feature).
//!
//! UDF descriptors carry a CRC-16-CCITT over the descriptor body
//! (ECMA-167 §7.2.3 tag-CRC field). The existing parser uses a
//! naive bit-by-bit implementation that's adequate for the tiny
//! descriptors but won't keep up when we extend coverage to
//! whole-payload checksums (e.g. SquashFS metadata blocks or
//! VHDX log entries).
//!
//! This module exposes a table-based [`crc16_ccitt`] that processes
//! one byte per iteration with a 512-byte lookup table. On the
//! 16–512-byte payloads UDF actually verifies it's a small win over
//! the bitwise version; on larger payloads it's ~10× faster.
//!
//! ## SIMD intrinsics — TODO
//!
//! The feature is named `simd` because a true PMULL/CLMUL CRC
//! implementation belongs here when a CRC-bound workload appears.
//! The current body is scalar slice-by-1 with `// TODO: PMULL` notes
//! marking the obvious next step. Real SIMD CRC has nontrivial
//! Barrett-reduction setup; that cost only amortizes above ~1 KiB.
//! UDF descriptors are smaller. Adding the intrinsics now would be
//! infrastructure without users.
//!
//! When the first CRC-bound workload lands (likely SquashFS), the
//! follow-on agent should:
//!
//! 1. Add `#[target_feature(enable = "pclmulqdq")]` x86_64 and
//!    `#[target_feature(enable = "aes")]` aarch64 specializations
//!    behind `is_x86_feature_detected!`/`is_aarch64_feature_detected!`.
//! 2. Keep the scalar path as the fallback.
//! 3. Cross-validate by checksumming a 1 MiB corpus with both
//!    implementations and asserting equality.

pub mod crc;

pub use crc::crc16_ccitt;
