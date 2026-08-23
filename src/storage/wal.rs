//! Fase 2: Direct I/O Write-Ahead Log
use core::cmp::min;

pub const PAGE_SIZE: usize = 4096;

/// Trait abstracto para sinks que pueden consumir páginas alineadas.
pub trait WalWriter {
	/// Escribe exactamente una página (`PAGE_SIZE` bytes). Implementación debe ser
	/// no-alloc y preferiblemente usar Direct I/O en plataforma específica.
	fn write_page(&mut self, page: &[u8]) -> Result<(), &'static str>;
}

/// Buffer alineado a `PAGE_SIZE` usado por `DirectWal`.
#[repr(align(4096))]
pub struct PageAlignedBuffer<const PAGES: usize>(pub [[u8; PAGE_SIZE]; PAGES]);

impl<const PAGES: usize> PageAlignedBuffer<PAGES> {
	pub const fn new() -> Self {
		Self([[0u8; PAGE_SIZE]; PAGES])
	}
}

impl<const PAGES: usize> Default for PageAlignedBuffer<PAGES> {
	fn default() -> Self {
		Self::new()
	}
}

/// Write-Ahead Log in-memory preparer para Direct I/O.
/// - `PAGES` es número de páginas de 4096 bytes cada una.
/// - No realiza asignaciones dinámicas; todo en stack/data estático.
pub struct DirectWal<const PAGES: usize> {
	buf: PageAlignedBuffer<PAGES>,
	/// bytes ocupados actualmente en el buffer (head)
	head: usize,
}

impl<const PAGES: usize> DirectWal<PAGES> {
	pub const fn new() -> Self {
		Self { buf: PageAlignedBuffer::new(), head: 0 }
	}

	pub fn capacity_bytes(&self) -> usize { PAGES * PAGE_SIZE }

	pub fn remaining(&self) -> usize { self.capacity_bytes().saturating_sub(self.head) }

	/// Appends up to `src.len()` bytes into the WAL buffer. Returns bytes written.
	/// Zero-allocation, copies into the page-aligned buffer. Caller may call
	/// `flush_pages` when needed.
	pub fn append(&mut self, src: &[u8]) -> Result<usize, &'static str> {
		let can = self.remaining();
		if can == 0 { return Err("wal full"); }
		let to_write = min(can, src.len());
		// write across page boundaries if necessary
		let mut remaining = to_write;
		let mut write_pos = self.head;
		let mut src_off = 0;
		while remaining > 0 {
			let page_idx = (write_pos / PAGE_SIZE) % PAGES;
			let page_off = write_pos % PAGE_SIZE;
			let can_write = min(remaining, PAGE_SIZE - page_off);
			let dst = &mut self.buf.0[page_idx][page_off..page_off + can_write];
			dst.copy_from_slice(&src[src_off..src_off + can_write]);
			write_pos += can_write;
			src_off += can_write;
			remaining -= can_write;
		}
		self.head = self.head.wrapping_add(to_write);
		Ok(to_write)
	}

	/// Returns how many full pages are ready to flush.
	pub fn full_pages(&self) -> usize { self.head / PAGE_SIZE }

	/// Flushes all full pages via the provided `WalWriter`.
	/// After successful flush, any remaining partial page is moved to buffer start.
	pub fn flush_pages<W: WalWriter>(&mut self, writer: &mut W) -> Result<(), &'static str> {
		let pages = self.full_pages();
		if pages == 0 { return Ok(()); }
		for i in 0..pages {
			writer.write_page(&self.buf.0[i][..])?;
		}
		// move leftover bytes (partial page) to the start
		let rem = self.head - pages * PAGE_SIZE;
		if rem > 0 {
			unsafe {
				let base_ptr = self.buf.0.as_mut_ptr() as *mut u8;
				let src_ptr = base_ptr.add(pages * PAGE_SIZE);
				core::ptr::copy(src_ptr, base_ptr, rem);
			}
		}
		self.head = rem;
		Ok(())
	}

	/// Reset WAL buffer (drop contents).
	pub fn clear(&mut self) { self.head = 0; }
}

impl<const PAGES: usize> Default for DirectWal<PAGES> {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::storage::ring_buffer::LockFreeRingBuffer;

	struct TestSink<const P: usize> {
		pages: [[u8; PAGE_SIZE]; P],
		count: usize,
	}

	impl<const P: usize> TestSink<P> {
		fn new() -> Self {
			Self { pages: [[0u8; PAGE_SIZE]; P], count: 0 }
		}
	}

	impl<const P: usize> WalWriter for TestSink<P> {
		fn write_page(&mut self, page: &[u8]) -> Result<(), &'static str> {
			if self.count >= P { return Err("sink full"); }
			self.pages[self.count].copy_from_slice(page);
			self.count += 1;
			Ok(())
		}
	}

	#[test]
	fn wal_append_and_flush_from_ring() {
		const RB_SZ: usize = 8192;
		let mut rb: LockFreeRingBuffer<RB_SZ> = LockFreeRingBuffer::new();
		let (mut prod, mut cons) = rb.split();

		let chunk = [0xABu8; 3000];
		assert!(prod.push(&chunk).is_ok());

		let mut tmp = [0u8; 3000];
		let n = cons.pop(&mut tmp);
		assert_eq!(n, 3000);

		const PAGES: usize = 2;
		let mut wal: DirectWal<PAGES> = DirectWal::new();
		let written = wal.append(&tmp).expect("append ok");
		assert_eq!(written, 3000);

		let mut sink: TestSink<PAGES> = TestSink::new();
		wal.flush_pages(&mut sink).expect("flush ok");
		assert_eq!(sink.count, 0);

		let big = [0x7Fu8; PAGE_SIZE];
		let _ = wal.append(&big).unwrap();
		wal.flush_pages(&mut sink).expect("flush ok");
		assert_eq!(sink.count, 1);
	}
}