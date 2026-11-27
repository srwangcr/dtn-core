//! Zero-allocation serial CLA using COBS with a `0x00` frame delimiter.
//!
//! UART/RS-485 links on microcontrollers have no message boundaries. COBS
//! (Consistent Overhead Byte Stuffing, RFC 9171 convergence-layer concern)
//! removes every `0x00` from the payload and appends one `0x00` delimiter, so
//! the receiver can frame packets with a single-byte state machine. No
//! sockets, no async runtime and no heap are required.
//!
//! ```
//! use dtn_core::cla::serial::SerialFraming;
//! let mut link = SerialFraming::<64, 64>::new();
//! let wire = link.encode_frame(b"BP\0v7").unwrap().to_vec(); // Vec only in this doctest
//! let mut decoded = None;
//! for byte in wire {
//!     if let Some(frame) = link.push_byte(byte).unwrap() {
//!         decoded = Some(frame.to_vec());
//!     }
//! }
//! assert_eq!(decoded.unwrap(), b"BP\0v7");
//! ```
//!
//! Note: the doctest uses `Vec` for illustration only; the library itself
//! never allocates.

use crate::error::{DtnError, DtnResult};
use crate::processor::pipeline::{IngestionPipeline, RouteDecision};
use crate::processor::fragmentation::ReassemblySlot;
use crate::routing::interval_tree::CgrIntervalTree;
use crate::storage::outbound::OutboundBundleQueue;
use crate::storage::wal::DirectWal;
use crate::telemetry::metrics::SystemMetrics;

/// Byte used to delimit COBS frames on the wire.
pub const COBS_DELIMITER: u8 = 0;

/// Largest COBS code/group length (a code byte can encode 254 data bytes).
pub const COBS_MAX_GROUP: usize = 254;

/// Computes an upper bound for the encoded length of `decoded_len` bytes.
///
/// The bound includes the trailing delimiter and never overflows silently.
pub fn cobs_max_encoded_len(decoded_len: usize) -> usize {
    // Each 254-byte non-zero run needs one extra code byte; zero bytes replace
    // themselves with a code byte and therefore add no length.
    let groups = decoded_len
        .checked_add(COBS_MAX_GROUP - 1)
        .map(|v| v / COBS_MAX_GROUP)
        .unwrap_or(usize::MAX);
    decoded_len
        .checked_add(groups)
        .and_then(|v| v.checked_add(1))
        .unwrap_or(usize::MAX)
}

/// Encodes `input` as a COBS frame terminated by `0x00`.
///
/// Returns the number of bytes written to `output`, including the delimiter.
/// Fails with [`DtnError::BufferTooSmall`] rather than writing out of bounds.
pub fn cobs_encode(input: &[u8], output: &mut [u8]) -> DtnResult<usize> {
    if output.len() < cobs_max_encoded_len(input.len()) {
        return Err(DtnError::BufferTooSmall);
    }
    if input.is_empty() {
        output[0] = COBS_DELIMITER;
        return Ok(1);
    }

    let mut code_index = 0usize;
    let mut write = 1usize;
    output[code_index] = 1;
    for &byte in input {
        if byte == COBS_DELIMITER {
            output[code_index] = (write - code_index) as u8;
            code_index = write;
            write += 1;
        } else {
            output[write] = byte;
            write += 1;
            if write - code_index == COBS_MAX_GROUP + 1 {
                output[code_index] = 0xff;
                code_index = write;
                write += 1;
            }
        }
    }
    output[code_index] = (write - code_index) as u8;
    output[write] = COBS_DELIMITER;
    Ok(write + 1)
}

/// Decodes one delimiter-terminated COBS frame into `output`.
///
/// `input` must contain exactly one trailing `0x00` and no embedded `0x00`.
/// Returns the decoded payload length.
pub fn cobs_decode(input: &[u8], output: &mut [u8]) -> DtnResult<usize> {
    let frame = match input.split_last() {
        Some((&COBS_DELIMITER, rest)) => rest,
        _ => return Err(DtnError::InvalidFrame),
    };
    cobs_decode_inner(frame, output)
}

/// Decodes a COBS payload that already excludes the trailing delimiter.
fn cobs_decode_inner(frame: &[u8], output: &mut [u8]) -> DtnResult<usize> {
    if frame.contains(&COBS_DELIMITER) {
        return Err(DtnError::InvalidFrame);
    }
    let mut read = 0usize;
    let mut write = 0usize;
    while read < frame.len() {
        let code = frame[read] as usize;
        read += 1;
        if code == 0 {
            return Err(DtnError::InvalidFrame);
        }
        let count = code - 1;
        if count > frame.len() - read {
            return Err(DtnError::Truncated);
        }
        for _ in 0..count {
            if write >= output.len() {
                return Err(DtnError::BufferTooSmall);
            }
            output[write] = frame[read];
            write += 1;
            read += 1;
        }
        if code != 0xff && read < frame.len() {
            if write >= output.len() {
                return Err(DtnError::BufferTooSmall);
            }
            output[write] = COBS_DELIMITER;
            write += 1;
        }
    }
    Ok(write)
}

