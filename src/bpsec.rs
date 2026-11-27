//! BPSec Block Integrity Block (BIB, RFC 9172) for BPv7 bundles.
//!
//! The module implements a compact, fixed-footprint BIB profile suitable for
//! microcontrollers: exactly one security target (the payload block), one
//! security context and one result. The BIB is a canonical extension block of
//! IANA type **11** and is inserted *before* the payload block so that the
//! RFC 9171 rule "payload is the last canonical block" still holds.
//!
//! Block-type-specific data is the CBOR array
//! `[context_id, scope, h'mac']` where `context_id` follows the RFC 9173
//! default-context registry (1 = CRC-32C, 2 = HMAC-SHA-256) and `scope` is 1
//! for payload-only or 2 for primary-block-plus-payload. The MAC input is
//! domain-separated and length-prefixed:
//!
//! ```text
//! scope || context || len(primary) || primary || len(payload) || payload
//! ```
//!
//! Verification is streamed through `hmac`/`sha2` or the table-free CRC32C
//! implementation, so no intermediate copy of the bundle is made and no
//! allocator is needed. The `bpsec` feature gates the HMAC path; CRC-32C is
//! always available.
//!
//! ```
//! use dtn_core::bpsec::{BibScope, BibSigner, BibVerifier, SecurityContext, BibStatus};
//!
//! let bundle = [
//!     0x9f, 0x88, 0x07, 0x00, 0x00,
//!     0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
//!     0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
//!     0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58, 0xff,
//! ];
//! let key = [0x11u8; 32];
//! // CRC-32C is always available; use `SecurityContext::HmacSha256` with the
//! // `bpsec` feature enabled for a keyed 32-byte tag.
//! let signer = BibSigner::new(key, SecurityContext::Crc32C, BibScope::PrimaryAndPayload);
//! let mut signed = [0u8; 128];
//! let len = signer.attach(&bundle, &mut signed).unwrap();
//! let verifier = BibVerifier::required(key);
//! let header = dtn_core::parser::primary_block::parse_primary_block(&signed[..len]).unwrap();
//! assert!(matches!(
//!     verifier.verify(&signed[..len], &header, &[0x58]),
//!     Ok(BibStatus::Verified { .. })
//! ));
//! ```

use crate::error::{DtnError, DtnResult};
use crate::parser::cbor::{decode_array_header, decode_byte_string, decode_unsigned};
use crate::parser::extension_blocks::{parse_canonical_block, BlockType};
use crate::parser::primary_block::ParsedBundleHeader;
use crate::storage::crc32c::crc32c;
#[cfg(feature = "bpsec")]
use hmac::{Hmac, Mac};
#[cfg(feature = "bpsec")]
use sha2::Sha256;

/// IANA BPv7 block type code of the Bundle Integrity Block.
pub const BIB_BLOCK_TYPE: u64 = 11;
/// Block number used for the single BIB appended by this crate.
pub const BIB_BLOCK_NUMBER: u64 = 2;
/// RFC 9173 security context 1: CRC-32C.
pub const SECURITY_CONTEXT_CRC32C: u64 = 1;
/// RFC 9173 security context 2: HMAC with SHA-256.
pub const SECURITY_CONTEXT_HMAC_SHA256: u64 = 2;
/// Scope value: cover only the payload block.
pub const BIB_SCOPE_PAYLOAD: u64 = 1;
/// Scope value: cover the primary block and the payload block.
pub const BIB_SCOPE_PRIMARY_AND_PAYLOAD: u64 = 2;
/// Largest supported authentication tag (SHA-256).
pub const MAX_MAC_LEN: usize = 32;
/// Largest encoded BIB canonical block produced by [`BibSigner`].
pub const MAX_BIB_BLOCK_LEN: usize = 64;

/// Security context used to compute the BIB result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityContext {
    /// RFC 9173 CRC-32C (4-byte result, no key).
    Crc32C,
    /// RFC 9173 HMAC-SHA-256 (32-byte result, requires the `bpsec` feature).
    HmacSha256,
}

