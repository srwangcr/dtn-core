//! Zero-copy RFC 9171 Primary Block parser (section 4.3.1).
//!
//! A conforming primary block is a definite CBOR array of eight, nine, ten or
//! eleven elements, optionally wrapped by the bundle's indefinite array
//! (`0x9f`). For compatibility with existing deployment fixtures this module
//! also accepts the legacy map-shaped encoding previously emitted by this
//! crate; [`crate::parser::primary_block::ParsedBundleHeader::rfc_array`] records which form was decoded so
//! callers can apply strict conformance checks where needed.
//!
//! ```
//! use dtn_core::parser::primary_block::parse_primary_block;
//! let bytes = [
//!     0x88, 0x07, 0x00, 0x00,
//!     0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
//!     0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
//! ];
//! let header = parse_primary_block(&bytes).unwrap();
//! assert_eq!(header.version, 7);
//! assert_eq!(header.lifetime, Some(60_000));
//! assert!(header.rfc_array);
//! ```

use crate::error::{DtnError, DtnResult};
use crate::parser::cbor::{
    decode_array_header, decode_byte_string, decode_definite_length, decode_map_header,
    decode_text_string, decode_unsigned, is_definite_array, is_definite_map, major_type, skip_item,
    CborType,
};

/// Maximum number of key/value pairs accepted in a legacy map primary block.
pub const MAX_LEGACY_MAP_PAIRS: usize = 64;

/// Borrowed fields of a BPv7 primary block.
///
/// All borrowed slices point into the original packet; the struct is `Copy`
/// and has $O(1)$ size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedBundleHeader<'a> {
    /// Bundle Protocol version; RFC 9171 uses `7`.
    pub version: u64,
    /// Bundle processing control flags (bit 0 = fragment).
    pub processing_flags: u64,
    /// CRC type: 0 = none, 1 = CRC-16/X-25, 2 = CRC32C.
    pub crc_type: u64,
    /// Destination EID text when encoded as a `dtn` text SSP.
    pub destination: Option<&'a str>,
    /// Source node EID text when encoded as a `dtn` text SSP.
    pub source: Option<&'a str>,
    /// Destination node number for `ipn` EIDs.
    pub destination_node_id: Option<u64>,
    /// Source node number for `ipn` EIDs.
    pub source_node_id: Option<u64>,
    /// Creation time in milliseconds since the DTN epoch (RFC 9171 DTN time).
    ///
    /// Legacy map fixtures encode seconds in the same field; the expiry
    /// calculation compares like-for-like with [`Self::lifetime`].
    pub creation_ts_sec: Option<u64>,
    /// Monotonically increasing creation timestamp sequence number.
    pub creation_seq: Option<u64>,
    /// Lifetime in milliseconds past creation time.
    pub lifetime: Option<u64>,
    /// Offset into the original ADU when the bundle is a fragment.
    pub fragment_offset: Option<u64>,
    /// Total ADU length when the bundle is a fragment.
    pub total_app_data_len: Option<u64>,
    /// Encoded primary block (including an optional leading `0x9f`).
    pub raw: &'a [u8],
    /// `true` when the RFC 9171 array encoding was decoded.
    pub rfc_array: bool,
}

impl<'a> ParsedBundleHeader<'a> {
    /// Returns `true` when bundle processing flag bit 0 (fragment) is set.
    pub const fn is_fragment(&self) -> bool {
        self.processing_flags & 0x01 != 0
    }

    /// Returns the absolute expiry time using saturating arithmetic.
    ///
    /// A malformed bundle with `creation + lifetime` overflowing `u64` is
    /// treated as expiring at `u64::MAX` rather than wrapping into the past.
    pub fn expiry_time(&self) -> Option<u64> {
        match (self.creation_ts_sec, self.lifetime) {
            (Some(created), Some(lifetime)) => Some(created.saturating_add(lifetime)),
            _ => None,
        }
    }
}

impl core::fmt::Display for ParsedBundleHeader<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ParsedBundleHeader v{} flags={} crc={} rfc_array={}",
            self.version, self.processing_flags, self.crc_type, self.rfc_array
        )
    }
}

/// Cheap O(1) pre-validation of a primary block header byte.
///
/// Returns `true` for definite CBOR maps, definite CBOR arrays and the
/// indefinite bundle array used by RFC 9171. This is only a filter; full
/// validation is performed by [`parse_primary_block`].
pub const fn quick_validate_primary_block(buf: &[u8]) -> bool {
    let Some(&first) = buf.first() else { return false; };
    if first == 0x9f {
        return buf.len() > 1 && matches!(major_type(buf[1]), CborType::Array);
    }
    matches!(major_type(first), CborType::Map | CborType::Array)
}