/// Stateful, zero-allocation UART framer with separate RX/TX buffers.
///
/// `RX` bounds an incoming encoded frame and also bounds the decoded bundle
/// (decoded data is always shorter than encoded data). `TX` bounds an outgoing
/// encoded frame.
pub struct SerialFraming<const RX: usize, const TX: usize> {
    rx_encoded: [u8; RX],
    rx_decoded: [u8; RX],
    rx_len: usize,
    decoded_len: usize,
    tx: [u8; TX],
    tx_len: usize,
}

impl<const RX: usize, const TX: usize> SerialFraming<RX, TX> {
    /// Creates an empty framer. Both capacities must be non-zero.
    pub const fn new() -> Self {
        Self {
            rx_encoded: [0; RX],
            rx_decoded: [0; RX],
            rx_len: 0,
            decoded_len: 0,
            tx: [0; TX],
            tx_len: 0,
        }
    }

    /// Drops any partially received frame.
    pub fn clear_rx(&mut self) {
        self.rx_len = 0;
    }

    /// Encodes a BPv7 bundle into the TX buffer and returns the wire frame.
    pub fn encode_frame(&mut self, bundle: &[u8]) -> DtnResult<&[u8]> {
        self.tx_len = cobs_encode(bundle, &mut self.tx)?;
        Ok(&self.tx[..self.tx_len])
    }

    /// Returns the last encoded frame.
    pub fn encoded_frame(&self) -> &[u8] {
        &self.tx[..self.tx_len]
    }

    /// Feeds one byte from the UART ISR.
    ///
    /// Returns `Some(decoded_bundle)` when the `0x00` delimiter completes a
    /// frame, or `None` while the frame is still in progress.
    pub fn push_byte(&mut self, byte: u8) -> DtnResult<Option<&[u8]>> {
        if byte == COBS_DELIMITER {
            if self.rx_len == 0 {
                return Err(DtnError::InvalidFrame);
            }
            let used = self.rx_len;
            self.rx_len = 0;
            let decoded_len = cobs_decode_inner(&self.rx_encoded[..used], &mut self.rx_decoded)?;
            self.decoded_len = decoded_len;
            return Ok(Some(&self.rx_decoded[..decoded_len]));
        }
        if self.rx_len >= RX {
            self.rx_len = 0;
            return Err(DtnError::BufferTooSmall);
        }
        self.rx_encoded[self.rx_len] = byte;
        self.rx_len += 1;
        Ok(None)
    }

    /// Feeds a contiguous chunk of UART bytes and calls `visit` per frame.
    ///
    /// Returns the number of complete frames decoded.
    pub fn feed<F: FnMut(&[u8])>(&mut self, bytes: &[u8], mut visit: F) -> DtnResult<usize> {
        let mut frames = 0usize;
        for &byte in bytes {
            if let Some(frame) = self.push_byte(byte)? {
                visit(frame);
                frames += 1;
            }
        }
        Ok(frames)
    }

    /// Directly ingests the most recently decoded frame into the DTN pipeline.
    #[allow(clippy::too_many_arguments)]
    pub fn process_to_pipeline<
        const CAP: usize,
        const OUT_SLOTS: usize,
        const MAX_BUNDLE_SIZE: usize,
        const REASSEMBLY_BUF: usize,
        const WAL_PAGES: usize,
    >(
        &self,
        current_ts: u64,
        router: &CgrIntervalTree<CAP>,
        outbound: &mut OutboundBundleQueue<OUT_SLOTS, MAX_BUNDLE_SIZE>,
        reassembly: Option<&mut ReassemblySlot<REASSEMBLY_BUF>>,
        wal: Option<&mut DirectWal<WAL_PAGES>>,
        metrics: Option<&SystemMetrics>,
    ) -> RouteDecision {
        let frame = &self.rx_decoded[..self.decoded_len()];
        IngestionPipeline::ingest_and_route::<
            CAP,
            OUT_SLOTS,
            MAX_BUNDLE_SIZE,
            REASSEMBLY_BUF,
            WAL_PAGES,
        >(
            frame, current_ts, router, outbound, reassembly, wal, metrics,
        )
    }

    /// Length of the most recently decoded frame.
    pub const fn decoded_len(&self) -> usize {
        self.decoded_len
    }

    /// Borrows the most recently decoded frame.
    pub fn decoded_frame(&self) -> &[u8] {
        &self.rx_decoded[..self.decoded_len]
    }
}

impl<const RX: usize, const TX: usize> Default for SerialFraming<RX, TX> {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience alias kept for callers that only need framing helpers.
pub type SerialCla<const RX: usize, const TX: usize> = SerialFraming<RX, TX>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::interval_tree::ContactInterval;
    use crate::storage::outbound::OutboundBundleQueue;

