//! Direct-I/O write-ahead log with CRC32C page integrity and crash recovery.
//!
//! Each 4096-byte page carries a 24-byte header:
//!
//! | Offset | Size | Field |
//! |--------|------|-------|
//! | 0 | 4 | magic `b"DTNW"` |
//! | 4 | 8 | monotonic sequence number (little endian) |
//! | 12 | 4 | payload length (little endian) |
//! | 16 | 4 | CRC32C over bytes `0..16` and the payload |
//! | 20 | 4 | reserved (zero) |
//!
//! A torn write, a bit flip or a stale page is detected by CRC32C and/or a
//! non-monotonic sequence number. [`crate::storage::wal::DirectWal::recover`] and
//! [`crate::storage::disk_sink::DiskBlockStore::recover`] discard the first
//! invalid page and everything after it, returning only the consistent prefix.
//!
//! ```
//! use dtn_core::storage::wal::{DirectWal, recover_pages, WAL_PAYLOAD_CAPACITY};
//! let mut wal = DirectWal::<2>::new();
//! let _ = wal.append(b"pending bundle").unwrap();
//! let mut pages = [[0u8; 4096]; 2];
//! wal.write_sealed_pages(&mut pages);
//! let mut recovered = [0u8; 64];
//! let mut n = 0;
//! let report = recover_pages(&pages, |data| {
//!     let count = data.len().min(recovered.len() - n);
//!     recovered[n..n + count].copy_from_slice(&data[..count]);
//!     n += count;
//! });
//! assert_eq!(report.pages_valid, 1); // append now seals one page per record
//! let _ = WAL_PAYLOAD_CAPACITY;
//! ```

use crate::error::{DtnError, DtnResult};
use crate::storage::crc32c::crc32c;

/// Size of a flash sector / WAL page in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Magic bytes identifying a sealed WAL page.
pub const WAL_PAGE_MAGIC: [u8; 4] = *b"DTNW";

/// Size of the WAL page header in bytes.
pub const WAL_PAGE_HEADER_LEN: usize = 24;

/// Maximum number of payload bytes stored in one WAL page.
pub const WAL_PAYLOAD_CAPACITY: usize = PAGE_SIZE - WAL_PAGE_HEADER_LEN;

const OFF_SEQ: usize = 4;
const OFF_LEN: usize = 12;
const OFF_CRC: usize = 16;
const OFF_RESERVED: usize = 20;

/// Trait abstracting a sink that accepts page-aligned writes.
///
/// Implementations must not allocate; they receive exactly `PAGE_SIZE` bytes.
pub trait WalWriter {
    /// Writes one complete page.
    fn write_page(&mut self, page: &[u8]) -> DtnResult<()>;
}

/// Buffer aligned to a flash sector boundary, holding `PAGES` pages.
#[repr(align(4096))]
pub struct PageAlignedBuffer<const PAGES: usize>(pub [[u8; PAGE_SIZE]; PAGES]);

impl<const PAGES: usize> PageAlignedBuffer<PAGES> {
    /// Creates a zeroed page buffer in `const` context.
    pub const fn new() -> Self {
        Self([[0u8; PAGE_SIZE]; PAGES])
    }
}

impl<const PAGES: usize> Default for PageAlignedBuffer<PAGES> {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of scanning WAL pages after a power loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// Number of pages inspected (including blank/trailing pages).
    pub pages_scanned: usize,
    /// Number of CRC-valid, in-sequence pages with records.
    pub pages_valid: usize,
    /// Number of pages discarded because of corruption or bad sequence.
    pub pages_discarded: usize,
    /// Number of payload bytes in the recovered prefix.
    pub bytes_recovered: usize,
}

impl RecoveryReport {
    /// Returns `true` when every scanned page was recovered.
    pub const fn is_clean(&self) -> bool {
        self.pages_discarded == 0
    }
}

/// Write-ahead log preparer for deterministic store-and-forward.
///
/// `PAGES` is the number of 4096-byte sectors available in SRAM. Each call to
/// [`Self::append`] seals one page with a fresh sequence number and CRC32C, so
/// every record is independently recoverable after a power loss. Records
/// larger than [`WAL_PAYLOAD_CAPACITY`] are rejected atomically instead of
/// being split across pages: a partially persisted bundle must never be
/// reported as durable.
pub struct DirectWal<const PAGES: usize> {
    buf: PageAlignedBuffer<PAGES>,
    ready_pages: usize,
    sequence: u64,
}

