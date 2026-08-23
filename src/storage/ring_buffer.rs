//! Fase 2: Lock-Free SPSC Ring Buffer alineado a 64 bytes (Cache Line)

#[repr(align(64))]
pub struct LockFreeRingBuffer<const SIZE: usize> {
    buffer: [u8; SIZE],
    head: core::sync::atomic::AtomicUsize,
    tail: core::sync::atomic::AtomicUsize,
}

impl<const SIZE: usize> LockFreeRingBuffer<SIZE> {
    pub const fn new() -> Self {
        Self {
            buffer: [0; SIZE],
            head: core::sync::atomic::AtomicUsize::new(0),
            tail: core::sync::atomic::AtomicUsize::new(0),
        }
    }
}

use core::sync::atomic::Ordering;

impl<const SIZE: usize> LockFreeRingBuffer<SIZE> {
    /// Returns available capacity for writing (max contiguous across wrap is not guaranteed).
    pub fn capacity(&self) -> usize {
        // We reserve one slot to distinguish full vs empty
        if SIZE == 0 { return 0; }
        SIZE - 1
    }

    /// Number of bytes currently stored in the buffer.
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Try to push `src` into the ring buffer. Returns `Ok(())` on success,
    /// or `Err(available_space)` if there's not enough free space.
    /// Single-producer only.
    pub fn push(&self, src: &[u8]) -> Result<(), usize> {
        let len = src.len();
        if len == 0 { return Ok(()); }
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        let used = head.wrapping_sub(tail);
        let free = SIZE.saturating_sub(used);
        if len > free.saturating_sub(1) { // reserve one slot
            return Err(free.saturating_sub(1));
        }

        let idx = head % SIZE;
        let first = core::cmp::min(len, SIZE - idx);

        unsafe {
            let dst = self.buffer.as_ptr().add(idx) as *mut u8;
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst, first);
            if len > first {
                let dst2 = self.buffer.as_ptr() as *mut u8;
                core::ptr::copy_nonoverlapping(src.as_ptr().add(first), dst2, len - first);
            }
        }

        // publish new head so consumer can Acquire it
        self.head.store(head.wrapping_add(len), Ordering::Release);
        Ok(())
    }

    /// Pop up to `dst.len()` bytes into `dst`. Returns number of bytes popped.
    /// Single-consumer only.
    pub fn pop(&self, dst: &mut [u8]) -> usize {
        let req = dst.len();
        if req == 0 { return 0; }
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        let available = head.wrapping_sub(tail);
        if available == 0 { return 0; }
        let to_read = core::cmp::min(req, available);
        let idx = tail % SIZE;
        let first = core::cmp::min(to_read, SIZE - idx);

        unsafe {
            let src = self.buffer.as_ptr().add(idx);
            core::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), first);
            if to_read > first {
                let src2 = self.buffer.as_ptr();
                core::ptr::copy_nonoverlapping(src2, dst.as_mut_ptr().add(first), to_read - first);
            }
        }

        // publish new tail so producer can Acquire it
        self.tail.store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }

    /// Convenience single-byte push. Returns `Ok(())` or `Err(available_space)`.
    pub fn push_byte(&self, b: u8) -> Result<(), usize> {
        self.push(core::slice::from_ref(&b))
    }

    /// Convenience single-byte pop. Returns `Some(byte)` or `None` if empty.
    pub fn pop_byte(&self) -> Option<u8> {
        let mut out = [0u8;1];
        let n = self.pop(&mut out);
        if n == 1 { Some(out[0]) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spsc_basic_push_pop() {
        const N: usize = 16;
        let rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();
        let data = [1u8,2,3,4,5];
        assert!(rb.push(&data).is_ok());
        let mut out = [0u8;5];
        let n = rb.pop(&mut out);
        assert_eq!(n, 5);
        assert_eq!(out, data);
    }

    #[test]
    fn wrap_around() {
        const N: usize = 8;
        let rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();
        // fill near end
        let a = [1u8,2,3,4,5];
        assert!(rb.push(&a).is_ok());
        let mut tmp = [0u8;3];
        assert_eq!(rb.pop(&mut tmp), 3);
        // push more to force wrap
        let b = [6u8,7,8,9];
        assert!(rb.push(&b).is_ok());
        let mut out = [0u8;6];
        let n = rb.pop(&mut out);
        assert!(n >= 0);
        // remaining items should be 2(from a) + 4(from b) = 6
        assert_eq!(n, 6);
    }
}
