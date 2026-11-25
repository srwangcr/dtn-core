//! RFC 9171 bundle processing state machine for `no_std` systems.
//!
//! The processor performs, in order: quick header validation, full primary
//! block decode, expiry determination (timestamp or Bundle Age Block), a
//! complete walk of all canonical blocks, and enforcement of Hop Count /
//! Bundle Age constraints. It never mutates the packet and returns borrowed
//! views of the primary header and payload.
//!
//! ```
//! use dtn_core::processor::state_machine::{BundleProcessor, ProcessStatus};
//! // Clock-less bundle: creation time 0, 1000 ms Bundle Age Block, 3600 ms lifetime.
//! let bundle = [
//!     0x9f, 0x88, 0x07, 0x00, 0x00,
//!     0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
//!     0x82, 0x00, 0x01, 0x19, 0x0e, 0x10,
//!     0x85, 0x07, 0x02, 0x00, 0x00, 0x43, 0x19, 0x03, 0xe8,
//!     0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58,
//!     0xff,
//! ];
//! let result = BundleProcessor::process_bundle(&bundle, 123_456_789, None);
//! assert_eq!(result.status, ProcessStatus::Accepted);
//! assert_eq!(result.bundle_age_ms, Some(1000));
//! ```

use crate::parser::extension_blocks::{
    parse_bundle_age_data, parse_canonical_block, parse_hop_count_data,
    parse_previous_node_data, validate_bundle_age, validate_hop_count, BlockType, HopCount,
};
use crate::parser::primary_block::{parse_primary_block, quick_validate_primary_block, ParsedBundleHeader};
use crate::telemetry::metrics::SystemMetrics;

/// Maximum number of canonical blocks processed per bundle (DoS guard).
pub const MAX_CANONICAL_BLOCKS: usize = 32;

/// Outcome of passing a bundle through the processing state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Bundle is well formed and safe to forward.
    Accepted,
    /// CBOR structure, block ordering or mandatory blocks are invalid.
    RejectedMalformed,
    /// Timestamp-based lifetime was reached.
    Expired,
    /// Bundle Age Block exceeds the (possibly overridden) lifetime.
    BundleAgeExceeded,
    /// Hop Count Block reached/exceeded its hop limit.
    HopLimitExceeded,
}

/// Zero-copy result of bundle processing.
#[derive(Debug)]
pub struct ProcessResult<'a> {
    /// Final processing status.
    pub status: ProcessStatus,
    /// Decoded primary block, when decoding succeeded.
    pub primary_header: Option<ParsedBundleHeader<'a>>,
    /// Whether exactly one payload block was found.
    pub has_payload: bool,
    /// Number of bundle bytes consumed (primary + canonical blocks).
    pub bytes_processed: usize,
    /// Borrowed application data from the payload block.
    pub payload: Option<&'a [u8]>,
    /// Hop Count Block data, when present and valid.
    pub hop_count: Option<HopCount>,
    /// Bundle Age Block value in milliseconds, when present.
    pub bundle_age_ms: Option<u64>,
}

impl<'a> ProcessResult<'a> {
    fn new(
        status: ProcessStatus,
        primary_header: Option<ParsedBundleHeader<'a>>,
        payload: Option<&'a [u8]>,
        bytes_processed: usize,
    ) -> Self {
        Self {
            status,
            primary_header,
            has_payload: payload.is_some(),
            bytes_processed,
            payload,
            hop_count: None,
            bundle_age_ms: None,
        }
    }
}

/// Optional policy knobs for validation beyond the mandatory RFC checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingPolicy {
    /// Overrides the asserted bundle lifetime with a shorter local bound.
    ///
    /// `None` uses the primary block lifetime as the effective age limit.
    pub max_bundle_age_ms: Option<u64>,
    /// Enable rejection of hop counts that exceed their limit.
    pub enforce_hop_limit: bool,
    /// Maximum canonical blocks accepted per bundle.
    pub max_blocks: usize,
}

impl Default for ProcessingPolicy {
    fn default() -> Self {
        Self {
            max_bundle_age_ms: None,
            enforce_hop_limit: true,
            max_blocks: MAX_CANONICAL_BLOCKS,
        }
    }
}

/// Stateless BPv7 bundle processor.
pub struct BundleProcessor;

