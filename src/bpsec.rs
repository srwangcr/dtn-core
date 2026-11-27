//! BPSec Block Integrity Block (BIB, RFC 9172) for BPv7 bundles.
//!
//! This module implements the standards-track profile: the BIB is canonical
//! extension block type **11** and its block-type-specific data is an
//! **Abstract Security Block (ASB)** as required by RFC 9172 section 3.6/3.7:
//!
//! ```text
//! [ targets, context_id, flags, source, parameters, results ]
//! ```
//!
//! The only supported context is RFC 9173
//! [`SECURITY_CONTEXT_BIB_HMAC_SHA2`] = 1 with SHA variant 5 (HMAC 256/256).
//! There is no invented CRC-32C security context. The ASB carries exactly one
//! target (the payload block, number 1), one parameter pair for the SHA variant
//! and one for the RFC 9173 integrity scope flags, and one 32-byte MAC result.
//!
//! Before hashing, the Integrity Protected Plain Text (IPPT) is built exactly
//! as specified in RFC 9173 section 3.7: the canonical CBOR encoding of the
//! scope flags, followed (when the primary-block flag is set) by the canonical
//! primary block, followed by the canonical CBOR byte string of the payload
//! block's block-type-specific data. This profile uses `Scope Flags = 1`
//! ([`BIB_SCOPE_PRIMARY_BLOCK`] = `0x01`, Primary + Payload); the target block's
//! own header is **not** part of the current scope, only its data. Example IPPT
//! for scope 0 and payload
//! `h'526561647920746f2067656e657261746520612033322d62797465207061796c6f6164'`
//! is `h'005823526561647920...` — the length and header belong to the IPPT.
//!
//! No allocator is used: the signer writes into a caller-owned buffer and the
//! verifier streams the IPPT into HMAC-SHA-256.
//!
//! ```
//! use dtn_core::bpsec::{BibScope, BibSigner, BibVerifier, BibStatus};
//!
//! let bundle = [
//!     0x9f, 0x88, 0x07, 0x00, 0x00,
//!     0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
//!     0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
//!     0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58, 0xff,
//! ];
//! let key = [0x11u8; 32];
//! let signer = BibSigner::new(key, BibScope::PrimaryAndPayload);
//! let mut signed = [0u8; 128];
//! let len = signer.attach(&bundle, &mut signed).unwrap();
//! let verifier = BibVerifier::required(key);
//! let header = dtn_core::parser::primary_block::parse_primary_block(&signed[..len]).unwrap();
//! assert!(matches!(
//!     verifier.verify(&signed[..len], &header, &[0x58]),
//!     Ok(BibStatus::Verified { context: 1 })
//! ));
//! ```

use crate::error::{DtnError, DtnResult};
use crate::parser::cbor::{decode_array_header, decode_byte_string, decode_unsigned};
use crate::parser::extension_blocks::{parse_canonical_block, BlockType};
use crate::parser::primary_block::ParsedBundleHeader;
#[cfg(feature = "bpsec")]
use hmac::{Hmac, Mac};
#[cfg(feature = "bpsec")]
use sha2::Sha256;

/// IANA BPv7 block type code of the Bundle Integrity Block.
pub const BIB_BLOCK_TYPE: u64 = 11;
/// Block number used for the single BIB appended by this crate.
pub const BIB_BLOCK_NUMBER: u64 = 2;
/// RFC 9173 default integrity context BIB-HMAC-SHA2.
pub const SECURITY_CONTEXT_BIB_HMAC_SHA2: u64 = 1;
/// RFC 9173 SHA Variant parameter value for HMAC 256/256.
pub const BIB_HMAC_SHA256_VARIANT: u64 = 5;
/// RFC 9172 parameter ID of the SHA Variant.
pub const BIB_PARAM_SHA_VARIANT: u64 = 1;
/// RFC 9173 parameter ID of the Integrity Scope Flags.
pub const BIB_PARAM_INTEGRITY_SCOPE_FLAGS: u64 = 3;
/// Integrity scope: only the security target's data is included in the IPPT.
pub const BIB_SCOPE_PAYLOAD_ONLY: u64 = 0x00;
/// Integrity scope: the canonical primary block is prepended to the IPPT.
pub const BIB_SCOPE_PRIMARY_BLOCK: u64 = 0x01;
/// Length of the HMAC-SHA-256 authentication tag.
pub const MAX_MAC_LEN: usize = 32;
/// Largest encoded BIB canonical block produced by [`BibSigner`].
pub const MAX_BIB_BLOCK_LEN: usize = 128;

