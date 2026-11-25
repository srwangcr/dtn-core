//! Canonical CBOR helpers for `no_std`, zero-copy BPv7 parsing.
//!
//! Every helper operates on a borrowed `&[u8]` and returns either a decoded
//! scalar plus the number of consumed bytes, or a borrowed sub-slice. No heap,
//! no `alloc`, no panics: truncated or malformed encodings are reported as
//! [`crate::error::DtnError`] values. All additions that combine a length with an offset are
//! checked, which is essential on 16/32-bit microcontrollers where a `u64`
//! length from the wire may not fit in `usize`.

use crate::error::{DtnError, DtnResult};

/// Maximum nesting depth accepted by [`skip_item`].
///
/// BPv7 primary blocks contain at most three nested CBOR arrays (EID, SSP,
/// `ipn` pair). A small bound keeps recursive skipping constant-space even
/// when a hostile bundle is supplied.
pub const MAX_SKIP_DEPTH: u8 = 8;

/// CBOR major type of an initial byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CborType {
    /// Unsigned integer (major type 0).
    UnsignedInteger,
    /// Negative integer (major type 1).
    NegativeInteger,
    /// Byte string (major type 2).
    ByteString,
    /// Text string (major type 3).
    TextString,
    /// Array (major type 4).
    Array,
    /// Map (major type 5).
    Map,
    /// Semantic tag (major type 6).
    Tag,
    /// Simple value or floating point number (major type 7).
    SimpleOrFloat,
}

/// Extracts the major type of a CBOR initial byte.
pub const fn major_type(byte: u8) -> CborType {
    match byte >> 5 {
        0 => CborType::UnsignedInteger,
        1 => CborType::NegativeInteger,
        2 => CborType::ByteString,
        3 => CborType::TextString,
        4 => CborType::Array,
        5 => CborType::Map,
        6 => CborType::Tag,
        _ => CborType::SimpleOrFloat,
    }
}

/// Decodes a CBOR unsigned integer (major type 0).
///
/// Returns the value and its encoded width (1, 2, 3, 5 or 9 bytes). Additional
/// information values 28..=31 are rejected: 28..=30 are unassigned and 31
/// would denote an indefinite-length integer, which CBOR does not define.
pub fn decode_unsigned(buf: &[u8]) -> DtnResult<(u64, usize)> {
    let ib = *buf.first().ok_or(DtnError::BufferEmpty)?;
    if major_type(ib) != CborType::UnsignedInteger {
        return Err(DtnError::InvalidMajorType);
    }
    match ib & 0x1f {
        v @ 0..=23 => Ok((v as u64, 1)),
        24 => Ok((read_u8(buf, 1)? as u64, 2)),
        25 => Ok((u16::from_be_bytes(read_array::<2>(buf, 1)?) as u64, 3)),
        26 => Ok((u32::from_be_bytes(read_array::<4>(buf, 1)?) as u64, 5)),
        27 => Ok((u64::from_be_bytes(read_array::<8>(buf, 1)?), 9)),
        _ => Err(DtnError::InvalidAdditionalInfo),
    }
}

/// Decodes a definite-length header for a byte or text string (major type 2/3).
///
/// The returned `usize` length is range-checked on 32-bit targets; a wire
/// value of `u64::MAX` can never silently wrap into a small allocation.
pub fn decode_definite_length(buf: &[u8]) -> DtnResult<(usize, usize)> {
    let ib = *buf.first().ok_or(DtnError::BufferEmpty)?;
    if !matches!(major_type(ib), CborType::ByteString | CborType::TextString) {
        return Err(DtnError::InvalidMajorType);
    }
    let raw = match ib & 0x1f {
        v @ 0..=23 => v as u64,
        24 => read_u8(buf, 1)? as u64,
        25 => u16::from_be_bytes(read_array::<2>(buf, 1)?) as u64,
        26 => u32::from_be_bytes(read_array::<4>(buf, 1)?) as u64,
        27 => u64::from_be_bytes(read_array::<8>(buf, 1)?),
        31 => return Err(DtnError::IndefiniteLengthForbidden),
        _ => return Err(DtnError::InvalidAdditionalInfo),
    };
    let len = usize::try_from(raw).map_err(|_| DtnError::LengthOverflow)?;
    let start = header_width(ib).checked_add(len).ok_or(DtnError::LengthOverflow)?;
    // Reject a header whose body is not present in `buf`. This avoids
    // reporting a "valid" length for truncated input and keeps every caller
    // free from range checks against the original slice.
    buf.get(start - len..start).ok_or(DtnError::Truncated)?;
    Ok((len, header_width(ib)))
}

