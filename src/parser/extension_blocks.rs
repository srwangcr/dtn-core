//! Zero-copy parsers for RFC 9171 canonical extension blocks.
//!
//! Canonical blocks follow the primary block inside the bundle's indefinite
//! CBOR array. The four block types mandated by RFC 9171 section 4.4 and
//! section 4.1 are recognised explicitly:
//!
//! | Type | Block | Module support |
//! |------|-------|----------------|
//! | 1 | Payload | [`crate::parser::extension_blocks::parse_canonical_block`], [`crate::parser::extension_blocks::payload_from_block`] |
//! | 6 | Previous Node | [`crate::parser::extension_blocks::parse_previous_node_data`] |
//! | 7 | Bundle Age | [`crate::parser::extension_blocks::parse_bundle_age_data`], [`crate::parser::extension_blocks::validate_bundle_age`] |
//! | 10 | Hop Count | [`crate::parser::extension_blocks::parse_hop_count_data`], [`crate::parser::extension_blocks::validate_hop_count`] |
//!
//! Every accessor borrows from the caller's packet slice. The `u8` type code
//! for Hop Count is **10**, not 12: the latter is unassigned in the IANA
//! "Bundle Block Types" registry.
//!
//! ```
//! use dtn_core::parser::extension_blocks::{parse_canonical_block, BlockType};
//! // [type=7, number=2, flags=0, crc=0, h'1903e8'] (1000 ms age)
//! let block = [0x85, 0x07, 0x02, 0x00, 0x00, 0x43, 0x19, 0x03, 0xe8];
//! let (header, used) = parse_canonical_block(&block).unwrap();
//! assert_eq!(used, block.len());
//! assert_eq!(header.block_type, BlockType::BundleAge);
//! assert_eq!(header.data_slice, &[0x19, 0x03, 0xe8]);
//! ```

use crate::error::{DtnError, DtnResult};
use crate::parser::cbor::{
    decode_array_header, decode_byte_string, decode_text_string, decode_unsigned, major_type,
    CborType,
};

/// IANA BPv7 block type code of the Payload Block.
pub const BLOCK_TYPE_PAYLOAD: u64 = 1;
/// IANA BPv7 block type code of the Previous Node Block.
pub const BLOCK_TYPE_PREVIOUS_NODE: u64 = 6;
/// IANA BPv7 block type code of the Bundle Age Block.
pub const BLOCK_TYPE_BUNDLE_AGE: u64 = 7;
/// IANA BPv7 block type code of the Hop Count Block.
pub const BLOCK_TYPE_HOP_COUNT: u64 = 10;

/// Block type code of a canonical BPv7 block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// Bundle Payload Block (type 1).
    Payload,
    /// Previous Node Block (type 6).
    PreviousNode,
    /// Bundle Age Block (type 7).
    BundleAge,
    /// Hop Count Block (type 10).
    HopCount,
    /// Unrecognised or private/experimental block type.
    Unknown(u64),
}

impl BlockType {
    /// Returns the numeric IANA block type code.
    pub const fn code(self) -> u64 {
        match self {
            Self::Payload => BLOCK_TYPE_PAYLOAD,
            Self::PreviousNode => BLOCK_TYPE_PREVIOUS_NODE,
            Self::BundleAge => BLOCK_TYPE_BUNDLE_AGE,
            Self::HopCount => BLOCK_TYPE_HOP_COUNT,
            Self::Unknown(code) => code,
        }
    }
}

impl From<u64> for BlockType {
    fn from(value: u64) -> Self {
        match value {
            BLOCK_TYPE_PAYLOAD => Self::Payload,
            BLOCK_TYPE_PREVIOUS_NODE => Self::PreviousNode,
            BLOCK_TYPE_BUNDLE_AGE => Self::BundleAge,
            BLOCK_TYPE_HOP_COUNT => Self::HopCount,
            other => Self::Unknown(other),
        }
    }
}

/// Borrowed view of a canonical block header (RFC 9171 section 4.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalBlockHeader<'a> {
    /// Block type code, including the canonical types above.
    pub block_type: BlockType,
    /// Block number; the payload block always uses number 1.
    pub block_number: u64,
    /// Block processing control flags (bit field).
    pub flags: u64,
    /// CRC type: 0 = absent, 1 = CRC-16/X-25, 2 = CRC32C.
    pub crc_type: u64,
    /// CBOR-encoded block-type-specific data, borrowed from the packet.
    pub data_slice: &'a [u8],
    /// Complete encoded block, borrowed from the packet.
    pub raw_slice: &'a [u8],
}

