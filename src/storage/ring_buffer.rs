//! Fase 2: Lock-Free SPSC Ring Buffer alineado a 64 bytes (Cache Line) con UnsafeCell y SPSC Handles.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Wrapper alineado a 64 bytes para aislar variables atómicas en líneas de caché L1/L2 independientes
#[repr(align(64))]
struct CachePaddedAtomic(AtomicUsize);

/// Ring Buffer SPSC Lock-Free optimizado a nivel de microarquitectura.
/// Requiere strictly que `SIZE` sea una potencia de 2.
#[repr(align(64))]
pub struct LockFreeRingBuffer<const SIZE: usize> {
    buffer: UnsafeCell<[u8; SIZE]>,
    head: CachePaddedAtomic,
    tail: CachePaddedAtomic,
}

// SAFETY: El contrato Single-Producer Single-Consumer (SPSC) garantiza mediante
// el sistema de tipos (`Producer` y `Consumer`) que nunca existirán accesos concurrentes
// simultáneos al mismo segmento de memoria dentro del buffer. La sincronización
// de visibilidad de los punteros/datos se gestiona mediante barreras Acquire/Release en head y tail.
unsafe impl<const SIZE: usize> Sync for LockFreeRingBuffer<SIZE> {}

/// Handle exclusivo de producción (Single-Producer)
pub struct Producer<'a, const SIZE: usize>(&'a LockFreeRingBuffer<SIZE>);

/// Handle exclusivo de consumo (Single-Consumer)
pub struct Consumer<'a, const SIZE: usize>(&'a LockFreeRingBuffer<SIZE>);

impl<const SIZE: usize> LockFreeRingBuffer<SIZE> {
    const ASSERT_POWER_OF_TWO: () = {
        assert!(SIZE > 0 && (SIZE & (SIZE - 1)) == 0, "LockFreeRingBuffer: SIZE must be a power of two!");
    };

    pub const fn new() -> Self {
        let () = Self::ASSERT_POWER_OF_TWO;

        Self {
            buffer: UnsafeCell::new([0; SIZE]),
            head: CachePaddedAtomic(AtomicUsize::new(0)),
            tail: CachePaddedAtomic(AtomicUsize::new(0)),
        }
    }

    /// Divide el buffer en dos handles únicos para garantizar SPSC a nivel de tipos
    pub fn split(&mut self) -> (Producer<'_, SIZE>, Consumer<'_, SIZE>) {
        (Producer(self), Consumer(self))
    }

    #[inline(always)]
    fn mask(&self) -> usize {
        SIZE - 1
    }

    pub fn capacity(&self) -> usize {
        SIZE
    }

    pub fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        head.wrapping_sub(tail)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn push_internal(&self, src: &[u8]) -> Result<(), usize> {
        let len = src.len();
        if len == 0 { return Ok(()); }
        
        // Relaxed en head porque solo el hilo productor escribe su propia cabeza
        let head = self.head.0.load(Ordering::Relaxed);
        // Acquire en tail para sincronizar con la lectura del consumidor
        let tail = self.tail.0.load(Ordering::Acquire);
        
        let used = head.wrapping_sub(tail);
        let free = SIZE.saturating_sub(used);

        if len > free {
            return Err(free);
        }

        let idx = head & self.mask();
        let first = core::cmp::min(len, SIZE - idx);

        unsafe {
            // Acceso seguro mediante UnsafeCell::get() para evitar aliasing UB
            let raw_buf = self.buffer.get() as *mut u8;
            let dst = raw_buf.add(idx);
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst, first);
            if len > first {
                core::ptr::copy_nonoverlapping(src.as_ptr().add(first), raw_buf, len - first);
            }
        }

        self.head.0.store(head.wrapping_add(len), Ordering::Release);
        Ok(())
    }

    fn pop_internal(&self, dst: &mut [u8]) -> usize {
        let req = dst.len();
        if req == 0 { return 0; }

        // Acquire en head para sincronizar con las escrituras del productor
        let head = self.head.0.load(Ordering::Acquire);
        // Relaxed en tail porque solo el hilo consumidor escribe su propia cola
        let tail = self.tail.0.load(Ordering::Relaxed);
        
        let available = head.wrapping_sub(tail);

        if available == 0 { return 0; }
        let to_read = core::cmp::min(req, available);

        let idx = tail & self.mask();
        let first = core::cmp::min(to_read, SIZE - idx);

        unsafe {
            // Acceso seguro mediante UnsafeCell::get()
            let raw_buf = self.buffer.get() as *const u8;
            let src = raw_buf.add(idx);
            core::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), first);
            if to_read > first {
                core::ptr::copy_nonoverlapping(raw_buf, dst.as_mut_ptr().add(first), to_read - first);
            }
        }

        self.tail.0.store(tail.wrapping_add(to_read), Ordering::Release);
        to_read
    }
}

impl<const SIZE: usize> Default for LockFreeRingBuffer<SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

// Métodos expuestos al Productor
impl<'a, const SIZE: usize> Producer<'a, SIZE> {
    pub fn push(&mut self, src: &[u8]) -> Result<(), usize> {
        self.0.push_internal(src)
    }

    pub fn push_byte(&mut self, b: u8) -> Result<(), usize> {
        self.push(core::slice::from_ref(&b))
    }
}

// Métodos expuestos al Consumidor
impl<'a, const SIZE: usize> Consumer<'a, SIZE> {
    pub fn pop(&mut self, dst: &mut [u8]) -> usize {
        self.0.pop_internal(dst)
    }

    pub fn pop_byte(&mut self) -> Option<u8> {
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
        let mut rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();
        let (mut prod, mut cons) = rb.split();
        
        let data = [1u8, 2, 3, 4, 5];
        assert!(prod.push(&data).is_ok());
        let mut out = [0u8; 5];
        let n = cons.pop(&mut out);
        assert_eq!(n, 5);
        assert_eq!(out, data);
    }

    #[test]
    fn wrap_around() {
        const N: usize = 8;
        let mut rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();
        let (mut prod, mut cons) = rb.split();

        let a = [1u8, 2, 3, 4, 5];
        assert!(prod.push(&a).is_ok());
        let mut tmp = [0u8; 3];
        assert_eq!(cons.pop(&mut tmp), 3);
        
        let b = [6u8, 7, 8, 9];
        assert!(prod.push(&b).is_ok());
        let mut out = [0u8; 6];
        let n = cons.pop(&mut out);
        assert_eq!(n, 6);
    }

    #[test]
    fn power_of_two_and_padding_validation() {
        let rb: LockFreeRingBuffer<64> = LockFreeRingBuffer::new();
        let head_ptr = &rb.head as *const _ as usize;
        let tail_ptr = &rb.tail as *const _ as usize;
        
        let distance = head_ptr.abs_diff(tail_ptr);
        assert!(distance >= 64, "head y tail deben estar al menos a 64 bytes para evitar False Sharing");
    }

    #[test]
    fn push_until_full_capacity() {
        const N: usize = 16;
        let mut rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();
        let (mut prod, mut cons) = rb.split();

        // Llenar exactamente la capacidad total (16 bytes)
        let data = [0xAAu8; N];
        assert!(prod.push(&data).is_ok());

        // Intentar meter 1 byte extra debe fallar indicando 0 espacio disponible
        assert_eq!(prod.push_byte(0xFF), Err(0));

        // Consumir todo y verificar integridad
        let mut out = [0u8; N];
        assert_eq!(cons.pop(&mut out), N);
        assert_eq!(out, data);
    }
}