/// Decodes a definite-length CBOR array header (major type 4).
pub fn decode_array_header(buf: &[u8]) -> DtnResult<(usize, usize)> {
    let ib = *buf.first().ok_or(DtnError::BufferEmpty)?;
    if major_type(ib) != CborType::Array {
        return Err(DtnError::InvalidMajorType);
    }
    let raw = match ib & 0x1f {
        v @ 0..=23 => v as u64,
        24 => read_u8(buf, 1)? as u64,
        25 => u16::from_be_bytes(read_array::<2>(buf, 1)?) as u64,
        26 => u32::from_be_bytes(read_array::<4>(buf, 1)?) as u64,
        27 => u64::from_be_bytes(read_array::<8>(buf, 1)?),
        31 => return Err(DtnError::IndefiniteLengthForbidden),
        _ => return Err(DtnError::InvalidAdditionalInfo),
    };
    let len = usize::try_from(raw).map_err(|_| DtnError::LengthOverflow)?;
    Ok((len, header_width(ib)))
}

/// Decodes a definite-length CBOR map header (major type 5).
///
/// Used only by the legacy map-shaped primary block decoder retained for
/// compatibility with pre-RFC test vectors.
pub fn decode_map_header(buf: &[u8]) -> DtnResult<(usize, usize)> {
    let ib = *buf.first().ok_or(DtnError::BufferEmpty)?;
    if major_type(ib) != CborType::Map {
        return Err(DtnError::InvalidMajorType);
    }
    match ib & 0x1f {
        31 => Err(DtnError::IndefiniteLengthForbidden),
        _ => decode_array_header_for(ib, buf),
    }
}

/// Borrows the content of a definite-length CBOR byte string.
///
/// Returns `(content, total_consumed_bytes)`.
pub fn decode_byte_string(buf: &[u8]) -> DtnResult<(&[u8], usize)> {
    let (len, header) = decode_definite_length(buf)?;
    if major_type(buf[0]) != CborType::ByteString {
        return Err(DtnError::InvalidMajorType);
    }
    let start = header.checked_add(len).ok_or(DtnError::LengthOverflow)?;
    let body = buf.get(header..start).ok_or(DtnError::Truncated)?;
    Ok((body, start))
}

/// Borrows the content of a definite-length CBOR text string.
pub fn decode_text_string(buf: &[u8]) -> DtnResult<(&str, usize)> {
    let (len, header) = decode_definite_length(buf)?;
    if major_type(buf[0]) != CborType::TextString {
        return Err(DtnError::InvalidMajorType);
    }
    let start = header.checked_add(len).ok_or(DtnError::LengthOverflow)?;
    let body = buf.get(header..start).ok_or(DtnError::Truncated)?;
    let text = core::str::from_utf8(body).map_err(|_| DtnError::InvalidUtf8)?;
    Ok((text, start))
}

/// Skips exactly one CBOR item and returns the number of consumed bytes.
///
/// Recursion depth is bounded by [`MAX_SKIP_DEPTH`]; beyond that the function
/// fails instead of allowing a hostile nested item to exhaust a small stack.
/// Indefinite-length arrays/maps are skipped until their `0xff` break code.
pub fn skip_item(buf: &[u8], depth: u8) -> DtnResult<usize> {
    if depth > MAX_SKIP_DEPTH {
        return Err(DtnError::InvalidLength);
    }
    let ib = *buf.first().ok_or(DtnError::BufferEmpty)?;
    let ai = ib & 0x1f;
    match major_type(ib) {
        CborType::UnsignedInteger
        | CborType::NegativeInteger
        | CborType::SimpleOrFloat => Ok(scalar_width(ai)?),
        CborType::Tag => {
            let width = scalar_width(ai)?;
            let rest = buf.get(width..).ok_or(DtnError::Truncated)?;
            width.checked_add(skip_item(rest, depth + 1)?)
                .ok_or(DtnError::LengthOverflow)
        }
        CborType::ByteString | CborType::TextString => {
            if ai == 31 {
                return Err(DtnError::IndefiniteLengthForbidden);
            }
            let (len, header) = decode_definite_length(buf)?;
            header.checked_add(len).ok_or(DtnError::LengthOverflow)
        }
        CborType::Array | CborType::Map => {
            let is_map = major_type(ib) == CborType::Map;
            if ai == 31 {
                let mut off = 1usize;
                loop {
                    let next = *buf.get(off).ok_or(DtnError::Truncated)?;
                    if next == 0xff {
                        return off.checked_add(1).ok_or(DtnError::LengthOverflow);
                    }
                    let consumed = skip_item(&buf[off..], depth + 1)?;
                    off = off.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
                    if is_map {
                        let value = *buf.get(off).ok_or(DtnError::Truncated)?;
                        if value == 0xff {
                            return Err(DtnError::InvalidLength);
                        }
                        let consumed = skip_item(&buf[off..], depth + 1)?;
                        off = off.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
                    }
                }
            }
            let (count, header) = decode_array_header_for(ib, buf)?;
            let items = if is_map { count.checked_mul(2).ok_or(DtnError::LengthOverflow)? } else { count };
            let mut off = header;
            for _ in 0..items {
                let consumed = skip_item(buf.get(off..).ok_or(DtnError::Truncated)?, depth + 1)?;
                off = off.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
            }
            Ok(off)
        }
    }
}

/// Returns `true` when `buf` starts with a definite-length CBOR array header.
pub fn is_definite_array(buf: &[u8]) -> bool {
    !buf.is_empty() && major_type(buf[0]) == CborType::Array && buf[0] & 0x1f != 31
}