/// Parses one canonical block and returns `(header, consumed_bytes)`.
///
/// Both five-element (no CRC) and six-element (CRC present) arrays are
/// accepted. A CRC field, when present, must be a byte string of two bytes
/// for CRC type 1 or four bytes for CRC type 2. Truncated fields are errors;
/// no slice index can panic on hostile input.
pub fn parse_canonical_block(buf: &[u8]) -> DtnResult<(CanonicalBlockHeader<'_>, usize)> {
    let (array_len, mut cursor) = decode_array_header(buf)?;
    if array_len != 5 && array_len != 6 {
        return Err(DtnError::InvalidLength);
    }

    let (block_type_raw, used) = decode_unsigned(buf.get(cursor..).ok_or(DtnError::Truncated)?)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    let (block_number, used) = decode_unsigned(buf.get(cursor..).ok_or(DtnError::Truncated)?)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    let (flags, used) = decode_unsigned(buf.get(cursor..).ok_or(DtnError::Truncated)?)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    let (crc_type, used) = decode_unsigned(buf.get(cursor..).ok_or(DtnError::Truncated)?)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    let (data_slice, used) = decode_byte_string(buf.get(cursor..).ok_or(DtnError::Truncated)?)?;
    cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;

    if array_len == 6 {
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
    } else if crc_type != 0 {
        return Err(DtnError::InvalidLength);
    }

    Ok((
        CanonicalBlockHeader {
            block_type: BlockType::from(block_type_raw),
            block_number,
            flags,
            crc_type,
            data_slice,
            raw_slice: buf.get(..cursor).ok_or(DtnError::Truncated)?,
        },
        cursor,
    ))
}

/// Returns the raw application data of a Payload Block.
///
/// RFC 9171 encodes the ADU as the content of the block's definite CBOR byte
/// string, so the borrowed `data_slice` is already the payload.
pub fn payload_from_block<'a>(block: &CanonicalBlockHeader<'a>) -> DtnResult<&'a [u8]> {
    if block.block_type != BlockType::Payload {
        return Err(DtnError::InvalidMajorType);
    }
    if block.block_number != 1 {
        return Err(DtnError::InvalidLength);
    }
    Ok(block.data_slice)
}

/// Borrowed endpoint identifier carried by a Previous Node Block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviousNodeEid<'a> {
    /// URI scheme code (1 = `dtn`, 2 = `ipn`).
    pub scheme: u64,
    /// Text scheme-specific part, when the SSP is a CBOR text string.
    pub ssp_text: Option<&'a str>,
    /// Numeric node id when the SSP is a CBOR unsigned integer or an `ipn` pair.
    pub node_number: Option<u64>,
}

/// Parses the block-type-specific data of a Previous Node Block (type 6).
///
/// The data is an EID encoded as `[scheme, SSP]`; this function recognises
/// `dtn` text SSPs, `dtn:none` (SSP = 0), and `ipn` node/service pairs.
pub fn parse_previous_node_data(data: &[u8]) -> DtnResult<PreviousNodeEid<'_>> {
    let (count, mut off) = decode_array_header(data)?;
    if count != 2 {
        return Err(DtnError::InvalidLength);
    }
    let (scheme, used) = decode_unsigned(data.get(off..).ok_or(DtnError::Truncated)?)?;
    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let ssp = data.get(off..).ok_or(DtnError::Truncated)?;

    let mut result = PreviousNodeEid { scheme, ssp_text: None, node_number: None };
    match major_type(*ssp.first().ok_or(DtnError::Truncated)?) {
        CborType::TextString => {
            let (text, used) = decode_text_string(ssp)?;
            result.ssp_text = Some(text);
            if used != ssp.len() {
                return Err(DtnError::InvalidLength);
            }
        }
        CborType::UnsignedInteger => {
            let (node, used) = decode_unsigned(ssp)?;
            if used != ssp.len() {
                return Err(DtnError::InvalidLength);
            }
            result.node_number = Some(node);
        }
        CborType::Array => {
            let (pair_len, used) = decode_array_header(ssp)?;
            if pair_len != 2 {
                return Err(DtnError::InvalidLength);
            }
            let (node, node_used) =
                decode_unsigned(ssp.get(used..).ok_or(DtnError::Truncated)?)?;
            let tail = ssp
                .get(used + node_used..)
                .ok_or(DtnError::Truncated)?;
            let (_, service_used) = decode_unsigned(tail)?;
            let consumed = used
                .checked_add(node_used)
                .and_then(|v| v.checked_add(service_used))
                .ok_or(DtnError::LengthOverflow)?;
            if consumed != ssp.len() {
                return Err(DtnError::InvalidLength);
            }
            result.node_number = Some(node);
        }
        _ => return Err(DtnError::InvalidMajorType),
    }
    Ok(result)
}

