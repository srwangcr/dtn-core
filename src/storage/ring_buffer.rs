//! Fase 2: Lock-Free SPSC Ring Buffer alineado a 64 bytes (Cache Line)

use core::sync::atomic::{AtomicUsize, Ordering};

/// Wrapper alineado a 64 bytes para aislar variables atómicas en líneas de caché L1/L2 independientes
#[repr(align(64))]
struct CachePaddedAtomic(AtomicUsize);

/// Ring Buffer SPSC Lock-Free optimizado a nivel de microarquitectura.
/// Requiere estrictamente que `SIZE` sea una potencia de 2.
#[repr(align(64))]
pub struct LockFreeRingBuffer<const SIZE: usize> {
    buffer: [u8; SIZE],
    head: CachePaddedAtomic,
    tail: CachePaddedAtomic,
}

impl<const SIZE: usize> LockFreeRingBuffer<SIZE> {
    // Falla la compilación si SIZE no es potencia de 2 o si es 0
    const ASSERT_POWER_OF_TWO: () = {
        assert!(SIZE > 0 && (SIZE & (SIZE - 1)) == 0, "LockFreeRingBuffer: SIZE must be a power of two!");
    };

    pub const fn new() -> Self {
        let _ = Self::ASSERT_POWER_OF_TWO;

        Self {
            buffer: [0; SIZE],
            head: CachePaddedAtomic(AtomicUsize::new(0)),
            tail: CachePaddedAtomic(AtomicUsize::new(0)),
        }
    }

    /// Máscara bitwise para cálculo de índice en O(1) ciclo de CPU
    #[inline(always)]
    fn mask(&self) -> usize {
        SIZE - 1
    }

    /// Retorna la capacidad total disponible para escritura
    pub fn capacity(&self) -> usize {
        SIZE - 1
    }

    /// Cantidad de bytes almacenados actualmente
    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    /// Insertar slice de bytes (Single-Producer)
    pub fn push(&self, src: &[u8]) -> Result<(), usize> {
        let len = src.len();
        if len == 0 { return Ok(()); }
        
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        let used = head.wrapping_sub(tail);
        let free = SIZE.saturating_sub(used);

        if len > free.saturating_sub(1) {
            return Err(free.saturating_sub(1));
        }

        // MÁSCARA BITWISE: Reemplaza head % SIZE por head & mask()
        let idx = head & self.mask();
        let first = core::cmp::min(len, SIZE - idx);

        unsafe {
            let dst = self.buffer.as_ptr().add(idx) as *mut u8;
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst, first);
            if len > first {
                let dst2 = self.buffer.as_ptr() as *mut u8;
                core::ptr::copy_nonoverlapping(src.as_ptr().add(first), dst2, len - first);
            }
        }

        self.head.0.store(head.wrapping_add(len), Ordering::Release);
        Ok(())
    }

    /// Extraer bytes hacia un buffer destino (Single-Consumer)
    pub fn pop(&self, dst: &mut [u8]) -> usize {
        let req = dst.len();
        if req == 0 { return 0; }

        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        let available = head.wrapping_sub(tail);

        if available == 0 { return 0; }
        let to_read = core::cmp::min(req, available);

        // MÁSCARA BITWISE: Reemplaza tail % SIZE por tail & mask()
        let idx = tail & self.mask();
        let first = core::cmp::min(to_read, SIZE - idx);

        unsafe {
            let src = self.buffer.as_ptr().add(idx);
            core::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), first);
            if to_read > first {
                let src2 = self.buffer.as_ptr();
                core::ptr::copy_nonoverlapping(src2, dst.as_mut_ptr().add(first), to_read - first);
            }
        }

        self.tail.0.store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }

    pub fn push_byte(&self, b: u8) -> Result<(), usize> {
        self.push(core::slice::from_ref(&b))
    }

    pub fn pop_byte(&self) -> Option<u8> {
        let mut out = [0u8; 1];
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
        let data = [1u8, 2, 3, 4, 5];
        assert!(rb.push(&data).is_ok());
        let mut out = [0u8; 5];
        let n = rb.pop(&mut out);
        assert_eq!(n, 5);
        assert_eq!(out, data);
    }

    #[test]
    fn wrap_around() {
        const N: usize = 8;
        let rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();
        let a = [1u8, 2, 3, 4, 5];
        assert!(rb.push(&a).is_ok());
        let mut tmp = [0u8; 3];
        assert_eq!(rb.pop(&mut tmp), 3);
        
        let b = [6u8, 7, 8, 9];
        assert!(rb.push(&b).is_ok());
        let mut out = [0u8; 6];
        let n = rb.pop(&mut out);
        assert_eq!(n, 6);
    }

    #[test]
    fn power_of_two_and_padding_validation() {
        let rb: LockFreeRingBuffer<64> = LockFreeRingBuffer::new();
        let head_ptr = &rb.head as *const _ as usize;
        let tail_ptr = &rb.tail as *const _ as usize;
        
        let distance = if head_ptr > tail_ptr { head_ptr - tail_ptr } else { tail_ptr - head_ptr };
        assert!(distance >= 64, "head y tail deben estar al menos a 64 bytes para evitar False Sharing");
    }
}