    #[test]
    fn cobs_roundtrip_with_embedded_zeros() {
        let input = [0x01, 0x00, 0x02, 0x00, 0x00, 0x03];
        let mut encoded = [0u8; 32];
        let used = cobs_encode(&input, &mut encoded).unwrap();
        assert_eq!(used, input.len() + 2);
        assert_eq!(encoded[used - 1], COBS_DELIMITER);
        assert!(!encoded[..used - 1].contains(&0));
        let mut decoded = [0u8; 32];
        assert_eq!(cobs_decode(&encoded[..used], &mut decoded), Ok(input.len()));
        assert_eq!(&decoded[..input.len()], &input);
    }

    #[test]
    fn cobs_empty_frame() {
        let mut encoded = [0xff; 4];
        assert_eq!(cobs_encode(&[], &mut encoded), Ok(1));
        assert_eq!(encoded[0], 0);
        let mut decoded = [0u8; 4];
        assert_eq!(cobs_decode(&encoded[..1], &mut decoded), Ok(0));
    }

    #[test]
    fn cobs_large_group_inserts_extra_code_byte() {
        let input = [0x5au8; 600];
        let mut encoded = [0u8; 700];
        let used = cobs_encode(&input, &mut encoded).unwrap();
        assert!(used <= cobs_max_encoded_len(input.len()));
        let mut decoded = [0u8; 600];
        assert_eq!(cobs_decode(&encoded[..used], &mut decoded), Ok(600));
        assert_eq!(&decoded, &input);
    }

    #[test]
    fn cobs_rejects_small_buffers_and_malformed_frames() {
        let mut small = [0u8; 4];
        assert_eq!(cobs_encode(b"123456789", &mut small), Err(DtnError::BufferTooSmall));
        assert_eq!(cobs_decode(&[0x01], &mut small), Err(DtnError::InvalidFrame));
        assert_eq!(cobs_decode(&[0x01, 0x00, 0x02, 0x00], &mut small), Err(DtnError::InvalidFrame));
        assert_eq!(cobs_decode(&[0x05, 0x41, 0x00], &mut small), Err(DtnError::Truncated));
        let mut tiny = [];
        assert_eq!(cobs_decode(&[0x02, 0xaa, 0x00], &mut tiny), Err(DtnError::BufferTooSmall));
    }

    #[test]
    fn serial_framing_emits_one_frame_per_delimiter() {
        let mut link = SerialFraming::<64, 64>::new();
        let mut wire = [0u8; 128];
        let mut wire_len = 0usize;
        for frame in [&b"one"[..], &b"two\0three"[..]] {
            let encoded = link.encode_frame(frame).unwrap();
            wire[wire_len..wire_len + encoded.len()].copy_from_slice(encoded);
            wire_len += encoded.len();
        }

        let mut frames = [[0u8; 16]; 2];
        let mut lens = [0usize; 2];
        let count = link.feed(&wire[..wire_len], |frame| {
            let slot = if lens[0] == 0 { 0 } else { 1 };
            let n = frame.len().min(frames[slot].len());
            frames[slot][..n].copy_from_slice(&frame[..n]);
            lens[slot] = n;
        });
        assert_eq!(count, Ok(2));
        assert_eq!(&frames[0][..lens[0]], b"one");
        assert_eq!(&frames[1][..lens[1]], b"two\0three");
    }

    #[test]
    fn serial_rx_overflow_resets_partial_frame() {
        let mut link = SerialFraming::<2, 8>::new();
        assert_eq!(link.push_byte(0x01), Ok(None));
        assert_eq!(link.push_byte(0x02), Ok(None));
        assert_eq!(link.push_byte(0x03), Err(DtnError::BufferTooSmall));
        assert_eq!(link.push_byte(0x00), Err(DtnError::InvalidFrame));
    }

    #[test]
    fn serial_frame_feeds_pipeline() {
        use crate::processor::state_machine::ProcessStatus;
        let bundle = [
            0x9f, 0x88, 0x07, 0x00, 0x00,
            0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
            0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
            0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58, 0xff,
        ];
        let mut link = SerialFraming::<128, 128>::new();
        let encoded = link.encode_frame(&bundle).unwrap();
        let mut wire = [0u8; 128];
        wire[..encoded.len()].copy_from_slice(encoded);
        for &byte in &wire[..encoded.len()] {
            let _ = link.push_byte(byte).unwrap();
        }
        assert_eq!(
            crate::processor::state_machine::BundleProcessor::process_bundle(
                link.decoded_frame(), 1_000, None
            ).status,
            ProcessStatus::Accepted
        );

        let mut router = CgrIntervalTree::<4>::new();
        assert!(router.insert(ContactInterval::new(0, 10_000, 9)).is_ok());
        let mut outbound = OutboundBundleQueue::<4, 128>::new();
        assert_eq!(
            link.process_to_pipeline::<4, 4, 128, 64, 2>(
                1_000, &router, &mut outbound, None, None, None
            ),
            RouteDecision::ForwardNextHop { node_id: 9, start_time: 0 }
        );
        assert_eq!(outbound.front_parts().unwrap(), (9, &bundle[..]));
    }
}
