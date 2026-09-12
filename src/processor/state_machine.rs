//! BPv7 Bundle Processing State Machine (RFC 9171) for no_std contexts.

#[cfg(test)]
extern crate std;

use crate::parser::extension_block::{parse_canonical_block, BlockType};
use crate::parser::primary_block::{parse_primary_block, quick_validate_primary_block, ParsedBundleHeader};
use crate::telemetry::metrics::SystemMetrics;

/// Estados de procesamiento de un Bundle dentro del Pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Accepted,
    RejectedMalformed,
    Expired,
}

/// Resultado del paso por la máquina de estados sin mutar la memoria subyacente.
#[derive(Debug)]
pub struct ProcessResult<'a> {
    pub status: ProcessStatus,
    pub primary_header: Option<ParsedBundleHeader<'a>>,
    pub has_payload: bool,
    pub bytes_processed: usize,
}

/// Procesador central de Bundles BPv7.
pub struct BundleProcessor;

impl BundleProcessor {
    /// Procesa un slice directo de memoria (Zero-Copy) ejecutando las etapas de pipeline.
    pub fn process_bundle<'a>(
        raw_bytes: &'a [u8],
        current_timestamp: u64,
        metrics: Option<&SystemMetrics>,
    ) -> ProcessResult<'a> {
        if let Some(m) = metrics {
            m.record_packet();
        }

        // 1. Quick Validation O(1)
        let quick_val = quick_validate_primary_block(raw_bytes);
        #[cfg(test)]
        std::println!("DEBUG: quick_validate_primary_block = {}", quick_val);

        if !quick_val {
            if let Some(m) = metrics {
                m.record_drop();
            }
            return ProcessResult {
                status: ProcessStatus::RejectedMalformed,
                primary_header: None,
                has_payload: false,
                bytes_processed: 0,
            };
        }

        // 2. Full Parse del Primary Block
        let primary = match parse_primary_block(raw_bytes) {
            Ok(p) => p,
            Err(_e) => {
                #[cfg(test)]
                std::println!("DEBUG: parse_primary_block error");

                if let Some(m) = metrics {
                    m.record_drop();
                }
                return ProcessResult {
                    status: ProcessStatus::RejectedMalformed,
                    primary_header: None,
                    has_payload: false,
                    bytes_processed: 0,
                };
            }
        };

        // 3. Validación de Expiración por Timestamp (creation_ts_sec + lifetime)
        if let (Some(creation_ts), Some(lifetime)) = (primary.creation_ts_sec, primary.lifetime) {
            let expiry_time = creation_ts.saturating_add(lifetime);
            if current_timestamp >= expiry_time {
                if let Some(m) = metrics {
                    m.record_drop();
                }
                let raw_len = primary.raw.len();
                return ProcessResult {
                    status: ProcessStatus::Expired,
                    primary_header: Some(primary),
                    has_payload: false,
                    bytes_processed: raw_len,
                };
            }
        }

        // 4. Recorrido exhaustivo de Bloques Canónicos (sin rompimiento prematuro al detectar Payload)
        let mut cursor = primary.raw.len();
        let mut has_payload = false;

        while cursor < raw_bytes.len() {
            match parse_canonical_block(&raw_bytes[cursor..]) {
                Ok((block, consumed)) => {
                    cursor += consumed;
                    if block.block_type == BlockType::Payload {
                        has_payload = true;
                    }
                }
                Err(_e) => {
                    #[cfg(test)]
                    std::println!("DEBUG: parse_canonical_block error at cursor {}", cursor);

                    if let Some(m) = metrics {
                        m.record_drop();
                    }
                    return ProcessResult {
                        status: ProcessStatus::RejectedMalformed,
                        primary_header: Some(primary),
                        has_payload: false,
                        bytes_processed: cursor,
                    };
                }
            }
        }

        // Un bundle válido según RFC 9171 DEBE incluir al menos un Payload Block
        if !has_payload {
            #[cfg(test)]
            std::println!("DEBUG: bundle missing payload block");

            if let Some(m) = metrics {
                m.record_drop();
            }
            return ProcessResult {
                status: ProcessStatus::RejectedMalformed,
                primary_header: Some(primary),
                has_payload: false,
                bytes_processed: cursor,
            };
        }

        ProcessResult {
            status: ProcessStatus::Accepted,
            primary_header: Some(primary),
            has_payload,
            bytes_processed: cursor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_valid_primary() -> &'static [u8] {
        &[
            // --- Primary Block ---
            0xA6,
            0x01, 0x07,
            0x02, 0x03,
            0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c',
            0x06, 0x82, 0x19, 0x04, 0xD2, 0x01, // creation_ts_sec = 1234
            0x07, 0x19, 0x0E, 0x10,             // lifetime = 3600 -> Expiration = 4834

            // --- Payload Block Canónico adjunto ---
            0x85,       // CBOR Array de 5 elementos
            0x01,       // Block Type: 1 (Payload)
            0x01,       // Block Number: 1
            0x00,       // Block Processing Control Flags: 0
            0x00,       // CRC Type: 0 (No CRC)
            0x45,       // Byte string de 5 bytes ("hello")
            b'h', b'e', b'l', b'l', b'o',
        ]
    }

    #[test]
    fn process_valid_primary_bundle() {
        let buf = make_valid_primary();
        let metrics = SystemMetrics::new();

        let res = BundleProcessor::process_bundle(buf, 2000, Some(&metrics));
        assert_eq!(res.status, ProcessStatus::Accepted);
        assert!(res.primary_header.is_some());

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_processed, 1);
        assert_eq!(snap.packets_dropped, 0);
    }

    #[test]
    fn process_expired_bundle() {
        let buf = make_valid_primary();
        let metrics = SystemMetrics::new();

        // 5000 >= 1234 + 3600 (4834)
        let res = BundleProcessor::process_bundle(buf, 5000, Some(&metrics));
        assert_eq!(res.status, ProcessStatus::Expired);

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_processed, 1);
        assert_eq!(snap.packets_dropped, 1);
    }

    #[test]
    fn process_bundle_expires_at_exact_deadline() {
        let buf = make_valid_primary();
        let metrics = SystemMetrics::new();

        let res = BundleProcessor::process_bundle(buf, 4834, Some(&metrics));
        assert_eq!(res.status, ProcessStatus::Expired);

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_processed, 1);
        assert_eq!(snap.packets_dropped, 1);
    }

    #[test]
    fn process_bundle_rejects_missing_payload_block() {
        let buf = &[
            0xA6,
            0x01, 0x07,
            0x02, 0x03,
            0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c',
            0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
            0x07, 0x19, 0x0E, 0x10,
        ];

        let metrics = SystemMetrics::new();
        let res = BundleProcessor::process_bundle(buf, 2000, Some(&metrics));
        assert_eq!(res.status, ProcessStatus::RejectedMalformed);
        assert_eq!(res.has_payload, false);
    }

    #[test]
    fn process_bundle_rejects_truncated_payload_block() {
        let buf = &[
            0xA6,
            0x01, 0x07,
            0x02, 0x03,
            0x04, 0x64, b'd', b'e', b's', b't',
            0x05, 0x63, b's', b'r', b'c',
            0x06, 0x82, 0x19, 0x04, 0xD2, 0x01,
            0x07, 0x19, 0x0E, 0x10,
            0x85,
            0x01,
            0x01,
            0x00,
            0x00,
            0x45,
            b'h', b'e',
        ];

        let metrics = SystemMetrics::new();
        let res = BundleProcessor::process_bundle(buf, 2000, Some(&metrics));
        assert_eq!(res.status, ProcessStatus::RejectedMalformed);
    }
}