//! Single-producer/single-consumer lock-free byte ring buffer.
//!
//! The head/tail atomics are padded to separate 64-byte cache lines to avoid
//! false sharing. Memory ordering is correct on weakly ordered architectures
//! (ARM, RISC-V, ESP32), not only on x86:
//!
//! * the producer loads `tail` with **Acquire** (observing the consumer's
//!   release) before overwriting cells, and publishes `head` with **Release**;
//! * the consumer loads `head` with **Acquire** before reading cells, and
//!   publishes `tail` with **Release**;
//! * own-side cursors are loaded with **Relaxed** because only one thread
//!   writes them.
//!
//! `SIZE` must be a power of two, enforced by a compile-time assertion.
//!
//! ```
//! use dtn_core::storage::ring_buffer::LockFreeRingBuffer;
//! let mut rb = LockFreeRingBuffer::<16>::new();
//! let (mut producer, mut consumer) = rb.split();
//! producer.push(b"abc").unwrap();
//! let mut out = [0u8; 3];
//! assert_eq!(consumer.pop(&mut out), 3);
//! assert_eq!(&out, b"abc");
//! ```

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Atomic counter isolated in its own cache line.
#[repr(align(64))]
struct CachePaddedAtomic(AtomicUsize);

/// Fixed-capacity SPSC ring buffer with $O(1)$ push/pop.
///
/// The type is `Sync` because [`Self::split`] hands out exclusive [`Producer`]
/// and [`Consumer`] handles; no two threads can mutate the same cursor.
#[repr(align(64))]
pub struct LockFreeRingBuffer<const SIZE: usize> {
    buffer: UnsafeCell<[u8; SIZE]>,
    head: CachePaddedAtomic,
    tail: CachePaddedAtomic,
}

// SAFETY: the SPSC contract is enforced by the `Producer`/`Consumer` handles
// returned from `split`: exactly one thread owns each cursor. Data visible
// through `UnsafeCell` is published only after a Release store and read only
// after the matching Acquire load, so no concurrent aliasing access occurs.
unsafe impl<const SIZE: usize> Sync for LockFreeRingBuffer<SIZE> {}

/// Exclusive producer handle for one ring buffer.
pub struct Producer<'a, const SIZE: usize>(&'a LockFreeRingBuffer<SIZE>);

/// Exclusive consumer handle for one ring buffer.
pub struct Consumer<'a, const SIZE: usize>(&'a LockFreeRingBuffer<SIZE>);

impl<const SIZE: usize> LockFreeRingBuffer<SIZE> {
    const ASSERT_POWER_OF_TWO: () = {
        assert!(SIZE > 0 && (SIZE & (SIZE - 1)) == 0, "SIZE must be a power of two");
    };

    /// Creates an empty ring buffer.
    pub const fn new() -> Self {
        let () = Self::ASSERT_POWER_OF_TWO;
        Self {
            buffer: UnsafeCell::new([0; SIZE]),
            head: CachePaddedAtomic(AtomicUsize::new(0)),
            tail: CachePaddedAtomic(AtomicUsize::new(0)),
        }
    }

    /// Splits the buffer into its unique producer and consumer handles.
    pub fn split(&mut self) -> (Producer<'_, SIZE>, Consumer<'_, SIZE>) {
        (Producer(self), Consumer(self))
    }

    #[inline(always)]
    fn mask(&self) -> usize {
        SIZE - 1
    }

    /// Total capacity in bytes.
    pub const fn capacity(&self) -> usize {
        SIZE
    }

    /// Approximate number of unread bytes.
    ///
    /// Two independent atomic loads cannot form an instantaneous snapshot on a
    /// concurrent system; the result is intended for telemetry only.
    pub fn len(&self) -> usize {
        let tail = self.tail.0.load(Ordering::Acquire);
        let head = self.head.0.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Returns `true` when no bytes are available to the consumer.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes that can still be enqueued.
    pub fn free_space(&self) -> usize {
        SIZE.saturating_sub(self.len())
    }

    fn push_internal(&self, src: &[u8]) -> Result<(), usize> {
        let len = src.len();
        if len == 0 {
            return Ok(());
        }
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);
        let used = head.wrapping_sub(tail);
        let free = SIZE.saturating_sub(used);
        if len > free {
            return Err(free);
        }

        let idx = head & self.mask();
        let first = len.min(SIZE - idx);
        unsafe {
            // SAFETY: `idx < SIZE` and the Acquire load guarantees the consumer
            // no longer reads the cells we overwrite. `copy` (not
            // `copy_nonoverlapping`) tolerates a caller slice aliasing the
            // buffer, and `cast` preserves pointer provenance explicitly.
            let raw = self.buffer.get().cast::<u8>();
            core::ptr::copy(src.as_ptr(), raw.add(idx), first);
            if len > first {
                core::ptr::copy(src.as_ptr().add(first), raw, len - first);
            }
        }
        // SAFETY: Release pairs with the consumer's Acquire load of `head`.
        self.head.0.store(head.wrapping_add(len), Ordering::Release);
        Ok(())
    }

