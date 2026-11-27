//! Stop-and-Wait ARQ over the COBS serial CLA (no heap, no OS timers).
//!
//! `ReliableSerialCla` adds a three-byte link header to every COBS frame:
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0 | 1 | [`FRAME_TYPE_DATA`] = `0x01`, [`FRAME_TYPE_ACK`] = `0x02`, [`FRAME_TYPE_NACK`] = `0x03` |
//! | 1 | 2 | Sequence identifier, big endian (`u16`, wrapping) |
//! | 3 | N | Payload (a BPv7 bundle or a forwarded ADU) |
//!
//! Only one frame is in flight at a time. Time is supplied by the caller as a
//! monotonically increasing `u64` tick (microseconds on the ESP32 firmware), so
//! the state machine needs neither `std::time` nor an RTOS timer service. A
//! timeout or a NACK triggers retransmission from a caller-owned copy of the
//! payload; after `max_retries` failures the link enters [`ArqState::Failed`]
//! and the upper layer can keep the bundle in the WAL.
//!
//! ```
//! use dtn_core::cla::reliable::{ReliableSerialCla, RxEvent, ArqState};
//!
//! let mut tx = ReliableSerialCla::<128, 128, 32>::new(50, 2);
//! let mut rx = ReliableSerialCla::<128, 128, 32>::new(50, 2);
//! assert!(tx.send(b"bundle", 0).is_ok());
//! let mut event = None;
//! for &byte in tx.encoded_frame() {
//!     event = rx.push_byte(byte, 1).unwrap().or(event);
//! }
//! assert!(matches!(event, Some(RxEvent::Data { .. })));
//! assert_eq!(rx.rx_payload(), b"bundle");
//! // The ACK is already encoded in `rx`; feed it back to the sender.
//! for &byte in rx.encoded_frame() {
//!     let _ = tx.push_byte(byte, 2);
//! }
//! assert_eq!(tx.state(), ArqState::Idle);
//! ```

use crate::cla::serial::SerialFraming;
use crate::error::DtnResult;

/// Link header byte announcing a data frame.
pub const FRAME_TYPE_DATA: u8 = 0x01;
/// Link header byte announcing a positive acknowledgement.
pub const FRAME_TYPE_ACK: u8 = 0x02;
/// Link header byte announcing a negative acknowledgement.
pub const FRAME_TYPE_NACK: u8 = 0x03;
/// Number of bytes in the reliable-link header (`type + seq_id`).
pub const RELIABLE_HEADER_LEN: usize = 3;

/// State of the single in-flight Stop-and-Wait transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArqState {
    /// No frame is awaiting an acknowledgement.
    Idle,
    /// One data frame was sent and its ACK is pending.
    AwaitingAck,
    /// `max_retries` was exhausted; the upper layer retains the bundle.
    Failed,
}

/// Outcome of one fully decoded incoming frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxEvent {
    /// A new in-window data frame was accepted and an ACK was prepared.
    Data {
        /// Sequence identifier of the accepted frame.
        seq: u16,
        /// Number of payload bytes (borrow with [`ReliableSerialCla::rx_payload`]).
        payload_len: usize,
    },
    /// A retransmission of the previously accepted frame; ACK was repeated.
    DuplicateData {
        /// Sequence identifier of the duplicate frame.
        seq: u16,
        /// Number of payload bytes.
        payload_len: usize,
    },
    /// Frame carried a sequence outside the 1-bit window; a NACK was prepared.
    OutOfWindow {
        /// Sequence identifier that arrived.
        seq: u16,
        /// Sequence identifier expected next.
        expected: u16,
    },
    /// Positive acknowledgement for `seq`.
    Ack {
        /// Acknowledged sequence identifier.
        seq: u16,
    },
    /// Negative acknowledgement for `seq`.
    Nack {
        /// Rejected sequence identifier.
        seq: u16,
    },
    /// Truncated header or unknown frame type; nothing was enqueued.
    Invalid,
}

/// Allocation-free Stop-and-Wait ARQ adapter around [`SerialFraming`].
///
/// `RX` / `TX` bound the COBS framer and `PAYLOAD` bounds the retained copy of
/// the in-flight message **including** the 3-byte link header. The type-level
/// assertion guarantees that both COBS buffers are large enough for the
/// largest possible framed message.
pub struct ReliableSerialCla<const RX: usize, const TX: usize, const PAYLOAD: usize> {
    framing: SerialFraming<RX, TX>,
    pending: [u8; PAYLOAD],
    pending_len: usize,
    ack_header: [u8; RELIABLE_HEADER_LEN],
    state: ArqState,
    seq: u16,
    expected_seq: u16,
    last_sent_at: u64,
    timeout_ticks: u64,
    max_retries: u8,
    retries: u8,
    failures: u32,
}