/// RFC 9173 Integrity Scope Flags used by this profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibScope {
    /// Scope flags `0x00`: canonical payload byte string only.
    Payload,
    /// Scope flags `0x01`: canonical primary block followed by the payload.
    PrimaryAndPayload,
}

impl BibScope {
    /// Returns the RFC 9173 integrity scope flag value.
    pub const fn code(self) -> u64 {
        match self {
            Self::Payload => BIB_SCOPE_PAYLOAD_ONLY,
            Self::PrimaryAndPayload => BIB_SCOPE_PRIMARY_BLOCK,
        }
    }

    /// Maps an RFC 9173 integrity scope flag value back to a scope.
    pub const fn from_code(code: u64) -> Option<Self> {
        match code {
            BIB_SCOPE_PAYLOAD_ONLY => Some(Self::Payload),
            BIB_SCOPE_PRIMARY_BLOCK => Some(Self::PrimaryAndPayload),
            _ => None,
        }
    }
}

/// Outcome of a successful BIB check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BibStatus {
    /// A BIB was present and its RFC 9173 result matched the recomputed MAC.
    Verified {
        /// RFC 9173 security context identifier that was validated (always 1).
        context: u64,
    },
    /// No BIB was present and the verifier was configured as opportunistic.
    Absent,
}

/// Writes a raw CBOR structural byte (array/byte-string header) into `out`.
fn put_raw(out: &mut [u8], off: &mut usize, byte: u8) -> DtnResult<()> {
    let end = off.checked_add(1).ok_or(DtnError::LengthOverflow)?;
    out.get_mut(*off..end).ok_or(DtnError::BufferTooSmall)?[0] = byte;
    *off = end;
    Ok(())
}

/// Writes a minimal CBOR unsigned integer into `out`.
fn put_uint(out: &mut [u8], off: &mut usize, value: u64) -> DtnResult<()> {
    let bytes: &[u8] = match value {
        0..=23 => {
            let byte = [value as u8];
            out.get_mut(*off..*off + 1)
                .ok_or(DtnError::BufferTooSmall)?
                .copy_from_slice(&byte);
            *off += 1;
            return Ok(());
        }
        24..=0xff => &[0x18, value as u8],
        0x100..=0xffff => &[0x19, (value >> 8) as u8, value as u8],
        _ => return Err(DtnError::InvalidLength),
    };
    let end = off.checked_add(bytes.len()).ok_or(DtnError::LengthOverflow)?;
    out.get_mut(*off..end).ok_or(DtnError::BufferTooSmall)?.copy_from_slice(bytes);
    *off = end;
    Ok(())
}

/// Writes a definite CBOR byte-string header for `len` into `out`.
fn put_byte_string_header(out: &mut [u8], off: &mut usize, len: usize) -> DtnResult<()> {
    match len {
        0..=23 => {
            let byte = [0x40 | len as u8];
            out.get_mut(*off..*off + 1)
                .ok_or(DtnError::BufferTooSmall)?
                .copy_from_slice(&byte);
            *off += 1;
        }
        24..=0xff => {
            let bytes = [0x58, len as u8];
            let end = off.checked_add(2).ok_or(DtnError::LengthOverflow)?;
            out.get_mut(*off..end).ok_or(DtnError::BufferTooSmall)?.copy_from_slice(&bytes);
            *off = end;
        }
        0x100..=0xffff => {
            let bytes = [0x59, (len >> 8) as u8, len as u8];
            let end = off.checked_add(3).ok_or(DtnError::LengthOverflow)?;
            out.get_mut(*off..end).ok_or(DtnError::BufferTooSmall)?.copy_from_slice(&bytes);
            *off = end;
        }
        _ => return Err(DtnError::InvalidLength),
    }
    Ok(())
}

