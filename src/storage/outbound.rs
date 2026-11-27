//! Fixed-capacity outbound queue for **complete BPv7 bundles** (no heap).
//!
//! Every slot logically holds the frame
//!
//! ```text
//! [ next_hop: u64 LE | len: u16 LE | raw_bundle_bytes ]
//! ```
//!
//! The header and bundle bytes are stored in parallel const-sized arrays so
//! that no const-generic arithmetic is needed. [`OutboundBundleQueue::copy_front`]
//! materialises the exact byte-frame above, and [`OutboundBundleQueue::front_parts`]
//! borrows `(next_hop, bundle)` without copying.
//!
//! [`crate::processor::pipeline::IngestionPipeline`] enqueues the whole bundle
//! together with the selected next hop, instead of the old 8-byte node ID. The
//! consumer (typically a firmware main loop) borrows the front frame and
//! releases the slot with [`OutboundBundleQueue::pop`] only after the CLA has
//! copied the bytes. No `alloc`, atomics or dynamic dispatch are used; in the
//! firmware both ends run in the same super-loop.

use crate::error::{DtnError, DtnResult};

/// Number of bytes in the `[next_hop | len]` frame header.
pub const OUTBOUND_HEADER_LEN: usize = 10;

/// Parses a contiguous frame and returns `(next_hop, bundle)`.
pub fn parse_frame(frame: &[u8]) -> DtnResult<(u64, &[u8])> {
    let (hop, len, bundle) = parse_header_and_bundle(frame)?;
    let _ = (hop, len);
    Ok((hop, bundle))
}

fn parse_header_and_bundle(frame: &[u8]) -> DtnResult<(u64, usize, &[u8])> {
    let header = frame.get(..OUTBOUND_HEADER_LEN).ok_or(DtnError::Truncated)?;
    let mut hop = [0u8; 8];
    hop.copy_from_slice(&header[..8]);
    let mut len = [0u8; 2];
    len.copy_from_slice(&header[8..10]);
    let len = usize::from(u16::from_le_bytes(len));
    let bundle = frame.get(OUTBOUND_HEADER_LEN..OUTBOUND_HEADER_LEN + len)
        .ok_or(DtnError::Truncated)?;
    Ok((u64::from_le_bytes(hop), len, bundle))
}

/// Allocation-free FIFO of framed BPv7 bundles awaiting transmission.
#[derive(Debug)]
pub struct OutboundBundleQueue<const SLOTS: usize, const MAX_BUNDLE_SIZE: usize> {
    headers: [[u8; OUTBOUND_HEADER_LEN]; SLOTS],
    bundles: [[u8; MAX_BUNDLE_SIZE]; SLOTS],
    lens: [usize; SLOTS],
    head: usize,
    len: usize,
}