impl<const RX: usize, const TX: usize, const PAYLOAD: usize>
    ReliableSerialCla<RX, TX, PAYLOAD>
{
    const ASSERT_CAPACITIES: () = assert!(
        RX >= PAYLOAD && TX >= PAYLOAD + 1 + PAYLOAD.div_ceil(254),
        "ReliableSerialCla: RX/TX too small for PAYLOAD (header + ADU) and COBS overhead"
    );

    /// Creates a link with the given timeout (in caller ticks) and retry budget.
    pub const fn new(timeout_ticks: u64, max_retries: u8) -> Self {
        let () = Self::ASSERT_CAPACITIES;
        Self {
            framing: SerialFraming::<RX, TX>::new(),
            pending: [0u8; PAYLOAD],
            pending_len: 0,
            ack_header: [0u8; RELIABLE_HEADER_LEN],
            state: ArqState::Idle,
            seq: 0,
            expected_seq: 1, // the first send wraps the sender's seq to 1
            last_sent_at: 0,
            timeout_ticks,
            max_retries,
            retries: 0,
            failures: 0,
        }
    }

    /// Starts a new transaction: copies `payload`, assigns the next sequence
    /// number and prepares the data frame in the TX buffer.
    ///
    /// Returns `DtnError::BufferTooSmall` when the header plus `payload`
    /// exceed `PAYLOAD`, without changing link state.
    pub fn send(&mut self, payload: &[u8], now: u64) -> DtnResult<()> {
        let total = match payload.len().checked_add(RELIABLE_HEADER_LEN) {
            Some(total) if total <= PAYLOAD => total,
            _ => return Err(crate::error::DtnError::BufferTooSmall),
        };
        self.pending[0] = FRAME_TYPE_DATA;
        self.pending[1..RELIABLE_HEADER_LEN].copy_from_slice(&self.seq.wrapping_add(1).to_be_bytes());
        self.pending[RELIABLE_HEADER_LEN..total].copy_from_slice(payload);
        self.pending_len = total;
        self.seq = self.seq.wrapping_add(1);
        self.retries = 0;
        self.last_sent_at = now;
        self.state = ArqState::AwaitingAck;
        self.encode_pending()
    }

    /// Advances the link with the current tick.
    ///
    /// Returns `true` when a retransmission was prepared; the caller then
    /// writes [`Self::encoded_frame`] to the UART. Returns `false` when the
    /// timeout has not elapsed, no frame is in flight, or the retry budget was
    /// exhausted (in which case the state becomes [`ArqState::Failed`]).
    pub fn on_tick(&mut self, now: u64) -> bool {
        if self.state != ArqState::AwaitingAck {
            return false;
        }
        if now.wrapping_sub(self.last_sent_at) < self.timeout_ticks {
            return false;
        }
        if self.retries >= self.max_retries {
            self.state = ArqState::Failed;
            self.failures = self.failures.saturating_add(1);
            return false;
        }
        self.retries += 1;
        self.last_sent_at = now;
        self.encode_pending().is_ok()
    }

    /// Feeds one byte received from the UART.
    ///
    /// Returns an event when the byte completes a COBS frame. Payload bytes
    /// remain borrowed from the framer and can be copied out with
    /// [`Self::rx_payload`] before the next frame arrives.
    pub fn push_byte(&mut self, byte: u8, now: u64) -> DtnResult<Option<RxEvent>> {
        let parsed = match self.framing.push_byte(byte)? {
            Some(frame) => Self::parse_frame(frame),
            None => return Ok(None),
        };
        Ok(Some(self.dispatch(parsed, now)?))
    }

    /// Prepares an immediate retransmission after a NACK.
    ///
    /// Returns `true` when a frame is ready in the TX buffer.
    pub fn retransmit(&mut self, now: u64) -> bool {
        if self.state != ArqState::AwaitingAck || self.retries >= self.max_retries {
            return false;
        }
        self.retries += 1;
        self.last_sent_at = now;
        self.encode_pending().is_ok()
    }

    /// Resets the link (sequence numbers start again at zero).
    pub fn reset(&mut self) {
        self.state = ArqState::Idle;
        self.pending_len = 0;
        self.seq = 0;
        self.expected_seq = 1;
        self.retries = 0;
        self.failures = 0;
        self.last_sent_at = 0;
        self.framing.clear_rx();
    }

    /// Current ARQ state.
    pub const fn state(&self) -> ArqState {
        self.state
    }

    /// Number of retransmissions used by the current transaction.
    pub const fn retries(&self) -> u8 {
        self.retries
    }

    /// Number of transactions that exhausted their retry budget.
    pub const fn failures(&self) -> u32 {
        self.failures
    }

    /// Sequence identifier of the current / most recent transaction.
    pub const fn seq_id(&self) -> u16 {
        self.seq
    }

    /// Sequence identifier expected from the peer.
    pub const fn expected_seq_id(&self) -> u16 {
        self.expected_seq
    }

    /// Configured timeout in caller ticks.
    pub const fn timeout_ticks(&self) -> u64 {
        self.timeout_ticks
    }

    /// Configured maximum number of retransmissions.
    pub const fn max_retries(&self) -> u8 {
        self.max_retries
    }

    /// Replaces the timeout (in caller ticks) and retry budget.
    pub fn configure(&mut self, timeout_ticks: u64, max_retries: u8) {
        self.timeout_ticks = timeout_ticks;
        self.max_retries = max_retries;
    }

    /// Borrows the frame currently prepared for transmission.
    pub fn encoded_frame(&self) -> &[u8] {
        self.framing.encoded_frame()
    }

    /// Borrows the payload of the most recently accepted data frame.
    ///
    /// An empty slice is returned when the last frame had no payload or was
    /// not a data frame. The borrow is valid until the next call to
    /// [`Self::push_byte`] or [`Self::send`].
    pub fn rx_payload(&self) -> &[u8] {
        self.framing
            .decoded_frame()
            .get(RELIABLE_HEADER_LEN..)
            .unwrap_or(&[])
    }

    /// Number of bytes retained for retransmission (header + payload).
    pub const fn pending_len(&self) -> usize {
        self.pending_len
    }

    fn parse_frame(frame: &[u8]) -> Option<(u8, u16, usize)> {
        if frame.len() < RELIABLE_HEADER_LEN {
            return None;
        }
        let seq = u16::from_be_bytes([frame[1], frame[2]]);
        Some((frame[0], seq, frame.len() - RELIABLE_HEADER_LEN))
    }

    fn dispatch(&mut self, parsed: Option<(u8, u16, usize)>, now: u64) -> DtnResult<RxEvent> {
        let Some((frame_type, seq, payload_len)) = parsed else {
            return Ok(RxEvent::Invalid);
        };
        match frame_type {
            FRAME_TYPE_DATA => {
                if seq == self.expected_seq {
                    self.expected_seq = self.expected_seq.wrapping_add(1);
                    self.encode_ack(FRAME_TYPE_ACK, seq)?;
                    Ok(RxEvent::Data { seq, payload_len })
                } else if seq == self.expected_seq.wrapping_sub(1) {
                    // Duplicate of the previously accepted frame: re-ACK but
                    // never deliver the same ADU twice to the pipeline.
                    self.encode_ack(FRAME_TYPE_ACK, seq)?;
                    Ok(RxEvent::DuplicateData { seq, payload_len })
                } else {
                    // NACK names the rejected frame, so the sender can match
                    // it against its single in-flight transaction.
                    self.encode_ack(FRAME_TYPE_NACK, seq)?;
                    Ok(RxEvent::OutOfWindow { seq, expected: self.expected_seq })
                }
            }
            FRAME_TYPE_ACK => {
                if self.state == ArqState::AwaitingAck && seq == self.seq {
                    self.state = ArqState::Idle;
                    self.pending_len = 0;
                }
                Ok(RxEvent::Ack { seq })
            }
            FRAME_TYPE_NACK => {
                if self.state == ArqState::AwaitingAck
                    && seq == self.seq
                    && !self.retransmit(now)
                {
                    self.state = ArqState::Failed;
                }
                Ok(RxEvent::Nack { seq })
            }
            _ => Ok(RxEvent::Invalid),
        }
    }

    fn encode_pending(&mut self) -> DtnResult<()> {
        self.framing.encode_frame(&self.pending[..self.pending_len])?;
        Ok(())
    }

    fn encode_ack(&mut self, frame_type: u8, seq: u16) -> DtnResult<()> {
        self.ack_header[0] = frame_type;
        self.ack_header[1..RELIABLE_HEADER_LEN].copy_from_slice(&seq.to_be_bytes());
        self.framing.encode_frame(&self.ack_header)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Link = ReliableSerialCla<256, 256, 64>;

    /// Copies the prepared frame out of `src` and feeds it to `dst`.
    fn deliver(src: &Link, dst: &mut Link, now: u64) -> Option<RxEvent> {
        let mut wire = [0u8; 256];
        let len = src.encoded_frame().len();
        wire[..len].copy_from_slice(src.encoded_frame());
        let mut event = None;
        for &byte in &wire[..len] {
            if let Some(e) = dst.push_byte(byte, now).unwrap() {
                event = Some(e);
            }
        }
        event
    }

    #[test]
    fn stop_and_wait_completes_with_ack() {
        let mut tx = Link::new(50, 3);
        let mut rx = Link::new(50, 3);
        assert!(tx.send(b"bundle", 0).is_ok());
        assert_eq!(tx.state(), ArqState::AwaitingAck);
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::Data { seq: 1, payload_len: 6 }));
        assert_eq!(rx.rx_payload(), b"bundle");
        assert_eq!(deliver(&rx, &mut tx, 2), Some(RxEvent::Ack { seq: 1 }));
        assert_eq!(tx.state(), ArqState::Idle);
        assert_eq!(tx.retries(), 0);
    }

    #[test]
    fn lost_frame_is_retransmitted_after_timeout() {
        let mut tx = Link::new(50, 3);
        let mut rx = Link::new(50, 3);
        tx.send(b"payload", 100).unwrap();
        assert!(!tx.on_tick(149)); // before timeout
        assert!(tx.on_tick(150)); // exactly at timeout -> retransmit
        assert_eq!(tx.retries(), 1);
        assert_eq!(tx.state(), ArqState::AwaitingAck);
        assert_eq!(deliver(&tx, &mut rx, 151), Some(RxEvent::Data { seq: 1, payload_len: 7 }));
        assert_eq!(rx.rx_payload(), b"payload");
    }

    #[test]
    fn corrupted_frame_is_ignored_and_clean_retransmission_succeeds() {
        let mut tx = Link::new(10, 2);
        let mut rx = Link::new(10, 2);
        tx.send(b"integrity", 0).unwrap();
        let mut corrupt = [0u8; 256];
        let len = tx.encoded_frame().len();
        corrupt[..len].copy_from_slice(tx.encoded_frame());
        corrupt[2] ^= 0xff; // damage the encoded header / first payload byte
        let mut corrupt_event = None;
        let mut corrupt_err = false;
        for &byte in &corrupt[..len] {
            match rx.push_byte(byte, 1) {
                Ok(Some(event)) => corrupt_event = Some(event),
                Ok(None) => {}
                Err(_) => corrupt_err = true,
            }
        }
        assert!(!matches!(corrupt_event, Some(RxEvent::Data { .. })));
        let _ = corrupt_err; // COBS may reject the damaged frame outright
        assert!(tx.on_tick(10));
        assert_eq!(deliver(&tx, &mut rx, 11), Some(RxEvent::Data { seq: 1, payload_len: 9 }));
        assert_eq!(rx.rx_payload(), b"integrity");
    }

    #[test]
    fn retry_budget_exhaustion_marks_link_failed_and_keeps_payload() {
        let mut tx = Link::new(10, 2);
        tx.send(b"keep-me", 0).unwrap();
        assert!(tx.on_tick(10));
        assert!(tx.on_tick(20));
        assert!(!tx.on_tick(30));
        assert_eq!(tx.state(), ArqState::Failed);
        assert_eq!(tx.failures(), 1);
        assert_eq!(tx.pending_len(), 10); // 3-byte header + 7-byte payload, retained for WAL
        assert!(!tx.on_tick(40));
    }

    #[test]
    fn nack_triggers_immediate_retransmission() {
        let mut tx = Link::new(1_000, 2);
        let mut rx = Link::new(1_000, 2);
        rx.expected_seq = 7; // force the sender's seq=1 to be out of window
        tx.send(b"nack-me", 0).unwrap();
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::OutOfWindow { seq: 1, expected: 7 }));
        assert_eq!(deliver(&rx, &mut tx, 2), Some(RxEvent::Nack { seq: 1 }));
        assert_eq!(tx.retries(), 1);
        assert_eq!(tx.retries(), 1);
        assert_eq!(tx.state(), ArqState::AwaitingAck);
        // The first wire byte is the COBS code, not the link frame type; check
        // the retained message instead (3-byte header + 7-byte payload).
        assert_eq!(tx.pending_len(), 10);
    }

    #[test]
    fn duplicate_data_is_acked_without_second_delivery() {
        let mut tx = Link::new(10, 2);
        let mut rx = Link::new(10, 2);
        tx.send(b"once", 0).unwrap();
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::Data { seq: 1, payload_len: 4 }));
        assert_eq!(
            deliver(&tx, &mut rx, 2),
            Some(RxEvent::DuplicateData { seq: 1, payload_len: 4 })
        );
        assert_eq!(rx.expected_seq_id(), 2);
        assert_eq!(rx.encoded_frame().len(), 5); // COBS([ACK, 0x00, seq]) + delimiter
    }

    #[test]
    fn oversized_payload_and_short_frame_are_rejected() {
        let mut link = Link::new(10, 1);
        assert!(link.send(&[0u8; 65], 0).is_err());
        assert_eq!(link.state(), ArqState::Idle);
        assert_eq!(link.push_byte(0x02, 0).unwrap(), None); // COBS code for 1 byte
        assert_eq!(link.push_byte(0x11, 0).unwrap(), None); // 1-byte frame body
        // Delimiter closes a 1-byte frame: shorter than the 3-byte link header.
        assert_eq!(link.push_byte(0x00, 0).unwrap(), Some(RxEvent::Invalid));
    }
}