    fn pop_internal(&self, dst: &mut [u8]) -> usize {
        let req = dst.len();
        if req == 0 {
            return 0;
        }
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Relaxed);
        let available = head.wrapping_sub(tail);
        if available == 0 {
            return 0;
        }
        let to_read = req.min(available);

        let idx = tail & self.mask();
        let first = to_read.min(SIZE - idx);
        unsafe {
            // SAFETY: the Acquire load synchronises with the producer's data
            // writes; `idx < SIZE` and `first <= to_read <= dst.len()`.
            let raw = self.buffer.get().cast::<u8>();
            core::ptr::copy(raw.add(idx), dst.as_mut_ptr(), first);
            if to_read > first {
                core::ptr::copy(raw, dst.as_mut_ptr().add(first), to_read - first);
            }
        }
        // SAFETY: Release pairs with the producer's Acquire load of `tail`.
        self.tail.0.store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }
}

impl<const SIZE: usize> Default for LockFreeRingBuffer<SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SIZE: usize> Producer<'_, SIZE> {
    /// Bytes that can still be enqueued (approximate under concurrency).
    pub fn free_space(&self) -> usize {
        self.0.free_space()
    }

    /// Enqueues `src`, returning `Err(free_bytes)` if it does not fit.
    pub fn push(&mut self, src: &[u8]) -> Result<(), usize> {
        self.0.push_internal(src)
    }

    /// Enqueues a single byte.
    pub fn push_byte(&mut self, byte: u8) -> Result<(), usize> {
        self.push(core::slice::from_ref(&byte))
    }
}

impl<const SIZE: usize> Consumer<'_, SIZE> {
    /// Bytes available for dequeueing (approximate under concurrency).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` when no bytes are available.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Dequeues up to `dst.len()` bytes, returning the number read.
    pub fn pop(&mut self, dst: &mut [u8]) -> usize {
        self.0.pop_internal(dst)
    }

    /// Dequeues one byte, if any is available.
    pub fn pop_byte(&mut self) -> Option<u8> {
        let mut out = [0u8; 1];
        if self.pop(&mut out) == 1 { Some(out[0]) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spsc_basic_push_pop() {
        let mut rb = LockFreeRingBuffer::<16>::new();
        let (mut producer, mut consumer) = rb.split();
        let data = [1u8, 2, 3, 4, 5];
        assert!(producer.push(&data).is_ok());
        assert_eq!(consumer.len(), 5);
        let mut out = [0u8; 5];
        assert_eq!(consumer.pop(&mut out), 5);
        assert_eq!(out, data);
        assert!(consumer.is_empty());
    }

    #[test]
    fn wrap_around() {
        let mut rb = LockFreeRingBuffer::<8>::new();
        let (mut producer, mut consumer) = rb.split();
        assert!(producer.push(&[1, 2, 3, 4, 5]).is_ok());
        let mut tmp = [0u8; 3];
        assert_eq!(consumer.pop(&mut tmp), 3);
        assert!(producer.push(&[6, 7, 8, 9]).is_ok());
        let mut out = [0u8; 6];
        assert_eq!(consumer.pop(&mut out), 6);
        assert_eq!(&out, &[4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn power_of_two_and_padding_validation() {
        let rb = LockFreeRingBuffer::<64>::new();
        let head_ptr = &rb.head as *const _ as usize;
        let tail_ptr = &rb.tail as *const _ as usize;
        assert!(head_ptr.abs_diff(tail_ptr) >= 64);
        assert_eq!(rb.capacity(), 64);
    }

    #[test]
    fn push_until_full_capacity() {
        let mut rb = LockFreeRingBuffer::<16>::new();
        let (mut producer, mut consumer) = rb.split();
        let data = [0xaau8; 16];
        assert!(producer.push(&data).is_ok());
        assert_eq!(producer.free_space(), 0);
        assert_eq!(producer.push_byte(0xff), Err(0));
        let mut out = [0u8; 16];
        assert_eq!(consumer.pop(&mut out), 16);
        assert_eq!(out, data);
        assert_eq!(consumer.pop_byte(), None);
    }

    #[test]
    fn byte_helpers_round_trip() {
        let mut rb = LockFreeRingBuffer::<4>::new();
        let (mut producer, mut consumer) = rb.split();
        assert!(producer.push_byte(0x42).is_ok());
        assert_eq!(consumer.pop_byte(), Some(0x42));
        assert!(producer.push(&[]).is_ok());
        let mut empty = [];
        assert_eq!(consumer.pop(&mut empty), 0);
    }
}