impl<const SLOTS: usize, const MAX_BUNDLE_SIZE: usize>
    OutboundBundleQueue<SLOTS, MAX_BUNDLE_SIZE>
{
    const ASSERT_CAPACITIES: () = assert!(
        SLOTS > 0 && MAX_BUNDLE_SIZE > 0 && MAX_BUNDLE_SIZE <= u16::MAX as usize,
        "OutboundBundleQueue requires SLOTS > 0 and 0 < MAX_BUNDLE_SIZE <= u16::MAX"
    );

    /// Creates an empty queue using only caller-owned static storage.
    pub const fn new() -> Self {
        let () = Self::ASSERT_CAPACITIES;
        Self {
            headers: [[0; OUTBOUND_HEADER_LEN]; SLOTS],
            bundles: [[0; MAX_BUNDLE_SIZE]; SLOTS],
            lens: [0; SLOTS],
            head: 0,
            len: 0,
        }
    }

    /// Maximum number of queued bundles.
    pub const fn capacity(&self) -> usize {
        SLOTS
    }

    /// Number of bundles currently queued.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` when no bundle is queued.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns `true` when the queue cannot accept another bundle.
    pub const fn is_full(&self) -> bool {
        self.len == SLOTS
    }

    /// Copies `bundle` into the next free slot together with `next_hop`.
    ///
    /// Returns [`DtnError::QueueFull`] when all slots are occupied and
    /// [`DtnError::BufferTooSmall`] when the bundle exceeds the compile-time
    /// frame capacity. Neither failure changes queue state.
    pub fn enqueue(&mut self, next_hop: u64, bundle: &[u8]) -> DtnResult<()> {
        if self.len == SLOTS {
            return Err(DtnError::QueueFull);
        }
        if bundle.len() > MAX_BUNDLE_SIZE {
            return Err(DtnError::BufferTooSmall);
        }
        let slot = (self.head + self.len) % SLOTS;
        let header = &mut self.headers[slot];
        header[..8].copy_from_slice(&next_hop.to_le_bytes());
        let len = u16::try_from(bundle.len()).map_err(|_| DtnError::LengthOverflow)?;
        header[8..10].copy_from_slice(&len.to_le_bytes());
        self.bundles[slot][..bundle.len()].copy_from_slice(bundle);
        self.lens[slot] = bundle.len();
        self.len += 1;
        Ok(())
    }

    /// Borrows `(next_hop, bundle)` at the head of the queue.
    ///
    /// The bundle borrow stays valid until [`Self::pop`] is called.
    pub fn front_parts(&self) -> Option<(u64, &[u8])> {
        if self.len == 0 {
            return None;
        }
        let slot = self.head;
        let mut hop = [0u8; 8];
        hop.copy_from_slice(&self.headers[slot][..8]);
        let bundle_len = self.lens[slot];
        Some((u64::from_le_bytes(hop), &self.bundles[slot][..bundle_len]))
    }

    /// Writes the exact `[next_hop | len | bundle]` frame at the head into
    /// `out` and returns the number of bytes written.
    pub fn copy_front(&self, out: &mut [u8]) -> DtnResult<usize> {
        let (next_hop, bundle) = self.front_parts().ok_or(DtnError::BufferEmpty)?;
        let total = OUTBOUND_HEADER_LEN.checked_add(bundle.len())
            .ok_or(DtnError::LengthOverflow)?;
        if out.len() < total {
            return Err(DtnError::BufferTooSmall);
        }
        out[..8].copy_from_slice(&next_hop.to_le_bytes());
        let len = u16::try_from(bundle.len()).map_err(|_| DtnError::LengthOverflow)?;
        out[8..10].copy_from_slice(&len.to_le_bytes());
        out[OUTBOUND_HEADER_LEN..total].copy_from_slice(bundle);
        Ok(total)
    }

    /// Releases the head slot after it has been transmitted.
    pub fn pop(&mut self) {
        if self.len == 0 {
            return;
        }
        self.head = (self.head + 1) % SLOTS;
        self.len -= 1;
    }

    /// Convenience: parses and borrows the head as `(next_hop, bundle)`.
    pub fn front_parsed(&self) -> Option<DtnResult<(u64, &[u8])>> {
        self.front_parts().map(Ok)
    }
}

impl<const SLOTS: usize, const MAX_BUNDLE_SIZE: usize> Default
    for OutboundBundleQueue<SLOTS, MAX_BUNDLE_SIZE>
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Queue = OutboundBundleQueue<4, 32>;

    #[test]
    fn round_trip_preserves_next_hop_and_full_bundle() {
        let mut q = Queue::new();
        let bundle = [0x9fu8, 0x88, 0x07, 0x00, 0xff];
        assert!(q.is_empty());
        assert_eq!(q.enqueue(0x0102_0304_0506_0708, &bundle), Ok(()));
        assert_eq!(q.len(), 1);
        assert_eq!(q.front_parts(), Some((0x0102_0304_0506_0708, &bundle[..])));
        let mut frame = [0u8; OUTBOUND_HEADER_LEN + 32];
        let frame_len = q.copy_front(&mut frame).unwrap();
        assert_eq!(frame_len, OUTBOUND_HEADER_LEN + bundle.len());
        assert_eq!(parse_frame(&frame[..frame_len]), Ok((0x0102_0304_0506_0708, &bundle[..])));
        q.pop();
        assert!(q.is_empty());
    }

    #[test]
    fn fifo_order_and_wrap_around() {
        let mut q = Queue::new();
        for i in 0..4u64 {
            assert_eq!(q.enqueue(i, &[i as u8, 0xaa]), Ok(()));
        }
        assert!(q.is_full());
        assert_eq!(q.enqueue(99, b"x"), Err(DtnError::QueueFull));
        for i in 0..4u64 {
            let (hop, payload) = q.front_parts().unwrap();
            assert_eq!(hop, i);
            assert_eq!(payload, &[i as u8, 0xaa]);
            q.pop();
        }
        assert_eq!(q.enqueue(7, b"again"), Ok(()));
        assert_eq!(q.front_parts().unwrap().0, 7);
    }

    #[test]
    fn oversized_bundle_is_rejected_without_changing_state() {
        let mut q = Queue::new();
        assert_eq!(q.enqueue(1, &[0u8; 33]), Err(DtnError::BufferTooSmall));
        assert!(q.is_empty());
        assert_eq!(q.enqueue(1, b"ok"), Ok(()));
        assert_eq!(q.front_parts().unwrap().1, b"ok");
    }

    #[test]
    fn malformed_frame_is_rejected() {
        assert_eq!(parse_frame(&[0u8; 9]), Err(DtnError::Truncated));
        let mut frame = [0u8; 11];
        frame[8..10].copy_from_slice(&4u16.to_le_bytes());
        assert_eq!(parse_frame(&frame), Err(DtnError::Truncated));
    }
}