impl BundleProcessor {
    /// Processes a bundle with the default RFC policy.
    pub fn process_bundle<'a>(
        raw_bytes: &'a [u8],
        current_timestamp: u64,
        metrics: Option<&SystemMetrics>,
    ) -> ProcessResult<'a> {
        Self::process_bundle_with_policy(raw_bytes, current_timestamp, metrics, &ProcessingPolicy::default())
    }

    /// Processes a bundle with an explicit [`ProcessingPolicy`].
    ///
    /// `current_timestamp` is a DTN time in milliseconds. For nodes without a
    /// synchronised clock it may be any monotonic counter: the primary block's
    /// creation time of zero disables timestamp expiry, so a Bundle Age Block
    /// determines whether the bundle is still alive.
    pub fn process_bundle_with_policy<'a>(
        raw_bytes: &'a [u8],
        current_timestamp: u64,
        metrics: Option<&SystemMetrics>,
        policy: &ProcessingPolicy,
    ) -> ProcessResult<'a> {
        if let Some(m) = metrics {
            m.record_packet();
        }
        let reject = |primary: Option<ParsedBundleHeader<'a>>, bytes: usize, status| {
            if let Some(m) = metrics {
                m.record_drop();
            }
            ProcessResult::new(status, primary, None, bytes)
        };

        if !quick_validate_primary_block(raw_bytes) {
            return reject(None, 0, ProcessStatus::RejectedMalformed);
        }
        let primary = match parse_primary_block(raw_bytes) {
            Ok(header) if header.version == 7 => header,
            _ => return reject(None, 0, ProcessStatus::RejectedMalformed),
        };

        if primary.creation_ts_sec != Some(0) {
            if let Some(expiry) = primary.expiry_time() {
                if current_timestamp >= expiry {
                    return reject(Some(primary), primary.raw.len(), ProcessStatus::Expired);
                }
            }
        }

        let mut cursor = primary.raw.len();
        let mut payload: Option<&'a [u8]> = None;
        let mut hop_count: Option<HopCount> = None;
        let mut bundle_age_ms: Option<u64> = None;
        let mut previous_node_seen = false;
        let mut last_block_was_payload = false;
        let mut blocks = 0usize;

        while cursor < raw_bytes.len() {
            if raw_bytes[cursor] == 0xff {
                // CBOR break code terminating the bundle's indefinite array.
                cursor += 1;
                break;
            }
            if blocks >= policy.max_blocks {
                return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
            }
            let (block, consumed) = match parse_canonical_block(&raw_bytes[cursor..]) {
                Ok(parsed) => parsed,
                Err(_) => return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed),
            };
            if consumed == 0 {
                return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
            }
            cursor = match cursor.checked_add(consumed) {
                Some(next) => next,
                None => return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed),
            };
            blocks += 1;
            last_block_was_payload = block.block_type == BlockType::Payload;

            match block.block_type {
                BlockType::Payload => {
                    if payload.is_some() || block.block_number != 1 {
                        return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
                    }
                    payload = Some(block.data_slice);
                }
                BlockType::HopCount => {
                    if hop_count.is_some() {
                        return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
                    }
                    let hop = match parse_hop_count_data(block.data_slice) {
                        Ok(hop) => hop,
                        Err(_) => return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed),
                    };
                    if policy.enforce_hop_limit && validate_hop_count(hop, false).is_err() {
                        if let Some(m) = metrics {
                            m.record_drop();
                        }
                        let mut result = ProcessResult::new(
                            ProcessStatus::HopLimitExceeded,
                            Some(primary),
                            None,
                            cursor,
                        );
                        result.hop_count = Some(hop);
                        result.bundle_age_ms = bundle_age_ms;
                        return result;
                    }
                    hop_count = Some(hop);
                }
                BlockType::BundleAge => {
                    if bundle_age_ms.is_some() {
                        return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
                    }
                    let age = match parse_bundle_age_data(block.data_slice) {
                        Ok(age) => age,
                        Err(_) => return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed),
                    };
                    let effective_limit = policy
                        .max_bundle_age_ms
                        .or(primary.lifetime)
                        .unwrap_or(u64::MAX);
                    if validate_bundle_age(age, effective_limit).is_err() {
                        if let Some(m) = metrics {
                            m.record_drop();
                        }
                        let mut result = ProcessResult::new(
                            ProcessStatus::BundleAgeExceeded,
                            Some(primary),
                            None,
                            cursor,
                        );
                        result.bundle_age_ms = Some(age);
                        result.hop_count = hop_count;
                        return result;
                    }
                    bundle_age_ms = Some(age);
                }
                BlockType::PreviousNode => {
                    if previous_node_seen || parse_previous_node_data(block.data_slice).is_err() {
                        return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
                    }
                    previous_node_seen = true;
                }
                BlockType::Unknown(_) => {
                    // RFC 9171: unrecognised extension blocks are ignored unless
                    // their control flags demand bundle deletion (bit 2).
                    if block.flags & 0x04 != 0 {
                        return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
                    }
                }
            }
        }

        // Trailing bytes after the break code are not part of the bundle.
        if cursor > 0 && raw_bytes.get(cursor.wrapping_sub(1)) != Some(&0xff) {
            // Legacy bundles have no break code; accept them for compatibility.
            if primary.rfc_array {
                return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
            }
        }
        if primary.rfc_array && !last_block_was_payload {
            // RFC 9171 section 4.1: the last canonical block must be payload.
            return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed);
        }
        let payload = match payload {
            Some(value) => value,
            None => return reject(Some(primary), cursor, ProcessStatus::RejectedMalformed),
        };

        let mut result = ProcessResult::new(ProcessStatus::Accepted, Some(primary), Some(payload), cursor);
        result.hop_count = hop_count;
        result.bundle_age_ms = bundle_age_ms;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 192;

    fn primary_bytes() -> &'static [u8] {
        &[
            0x88, 0x07, 0x00, 0x00,
            0x82, 0x01, 0x63, b'd', b'e', b's', // destination dtn:des
            0x82, 0x01, 0x63, b's', b'r', b'c', // source dtn:src
            0x82, 0x01, 0x00, // report-to dtn:none
            0x82, 0x19, 0x04, 0xd2, 0x01, // creation ts [1234, 1]
            0x19, 0x0e, 0x10, // lifetime 3600
        ]
    }

    fn payload_bytes() -> &'static [u8] {
        &[0x85, 0x01, 0x01, 0x00, 0x00, 0x45, b'h', b'e', b'l', b'l', b'o']
    }

    /// Builds a bundle in a fixed stack buffer; returns `(buffer, used_len)`.
    fn bundle(extensions: &[u8]) -> ([u8; CAP], usize) {
        let mut bytes = [0u8; CAP];
        let mut n = 0usize;
        bytes[n] = 0x9f;
        n += 1;
        let primary = primary_bytes();
        bytes[n..n + primary.len()].copy_from_slice(primary);
        n += primary.len();
        bytes[n..n + extensions.len()].copy_from_slice(extensions);
        n += extensions.len();
        let payload = payload_bytes();
        bytes[n..n + payload.len()].copy_from_slice(payload);
        n += payload.len();
        bytes[n] = 0xff;
        n += 1;
        (bytes, n)
    }

    #[test]
    fn accepts_valid_bundle_and_exposes_borrowed_payload() {
        let (bytes, len) = bundle(&[]);
        let metrics = SystemMetrics::new();
        let result = BundleProcessor::process_bundle(&bytes[..len], 2000, Some(&metrics));
        assert_eq!(result.status, ProcessStatus::Accepted);
        assert_eq!(result.payload, Some(&b"hello"[..]));
        assert!(result.has_payload);
        assert_eq!(result.bytes_processed, len);
        let snap = metrics.snapshot();
        assert_eq!(snap.packets_processed, 1);
        assert_eq!(snap.packets_dropped, 0);
    }

    #[test]
    fn expires_bundle_at_deadline() {
        let (bytes, len) = bundle(&[]);
        let result = BundleProcessor::process_bundle(&bytes[..len], 1234 + 3600, None);
        assert_eq!(result.status, ProcessStatus::Expired);
    }

    #[test]
    fn rejects_missing_or_duplicate_payload() {
        let mut no_payload = [0u8; CAP];
        no_payload[0] = 0x9f;
        let primary = primary_bytes();
        no_payload[1..1 + primary.len()].copy_from_slice(primary);
        no_payload[1 + primary.len()] = 0xff;
        let len = 2 + primary.len();
        assert_eq!(
            BundleProcessor::process_bundle(&no_payload[..len], 2000, None).status,
            ProcessStatus::RejectedMalformed
        );

        let (mut bytes, len) = bundle(&[]);
        let payload = payload_bytes();
        // `len` includes the trailing 0x00 break; insert a second payload
        // immediately before it so both payload blocks are inside the array.
        let break_index = len - 1;
        bytes[break_index..break_index + payload.len()].copy_from_slice(payload);
        bytes[break_index + payload.len()] = 0xff;
        let duplicated_len = break_index + payload.len() + 1;
        assert_eq!(
            BundleProcessor::process_bundle(&bytes[..duplicated_len], 2000, None).status,
            ProcessStatus::RejectedMalformed
        );
    }

    #[test]
    fn rejects_truncated_payload_without_panic() {
        let bytes = [
            0xa6, 0x01, 0x07, 0x02, 0x03, 0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c', 0x06, 0x82, 0x19, 0x04, 0xd2, 0x01,
            0x07, 0x19, 0x0e, 0x10, 0x85, 0x01, 0x01, 0x00, 0x00, 0x45, b'h', b'e',
        ];
        assert_eq!(
            BundleProcessor::process_bundle(&bytes, 2000, None).status,
            ProcessStatus::RejectedMalformed
        );
    }

    #[test]
    fn enforces_hop_count_limit() {
        // Hop Count [limit=2, count=3] -> exceeded.
        let (bytes, len) = bundle(&[0x85, 0x0a, 0x02, 0x00, 0x00, 0x43, 0x82, 0x02, 0x03]);
        let result = BundleProcessor::process_bundle(&bytes[..len], 2000, None);
        assert_eq!(result.status, ProcessStatus::HopLimitExceeded);
        assert_eq!(result.hop_count, Some(HopCount { limit: 2, count: 3 }));

        let (bytes, len) = bundle(&[0x85, 0x0a, 0x02, 0x00, 0x00, 0x43, 0x82, 0x02, 0x02]);
        assert_eq!(
            BundleProcessor::process_bundle(&bytes[..len], 2000, None).status,
            ProcessStatus::Accepted
        );
    }

    #[test]
    fn enforces_bundle_age_against_lifetime_and_override() {
        // Bundle Age = 4000 ms, asserted lifetime = 3600 ms.
        let (bytes, len) = bundle(&[0x85, 0x07, 0x02, 0x00, 0x00, 0x43, 0x19, 0x0f, 0xa0]);
        // Keep the current time inside the primary block's lifetime so the
        // Bundle Age Block, not timestamp expiry, decides the outcome.
        let result = BundleProcessor::process_bundle(&bytes[..len], 2000, None);
        assert_eq!(result.status, ProcessStatus::BundleAgeExceeded);
        assert_eq!(result.bundle_age_ms, Some(4000));

        let policy = ProcessingPolicy {
            max_bundle_age_ms: Some(10_000),
            ..ProcessingPolicy::default()
        };
        let result = BundleProcessor::process_bundle_with_policy(&bytes[..len], 2000, None, &policy);
        assert_eq!(result.status, ProcessStatus::Accepted);
    }

    #[test]
    fn clockless_bundle_uses_age_block_instead_of_timestamp() {
        let mut clockless = [0u8; CAP];
        let mut n = 0usize;
        clockless[n] = 0x9f;
        n += 1;
        let primary = [
            0x88, 0x07, 0x00, 0x00,
            0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
            0x82, 0x00, 0x01, 0x19, 0x0e, 0x10,
        ];
        clockless[n..n + primary.len()].copy_from_slice(&primary);
        n += primary.len();
        let age = [0x85, 0x07, 0x02, 0x00, 0x00, 0x43, 0x19, 0x03, 0xe8];
        clockless[n..n + age.len()].copy_from_slice(&age);
        n += age.len();
        let payload = payload_bytes();
        clockless[n..n + payload.len()].copy_from_slice(payload);
        n += payload.len();
        clockless[n] = 0xff;
        n += 1;
        let result = BundleProcessor::process_bundle(&clockless[..n], u64::MAX - 1, None);
        assert_eq!(result.status, ProcessStatus::Accepted);
        assert_eq!(result.bundle_age_ms, Some(1000));
    }

    #[test]
    fn previous_node_block_is_validated() {
        let (bytes, len) = bundle(&[0x85, 0x06, 0x03, 0x00, 0x00, 0x43, 0x82, 0x01, 0x00]);
        assert_eq!(
            BundleProcessor::process_bundle(&bytes[..len], 2000, None).status,
            ProcessStatus::Accepted
        );
        let (bytes, len) = bundle(&[0x85, 0x06, 0x03, 0x00, 0x00, 0x41, 0x00]);
        assert_eq!(
            BundleProcessor::process_bundle(&bytes[..len], 2000, None).status,
            ProcessStatus::RejectedMalformed
        );
    }

    #[test]
    fn unknown_block_flags_can_force_rejection() {
        let (bytes, len) = bundle(&[0x85, 0x64, 0x09, 0x04, 0x00, 0x41, 0x00]);
        assert_eq!(
            BundleProcessor::process_bundle(&bytes[..len], 2000, None).status,
            ProcessStatus::RejectedMalformed
        );
    }
}
