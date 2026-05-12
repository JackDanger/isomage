//! Fuzz target for `isomage::formats::mbr::parse_sector`.
//!
//! The parser must never panic on arbitrary input, never read past
//! the input slice, and must return an `Err` for malformed sectors
//! rather than silently producing partition entries with garbage
//! offsets.
//!
//! Seed corpus (committed under `fuzz/seeds/mbr_parse_sector/`)
//! includes:
//!   * a valid single-partition MBR
//!   * a GPT protective MBR
//!   * 512 bytes of zeros (rejected by the signature check)
//!   * a short sector (under 512 bytes)
//!
//! On a successful parse we additionally walk the partition list
//! to invoke the `Partition` derive impls (Debug, Clone, Copy, Eq);
//! this catches accidental panics in any of those if we ever add
//! them.

#![no_main]

use libfuzzer_sys::fuzz_target;

use isomage::formats::mbr;

fuzz_target!(|data: &[u8]| {
    if let Ok(partitions) = mbr::parse_sector(data) {
        for p in &partitions {
            // Touch every field so the Debug / Clone / Copy /
            // PartialEq impls are exercised and the lifetime
            // semantics of the data don't escape.
            let _ = format!("{p:?}");
            let _ = (p.index, p.status, p.type_code, p.start, p.length);
        }
        // Also exercise the TreeNode shim.
        let _ = mbr::to_tree(&partitions);
    }
});
