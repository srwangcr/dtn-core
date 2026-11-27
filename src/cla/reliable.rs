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
//! **ACKs are never automatic.** A `Data` frame only creates a
//! [`PendingCommit`]; the upper layer must call [`ReliableSerialCla::commit_rx_ack`]
//! after it has accepted the bundle *and* verified its Flash/WAL write
//! (`erase -> write -> read-back -> CRC32C`). If BPSec or persistence fails it
//! calls [`ReliableSerialCla::reject_rx_nack`] instead. Only out-of-window
//! frames are NACKed immediately, because they were never accepted.
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
//! // No ACK exists yet: persistence must be verified first.
//! assert!(rx.encoded_frame().is_empty());
//! // Upper layer verified the write; now the ACK is prepared.
//! assert!(rx.pending_commit().is_some());
//! rx.commit_rx_ack().unwrap();
//! for &byte in rx.encoded_frame() {
//!     let _ = tx.push_byte(byte, 2);
//! }
//! assert_eq!(tx.state(), ArqState::Idle);
//! ```

use crate::cla::serial::SerialFraming;
use crate::error::{DtnError, DtnResult};

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

/// A data frame has been decoded but **no ACK has been sent yet**.
///
/// The upper layer must verify BPSec and persisted Flash contents, then call
/// [`ReliableSerialCla::commit_rx_ack`] (or [`ReliableSerialCla::reject_rx_nack`]
/// on failure). The borrow returned by [`ReliableSerialCla::rx_payload`] is only
/// valid until the next `push_byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCommit {
    seq: u16,
    payload_len: usize,
}

impl PendingCommit {
    /// Sequence identifier that will be acknowledged or rejected.
    pub const fn seq(&self) -> u16 {
        self.seq
    }

    /// Number of bundle bytes delivered with this data frame.
    pub const fn payload_len(&self) -> usize {
        self.payload_len
    }
}

