//! Table-free CRC32C (Castagnoli) for WAL and BP block integrity.
//!
//! Uses the reflected polynomial `0x82F63B78` and a bitwise implementation:
//! no static lookup table, no dynamic allocation, constant stack usage. This
//! is slower than a 256-entry table but keeps flash/RAM footprint minimal on
//! small microcontrollers and is easy to audit formally.

/// Reflected Castagnoli polynomial (IEEE 802.3 / RFC 4960 CRC32C).
pub const CRC32C_POLY: u32 = 0x82F6_3B78;

/// Computes (or continues) a CRC32C over `data`.
///
/// Pass `0` for a fresh CRC. To continue a CRC across buffers, feed the
/// previously returned value as `seed`: the function internally restores the
/// pre-finalisation state, so `crc32c(b, crc32c(a, 0)) == crc32c(a||b, 0)`.
pub const fn crc32c(data: &[u8], seed: u32) -> u32 {
    let mut crc = seed ^ 0xFFFF_FFFF;
    let mut i = 0;
    while i < data.len() {
        crc ^= data[i] as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ CRC32C_POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        i += 1;
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        // RFC 4960 / RFC 7143 appendix: CRC32C("123456789") = e3069283.
        assert_eq!(crc32c(b"123456789", 0), 0xE306_9283);
        assert_eq!(crc32c(b"", 0), 0);
    }

    #[test]
    fn chunked_matches_single_pass() {
        let first = crc32c(b"1234", 0);
        assert_eq!(crc32c(b"56789", first), crc32c(b"123456789", 0));
    }

    #[test]
    fn single_bit_flip_changes_crc() {
        let original = crc32c(&[0xde, 0xad, 0xbe, 0xef], 0);
        let mut corrupted = [0xde, 0xad, 0xbe, 0xef];
        corrupted[2] ^= 0x01;
        assert_ne!(crc32c(&corrupted, 0), original);
    }
}