/// Reads the next CBOR array header and advances `off`.
fn take_array(data: &[u8], off: &mut usize) -> DtnResult<usize> {
    let (len, used) = decode_array_header(data.get(*off..).ok_or(DtnError::Truncated)?)
        .map_err(|_| DtnError::InvalidBib)?;
    *off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    usize::try_from(len).map_err(|_| DtnError::LengthOverflow)
}

/// Reads the next CBOR unsigned integer and advances `off`.
fn take_uint(data: &[u8], off: &mut usize) -> DtnResult<u64> {
    let (value, used) = decode_unsigned(data.get(*off..).ok_or(DtnError::Truncated)?)
        .map_err(|_| DtnError::InvalidBib)?;
    *off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    Ok(value)
}

/// Reads the next CBOR byte string and advances `off`.
fn take_byte_string<'a>(data: &'a [u8], off: &mut usize) -> DtnResult<&'a [u8]> {
    let (value, used) = decode_byte_string(data.get(*off..).ok_or(DtnError::Truncated)?)
        .map_err(|_| DtnError::InvalidBib)?;
    *off = off.checked_add(used).ok_or(DtnError::LengthOverflow)?;
    Ok(value)
}

/// Streams the RFC 9173 section 3.7 IPPT into HMAC-SHA-256.
///
/// `primary` must be the deterministic-CBOR primary block and `payload` the
/// content of the payload block's block-type-specific data. The canonical form
/// of that data is its CBOR byte string **including** its header.
fn compute_mac(
    scope: BibScope,
    key: &[u8; 32],
    primary: &[u8],
    payload: &[u8],
    out: &mut [u8; MAX_MAC_LEN],
) -> DtnResult<usize> {
    #[cfg(not(feature = "bpsec"))]
    {
        let _ = (scope, key, primary, payload, out);
        return Err(DtnError::InvalidBib);
    }
    #[cfg(feature = "bpsec")]
    {
        // Reserved/unassigned scope bits are zero at the security source and
        // ignored by verifiers; only bits 0..2 are defined by RFC 9173.
        let flags = scope.code() & 0x07;
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
            .map_err(|_| DtnError::InvalidLength)?;
        mac.update(&[flags as u8]);
        if flags & BIB_SCOPE_PRIMARY_BLOCK != 0 {
            mac.update(primary);
        }
        let mut header = [0u8; 3];
        let mut header_len = 0usize;
        put_byte_string_header(&mut header, &mut header_len, payload.len())?;
        mac.update(&header[..header_len]);
        mac.update(payload);
        let tag = mac.finalize().into_bytes();
        out[..tag.len()].copy_from_slice(&tag);
        Ok(tag.len())
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

/// Encodes the RFC 9172 Abstract Security Block (BIB data) into `out`.
fn encode_asb(
    scope: BibScope,
    tag: &[u8],
    out: &mut [u8; MAX_BIB_BLOCK_LEN],
) -> DtnResult<usize> {
    let mut off = 0usize;
    // RFC 9172 ASB: 6-element array.
    put_raw(out, &mut off, 0x86)?;
    // Security targets: [1] (the payload block).
    put_raw(out, &mut off, 0x81)?;
    put_uint(out, &mut off, 1)?;
    // Security context id and flags (context parameters present).
    put_uint(out, &mut off, SECURITY_CONTEXT_BIB_HMAC_SHA2)?;
    put_uint(out, &mut off, 0x01)?;
    // Security source: dtn:none, encoded as [1, null] per RFC 9171.
    put_raw(out, &mut off, 0x82)?;
    put_uint(out, &mut off, 1)?;
    put_raw(out, &mut off, 0xf6)?;
    // Security context parameters: [[1, 5], [3, scope_flags]].
    put_raw(out, &mut off, 0x82)?;
    put_raw(out, &mut off, 0x82)?;
    put_uint(out, &mut off, BIB_PARAM_SHA_VARIANT)?;
    put_uint(out, &mut off, BIB_HMAC_SHA256_VARIANT)?;
    put_raw(out, &mut off, 0x82)?;
    put_uint(out, &mut off, BIB_PARAM_INTEGRITY_SCOPE_FLAGS)?;
    put_uint(out, &mut off, scope.code())?;
    // Security results: [[[1, h'tag']]] (the tuple is a 2-element array).
    put_raw(out, &mut off, 0x81)?;
    put_raw(out, &mut off, 0x81)?;
    put_raw(out, &mut off, 0x82)?;
    put_uint(out, &mut off, 1)?;
    put_byte_string_header(out, &mut off, tag.len())?;
    let end = off.checked_add(tag.len()).ok_or(DtnError::LengthOverflow)?;
    out.get_mut(off..end).ok_or(DtnError::BufferTooSmall)?.copy_from_slice(tag);
    Ok(end)
}

/// Encodes the BIB canonical block (type 11) into a caller-owned buffer.
fn encode_bib_block(
    scope: BibScope,
    tag: &[u8],
    out: &mut [u8; MAX_BIB_BLOCK_LEN],
) -> DtnResult<usize> {
    let mut asb = [0u8; MAX_BIB_BLOCK_LEN];
    let asb_len = encode_asb(scope, tag, &mut asb)?;
    let mut off = 0usize;
    out.get_mut(off..off + 5).ok_or(DtnError::BufferTooSmall)?.copy_from_slice(&[
        0x85,
        BIB_BLOCK_TYPE as u8,
        BIB_BLOCK_NUMBER as u8,
        0, // block processing control flags
        0, // CRC type 0 (the BIB result covers integrity)
    ]);
    off += 5;
    put_byte_string_header(out, &mut off, asb_len)?;
    let end = off.checked_add(asb_len).ok_or(DtnError::LengthOverflow)?;
    out.get_mut(off..end).ok_or(DtnError::BufferTooSmall)?.copy_from_slice(&asb[..asb_len]);
    Ok(end)
}

/// Parses an RFC 9172 ASB and returns `(scope_flags, tag)`.
fn parse_asb(data: &[u8]) -> DtnResult<(u64, &[u8])> {
    let mut off = 0usize;
    if take_array(data, &mut off)? != 6 {
        return Err(DtnError::InvalidBib);
    }
    if take_array(data, &mut off)? != 1 || take_uint(data, &mut off)? != 1 {
        return Err(DtnError::InvalidBib);
    }
    if take_uint(data, &mut off)? != SECURITY_CONTEXT_BIB_HMAC_SHA2 {
        return Err(DtnError::InvalidBib);
    }
    if take_uint(data, &mut off)? & 0x01 == 0 {
        return Err(DtnError::InvalidBib); // parameters are required by this profile
    }
    if take_array(data, &mut off)? != 2 {
        return Err(DtnError::InvalidBib);
    }
    // dtn:none is [1, null]; this profile emits no other security source.
    let eid_scheme = take_uint(data, &mut off)?;
    if eid_scheme != 1 {
        return Err(DtnError::InvalidBib);
    }
    let eid_null = data.get(off).copied().ok_or(DtnError::InvalidBib)?;
    if eid_null != 0xf6 {
        return Err(DtnError::InvalidBib);
    }
    off = off.checked_add(1).ok_or(DtnError::LengthOverflow)?;
    if take_array(data, &mut off)? != 2 {
        return Err(DtnError::InvalidBib);
    }
    let mut scope_flags = None;
    let mut sha_variant = None;
    for _ in 0..2 {
        if take_array(data, &mut off)? != 2 {
            return Err(DtnError::InvalidBib);
        }
        let id = take_uint(data, &mut off)?;
        let value = take_uint(data, &mut off)?;
        match id {
            BIB_PARAM_SHA_VARIANT if value == BIB_HMAC_SHA256_VARIANT => sha_variant = Some(value),
            BIB_PARAM_INTEGRITY_SCOPE_FLAGS if value <= 0x07 => scope_flags = Some(value),
            _ => return Err(DtnError::InvalidBib),
        }
    }
    if sha_variant != Some(BIB_HMAC_SHA256_VARIANT) || scope_flags.is_none() {
        return Err(DtnError::InvalidBib);
    }
    if take_array(data, &mut off)? != 1 || take_array(data, &mut off)? != 1 {
        return Err(DtnError::InvalidBib);
    }
    if take_array(data, &mut off)? != 2 || take_uint(data, &mut off)? != 1 {
        return Err(DtnError::InvalidBib);
    }
    let tag = take_byte_string(data, &mut off)?;
    if tag.len() != MAX_MAC_LEN || off != data.len() {
        return Err(DtnError::InvalidBib);
    }
    Ok((scope_flags.unwrap_or(0), tag))
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

/// Attaches an RFC 9172/9173 BIB to a BPv7 bundle.
pub struct BibSigner {
    key: [u8; 32],
    scope: BibScope,
}

impl BibSigner {
    /// Creates a signer using BIB-HMAC-SHA2 (SHA-256) with `scope`.
    pub const fn new(key: [u8; 32], scope: BibScope) -> Self {
        Self { key, scope }
    }

    /// Writes `bundle` with an inserted BIB block into `output`.
    ///
    /// Returns the number of bytes written. `output` must not alias `bundle`:
    /// the bundle is copied around the insertion point.
    pub fn attach(&self, bundle: &[u8], output: &mut [u8]) -> DtnResult<usize> {
        let header = crate::parser::primary_block::parse_primary_block(bundle)?;
        let (payload_offset, payload) = find_payload_block(bundle, header.raw.len())?;
        let mut tag = [0u8; MAX_MAC_LEN];
        let tag_len = compute_mac(self.scope, &self.key, header.raw, payload, &mut tag)?;
        let mut block = [0u8; MAX_BIB_BLOCK_LEN];
        let block_len = encode_bib_block(self.scope, &tag[..tag_len], &mut block)?;
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
        let (scope_flags, tag) = parse_asb(data)?;
        let scope = BibScope::from_code(scope_flags).ok_or(DtnError::InvalidBib)?;

        let mut expected = [0u8; MAX_MAC_LEN];
        let expected_len = compute_mac(scope, &self.key, header.raw, payload, &mut expected)?;
        if !constant_time_eq(tag, &expected[..expected_len]) {
            return Err(DtnError::BibVerificationFailed);
        }
        Ok(BibStatus::Verified { context: SECURITY_CONTEXT_BIB_HMAC_SHA2 })
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

    fn signed_bundle(scope: BibScope) -> ([u8; 160], usize) {
        let signer = BibSigner::new(KEY, scope);
        let mut out = [0u8; 160];
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
    fn rfc9173_sha256_bib_verifies_and_rejects_tampering() {
        let (signed, len) = signed_bundle(BibScope::PrimaryAndPayload);
        let verifier = BibVerifier::required(KEY);
        assert_eq!(
            verify(&signed[..len], &verifier),
            Ok(BibStatus::Verified { context: SECURITY_CONTEXT_BIB_HMAC_SHA2 })
        );

        let mut tampered = signed;
        tampered[len - 2] ^= 0x01; // payload byte before the break
        assert_eq!(verify(&tampered[..len], &verifier), Err(DtnError::BibVerificationFailed));
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn tampered_mac_and_wrong_key_are_rejected() {
        let (signed, len) = signed_bundle(BibScope::Payload);
        let verifier = BibVerifier::required(KEY);
        let mut tampered = signed;
        // The 32-byte tag sits immediately before the payload block header.
        let payload_start = tampered[..len]
            .windows(6)
            .position(|w| w == [0x85, 0x01, 0x01, 0x00, 0x00, 0x45])
            .unwrap();
        tampered[payload_start - 1] ^= 0x80;
        assert_eq!(verify(&tampered[..len], &verifier), Err(DtnError::BibVerificationFailed));
        assert_eq!(
            verify(&signed[..len], &BibVerifier::required([0x11; 32])),
            Err(DtnError::BibVerificationFailed)
        );
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn primary_scope_changes_the_ippt() {
        let payload_only = signed_bundle(BibScope::Payload);
        let primary_and_payload = signed_bundle(BibScope::PrimaryAndPayload);
        assert_ne!(payload_only.0[..payload_only.1], primary_and_payload.0[..primary_and_payload.1]);
        assert_eq!(
            verify(&payload_only.0[..payload_only.1], &BibVerifier::required(KEY)),
            Ok(BibStatus::Verified { context: SECURITY_CONTEXT_BIB_HMAC_SHA2 })
        );
        assert_eq!(
            verify(
                &primary_and_payload.0[..primary_and_payload.1],
                &BibVerifier::required(KEY)
            ),
            Ok(BibStatus::Verified { context: SECURITY_CONTEXT_BIB_HMAC_SHA2 })
        );
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn asb_uses_rfc9172_layout_and_sha256_variant() {
        let (signed, len) = signed_bundle(BibScope::Payload);
        // [targets], context=1, flags=1, dtn:none, [[1,5],[3,0]], [[[1,bstr32]]]
        const PREFIX: [u8; 21] = [
            0x86, // RFC 9172 ASB: 6-element array
            0x81, 0x01, // targets: [1]
            0x01, // BIB-HMAC-SHA2
            0x01, // context parameters present
            0x82, 0x01, 0xf6, // source: dtn:none ([1, null])
            0x82, 0x82, 0x01, 0x05, 0x82, 0x03, 0x00, // [[1,5],[3,0]]
            0x81, 0x81, 0x82, 0x01, 0x58, 0x20, // results: [[[1, h'...32]]]
        ];
        assert!(
            signed[..len].windows(PREFIX.len()).any(|window| window == PREFIX),
            "ASB prefix does not follow RFC 9172 section 3.6"
        );
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn missing_bib_is_rejected_when_required_and_allowed_when_opportunistic() {
        let bundle = unsigned_bundle();
        assert_eq!(verify(&bundle, &BibVerifier::required(KEY)), Err(DtnError::MissingBib));
        assert_eq!(
            verify(&bundle, &BibVerifier::opportunistic(KEY)),
            Ok(BibStatus::Absent)
        );
    }

    #[cfg(feature = "bpsec")]
    #[test]
    fn bib_is_inserted_before_payload_and_block_type_is_eleven() {
        let (signed, len) = signed_bundle(BibScope::PrimaryAndPayload);
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
    fn malformed_asb_is_rejected() {
        assert_eq!(parse_asb(&[0x82, 0x01, 0x01]), Err(DtnError::InvalidBib));
        assert_eq!(parse_asb(&[0x81, 0x01, 0x02]), Err(DtnError::InvalidBib));
        // Context 2 (the old, invented HMAC id) is no longer accepted.
        let mut asb = [0u8; MAX_BIB_BLOCK_LEN];
        let mut off = 0usize;
        let _ = put_uint(&mut asb, &mut off, 0x81);
        let _ = put_uint(&mut asb, &mut off, 1);
        let _ = put_uint(&mut asb, &mut off, 2);
        assert_eq!(parse_asb(&asb[..off]), Err(DtnError::InvalidBib));
    }
}