impl SecurityContext {
    /// Returns the RFC 9173 registry identifier.
    pub const fn code(self) -> u64 {
        match self {
            Self::Crc32C => SECURITY_CONTEXT_CRC32C,
            Self::HmacSha256 => SECURITY_CONTEXT_HMAC_SHA256,
        }
    }

    /// Maps a registry identifier back to a context.
    pub const fn from_code(code: u64) -> Option<Self> {
        match code {
            SECURITY_CONTEXT_CRC32C => Some(Self::Crc32C),
            SECURITY_CONTEXT_HMAC_SHA256 => Some(Self::HmacSha256),
            _ => None,
        }
    }
}

/// Portion of the bundle covered by the BIB result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibScope {
    /// MAC over the payload block data only.
    Payload,
    /// MAC over the encoded primary block and the payload block data.
    PrimaryAndPayload,
}

impl BibScope {
    /// Returns the on-wire scope discriminator.
    pub const fn code(self) -> u64 {
        match self {
            Self::Payload => BIB_SCOPE_PAYLOAD,
            Self::PrimaryAndPayload => BIB_SCOPE_PRIMARY_AND_PAYLOAD,
        }
    }

    /// Maps an on-wire scope discriminator back to a scope.
    pub const fn from_code(code: u64) -> Option<Self> {
        match code {
            BIB_SCOPE_PAYLOAD => Some(Self::Payload),
            BIB_SCOPE_PRIMARY_AND_PAYLOAD => Some(Self::PrimaryAndPayload),
            _ => None,
        }
    }
}

/// Outcome of a successful BIB check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibStatus {
    /// A BIB was present and its result matched the recomputed MAC.
    Verified {
        /// RFC 9173 security context identifier that was validated.
        context: u64,
    },
    /// No BIB was present and the verifier was configured as opportunistic.
    Absent,
}

/// Streams the canonical MAC input into a CRC32C or HMAC-SHA256 accumulator.
fn compute_mac(
    context: SecurityContext,
    scope: BibScope,
    key: &[u8; 32],
    primary: &[u8],
    payload: &[u8],
    out: &mut [u8; MAX_MAC_LEN],
) -> DtnResult<usize> {
    let domain = [scope.code() as u8, context.code() as u8];
    let primary_len = (primary.len() as u32).to_be_bytes();
    let payload_len = (payload.len() as u32).to_be_bytes();

    match context {
        SecurityContext::Crc32C => {
            let mut crc = crc32c(&domain, 0);
            if scope == BibScope::PrimaryAndPayload {
                crc = crc32c(&primary_len, crc);
                crc = crc32c(primary, crc);
            }
            crc = crc32c(&payload_len, crc);
            crc = crc32c(payload, crc);
            out[..4].copy_from_slice(&crc.to_be_bytes());
            Ok(4)
        }
        SecurityContext::HmacSha256 => {
            #[cfg(feature = "bpsec")]
            {
                type HmacSha256 = Hmac<Sha256>;
                let mut mac = <HmacSha256 as Mac>::new_from_slice(key)
                    .map_err(|_| DtnError::InvalidLength)?;
                mac.update(&domain);
                if scope == BibScope::PrimaryAndPayload {
                    mac.update(&primary_len);
                    mac.update(primary);
                }
                mac.update(&payload_len);
                mac.update(payload);
                let tag = mac.finalize().into_bytes();
                out[..tag.len()].copy_from_slice(&tag);
                Ok(tag.len())
            }
            #[cfg(not(feature = "bpsec"))]
            {
                let _ = (key, primary, payload, out, primary_len, payload_len);
                Err(DtnError::InvalidBib)
            }
        }
    }
}

/// Locates the payload canonical block and returns `(block_start, data)`.
fn find_payload_block(raw: &[u8], primary_len: usize) -> DtnResult<(usize, &[u8])> {
    let mut cursor = primary_len;
    while cursor < raw.len() {
        if raw[cursor] == 0xff {
            break;
        }
        let (block, used) = parse_canonical_block(raw.get(cursor..).ok_or(DtnError::Truncated)?)
            .map_err(|_| DtnError::InvalidBib)?;
        if block.block_type == BlockType::Payload {
            if block.block_number != 1 {
                return Err(DtnError::InvalidLength);
            }
            return Ok((cursor, block.data_slice));
        }
        cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    }
    Err(DtnError::InvalidLength)
}

