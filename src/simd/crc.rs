//! CRC-16-CCITT (polynomial `0x1021`, initial value `0x0000`,
//! reflected input/output: no, final XOR: 0).
//!
//! This is the variant UDF descriptors use for the tag-CRC field
//! (ECMA-167 §7.2.3). It is _not_ the same as "CRC-16/CCITT-FALSE"
//! (initial value 0xFFFF) — be careful when cross-referencing tables.
//!
//! Implementation is a 256-entry lookup table that processes one
//! byte per iteration. See `super::mod.rs` for the SIMD upgrade path.

/// Precomputed table for CRC-16-CCITT, polynomial 0x1021, no reflection.
///
/// Entry `i` is the CRC of a single byte `i` followed by 14 zero bits,
/// which is the standard "byte-at-a-time" formulation:
/// `crc = (crc << 8) ^ TABLE[((crc >> 8) ^ byte) as usize & 0xFF]`.
const TABLE: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            if c & 0x8000 != 0 {
                c = (c << 1) ^ 0x1021;
            } else {
                c <<= 1;
            }
            j += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-16-CCITT (polynomial 0x1021, initial 0x0000) over `bytes`.
///
/// # Example
///
/// ```
/// # #[cfg(feature = "simd")] {
/// use isomage::simd::crc16_ccitt;
/// // Empty input is the initial value.
/// assert_eq!(crc16_ccitt(&[]), 0x0000);
/// // Single-byte known vector.
/// assert_eq!(crc16_ccitt(&[0x00]), 0x0000);
/// // "123456789" — the de facto CRC test vector. With initial value
/// // 0x0000 (not the more common 0xFFFF), the answer is 0x31C3.
/// assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
/// # }
/// ```
#[inline]
pub fn crc16_ccitt(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in bytes {
        let idx = ((crc >> 8) as u8 ^ b) as usize;
        crc = (crc << 8) ^ TABLE[idx];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-by-bit reference implementation; slow but obviously correct.
    /// Used to validate the table-driven version against arbitrary input.
    fn reference(bytes: &[u8]) -> u16 {
        let mut crc: u16 = 0;
        for &b in bytes {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                if crc & 0x8000 != 0 {
                    crc = (crc << 1) ^ 0x1021;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    }

    #[test]
    fn empty() {
        assert_eq!(crc16_ccitt(&[]), 0);
    }

    #[test]
    fn single_byte_zero() {
        assert_eq!(crc16_ccitt(&[0x00]), 0x0000);
    }

    #[test]
    fn standard_test_vector() {
        // "123456789" with init 0x0000, poly 0x1021. Note that init
        // 0xFFFF (the "CCITT-FALSE" variant) yields 0x29B1, which
        // is what most online calculators default to.
        assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
    }

    #[test]
    fn matches_reference_over_many_lengths() {
        // Pseudo-random sequence; deterministic so failures are reproducible.
        let mut bytes = Vec::with_capacity(4096);
        let mut state: u32 = 0x12345678;
        for _ in 0..4096 {
            // xorshift32
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            bytes.push(state as u8);
        }
        for len in [
            0usize, 1, 2, 3, 15, 16, 17, 63, 64, 65, 511, 512, 513, 4095, 4096,
        ] {
            assert_eq!(
                crc16_ccitt(&bytes[..len]),
                reference(&bytes[..len]),
                "len={len}",
            );
        }
    }
}
