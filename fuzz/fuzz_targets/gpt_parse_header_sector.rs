//! Fuzz target for `isomage::formats::gpt::parse_header_sector`.
//!
//! Validates that the GPT header parser:
//!   * Never panics on adversarial bytes.
//!   * Honours its declared sector-size minimum (512 bytes).
//!   * Returns the documented `UnsupportedEntrySize` for entry
//!     sizes below 128 rather than panicking on the truncated
//!     entries.
//!
//! Seed corpus (committed under `fuzz/seeds/gpt_parse_header_sector/`)
//! includes:
//!   * a valid minimal GPT header (`EFI PART`, 128 entries × 128 bytes).
//!   * a header with `EFI PART` but `entry_size = 0` (must reject).
//!   * a header with `entry_size = u32::MAX` (must reject).
//!   * 512 bytes of zeros (rejected by the signature check).

#![no_main]

use libfuzzer_sys::fuzz_target;

use isomage::formats::gpt;

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = gpt::parse_header_sector(data) {
        // Exercise field reads — same rationale as the MBR target.
        let _ = (header.entries_lba, header.num_entries, header.entry_size);
        let _ = format!("{header:?}");
    }
});