/// Parses a primary block without copying or allocating.
///
/// The returned [`ParsedBundleHeader::raw`] covers exactly the consumed
/// prefix, allowing a caller to iterate the remaining canonical blocks.
pub fn parse_primary_block<'a>(buf: &'a [u8]) -> DtnResult<ParsedBundleHeader<'a>> {
    if buf.is_empty() {
        return Err(DtnError::BufferEmpty);
    }
    let prefix = if buf.first() == Some(&0x9f) { 1 } else { 0 };
    let rest = buf.get(prefix..).ok_or(DtnError::Truncated)?;
    if rest.first() == Some(&0x9f) || rest.first() == Some(&0xbf) {
        return Err(DtnError::IndefiniteLengthForbidden);
    }
    if is_definite_array(rest) {
        parse_rfc_array(buf, prefix)
    } else if is_definite_map(rest) {
        parse_legacy_map(buf, prefix)
    } else {
        Err(DtnError::InvalidMajorType)
    }
}

fn parse_rfc_array<'a>(buf: &'a [u8], prefix: usize) -> DtnResult<ParsedBundleHeader<'a>> {
    let rest = &buf[prefix..];
    let (fields, header_len) = decode_array_header(rest)?;
    if !(8..=11).contains(&fields) {
        return Err(DtnError::InvalidLength);
    }
    let mut cursor = prefix.checked_add(header_len).ok_or(DtnError::LengthOverflow)?;
    let (version, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let (processing_flags, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let (crc_type, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    let mut destination = None;
    let mut source = None;
    let mut destination_node_id = None;
    let mut source_node_id = None;
    for slot in 0..3 {
        let (text, node, consumed) = parse_eid(buf, cursor)?;
        cursor = cursor.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
        match slot {
            0 => {
                destination = text;
                destination_node_id = node;
            }
            1 => {
                source = text;
                source_node_id = node;
            }
            _ => {}
        }
    }

    let (creation_ts_sec, creation_seq, consumed) = parse_creation_timestamp(buf, cursor)?;
    cursor = cursor.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
    let (lifetime, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    let mut fragment_offset = None;
    let mut total_app_data_len = None;
    if fields >= 10 {
        let (value, used) = consume_uint(buf, cursor)?;
        fragment_offset = Some(value);
        cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
        let (value, used) = consume_uint(buf, cursor)?;
        total_app_data_len = Some(value);
        cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    }
    if fields == 9 || fields == 11 {
        let (crc, used) = decode_byte_string(buf.get(cursor..).ok_or(DtnError::Truncated)?)?;
        let expected = match crc_type {
            1 => 2,
            2 => 4,
            _ => return Err(DtnError::InvalidLength),
        };
        if crc.len() != expected {
            return Err(DtnError::InvalidLength);
        }
        cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    }
    Ok(ParsedBundleHeader {
        version,
        processing_flags,
        crc_type,
        destination,
        source,
        destination_node_id,
        source_node_id,
        creation_ts_sec: Some(creation_ts_sec),
        creation_seq: Some(creation_seq),
        lifetime: Some(lifetime),
        fragment_offset,
        total_app_data_len,
        raw: buf.get(..cursor).ok_or(DtnError::Truncated)?,
        rfc_array: true,
    })
}

/// Decodes one CBOR EID array `[scheme, SSP]`; returns text, node id, width.
fn parse_eid(buf: &[u8], off: usize) -> DtnResult<(Option<&str>, Option<u64>, usize)> {
    let slice = buf.get(off..).ok_or(DtnError::Truncated)?;
    let (count, header) = decode_array_header(slice)?;
    if count != 2 {
        return Err(DtnError::InvalidLength);
    }
    let mut cursor = off.checked_add(header).ok_or(DtnError::LengthOverflow)?;
    let (scheme, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let ssp = buf.get(cursor..).ok_or(DtnError::Truncated)?;
    let mut text = None;
    let mut node = None;
    match major_type(*ssp.first().ok_or(DtnError::Truncated)?) {
        CborType::TextString => {
            let (value, used) = decode_text_string(ssp)?;
            if scheme == 1 {
                text = Some(value);
            }
            let consumed = used;
            cursor = cursor.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
        }
        CborType::UnsignedInteger => {
            let (value, used) = decode_unsigned(ssp)?;
            if value != 0 {
                node = Some(value);
            }
            cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
        }
        CborType::Array => {
            let (pair_len, used) = decode_array_header(ssp)?;
            if pair_len != 2 || scheme != 2 {
                return Err(DtnError::InvalidLength);
            }
            let (node_number, node_used) = decode_unsigned(ssp.get(used..).ok_or(DtnError::Truncated)?)?;
            let (_, service_used) =
                decode_unsigned(ssp.get(used + node_used..).ok_or(DtnError::Truncated)?)?;
            let consumed = used
                .checked_add(node_used)
                .and_then(|v| v.checked_add(service_used))
                .ok_or(DtnError::LengthOverflow)?;
            node = Some(node_number);
            cursor = cursor.checked_add(consumed).ok_or(DtnError::LengthOverflow)?;
        }
        _ => return Err(DtnError::InvalidMajorType),
    }
    let total = cursor.checked_sub(off).ok_or(DtnError::LengthOverflow)?;
    Ok((text, node, total))
}

fn parse_creation_timestamp(buf: &[u8], off: usize) -> DtnResult<(u64, u64, usize)> {
    let slice = buf.get(off..).ok_or(DtnError::Truncated)?;
    let (count, header) = decode_array_header(slice)?;
    if count != 2 {
        return Err(DtnError::InvalidLength);
    }
    let mut cursor = off.checked_add(header).ok_or(DtnError::LengthOverflow)?;
    let (secs, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let (seq, used) = consume_uint(buf, cursor)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    Ok((secs, seq, cursor.checked_sub(off).ok_or(DtnError::LengthOverflow)?))
}

fn consume_uint(buf: &[u8], off: usize) -> DtnResult<(u64, usize)> {
    decode_unsigned(buf.get(off..).ok_or(DtnError::Truncated)?)
}

fn parse_legacy_map<'a>(buf: &'a [u8], prefix: usize) -> DtnResult<ParsedBundleHeader<'a>> {
    let rest = &buf[prefix..];
    let (pairs, mut off) = decode_map_header(rest)?;
    if pairs > MAX_LEGACY_MAP_PAIRS {
        return Err(DtnError::InvalidLength);
    }
    off += prefix;
    let mut version = 0;
    let mut processing_flags = 0;
    let mut crc_type = 0;
    let mut destination = None;
    let mut source = None;
    let mut creation_ts_sec = None;
    let mut creation_seq = None;
    let mut lifetime = None;

    for _ in 0..pairs {
        let (key, used) = consume_uint(buf, off)?;
        off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
        match major_type(*buf.get(off).ok_or(DtnError::Truncated)?) {
            CborType::UnsignedInteger => {
                let (value, used) = consume_uint(buf, off)?;
                match key {
                    1 => version = value,
                    2 => processing_flags = value,
                    3 => crc_type = value,
                    7 => lifetime = Some(value),
                    _ => {}
                }
                off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
            }
            CborType::ByteString | CborType::TextString => {
                let is_text = major_type(buf[off]) == CborType::TextString;
                let (len, header) = decode_definite_length(buf.get(off..).ok_or(DtnError::Truncated)?)?;
                let start = off.checked_add(header).ok_or(DtnError::LengthOverflow)?;
                let end = start.checked_add(len).ok_or(DtnError::LengthOverflow)?;
                let body = buf.get(start..end).ok_or(DtnError::Truncated)?;
                if is_text {
                    let text = core::str::from_utf8(body).map_err(|_| DtnError::InvalidUtf8)?;
                    match key {
                        4 => destination = Some(text),
                        5 => source = Some(text),
                        _ => {}
                    }
                }
                off = end;
            }
            CborType::Array => {
                let (count, header) = decode_array_header(buf.get(off..).ok_or(DtnError::Truncated)?)?;
                off = off.checked_add(header).ok_or(DtnError::LengthOverflow)?;
                for index in 0..count {
                    let (value, used) = consume_uint(buf, off)?;
                    if index == 0 {
                        creation_ts_sec = Some(value);
                    } else if index == 1 {
                        creation_seq = Some(value);
                    }
                    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
                }
            }
            _ => {
                let used = skip_item(buf.get(off..).ok_or(DtnError::Truncated)?, 0)?;
                off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
            }
        }
    }

    Ok(ParsedBundleHeader {
        version,
        processing_flags,
        crc_type,
        destination,
        source,
        destination_node_id: None,
        source_node_id: None,
        creation_ts_sec,
        creation_seq,
        lifetime,
        fragment_offset: None,
        total_app_data_len: None,
        raw: buf.get(..off).ok_or(DtnError::Truncated)?,
        rfc_array: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> &'static [u8] {
        &[
            0xa6, 0x01, 0x07, 0x02, 0x03, 0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c', 0x06, 0x82, 0x19, 0x04, 0xd2, 0x01,
            0x07, 0x19, 0x0e, 0x10,
        ]
    }

    fn rfc() -> &'static [u8] {
        &[
            0x88, 0x07, 0x00, 0x00,
            0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
            0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
        ]
    }

    #[test]
    fn parse_legacy_primary() {
        let header = parse_primary_block(legacy()).unwrap();
        assert_eq!(header.version, 7);
        assert_eq!(header.processing_flags, 3);
        assert_eq!(header.destination, Some("dest"));
        assert_eq!(header.source, Some("src"));
        assert_eq!(header.creation_ts_sec, Some(1234));
        assert_eq!(header.creation_seq, Some(1));
        assert_eq!(header.lifetime, Some(3600));
        assert!(!header.rfc_array);
        assert_eq!(header.raw.len(), legacy().len());
    }

    #[test]
    fn parse_rfc_primary_with_indefinite_bundle_prefix() {
        let mut wrapped = [0u8; 32];
        wrapped[0] = 0x9f;
        let primary = rfc();
        wrapped[1..1 + primary.len()].copy_from_slice(primary);
        wrapped[1 + primary.len()] = 0xff;
        let used = 2 + primary.len();
        let header = parse_primary_block(&wrapped[..used]).unwrap();
        assert!(header.rfc_array);
        assert_eq!(header.version, 7);
        assert_eq!(header.destination_node_id, None);
        assert_eq!(header.creation_ts_sec, Some(0));
        assert_eq!(header.lifetime, Some(60_000));
        assert_eq!(header.expiry_time(), Some(60_000));
        assert_eq!(header.raw.len(), primary.len() + 1);
    }

    #[test]
    fn parse_rfc_primary_ipn_and_fragment_fields() {
        let bytes = [
            0x8a, 0x07, 0x01, 0x00,
            0x82, 0x02, 0x82, 0x19, 0x04, 0xd2, 0x01, // ipn:1234.1 destination
            0x82, 0x01, 0x00, // dtn:none source
            0x82, 0x01, 0x63, b'r', b'x', b't', // report-to dtn:rxt
            0x82, 0x19, 0x04, 0xd2, 0x02, // creation ts [1234, 2]
            0x19, 0x0e, 0x10, // lifetime 3600
            0x19, 0x01, 0x00, // fragment offset 256
            0x19, 0x02, 0x00, // total ADU length 512
        ];
        let header = parse_primary_block(&bytes).unwrap();
        assert_eq!(header.destination_node_id, Some(1234));
        assert_eq!(header.source, None); // dtn:none is not a text SSP
        assert!(header.is_fragment());
        assert_eq!(header.fragment_offset, Some(256));
        assert_eq!(header.total_app_data_len, Some(512));
    }

    #[test]
    fn parse_primary_rejects_malformed_lengths_without_panicking() {
        let truncated_text = [0xa2, 0x01, 0x07, 0x04, 0x64, b'd', b'e'];
        assert!(matches!(
            parse_primary_block(&truncated_text),
            Err(DtnError::Truncated)
        ));
        assert_eq!(
            parse_primary_block(&[0xbf]),
            Err(DtnError::IndefiniteLengthForbidden)
        );
        assert_eq!(parse_primary_block(&[]), Err(DtnError::BufferEmpty));
        assert_eq!(parse_primary_block(&[0x81]), Err(DtnError::InvalidLength));
        // RFC array with too few fields
        assert_eq!(
            parse_primary_block(&[0x83, 0x07, 0x00, 0x00]),
            Err(DtnError::InvalidLength)
        );
    }

    #[test]
    fn saturating_expiry_never_wraps() {
        let mut header = parse_primary_block(rfc()).unwrap();
        header.creation_ts_sec = Some(u64::MAX - 1);
        header.lifetime = Some(10);
        assert_eq!(header.expiry_time(), Some(u64::MAX));
    }

    #[test]
    fn quick_validation_accepts_both_encodings() {
        assert!(quick_validate_primary_block(legacy()));
        assert!(quick_validate_primary_block(rfc()));
        assert!(!quick_validate_primary_block(&[0x9f]));
        assert!(!quick_validate_primary_block(&[]));
    }
}