impl<const PAGES: usize> DirectWal<PAGES> {
    const ASSERT_NONEMPTY: () = assert!(PAGES > 0, "DirectWal requires at least one page");

    /// Creates an empty WAL.
    pub const fn new() -> Self {
        let () = Self::ASSERT_NONEMPTY;
        Self { buf: PageAlignedBuffer::new(), ready_pages: 0, sequence: 0 }
    }

    /// Logical payload capacity in bytes (excluding page headers).
    pub const fn capacity_bytes() -> usize {
        PAGES * WAL_PAYLOAD_CAPACITY
    }

    /// Remaining payload capacity in bytes.
    pub const fn remaining(&self) -> usize {
        Self::capacity_bytes().saturating_sub(self.ready_pages.saturating_mul(WAL_PAYLOAD_CAPACITY))
    }

    /// Number of sealed pages waiting to be flushed.
    pub const fn full_pages(&self) -> usize {
        self.ready_pages
    }

    /// Next sequence number that will be assigned to a sealed page.
    pub const fn next_sequence(&self) -> u64 {
        self.sequence
    }

    /// Appends one complete bundle as a sealed page.
    ///
    /// Returns the payload length on success. Fails with [`DtnError::WalFull`]
    /// when the WAL is full or the record exceeds one page, without changing
    /// any state.
    pub fn append(&mut self, src: &[u8]) -> DtnResult<usize> {
        if src.len() > WAL_PAYLOAD_CAPACITY || self.ready_pages >= PAGES {
            return Err(DtnError::WalFull);
        }
        let index = self.ready_pages;
        let page = &mut self.buf.0[index];
        for byte in page.iter_mut() {
            *byte = 0;
        }
        page[WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + src.len()].copy_from_slice(src);
        seal_page(page, self.sequence, src.len())?;
        self.sequence = self.sequence.wrapping_add(1);
        self.ready_pages += 1;
        Ok(src.len())
    }

    /// Copies sealed pages into `dst` (useful for tests and simulated flash).
    ///
    /// Returns the number of pages copied.
    pub fn write_sealed_pages(&self, dst: &mut [[u8; PAGE_SIZE]]) -> usize {
        let count = self.ready_pages.min(dst.len());
        for i in 0..count {
            dst[i].copy_from_slice(&self.buf.0[i]);
        }
        count
    }

    /// Flushes every sealed page through `writer`.
    ///
    /// On success the ready pages are released for reuse; `sequence` keeps
    /// increasing so later records cannot be mistaken for older generations.
    pub fn flush_pages<W: WalWriter>(&mut self, writer: &mut W) -> DtnResult<()> {
        for i in 0..self.ready_pages {
            writer.write_page(&self.buf.0[i])?;
        }
        self.ready_pages = 0;
        Ok(())
    }

    /// Drops all buffered records and resets sequence numbers.
    pub fn clear(&mut self) {
        self.ready_pages = 0;
        self.sequence = 0;
        for page in self.buf.0.iter_mut() {
            for byte in page.iter_mut() {
                *byte = 0;
            }
        }
    }

    /// Scans the buffered sealed pages, calls `visit` for every recovered
    /// record and discards the invalid suffix.
    ///
    /// Corrupt or non-monotonic pages (and all pages after them) are
    /// discarded. The sequence counter is restored from the last valid page.
    pub fn recover<F: FnMut(&[u8])>(&mut self, mut visit: F) -> RecoveryReport {
        let sealed = self.ready_pages;
        let (report, valid_pages, last_sequence) =
            scan_pages(&self.buf.0[..sealed], &mut visit);
        // Scanning stops at the first invalid page, so pages `0..valid_pages`
        // are already the consistent prefix; later pages are discarded.
        self.ready_pages = valid_pages;
        self.sequence = last_sequence.wrapping_add(1);
        report
    }
}

impl<const PAGES: usize> Default for DirectWal<PAGES> {
    fn default() -> Self {
        Self::new()
    }
}

