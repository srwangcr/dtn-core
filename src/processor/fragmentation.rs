//! BPv7 fragment reassembly with deterministic, allocation-free state
//! (RFC 9171 sections 5.8 and 5.9).
//!
//! [`crate::processor::fragmentation::ReassemblySlot`] owns two fixed-size arrays: the ADU buffer and a small
//! table of received byte ranges. The range table makes duplicate, partially
//! overlapping and out-of-order fragments explicit instead of silently
//! inflating `bytes_received`, which was the original cause of premature
//! "reassembly complete" decisions.
//!
//! ```
//! use dtn_core::processor::fragmentation::ReassemblySlot;
//! let mut slot = ReassemblySlot::<64>::new();
//! assert!(!slot.insert_fragment(5, 10, b"world").unwrap());
//! assert!(slot.insert_fragment(0, 10, b"hello").unwrap());
//! assert_eq!(&slot.payload_buffer[..10], b"helloworld");
//! ```

use crate::error::{DtnError, DtnResult};

/// Header describing a BPv7 fragment (used by callers of the slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleFragmentHeader {
    /// Whether bundle processing flag bit 0 (fragment) is set.
    pub is_fragment: bool,
    /// Byte offset of this fragment inside the original ADU.
    pub fragment_offset: u64,
    /// Total length of the original ADU.
    pub total_app_data_len: u64,
}

/// Static reassembly slot for one fragmented ADU.
///
/// * `BUF_SIZE` bounds the reconstructed ADU in bytes.
/// * `MAX_FRAGMENTS` bounds the number of disjoint received ranges. It has a
///   conservative default of 16, keeping SRAM usage low on ESP32/RISC-V nodes.
///
/// All arithmetic that combines wire values with buffer indices is checked;
/// on 32-bit targets a `u64` offset from the wire cannot wrap into range.
pub struct ReassemblySlot<const BUF_SIZE: usize, const MAX_FRAGMENTS: usize = 16> {
    /// Reconstructed application data unit.
    pub payload_buffer: [u8; BUF_SIZE],
    /// Number of distinct payload bytes received so far.
    pub bytes_received: usize,
    /// Declared length of the original ADU.
    pub total_expected_len: u64,
    /// Whether this slot currently tracks a bundle.
    pub active: bool,
    ranges: [(usize, usize); MAX_FRAGMENTS],
    range_count: usize,
}

impl<const BUF_SIZE: usize, const MAX_FRAGMENTS: usize> ReassemblySlot<BUF_SIZE, MAX_FRAGMENTS> {
    /// Creates an empty slot (usable in `const` contexts).
    pub const fn new() -> Self {
        Self {
            payload_buffer: [0u8; BUF_SIZE],
            bytes_received: 0,
            total_expected_len: 0,
            active: false,
            ranges: [(0, 0); MAX_FRAGMENTS],
            range_count: 0,
        }
    }

    /// Clears all state so the slot can be reused for another bundle.
    pub fn reset(&mut self) {
        self.bytes_received = 0;
        self.total_expected_len = 0;
        self.active = false;
        self.range_count = 0;
    }

    /// Returns `true` once every byte of the declared ADU has been received.
    pub fn is_complete(&self) -> bool {
        self.active
            && self.bytes_received == usize::try_from(self.total_expected_len).unwrap_or(usize::MAX)
    }

    /// Returns the number of disjoint ranges currently tracked.
    pub fn fragment_count(&self) -> usize {
        self.range_count
    }