/// Hop Count Block data: `[hop_limit, hop_count]` (RFC 9171 section 4.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HopCount {
    /// Maximum number of hops, required to be in `1..=255`.
    pub limit: u8,
    /// Number of completed forwarding operations.
    pub count: u8,
}

impl HopCount {
    /// Validates the on-wire range (`1 <= limit <= 255`, `count <= 255`).
    pub fn checked(limit: u64, count: u64) -> DtnResult<Self> {
        if !(1..=u64::from(u8::MAX)).contains(&limit)
            || count > u64::from(u8::MAX)
        {
            return Err(DtnError::InvalidHopCount);
        }
        Ok(Self { limit: limit as u8, count: count as u8 })
    }

    /// Returns the count after one additional forwarding operation.
    pub fn after_forward(self) -> DtnResult<Self> {
        let count = self.count.checked_add(1).ok_or(DtnError::InvalidHopCount)?;
        Ok(Self { limit: self.limit, count })
    }
}

/// Parses and range-checks Hop Count Block data.
///
/// Values larger than `u8` are rejected rather than silently truncated, which
/// was a source of limit bypasses in earlier versions of the parser.
pub fn parse_hop_count_data(data: &[u8]) -> DtnResult<HopCount> {
    let (count, mut off) = decode_array_header(data)?;
    if count != 2 {
        return Err(DtnError::InvalidLength);
    }
    let (limit, used) = decode_unsigned(data.get(off..).ok_or(DtnError::Truncated)?)?;
    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let (hop_count, used) = decode_unsigned(data.get(off..).ok_or(DtnError::Truncated)?)?;
    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    if off != data.len() {
        return Err(DtnError::InvalidLength);
    }
    HopCount::checked(limit, hop_count)
}

/// Validates a hop count against its limit.
///
/// `after_forward` must be `true` when checking **before** forwarding a
/// bundle: RFC 9171 requires deletion when the count would *exceed* the limit.
/// An already-exceeded count is rejected in both modes.
pub fn validate_hop_count(hop: HopCount, after_forward: bool) -> DtnResult<()> {
    let effective = if after_forward {
        hop.after_forward()?.count
    } else {
        hop.count
    };
    if effective > hop.limit {
        Err(DtnError::HopLimitExceeded)
    } else {
        Ok(())
    }
}

/// Parses Bundle Age Block data: one unsigned integer in milliseconds.
pub fn parse_bundle_age_data(data: &[u8]) -> DtnResult<u64> {
    let (age, used) = decode_unsigned(data)?;
    if used != data.len() {
        return Err(DtnError::InvalidLength);
    }
    Ok(age)
}