/// Outcome of one fully decoded incoming frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxEvent {
    /// A new in-window data frame was decoded. An ACK is **not** prepared yet.
    Data {
        /// Sequence identifier of the frame.
        seq: u16,
        /// Number of payload bytes (borrow with [`ReliableSerialCla::rx_payload`]).
        payload_len: usize,
    },
    /// A retransmission of a frame that is already awaiting commit.
    DuplicateData {
        /// Sequence identifier of the duplicate frame.
        seq: u16,
        /// Number of payload bytes.
        payload_len: usize,
    },
    /// Frame carried a sequence outside the 1-bit window; a NACK was prepared
    /// because the frame was never accepted.
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
    rx_pending: Option<PendingCommit>,
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
            rx_pending: None,
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
    /// Returns [`DtnError::BufferTooSmall`] when the header plus `payload`
    /// exceed `PAYLOAD`, and [`DtnError::QueueFull`] when the single in-flight
    /// slot is occupied (`AwaitingAck`) or the link has failed. Neither failure
    /// changes link state.
    pub fn send(&mut self, payload: &[u8], now: u64) -> DtnResult<()> {
        // Only one transaction may be in flight. A failed link keeps its
        // retained payload for WAL recovery and also refuses new sends.
        if self.state != ArqState::Idle {
            return Err(DtnError::QueueFull);
        }
        let total = match payload.len().checked_add(RELIABLE_HEADER_LEN) {
            Some(total) if total <= PAYLOAD => total,
            _ => return Err(DtnError::BufferTooSmall),
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

    /// Resets only the transport after a failed transaction.
    ///
    /// Clears the in-flight TX slot, the retry budget and any partially
    /// received COBS frame, but **preserves `seq` and `expected_seq`**. This is
    /// mandatory when the peer already committed the ACK of `seq` and that ACK
    /// was lost: restarting at `seq = 1` would produce an endless
    /// `DuplicateData` / NACK loop against a receiver expecting `seq = 2`.
    /// Telemetry (`failures`) and the receive window are also preserved.
    ///
    /// Use this after the upper layer's backoff; use [`Self::reset_session`]
    /// only for a cold start with a fresh peer session.
    pub fn reset_transport(&mut self) {
        self.state = ArqState::Idle;
        self.pending_len = 0;
        self.retries = 0;
        self.last_sent_at = 0;
        self.framing.clear_rx();
    }

    /// Resets the whole session: sequence numbers start again at zero.
    ///
    /// This is a **cold-start** operation. On an established session prefer
    /// [`Self::reset_transport`], which cannot desynchronise the receive
    /// window of a peer that already committed an ACK.
    pub fn reset_session(&mut self) {
        self.state = ArqState::Idle;
        self.pending_len = 0;
        self.seq = 0;
        self.expected_seq = 1;
        self.rx_pending = None;
        self.retries = 0;
        self.failures = 0;
        self.last_sent_at = 0;
        self.framing.clear_rx();
    }

    /// Backwards-compatible alias for [`Self::reset_session`] (cold start only).
    pub fn reset(&mut self) {
        self.reset_session();
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

    /// Returns the data frame awaiting an explicit ACK/NACK decision.
    pub const fn pending_commit(&self) -> Option<PendingCommit> {
        self.rx_pending
    }

    /// Commits the received data frame and prepares a positive ACK.
    ///
    /// The caller must invoke this **only** after the bundle was accepted by
    /// the ingestion pipeline and the Flash/WAL page was verified by
    /// `erase -> write -> read_back -> CRC32C`. The receive window advances
    /// here, not when the frame was decoded. A retransmission of an already
    /// committed sequence is re-ACKed without advancing the window again.
    ///
    /// Returns [`DtnError::NoPendingCommit`] when no data frame awaits commit.
    pub fn commit_rx_ack(&mut self) -> DtnResult<()> {
        let pending = self.rx_pending.take().ok_or(DtnError::NoPendingCommit)?;
        // A retransmission re-commits the same sequence; only a genuinely new
        // frame advances the receive window.
        if pending.seq == self.expected_seq {
            self.expected_seq = self.expected_seq.wrapping_add(1);
        }
        self.encode_ack(FRAME_TYPE_ACK, pending.seq)
    }

    /// Rejects the received data frame and prepares a negative ACK.
    ///
    /// Call this when BPSec verification or Flash persistence failed. The
    /// receive window is **not** advanced, so a retransmission of the same
    /// sequence can be accepted after the fault is cleared.
    pub fn reject_rx_nack(&mut self) -> DtnResult<()> {
        let pending = self.rx_pending.take().ok_or(DtnError::NoPendingCommit)?;
        self.encode_ack(FRAME_TYPE_NACK, pending.seq)
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
                // A frame is deliverable only when it is the next expected
                // sequence or a retransmission awaiting / already holding a
                // commit. In both cases the ACK is deferred: the upper layer
                // must verify persistence before `commit_rx_ack()`.
                let is_new = self.rx_pending.is_none() && seq == self.expected_seq;
                let is_duplicate = match self.rx_pending {
                    Some(pending) => pending.seq == seq,
                    None => seq == self.expected_seq.wrapping_sub(1),
                };
                if is_new || is_duplicate {
                    self.rx_pending = Some(PendingCommit { seq, payload_len });
                    if is_new {
                        Ok(RxEvent::Data { seq, payload_len })
                    } else {
                        Ok(RxEvent::DuplicateData { seq, payload_len })
                    }
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
    fn stop_and_wait_completes_only_after_explicit_commit() {
        let mut tx = Link::new(50, 3);
        let mut rx = Link::new(50, 3);
        assert!(tx.send(b"bundle", 0).is_ok());
        assert_eq!(tx.state(), ArqState::AwaitingAck);
        // A second send must be refused while the single slot is in flight.
        assert_eq!(tx.send(b"other", 0), Err(DtnError::QueueFull));
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::Data { seq: 1, payload_len: 6 }));
        assert_eq!(rx.rx_payload(), b"bundle");
        // Decoding alone never produces an ACK.
        assert!(rx.encoded_frame().is_empty());
        let pending = rx.pending_commit().unwrap();
        assert_eq!((pending.seq(), pending.payload_len()), (1, 6));
        assert_eq!(rx.expected_seq_id(), 1);
        rx.commit_rx_ack().unwrap();
        assert!(rx.pending_commit().is_none());
        assert_eq!(rx.expected_seq_id(), 2);
        assert_eq!(deliver(&rx, &mut tx, 2), Some(RxEvent::Ack { seq: 1 }));
        assert_eq!(tx.state(), ArqState::Idle);
        assert_eq!(tx.retries(), 0);
        assert_eq!(tx.send(b"other", 3), Ok(()));
    }

    #[test]
    fn reject_keeps_receive_window_for_retransmission() {
        let mut tx = Link::new(50, 3);
        let mut rx = Link::new(50, 3);
        tx.send(b"bundle", 0).unwrap();
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::Data { seq: 1, payload_len: 6 }));
        assert!(rx.encoded_frame().is_empty());
        rx.reject_rx_nack().unwrap();
        assert_eq!(rx.expected_seq_id(), 1);
        assert_eq!(deliver(&tx, &mut rx, 2), Some(RxEvent::Data { seq: 1, payload_len: 6 }));
        rx.commit_rx_ack().unwrap();
        assert_eq!(rx.expected_seq_id(), 2);
        assert_eq!(rx.commit_rx_ack(), Err(DtnError::NoPendingCommit));
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
        assert_eq!(tx.send(b"new", 50), Err(DtnError::QueueFull));
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
    fn duplicate_data_is_reacked_only_after_commit() {
        let mut tx = Link::new(10, 2);
        let mut rx = Link::new(10, 2);
        tx.send(b"once", 0).unwrap();
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::Data { seq: 1, payload_len: 4 }));
        rx.commit_rx_ack().unwrap();
        assert_eq!(rx.expected_seq_id(), 2);
        let mut previous = [0u8; 256];
        let previous_len = rx.encoded_frame().len();
        previous[..previous_len].copy_from_slice(rx.encoded_frame());
        assert_eq!(
            deliver(&tx, &mut rx, 2),
            Some(RxEvent::DuplicateData { seq: 1, payload_len: 4 })
        );
        // Even a duplicate creates a pending commit: no *new* ACK is encoded
        // before the upper layer re-verifies the persisted record.
        assert_eq!(rx.encoded_frame().len(), previous_len);
        assert_eq!(&rx.encoded_frame()[..previous_len], &previous[..previous_len]);
        assert_eq!(rx.pending_commit().unwrap().seq(), 1);
        assert_eq!(rx.expected_seq_id(), 2);
        rx.commit_rx_ack().unwrap();
        assert_eq!(rx.expected_seq_id(), 2); // a retransmission must not skip seq=2
        assert_eq!(rx.encoded_frame().len(), 5); // COBS([ACK, 0x00, seq]) + delimiter
    }

    #[test]
    fn lost_ack_after_commit_recovers_with_reset_transport() {
        let mut tx = Link::new(10, 1);
        let mut rx = Link::new(10, 1);

        // First transaction: the receiver commits seq=1 and prepares its ACK.
        tx.send(b"first", 0).unwrap();
        assert_eq!(tx.seq_id(), 1);
        assert_eq!(deliver(&tx, &mut rx, 1), Some(RxEvent::Data { seq: 1, payload_len: 5 }));
        rx.commit_rx_ack().unwrap();
        assert_eq!(rx.expected_seq_id(), 2);

        // The ACK is lost in the air. The sender exhausts its retry budget and
        // enters Failed while the receiver has already advanced to seq=2.
        assert!(tx.on_tick(10)); // retransmission 1
        assert!(!tx.on_tick(20)); // budget exhausted -> Failed
        assert_eq!(tx.state(), ArqState::Failed);
        assert_eq!(tx.seq_id(), 1);

        // A transport-only reset keeps both windows in sync; a full reset()
        // would resend seq=1 and be deduplicated forever by the receiver.
        tx.reset_transport();
        assert_eq!(tx.state(), ArqState::Idle);
        assert_eq!(tx.seq_id(), 1);
        assert_eq!(tx.expected_seq_id(), 1); // sender's own receive window is unchanged

        assert!(tx.send(b"second", 30).is_ok());
        assert_eq!(tx.seq_id(), 2);
        assert_eq!(
            deliver(&tx, &mut rx, 31),
            Some(RxEvent::Data { seq: 2, payload_len: 6 })
        );
        assert_eq!(rx.rx_payload(), b"second"); // accepted as NEW, not a duplicate
        rx.commit_rx_ack().unwrap();
        assert_eq!(rx.expected_seq_id(), 3);
        assert_eq!(deliver(&rx, &mut tx, 32), Some(RxEvent::Ack { seq: 2 }));
        assert_eq!(tx.state(), ArqState::Idle);
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
