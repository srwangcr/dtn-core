//! Zero-copy RFC 9171 Primary Block Parser
use core::fmt;
use crate::parser::cbor::{decode_unsigned, decode_definite_length, major_type};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedBundleHeader<'a> {
    pub version: u64,
    pub processing_flags: u64,
    pub crc_type: u64,
    pub destination: Option<&'a str>,
    pub source: Option<&'a str>,
    pub creation_ts_sec: Option<u64>,
    pub creation_seq: Option<u64>,
    pub lifetime: Option<u64>,
    pub raw: &'a [u8],
}

impl<'a> fmt::Display for ParsedBundleHeader<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ParsedBundleHeader v{} flags={} crc={}", self.version, self.processing_flags, self.crc_type)
    }
}

pub fn quick_validate_primary_block(buf: &[u8]) -> bool {
    if buf.is_empty() { return false; }
    let mt = major_type(buf[0]);
    matches!(mt, crate::parser::cbor::CborType::Map | crate::parser::cbor::CborType::Array)
}

pub fn parse_primary_block<'a>(buf: &'a [u8]) -> Result<ParsedBundleHeader<'a>, &'static str> {
    if buf.is_empty() { return Err("empty buffer"); }

    let ib = buf[0];
    let mt = ib >> 5;
    if mt != 5 { return Err("expected CBOR map for primary block"); }

    let (pairs, mut off) = match ib & 0x1f {
    val @ 0..=23 => (val as u64, 1),
    24 => (buf.get(1).copied().ok_or("truncated cbor")? as u64, 2),
    25 => (u16::from_be_bytes(buf.get(1..3).ok_or("truncated cbor")?.try_into().unwrap()) as u64, 3),
    26 => (u32::from_be_bytes(buf.get(1..5).ok_or("truncated cbor")?.try_into().unwrap()) as u64, 5),
    27 => (u64::from_be_bytes(buf.get(1..9).ok_or("truncated cbor")?.try_into().unwrap()), 9),
    31 => return Err("indefinite-length cbor maps are strictly prohibited in canonical bpv7"),
    _ => return Err("invalid cbor map length encoding"),
    };

    //Anti-DoS protection by theoretical mass assignments
    if pairs > 64 {
        return Err("cbor map contains too many elements for static buffer");
    }

    let mut version: u64 = 0;
    let mut processing_flags: u64 = 0;
    let mut crc_type: u64 = 0;
    let mut destination: Option<&'a str> = None;
    let mut source: Option<&'a str> = None;
    let mut creation_ts_sec: Option<u64> = None;
    let mut creation_seq: Option<u64> = None;
    let mut lifetime: Option<u64> = None;

    for _ in 0..pairs {
        if off >= buf.len() { return Err("unexpected end while reading map key"); }
        let (key, ksz) = decode_unsigned(&buf[off..])?;
        off += ksz;

        if off >= buf.len() { return Err("unexpected end while reading map value"); }
        let val_mt = buf[off] >> 5;

        match val_mt {
            0 => {
                let (v, vksz) = decode_unsigned(&buf[off..])?;
                match key {
                    1 => version = v,
                    2 => processing_flags = v,
                    3 => crc_type = v,
                    7 => lifetime = Some(v),
                    _ => {}
                }
                off += vksz;
            }
            3 | 2 => {
                let (len, hsz) = decode_definite_length(&buf[off..])?;
                let start = off + hsz;
                let end = start + len;
                if end > buf.len() { return Err("string extends past buffer"); }
                if buf[off] >> 5 == 3 {
                    let s = core::str::from_utf8(&buf[start..end]).map_err(|_| "invalid utf8 in text string")?;
                    match key {
                        4 => destination = Some(s),
                        5 => source = Some(s),
                        _ => {}
                    }
                }
                off = end;
            }
            4 => {
                let ai = buf[off] & 0x1f;
                if ai > 31 { return Err("indefinite arrays not supported"); }
                let arr_len = ai as usize;
                off += 1;
                if arr_len >= 1 {
                    if off >= buf.len() { return Err("unexpected end in array ts_sec"); }
                    let (s, ssz) = decode_unsigned(&buf[off..])?;
                    creation_ts_sec = Some(s);
                    off += ssz;
                    
                    if arr_len >= 2 {
                        if off >= buf.len() { return Err("unexpected end in array ts_seq"); }
                        let (sq, sqsz) = decode_unsigned(&buf[off..])?;
                        creation_seq = Some(sq);
                        off += sqsz;
                    }
                    for _ in 2..arr_len {
                        if off >= buf.len() { return Err("unexpected end skipping array extra"); }
                        let (_v, _sz) = decode_unsigned(&buf[off..])?; 
                        off += _sz;
                    }
                }
            }
            _ => {
                let mt = buf[off] >> 5;
                match mt {
                    2 | 3 => { 
                        let (len, hsz) = decode_definite_length(&buf[off..])?; 
                        off += hsz + len; 
                    }
                    4 | 5 => return Err("nested arrays/maps with additional parsing not supported"),
                    _ => return Err("unsupported CBOR type in primary block"),
                }
            }
        }
    }

    Ok(ParsedBundleHeader {
        version,
        processing_flags,
        crc_type,
        destination,
        source,
        creation_ts_sec,
        creation_seq,
        lifetime,
        raw: &buf[..off],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_buf() -> &'static [u8] {
        &[
            0xA6,
            0x01, 0x07,
            0x02, 0x03,
            0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c',
            0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
            0x07, 0x19, 0x0E, 0x10,
        ]
    }

    #[test]
    fn parse_primary() {
        let buf = make_test_buf();
        let hdr = parse_primary_block(buf).expect("parse");
        assert_eq!(hdr.version, 7);
        assert_eq!(hdr.processing_flags, 3);
        assert_eq!(hdr.destination.unwrap(), "dest");
        assert_eq!(hdr.source.unwrap(), "src");
        assert_eq!(hdr.creation_ts_sec.unwrap(), 1234);
        assert_eq!(hdr.creation_seq.unwrap(), 1);
        assert_eq!(hdr.lifetime.unwrap(), 3600);
    }
}