/// Writes a definite CBOR byte-string header for `len <= 255`.
fn byte_string_header(len: usize) -> DtnResult<([u8; 2], usize)> {
    match len {
        0..=23 => Ok(([0x40 | len as u8, 0], 1)),
        24..=255 => Ok(([0x58, len as u8], 2)),
        _ => Err(DtnError::InvalidLength),
    }
}

/// Encodes the BIB canonical block into a fixed-size caller-owned buffer.
fn encode_bib_block(
    context: SecurityContext,
    scope: BibScope,
    tag: &[u8],
    out: &mut [u8; MAX_BIB_BLOCK_LEN],
) -> DtnResult<usize> {
    let (tag_header, tag_header_len) = byte_string_header(tag.len())?;
    let bib_data_len = 3 + tag_header_len + tag.len(); // [ctx, scope, bstr]
    let (data_header, data_header_len) = byte_string_header(bib_data_len)?;
    let block_len = 5 + data_header_len + bib_data_len;
    if block_len > out.len() {
        return Err(DtnError::BufferTooSmall);
    }

    out[0] = 0x85; // canonical block: 5-element array
    out[1] = BIB_BLOCK_TYPE as u8;
    out[2] = BIB_BLOCK_NUMBER as u8;
    out[3] = 0; // flags: no "delete bundle if unrecognised" bit
    out[4] = 0; // CRC type 0 (integrity is provided by the BIB result)
    out[5..5 + data_header_len].copy_from_slice(&data_header[..data_header_len]);
    let mut off = 5 + data_header_len;
    out[off] = 0x83; // BIB data: 3-element array
    off += 1;
    out[off] = context.code() as u8;
    off += 1;
    out[off] = scope.code() as u8;
    off += 1;
    out[off..off + tag_header_len].copy_from_slice(&tag_header[..tag_header_len]);
    off += tag_header_len;
    out[off..off + tag.len()].copy_from_slice(tag);
    Ok(block_len)
}