/// Returns `true` when `buf` starts with a definite-length CBOR map header.
pub fn is_definite_map(buf: &[u8]) -> bool {
    !buf.is_empty() && major_type(buf[0]) == CborType::Map && buf[0] & 0x1f != 31
}

fn decode_array_header_for(ib: u8, buf: &[u8]) -> DtnResult<(usize, usize)> {
    match ib & 0x1f {
        v @ 0..=23 => Ok((v as usize, 1)),
        24 => Ok((read_u8(buf, 1)? as usize, 2)),
        25 => Ok((u16::from_be_bytes(read_array::<2>(buf, 1)?) as usize, 3)),
        26 => {
            let raw = u32::from_be_bytes(read_array::<4>(buf, 1)?);
            Ok((usize::try_from(raw).map_err(|_| DtnError::LengthOverflow)?, 5))
        }
        27 => {
            let raw = u64::from_be_bytes(read_array::<8>(buf, 1)?);
            Ok((usize::try_from(raw).map_err(|_| DtnError::LengthOverflow)?, 9))
        }
        _ => Err(DtnError::InvalidAdditionalInfo),
    }
}

const fn header_width(ib: u8) -> usize {
    match ib & 0x1f {
        _v @ 0..=23 => 1,
        24 => 2,
        25 => 3,
        26 => 5,
        _ => 9,
    }
}

const fn scalar_width(ai: u8) -> DtnResult<usize> {
    match ai {
        _v @ 0..=23 => Ok(1),
        24 => Ok(2),
        25 => Ok(3),
        26 => Ok(5),
        27 => Ok(9),
        _ => Err(DtnError::InvalidAdditionalInfo),
    }
}

fn read_u8(buf: &[u8], at: usize) -> DtnResult<u8> {
    buf.get(at).copied().ok_or(DtnError::Truncated)
}

fn read_array<const N: usize>(buf: &[u8], at: usize) -> DtnResult<[u8; N]> {
    let slice = buf.get(at..at.checked_add(N).ok_or(DtnError::LengthOverflow)?)
        .ok_or(DtnError::Truncated)?;
    let mut out = [0u8; N];
    out.copy_from_slice(slice);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_small_uint() {
        assert_eq!(decode_unsigned(&[0x00]), Ok((0, 1)));
        assert_eq!(decode_unsigned(&[0x17]), Ok((23, 1)));
        assert_eq!(decode_unsigned(&[0x18, 0xff]), Ok((255, 2)));
        assert_eq!(decode_unsigned(&[0x19, 0x04, 0xd2]), Ok((1234, 3)));
    }

    #[test]
    fn decode_len() {
        assert_eq!(decode_definite_length(&[0x43, b'a', b'b', b'c']), Ok((3, 1)));
        assert_eq!(decode_definite_length(&[0x58, 0x01, 0xff]), Ok((1, 2)));
    }

    #[test]
    fn truncated_and_invalid_encodings_are_errors_not_panics() {
        assert_eq!(decode_unsigned(&[]), Err(DtnError::BufferEmpty));
        assert_eq!(decode_unsigned(&[0x18]), Err(DtnError::Truncated));
        assert_eq!(decode_unsigned(&[0x1c]), Err(DtnError::InvalidAdditionalInfo));
        assert_eq!(decode_unsigned(&[0x1f]), Err(DtnError::InvalidAdditionalInfo));
        assert_eq!(decode_definite_length(&[0x5f]), Err(DtnError::IndefiniteLengthForbidden));
        assert_eq!(decode_definite_length(&[0x41]), Err(DtnError::Truncated));
        assert_eq!(decode_unsigned(&[0x41]), Err(DtnError::InvalidMajorType));
    }

    #[test]
    fn huge_wire_length_cannot_wrap_on_32_bit() {
        let buf = [0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert!(decode_definite_length(&buf).is_err());
    }

    #[test]
    fn skip_nested_and_indefinite_items() {
        // [1, [2, 3], h'0102'] definite
        let definite = [0x83, 0x01, 0x82, 0x02, 0x03, 0x42, 0x01, 0x02];
        assert_eq!(skip_item(&definite, 0), Ok(definite.len()));
        // [_ 1, 2] indefinite
        let indefinite = [0x9f, 0x01, 0x02, 0xff];
        assert_eq!(skip_item(&indefinite, 0), Ok(indefinite.len()));
        // [_ [_ 1]] exceeds the allowed nesting for hostile input
        let deep = [0x9f, 0x9f, 0x01, 0xff, 0xff];
        assert!(skip_item(&deep, MAX_SKIP_DEPTH).is_err());
    }

    #[test]
    fn byte_and_text_helpers_borrow_without_allocation() {
        let bytes = [0x42, 0xde, 0xad, 0x00];
        assert_eq!(decode_byte_string(&bytes), Ok((&bytes[1..3], 3)));
        let text = [0x62, b'o', b'k', 0x00];
        assert_eq!(decode_text_string(&text), Ok(("ok", 3)));
        let invalid = [0x61, 0xff];
        assert_eq!(decode_text_string(&invalid), Err(DtnError::InvalidUtf8));
    }
}
