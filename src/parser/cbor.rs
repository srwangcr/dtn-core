//! Canonical CBOR zero-copy helpers for no_std contexts.
use core::convert::TryInto;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CborType {
    UnsignedInteger,
    NegativeInteger,
    ByteString,
    TextString,
    Array,
    Map,
    Tag,
    SimpleOrFloat,
}

pub fn major_type(byte: u8) -> CborType {
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

/// Decode an unsigned integer encoded in CBOR (major type 0).
/// Returns (value, bytes_consumed) or Err if buffer too small or not unsigned.
pub fn decode_unsigned(buf: &[u8]) -> Result<(u64, usize), &'static str> {
    if buf.is_empty() {
        return Err("buffer empty");
    }
    let ib = buf[0];
    let mt = ib >> 5;
    if mt != 0 {
        return Err("not unsigned integer");
    }
    let ai = ib & 0x1f;
    match ai {
        v @ 0..=23 => Ok((v as u64, 1)),
        24 => {
            if buf.len() < 2 { return Err("short buf for u8"); }
            Ok((buf[1] as u64, 2))
        }
        25 => {
            if buf.len() < 3 { return Err("short buf for u16"); }
            let val = u16::from_be_bytes(buf[1..3].try_into().unwrap()) as u64;
            Ok((val, 3))
        }
        26 => {
            if buf.len() < 5 { return Err("short buf for u32"); }
            let val = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as u64;
            Ok((val, 5))
        }
        27 => {
            if buf.len() < 9 { return Err("short buf for u64"); }
            let val = u64::from_be_bytes(buf[1..9].try_into().unwrap());
            Ok((val, 9))
        }
        _ => Err("invalid ai for unsigned"),
    }
}

/// Fast O(1) validation for a CBOR definite-length byte or text string header.
/// Returns (length, header_size) where header_size is how many bytes the length field consumed.
pub fn decode_definite_length(buf: &[u8]) -> Result<(usize, usize), &'static str> {
    if buf.is_empty() { return Err("empty"); }
    let ib = buf[0];
    let mt = ib >> 5;
    if mt != 2 && mt != 3 { return Err("not byte/text string"); }
    let ai = ib & 0x1f;
    match ai {
        v @ 0..=23 => Ok((v as usize, 1)),
        24 => {
            if buf.len() < 2 { return Err("short for u8 len"); }
            Ok((buf[1] as usize, 2))
        }
        25 => {
            if buf.len() < 3 { return Err("short for u16 len"); }
            let val = u16::from_be_bytes(buf[1..3].try_into().unwrap()) as usize;
            Ok((val, 3))
        }
        26 => {
            if buf.len() < 5 { return Err("short for u32 len"); }
            let val = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
            Ok((val, 5))
        }
        27 => {
            if buf.len() < 9 { return Err("short for u64 len"); }
            let val = u64::from_be_bytes(buf[1..9].try_into().unwrap()) as usize;
            Ok((val, 9))
        }
        _ => Err("invalid ai for length"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_small_uint() {
        let buf = [0x00u8];
        assert_eq!(decode_unsigned(&buf).unwrap(), (0,1));
        let buf = [0x17u8];
        assert_eq!(decode_unsigned(&buf).unwrap(), (23,1));
        let buf = [0x18u8, 0xffu8];
        assert_eq!(decode_unsigned(&buf).unwrap(), (255,2));
    }

    #[test]
    fn decode_len() {
        let b = [0x43u8, b'a', b'b', b'c']; // byte string length 3
        assert_eq!(decode_definite_length(&b).unwrap(), (3,1));
    }
}