/// Parses the compact BIB data array `[context, scope, bstr(tag)]`.
fn parse_bib_data(data: &[u8]) -> DtnResult<(u64, u64, &[u8])> {
    let (count, mut off) = decode_array_header(data).map_err(|_| DtnError::InvalidBib)?;
    if count != 3 {
        return Err(DtnError::InvalidBib);
    }
    let (context, used) = decode_unsigned(data.get(off..).ok_or(DtnError::Truncated)?)
        .map_err(|_| DtnError::InvalidBib)?;
    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let (scope, used) = decode_unsigned(data.get(off..).ok_or(DtnError::Truncated)?)
        .map_err(|_| DtnError::InvalidBib)?;
    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    let (tag, used) = decode_byte_string(data.get(off..).ok_or(DtnError::Truncated)?)
        .map_err(|_| DtnError::InvalidBib)?;
    off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    if off != data.len() || tag.len() > MAX_MAC_LEN || tag.is_empty() {
        return Err(DtnError::InvalidBib);
    }
    Ok((context, scope, tag))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Attaches a BIB to a BPv7 bundle in a caller-owned output buffer.
pub struct BibSigner {
    key: [u8; 32],
    context: SecurityContext,
    scope: BibScope,
}

impl BibSigner {
    /// Creates a signer for the given key, context and coverage scope.
    pub const fn new(key: [u8; 32], context: SecurityContext, scope: BibScope) -> Self {
        Self { key, context, scope }
    }

    /// Writes `bundle` with an inserted BIB block into `output`.
    ///
    /// Returns the number of bytes written. `output` must not alias `bundle`
    /// because the routine performs two non-overlapping `copy_from_slice`
    /// operations around the insertion point.
    pub fn attach(&self, bundle: &[u8], output: &mut [u8]) -> DtnResult<usize> {
        let header = crate::parser::primary_block::parse_primary_block(bundle)?;
        let (payload_offset, payload) = find_payload_block(bundle, header.raw.len())?;
        let mut tag = [0u8; MAX_MAC_LEN];
        let tag_len = compute_mac(
            self.context,
            self.scope,
            &self.key,
            header.raw,
            payload,
            &mut tag,
        )?;
        let mut block = [0u8; MAX_BIB_BLOCK_LEN];
        let block_len = encode_bib_block(self.context, self.scope, &tag[..tag_len], &mut block)?;
        let total = bundle.len().checked_add(block_len).ok_or(DtnError::LengthOverflow)?;
        if output.len() < total {
            return Err(DtnError::BufferTooSmall);
        }
        output[..payload_offset].copy_from_slice(&bundle[..payload_offset]);
        output[payload_offset..payload_offset + block_len].copy_from_slice(&block[..block_len]);
        output[payload_offset + block_len..total].copy_from_slice(&bundle[payload_offset..]);
        Ok(total)
    }
}

/// Validates BIB results before a bundle is enqueued by the pipeline.
pub struct BibVerifier {
    key: [u8; 32],
    require_bib: bool,
}

impl BibVerifier {
    /// Creates a verifier that rejects bundles without a BIB.
    pub const fn required(key: [u8; 32]) -> Self {
        Self { key, require_bib: true }
    }

    /// Creates a verifier that checks a BIB when present but accepts unsigned
    /// legacy bundles. Any BIB that *is* present must still verify.
    pub const fn opportunistic(key: [u8; 32]) -> Self {
        Self { key, require_bib: false }
    }

    /// Verifies the bundle's BIB against the primary header and payload.
    ///
    /// This is called by
    /// [`crate::processor::pipeline::IngestionPipeline::ingest_and_route_with_security`]
    /// before routing or WAL insertion.
    pub fn verify(
        &self,
        raw: &[u8],
        header: &ParsedBundleHeader<'_>,
        payload: &[u8],
    ) -> DtnResult<BibStatus> {
        let mut cursor = header.raw.len();
        let mut bib_data: Option<&[u8]> = None;
        while cursor < raw.len() {
            if raw[cursor] == 0xff {
                break;
            }
            let (block, used) = parse_canonical_block(raw.get(cursor..).ok_or(DtnError::Truncated)?)
                .map_err(|_| DtnError::InvalidBib)?;
            cursor = cursor.checked_add(used).ok_or(DtnError::LengthOverflow)?;
            if block.block_type == BlockType::Unknown(BIB_BLOCK_TYPE) {
                if bib_data.is_some() {
                    return Err(DtnError::InvalidBib); // duplicate BIB
                }
                bib_data = Some(block.data_slice);
            }
        }

        let Some(data) = bib_data else {
            return if self.require_bib {
                Err(DtnError::MissingBib)
            } else {
                Ok(BibStatus::Absent)
            };
        };
        let (context_code, scope_code, tag) = parse_bib_data(data)?;
        let context = SecurityContext::from_code(context_code).ok_or(DtnError::InvalidBib)?;
        let scope = BibScope::from_code(scope_code).ok_or(DtnError::InvalidBib)?;

        let mut expected = [0u8; MAX_MAC_LEN];
        let expected_len =
            compute_mac(context, scope, &self.key, header.raw, payload, &mut expected)?;
        if !constant_time_eq(tag, &expected[..expected_len]) {
            return Err(DtnError::BibVerificationFailed);
        }
        Ok(BibStatus::Verified { context: context_code })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::primary_block::parse_primary_block;

    const KEY: [u8; 32] = [0x42; 32];

    fn unsigned_bundle() -> [u8; 32] {
        [
            0x9f, 0x88, 0x07, 0x00, 0x00,
            0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
            0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
            0x85, 0x01, 0x01, 0x00, 0x00, 0x45, b'h', b'e', b'l', b'l', b'o', 0xff,
        ]
    }

    fn signed_bundle(context: SecurityContext) -> ([u8; 128], usize) {
        let signer = BibSigner::new(KEY, context, BibScope::PrimaryAndPayload);
        let mut out = [0u8; 128];
        let len = signer.attach(&unsigned_bundle(), &mut out).unwrap();
        (out, len)
    }

    fn verify(raw: &[u8], verifier: &BibVerifier) -> DtnResult<BibStatus> {
        let header = parse_primary_block(raw).unwrap();
        let (_, payload) = find_payload_block(raw, header.raw.len()).unwrap();
        verifier.verify(raw, &header, payload)
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn hmac_bib_accepts_signed_and_rejects_tampered_payload() {
        let (signed, len) = signed_bundle(SecurityContext::HmacSha256);
        let verifier = BibVerifier::required(KEY);
        assert!(matches!(
            verify(&signed[..len], &verifier),
            Ok(BibStatus::Verified { context: 2 })
        ));

        let mut tampered = signed;
        // The payload is the last canonical block, directly before the break.
        tampered[len - 2] ^= 0x01;
        assert_eq!(verify(&tampered[..len], &verifier), Err(DtnError::BibVerificationFailed));
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn tampered_mac_is_rejected_deterministically() {
        let (signed, len) = signed_bundle(SecurityContext::HmacSha256);
        let verifier = BibVerifier::required(KEY);
        let mut tampered = signed;
        // BIB sits before the payload; its tag ends before the payload header.
        let payload_start = tampered[..len]
            .windows(6)
            .position(|w| w == [0x85, 0x01, 0x01, 0x00, 0x00, 0x45])
            .unwrap();
        tampered[payload_start - 1] ^= 0x80;
        assert_eq!(verify(&tampered[..len], &verifier), Err(DtnError::BibVerificationFailed));
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn wrong_key_and_missing_bib_are_rejected() {
        let (signed, len) = signed_bundle(SecurityContext::HmacSha256);
        let wrong = BibVerifier::required([0x11; 32]);
        assert_eq!(verify(&signed[..len], &wrong), Err(DtnError::BibVerificationFailed));
        let bundle = unsigned_bundle();
        assert_eq!(verify(&bundle, &BibVerifier::required(KEY)), Err(DtnError::MissingBib));
        assert_eq!(verify(&bundle, &BibVerifier::opportunistic(KEY)), Ok(BibStatus::Absent));
    }

    #[test]
    fn crc32c_bib_is_accepted_without_the_hmac_feature() {
        let (signed, len) = signed_bundle(SecurityContext::Crc32C);
        let verifier = BibVerifier::required(KEY);
        assert!(matches!(
            verify(&signed[..len], &verifier),
            Ok(BibStatus::Verified { context: 1 })
        ));
        let mut tampered = signed;
        tampered[len - 2] ^= 0x40;
        assert_eq!(verify(&tampered[..len], &verifier), Err(DtnError::BibVerificationFailed));
    }

    #[test]
    fn bib_is_inserted_before_payload_and_block_type_is_eleven() {
        let (signed, len) = signed_bundle(SecurityContext::Crc32C);
        assert!(len > unsigned_bundle().len());
        assert_eq!(signed[0], 0x9f);
        assert_eq!(signed[len - 1], 0xff);
        let header = parse_primary_block(&signed[..len]).unwrap();
        let mut cursor = header.raw.len();
        let mut saw_bib = false;
        while cursor < len && signed[cursor] != 0xff {
            let (block, used) = parse_canonical_block(&signed[cursor..len]).unwrap();
            if block.block_type == BlockType::Unknown(BIB_BLOCK_TYPE) {
                assert!(!saw_bib);
                saw_bib = true;
                assert_eq!(block.block_number, BIB_BLOCK_NUMBER);
            } else if block.block_type == BlockType::Payload {
                assert!(saw_bib, "payload must remain the last canonical block");
            }
            cursor += used;
        }
        assert!(saw_bib);
    }

    #[test]
    fn malformed_bib_data_is_rejected() {
        assert_eq!(parse_bib_data(&[0x82, 0x01, 0x01]), Err(DtnError::InvalidBib));
        assert_eq!(parse_bib_data(&[0x83, 0x01, 0x01, 0x00]), Err(DtnError::InvalidBib));
    }
}
