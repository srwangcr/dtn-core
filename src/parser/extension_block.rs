//! BPv7 Extension Blocks and Payload Block Zero-Copy Parser (RFC 9171).

use crate::parser::cbor::{decode_definite_length, decode_unsigned};

/// Tipos de bloques estándar definidos en la IANA / RFC 9171.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum BlockType {
    Payload = 1,
    PreviousNode = 6,
    BundleAge = 7,
    HopCount = 12,
    Unknown(u64),
}

impl From<u64> for BlockType {
    fn from(val: u64) -> Self {
        match val {
            1 => BlockType::Payload,
            6 => BlockType::PreviousNode,
            7 => BlockType::BundleAge,
            12 => BlockType::HopCount,
            other => BlockType::Unknown(other),
        }
    }
}

/// Vista Zero-Copy de un Bloque BPv7 genérico (Extension o Payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalBlockHeader<'a> {
    pub block_type: BlockType,
    pub block_number: u64,
    pub flags: u64,
    pub crc_type: u64,
    pub data_slice: &'a [u8],
    pub raw_slice: &'a [u8],
}

/// Parse de un Bloque Canónico BPv7 sin asignación de memoria.
pub fn parse_canonical_block(buf: &[u8]) -> Result<(CanonicalBlockHeader<'_>, usize), &'static str> {
    if buf.is_empty() {
        return Err("buffer empty");
    }

    // Un bloque en BPv7 es un CBOR Array (típicamente de 5 elementos)
    let ib = buf[0];
    if (ib >> 5) != 4 {
        return Err("expected CBOR array for canonical block");
    }

    let array_len = ib & 0x1f;
    if array_len != 5 {
        return Err("canonical block array must have 5 elements");
    }

    let mut cursor = 1;

    // 1. Block Type
    let (block_type_raw, len) = decode_unsigned(&buf[cursor..])?;
    cursor += len;

    // 2. Block Number
    let (block_number, len) = decode_unsigned(&buf[cursor..])?;
    cursor += len;

    // 3. Block Processing Control Flags
    let (flags, len) = decode_unsigned(&buf[cursor..])?;
    cursor += len;

    // 4. CRC Type
    let (crc_type, len) = decode_unsigned(&buf[cursor..])?;
    cursor += len;

    // 5. Block Type Specific Data (Byte String CBOR)
    let (data_len, len_bytes) = decode_definite_length(&buf[cursor..])?;
    cursor += len_bytes;

    if buf[cursor..].len() < data_len {
        return Err("truncated block specific data");
    }

    let data_slice = &buf[cursor..cursor + data_len];
    cursor += data_len;

    let header = CanonicalBlockHeader {
        block_type: BlockType::from(block_type_raw),
        block_number,
        flags,
        crc_type,
        data_slice,
        raw_slice: &buf[..cursor],
    };

    Ok((header, cursor))
}

// ============================================================================
// PARSERS ESPECÍFICOS DE BLOQUES DE EXTENSIÓN (RFC 9171)
// ============================================================================

/// Parsed Hop Count Data (Tipo 12) -> [hop_limit, hop_count]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HopCount {
    pub limit: u8,
    pub count: u8,
}

pub fn parse_hop_count_data(data: &[u8]) -> Result<HopCount, &'static str> {
    let mut cursor = 0;
    
    // Verificamos el Array de 2 elementos de la data específica
    if data.is_empty() || (data[0] >> 5) != 4 || (data[0] & 0x1f) != 2 {
        return Err("invalid hop count CBOR array");
    }
    cursor += 1;

    let (limit, len) = decode_unsigned(&data[cursor..])?;
    cursor += len;

    let (count, _) = decode_unsigned(&data[cursor..])?;

    Ok(HopCount {
        limit: limit as u8,
        count: count as u8,
    })
}

/// Parsed Bundle Age Data (Tipo 7) -> Age in milliseconds (u64)
pub fn parse_bundle_age_data(data: &[u8]) -> Result<u64, &'static str> {
    let (age, _) = decode_unsigned(data)?;
    Ok(age)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_payload_block_header() {
        // Canonical CBOR Array (5 elem): [1 (type), 1 (number), 0 (flags), 0 (crc), h'44454144' (data "DEAD")]
        let raw_block = [
            0x85, // Array 5
            0x01, // Type 1 (Payload)
            0x01, // Number 1
            0x00, // Flags
            0x00, // CRC Type
            0x44, b'D', b'E', b'A', b'D', // Data (ByteString 4 bytes)
        ];

        let (header, consumed) = parse_canonical_block(&raw_block).expect("Parsing failed");
        assert_eq!(consumed, raw_block.len());
        assert_eq!(header.block_type, BlockType::Payload);
        assert_eq!(header.block_number, 1);
        assert_eq!(header.data_slice, b"DEAD");
    }

    #[test]
    fn parse_hop_count_extension() {
        // Data payload CBOR Array [limit=10, count=2] -> [0x82, 0x0A, 0x02]
        let hop_data = [0x82, 0x0A, 0x02];
        let hop = parse_hop_count_data(&hop_data).unwrap();
        assert_eq!(hop.limit, 10);
        assert_eq!(hop.count, 2);
    }
}