    /// Inserts a fragment at `offset` into the ADU of `total_len` bytes.
    ///
    /// Returns `Ok(true)` only when the last missing byte is filled. Exact
    /// duplicates return `Ok(false)` without changing state; partial overlaps
    /// are rejected with [`DtnError::OverlappingFragment`]. Fragments may
    /// arrive in any order.
    pub fn insert_fragment(
        &mut self,
        offset: u64,
        total_len: u64,
        fragment_data: &[u8],
    ) -> DtnResult<bool> {
        let total = usize::try_from(total_len).map_err(|_| DtnError::LengthOverflow)?;
        if self.is_complete() {
            return Err(DtnError::AlreadyComplete);
        }
        if self.active && total_len != self.total_expected_len {
            return Err(DtnError::LengthMismatch);
        }
        if total == 0 || total > BUF_SIZE {
            return Err(DtnError::FragmentOutOfRange);
        }
        let offset = usize::try_from(offset).map_err(|_| DtnError::LengthOverflow)?;
        let end = offset
            .checked_add(fragment_data.len())
            .ok_or(DtnError::LengthOverflow)?;
        if end > total {
            return Err(DtnError::FragmentOutOfRange);
        }
        if !self.active {
            // Only commit state after every check has passed: a rejected
            // fragment must never partially initialise the slot.
            self.total_expected_len = total_len;
            self.active = true;
        }

        if fragment_data.is_empty() {
            return Ok(self.is_complete());
        }

        for &(start, stop) in self.ranges.iter().take(self.range_count) {
            if (offset, end) == (start, stop) {
                return Ok(self.is_complete());
            }
            if offset < stop && start < end {
                return Err(DtnError::OverlappingFragment);
            }
        }
        if self.range_count >= MAX_FRAGMENTS {
            return Err(DtnError::FragmentTableFull);
        }

        self.payload_buffer[offset..end].copy_from_slice(fragment_data);
        self.ranges[self.range_count] = (offset, end);
        self.range_count += 1;
        self.bytes_received = self
            .bytes_received
            .checked_add(fragment_data.len())
            .ok_or(DtnError::LengthOverflow)?;
        Ok(self.is_complete())
    }
}

impl<const BUF_SIZE: usize, const MAX_FRAGMENTS: usize> Default
    for ReassemblySlot<BUF_SIZE, MAX_FRAGMENTS>
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassembles_out_of_order_fragments() {
        let mut slot = ReassemblySlot::<64>::new();
        assert!(!slot.insert_fragment(5, 10, b"world").unwrap());
        assert!(slot.insert_fragment(0, 10, b"hello").unwrap());
        assert!(slot.is_complete());
        assert_eq!(&slot.payload_buffer[..10], b"helloworld");
        assert_eq!(slot.fragment_count(), 2);
    }

    #[test]
    fn duplicate_fragment_is_idempotent() {
        let mut slot = ReassemblySlot::<64>::new();
        assert!(!slot.insert_fragment(0, 8, b"data").unwrap());
        assert!(!slot.insert_fragment(0, 8, b"data").unwrap());
        assert_eq!(slot.bytes_received, 4);
        assert_eq!(slot.fragment_count(), 1);
        assert_eq!(slot.insert_fragment(2, 8, b"ta!!"), Err(DtnError::OverlappingFragment));
        assert_eq!(slot.bytes_received, 4);
    }

    #[test]
    fn partial_overlap_is_rejected_without_state_change() {
        let mut slot = ReassemblySlot::<64>::new();
        slot.insert_fragment(0, 10, b"abcdefgh").unwrap();
        assert_eq!(
            slot.insert_fragment(6, 10, b"ghij"),
            Err(DtnError::OverlappingFragment)
        );
        assert_eq!(slot.bytes_received, 8);
        assert_eq!(slot.fragment_count(), 1);
        assert_eq!(&slot.payload_buffer[..8], b"abcdefgh");
    }

    #[test]
    fn invalid_offsets_lengths_and_caps_are_errors() {
        let mut slot = ReassemblySlot::<8>::new();
        assert_eq!(slot.insert_fragment(0, 9, b"x"), Err(DtnError::FragmentOutOfRange));
        assert_eq!(slot.insert_fragment(7, 8, b"xy"), Err(DtnError::FragmentOutOfRange));
        assert_eq!(slot.insert_fragment(u64::MAX, 8, b"x"), Err(DtnError::LengthOverflow));
        slot.insert_fragment(0, 8, b"abcd").unwrap();
        assert_eq!(slot.insert_fragment(4, 16, b"efgh"), Err(DtnError::LengthMismatch));
    }

    #[test]
    fn full_slot_rejects_extra_fragments_then_reset_reuses_it() {
        let mut slot = ReassemblySlot::<16, 2>::new();
        slot.insert_fragment(0, 6, b"ab").unwrap();
        slot.insert_fragment(2, 6, b"cd").unwrap();
        assert_eq!(slot.insert_fragment(4, 6, b"ef"), Err(DtnError::FragmentTableFull));
        slot.reset();
        assert!(!slot.active);
        assert_eq!(slot.fragment_count(), 0);
        assert!(slot.insert_fragment(0, 6, b"abcdef").unwrap());
    }

    #[test]
    fn completed_slot_cannot_accept_more_fragments() {
        let mut slot = ReassemblySlot::<8>::new();
        assert!(slot.insert_fragment(0, 2, b"ab").unwrap());
        assert_eq!(slot.insert_fragment(2, 2, b"cd"), Err(DtnError::AlreadyComplete));
    }
}