/// Seals one page: writes header fields and CRC32C over header + payload.
pub fn seal_page(page: &mut [u8; PAGE_SIZE], sequence: u64, payload_len: usize) -> DtnResult<()> {
    if payload_len > WAL_PAYLOAD_CAPACITY {
        return Err(DtnError::InvalidLength);
    }
    page[..4].copy_from_slice(&WAL_PAGE_MAGIC);
    page[OFF_SEQ..OFF_SEQ + 8].copy_from_slice(&sequence.to_le_bytes());
    let len = u32::try_from(payload_len).map_err(|_| DtnError::LengthOverflow)?;
    page[OFF_LEN..OFF_LEN + 4].copy_from_slice(&len.to_le_bytes());
    page[OFF_CRC..OFF_CRC + 4].fill(0);
    page[OFF_RESERVED..OFF_RESERVED + 4].fill(0);
    let crc = page_crc(page, payload_len);
    page[OFF_CRC..OFF_CRC + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// Validates a page and returns `(sequence_number, payload_slice)`.
pub fn read_page(page: &[u8; PAGE_SIZE]) -> DtnResult<(u64, &[u8])> {
    if page[..4] != WAL_PAGE_MAGIC {
        return Err(DtnError::InvalidPageMagic);
    }
    for byte in &page[OFF_RESERVED..OFF_RESERVED + 4] {
        if *byte != 0 {
            return Err(DtnError::CrcMismatch);
        }
    }
    let mut seq_bytes = [0u8; 8];
    seq_bytes.copy_from_slice(&page[OFF_SEQ..OFF_SEQ + 8]);
    let sequence = u64::from_le_bytes(seq_bytes);
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&page[OFF_LEN..OFF_LEN + 4]);
    let raw_len = u32::from_le_bytes(len_bytes);
    let payload_len = usize::try_from(raw_len).map_err(|_| DtnError::LengthOverflow)?;
    if payload_len > WAL_PAYLOAD_CAPACITY {
        return Err(DtnError::InvalidLength);
    }
    let mut crc_bytes = [0u8; 4];
    crc_bytes.copy_from_slice(&page[OFF_CRC..OFF_CRC + 4]);
    let stored = u32::from_le_bytes(crc_bytes);
    if stored != page_crc(page, payload_len) {
        return Err(DtnError::CrcMismatch);
    }
    let payload = page
        .get(WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + payload_len)
        .ok_or(DtnError::Truncated)?;
    Ok((sequence, payload))
}

/// Scans pages in order, invoking `visit` for every valid record.
///
/// Scanning stops at the first invalid page: later pages may belong to an
/// older, partially overwritten generation and must not be trusted.
pub fn recover_pages<F: FnMut(&[u8])>(pages: &[[u8; PAGE_SIZE]], mut visit: F) -> RecoveryReport {
    scan_pages(pages, &mut visit).0
}

/// Scans raw WAL pages, verifies CRC32C headers and sequence monotonicity, and yields valid records.
pub fn scan_pages<F: FnMut(&[u8])>(
    pages: &[[u8; PAGE_SIZE]],
    visit: &mut F,
) -> (RecoveryReport, usize, u64) {
    let mut report = RecoveryReport::default();
    let mut last_sequence = None;
    for page in pages {
        report.pages_scanned += 1;
        match read_page(page) {
            Ok((sequence, payload)) if last_sequence.map_or(true, |prev| sequence > prev) => {
                last_sequence = Some(sequence);
                report.pages_valid += 1;
                report.bytes_recovered += payload.len();
                visit(payload);
            }
            Ok(_) | Err(_) => {
                report.pages_discarded = pages.len().saturating_sub(report.pages_valid);
                return (report, report.pages_valid, last_sequence.unwrap_or(0));
            }
        }
    }
    (report, report.pages_valid, last_sequence.unwrap_or(0))
}

fn page_crc(page: &[u8; PAGE_SIZE], payload_len: usize) -> u32 {
    let header = crc32c(&page[..OFF_CRC], 0);
    let body = &page[WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + payload_len];
    crc32c(body, header)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fn write_page(&mut self, page: &[u8]) -> DtnResult<()> {
            if self.count >= P {
                return Err(DtnError::Io);
            }
            self.pages[self.count].copy_from_slice(page);
            self.count += 1;
            Ok(())
        }
    }

    #[test]
    fn wal_append_and_flush() {
        let mut wal = DirectWal::<2>::new();
        assert_eq!(wal.append(&[0xab; 3000]), Ok(3000));
        assert_eq!(wal.full_pages(), 1);
        assert_eq!(wal.remaining(), WAL_PAYLOAD_CAPACITY);
        let mut sink = TestSink::<2>::new();
        assert!(wal.flush_pages(&mut sink).is_ok());
        assert_eq!(sink.count, 1);
        assert_eq!(read_page(&sink.pages[0]), Ok((0, &[0xab; 3000][..])));

        let big = [0x7f; WAL_PAYLOAD_CAPACITY];
        assert_eq!(wal.append(&big), Ok(big.len()));
        assert_eq!(wal.full_pages(), 1);
        assert!(wal.flush_pages(&mut sink).is_ok());
        assert_eq!(sink.count, 2);
        assert_eq!(read_page(&sink.pages[1]).unwrap().0, 1);
    }

    #[test]
    fn append_is_atomic_when_wal_is_full() {
        let mut wal = DirectWal::<1>::new();
        assert_eq!(wal.append(&[1; WAL_PAYLOAD_CAPACITY]), Ok(WAL_PAYLOAD_CAPACITY));
        assert_eq!(wal.append(&[2]), Err(DtnError::WalFull));
        assert_eq!(wal.full_pages(), 1);
        assert_eq!(wal.remaining(), 0);
        assert_eq!(wal.next_sequence(), 1);
    }

    #[test]
    fn oversized_record_is_rejected_without_partial_write() {
        let mut wal = DirectWal::<2>::new();
        let mut big = [0u8; WAL_PAYLOAD_CAPACITY + 1];
        big[0] = 0x5a;
        assert_eq!(wal.append(&big), Err(DtnError::WalFull));
        assert_eq!(wal.full_pages(), 0);
        assert_eq!(wal.remaining(), DirectWal::<2>::capacity_bytes());
    }

    #[test]
    fn torn_write_and_bitflip_are_discarded() {
        let mut wal = DirectWal::<4>::new();
        assert_eq!(wal.append(b"first bundle"), Ok(12));
        assert_eq!(wal.append(b"second bundle"), Ok(13));
        assert_eq!(wal.append(b"third bundle"), Ok(12));
        assert_eq!(wal.full_pages(), 3);

        let mut pages = [[0u8; PAGE_SIZE]; 4];
        assert_eq!(wal.write_sealed_pages(&mut pages), 3);
        // Simulate a torn middle page: magic only partially written.
        pages[1][2] = 0x00;
        // Simulate a bit flip after the valid prefix.
        pages[2][WAL_PAGE_HEADER_LEN] ^= 0x80;

        let mut recovered = [[0u8; 64]; 4];
        let mut counts = [0usize; 4];
        let report = recover_pages(&pages, |data| {
            let slot = counts.iter().position(|&n| n == 0).unwrap_or(3);
            let n = data.len().min(recovered[slot].len());
            recovered[slot][..n].copy_from_slice(&data[..n]);
            counts[slot] = n;
        });
        assert_eq!(report.pages_valid, 1);
        // One torn page, one bit-flipped page and one blank trailing page.
        assert_eq!(report.pages_discarded, 3);
        assert_eq!(report.bytes_recovered, 12);
        assert_eq!(&recovered[0][..12], b"first bundle");
    }

    #[test]
    fn recover_compacts_wal_state_and_restores_sequence() {
        let mut wal = DirectWal::<3>::new();
        wal.append(b"page one").unwrap();
        wal.append(b"page two").unwrap();
        wal.append(b"page three").unwrap();
        assert_eq!(wal.full_pages(), 3);
        // Corrupt the second page in the internal buffer.
        wal.buf.0[1][WAL_PAGE_HEADER_LEN] ^= 0xff;
        let mut seen = 0usize;
        let report = wal.recover(|_| seen += 1);
        assert_eq!(report.pages_valid, 1);
        assert_eq!(wal.full_pages(), 1);
        assert_eq!(wal.next_sequence(), 1);
        assert_eq!(seen, 1);
        assert_eq!(read_page(&wal.buf.0[0]).unwrap().1, b"page one");
        // The WAL is usable again after recovery.
        assert!(wal.append(b"new record").is_ok());
    }

    #[test]
    fn duplicated_sequence_number_is_rejected() {
        let mut pages = [[0u8; PAGE_SIZE]; 2];
        seal_page(&mut pages[0], 7, 3).unwrap();
        pages[0][WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + 3].copy_from_slice(b"abc");
        seal_page(&mut pages[0], 7, 3).unwrap();
        let mut copy = [0u8; PAGE_SIZE];
        copy.copy_from_slice(&pages[0]);
        pages[1] = copy;
        let mut seen = 0;
        let report = recover_pages(&pages, |_| seen += 1);
        assert_eq!(report.pages_valid, 1);
        assert_eq!(seen, 1);
        assert_eq!(read_page(&pages[0]).unwrap().0, 7);
    }
}