/// Rejects a bundle whose age exceeds its effective lifetime.
///
/// Both values are in milliseconds. `effective_lifetime_ms` may be shorter
/// than the asserted lifetime when the node applies a local lifetime override
/// (RFC 9171 section 4.3.1).
pub fn validate_bundle_age(age_ms: u64, effective_lifetime_ms: u64) -> DtnResult<()> {
    if age_ms > effective_lifetime_ms {
        Err(DtnError::BundleAgeExceeded)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn age_block() -> [u8; 9] {
        [0x85, 0x07, 0x02, 0x00, 0x00, 0x43, 0x19, 0x03, 0xe8]
    }

    #[test]
    fn parse_valid_payload_block_header() {
        let raw = [0x85, 0x01, 0x01, 0x00, 0x00, 0x44, b'D', b'E', b'A', b'D'];
        let (header, used) = parse_canonical_block(&raw).unwrap();
        assert_eq!(used, raw.len());
        assert_eq!(header.block_type, BlockType::Payload);
        assert_eq!(header.block_number, 1);
        assert_eq!(payload_from_block(&header), Ok(&b"DEAD"[..]));
    }

    #[test]
    fn hop_count_type_is_ten() {
        assert_eq!(BlockType::from(10), BlockType::HopCount);
        assert_eq!(BlockType::HopCount.code(), 10);
        assert_eq!(BlockType::from(12), BlockType::Unknown(12));
    }

    #[test]
    fn parse_canonical_block_rejects_truncation_and_bad_lengths() {
        assert_eq!(parse_canonical_block(&[]), Err(DtnError::BufferEmpty));
        assert_eq!(parse_canonical_block(&[0x85, 0x01]), Err(DtnError::BufferEmpty));
        // Array with 4 elements is not a canonical block.
        assert_eq!(
            parse_canonical_block(&[0x84, 0x01, 0x01, 0x00, 0x00]),
            Err(DtnError::InvalidLength)
        );
        let mut bad = age_block();
        bad[5] = 0x44; // byte string says 4 bytes, only 3 follow
        assert_eq!(parse_canonical_block(&bad), Err(DtnError::Truncated));
        let mut no_crc_field = age_block();
        no_crc_field[4] = 0x02; // crc type 2 but 5-element array (no CRC field)
        assert_eq!(parse_canonical_block(&no_crc_field), Err(DtnError::InvalidLength));
    }

    #[test]
    fn crc_field_is_parsed_and_length_checked() {
        // [1,1,0,2,h'deadbeef',h'01020304'] -> 6 elements, CRC type 2
        let block = [
            0x86, 0x01, 0x01, 0x00, 0x02, 0x44, 0xde, 0xad, 0xbe, 0xef,
            0x44, 0x01, 0x02, 0x03, 0x04,
        ];
        let (header, used) = parse_canonical_block(&block).unwrap();
        assert_eq!(used, block.len());
        assert_eq!(header.crc_type, 2);
        assert_eq!(header.data_slice, &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn hop_count_parsing_and_limits() {
        let data = [0x82, 0x0a, 0x02];
        let hop = parse_hop_count_data(&data).unwrap();
        assert_eq!(hop, HopCount { limit: 10, count: 2 });
        assert_eq!(validate_hop_count(hop, false), Ok(()));
        assert_eq!(validate_hop_count(hop, true), Ok(()));

        let at_limit = HopCount { limit: 2, count: 2 };
        assert_eq!(validate_hop_count(at_limit, true), Err(DtnError::HopLimitExceeded));
        let exceeded = HopCount { limit: 2, count: 3 };
        assert_eq!(validate_hop_count(exceeded, false), Err(DtnError::HopLimitExceeded));
    }

    #[test]
    fn malformed_hop_counts_never_truncate_values() {
        assert_eq!(parse_hop_count_data(&[0x82, 0x19, 0x01, 0x00, 0x00]),
                   Err(DtnError::InvalidHopCount));
        assert_eq!(parse_hop_count_data(&[0x82, 0x00, 0x00]),
                   Err(DtnError::InvalidHopCount));
        assert_eq!(parse_hop_count_data(&[0x82, 0x0a]), Err(DtnError::BufferEmpty));
        // Two-element array followed by junk is not canonical.
        assert_eq!(parse_hop_count_data(&[0x82, 0x0a, 0x01, 0xff]),
                   Err(DtnError::InvalidLength));
    }

    #[test]
    fn bundle_age_validation() {
        let data = [0x19, 0x03, 0xe8];
        assert_eq!(parse_bundle_age_data(&data), Ok(1000));
        assert_eq!(parse_bundle_age_data(&[0x19, 0x03, 0xe8, 0x00]),
                   Err(DtnError::InvalidLength));
        assert_eq!(validate_bundle_age(1000, 1000), Ok(()));
        assert_eq!(validate_bundle_age(1001, 1000), Err(DtnError::BundleAgeExceeded));
    }

    #[test]
    fn previous_node_eid_variants() {
        let dtn = [0x82, 0x01, 0x63, b'n', b'o', b'd'];
        let eid = parse_previous_node_data(&dtn).unwrap();
        assert_eq!(eid.scheme, 1);
        assert_eq!(eid.ssp_text, Some("nod"));

        let none = [0x82, 0x01, 0x00];
        assert_eq!(parse_previous_node_data(&none).unwrap().node_number, Some(0));

        let ipn = [0x82, 0x02, 0x82, 0x19, 0x04, 0xd2, 0x01];
        assert_eq!(parse_previous_node_data(&ipn).unwrap().node_number, Some(1234));
        assert_eq!(parse_previous_node_data(&[0x82, 0x01]), Err(DtnError::Truncated));
    }
}
