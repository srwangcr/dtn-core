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

/// First flash sector of the circular WAL region (3 MiB into a 4 MiB NOR part).
pub const WAL_START_OFFSET: u32 = 0x0030_0000;

/// Number of 4 KiB sectors in the circular wear-leveling ring.
pub const WAL_SECTOR_COUNT: usize = 16;

/// Total size of the circular WAL region (64 KiB).
pub const WAL_REGION_SIZE: u32 = (WAL_SECTOR_COUNT as u32) * (PAGE_SIZE as u32);

/// Byte offset of one sector inside the circular WAL region.
///
/// `slot` is reduced modulo [`WAL_SECTOR_COUNT`], so callers may pass a
/// monotonically increasing cursor without a separate bounds check.
pub const fn wal_slot_offset(slot: usize) -> u32 {
    WAL_START_OFFSET + ((slot % WAL_SECTOR_COUNT) as u32) * (PAGE_SIZE as u32)
}

/// Outcome of a crash-recovery scan over the circular WAL ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RingRecoveryReport {
    /// Number of sectors inspected.
    pub pages_scanned: usize,
    /// CRC-valid, in-sequence sectors replayed to `visit`.
    pub pages_valid: usize,
    /// Blank, torn or stale sectors that were skipped.
    pub pages_discarded: usize,
    /// Payload bytes handed to `visit`.
    pub bytes_recovered: usize,
    /// Sector holding the highest sequence number seen (`0` on an empty ring).
    pub active_slot: usize,
    /// Highest sequence number found in the ring.
    pub max_seq: u64,
    /// Sector that must be written next: `(active_slot + 1) % 16`.
    pub next_write_slot: usize,
}

impl RingRecoveryReport {
    /// Returns `true` when no sector had to be discarded.
    pub const fn is_clean(&self) -> bool {
        self.pages_discarded == 0
    }
}

/// Scans a circular WAL ring using exactly one caller-owned page buffer.
///
/// `read_sector(slot, page)` fills `page` with the 4096 bytes of sector `slot`
/// and returns `false` when the sector cannot be read. `visit(slot, seq,
/// payload)` is called for every valid record, in ascending sequence order
/// (i.e. oldest-to-newest across the wrap point). Two passes over the ring are
/// used: the first locates the active slot / `max_seq`, the second replays the
/// records. The same `page` array is reused by both passes, so boot recovery
/// needs only 4 KiB of SRAM regardless of the 64 KiB flash region.
pub fn scan_wal_ring<R, F>(mut read_sector: R, visit: &mut F) -> RingRecoveryReport
where
    R: FnMut(usize, &mut [u8; PAGE_SIZE]) -> bool,
    F: FnMut(usize, u64, &[u8]),
{
    let mut report = RingRecoveryReport {
        pages_scanned: WAL_SECTOR_COUNT,
        ..RingRecoveryReport::default()
    };
    let mut page = [0u8; PAGE_SIZE];

    // Pass 1: locate the newest sealed page. Sequence numbers are monotonic,
    // so the sector with the highest value is the active tail of the ring.
    let mut active: Option<usize> = None;
    let mut max_seq = 0u64;
    for slot in 0..WAL_SECTOR_COUNT {
        if !read_sector(slot, &mut page) {
            continue;
        }
        if let Ok((seq, _)) = read_page(&page) {
            if active.is_none() || seq > max_seq {
                active = Some(slot);
                max_seq = seq;
            }
        }
    }

    let Some(active_slot) = active else {
        return report; // blank / erased ring: first write goes to slot 0
    };
    report.active_slot = active_slot;
    report.max_seq = max_seq;
    report.next_write_slot = (active_slot + 1) % WAL_SECTOR_COUNT;

    // Pass 2: replay from the sector after the active one up to and including
    // the active one. Stale generations and torn writes are skipped.
    let mut last_seq: Option<u64> = None;
    for step in 1..=WAL_SECTOR_COUNT {
        let slot = (active_slot + step) % WAL_SECTOR_COUNT;
        if !read_sector(slot, &mut page) {
            report.pages_discarded += 1;
            continue;
        }
        match read_page(&page) {
            Ok((seq, payload)) if last_seq.map_or(true, |prev| seq > prev) => {
                last_seq = Some(seq);
                report.pages_valid += 1;
                report.bytes_recovered += payload.len();
                visit(slot, seq, payload);
            }
            _ => report.pages_discarded += 1,
        }
    }
    report
}

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

    /// Copies the first sealed page into a single caller-owned page buffer.
    ///
    /// Returns `false` when no page is ready. Suitable for flash sinks that
    /// expose one 4096-byte scratch buffer and write a single sector at a time.
    pub fn write_first_sealed_page(&self, dst: &mut [u8; PAGE_SIZE]) -> bool {
        if self.ready_pages == 0 {
            return false;
        }
        dst.copy_from_slice(&self.buf.0[0]);
        true
    }

    /// Resumes sequence numbering after a crash-recovery scan.
    ///
    /// Must be called while no pages are pending. `next_sequence` is normally
    /// `RingRecoveryReport::max_seq + 1`; using a lower value would make a new
    /// page indistinguishable from an older generation in the ring.
    pub fn set_next_sequence(&mut self, next_sequence: u64) {
        self.sequence = next_sequence;
    }

    /// Releases successfully persisted pages for reuse without resetting the
    /// monotonic sequence counter (unlike [`Self::clear`]).
    pub fn release_ready(&mut self) {
        self.ready_pages = 0;
        for page in self.buf.0.iter_mut() {
            for byte in page.iter_mut() {
                *byte = 0;
            }
        }
    }

    /// Flushes every sealed page through `writer`.
    ///
    /// On success the ready pages are released for reuse; `sequence` keeps
    /// increasing so later records cannot be mistaken for older generations.
    pub fn flush_pages<W: WalWriter>(&mut self, writer: &mut W) -> DtnResult<()> {
        for page in self.buf.0.iter().take(self.ready_pages) {
            writer.write_page(page)?;
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

    fn ring_flash() -> [[u8; PAGE_SIZE]; WAL_SECTOR_COUNT] {
        [[0u8; PAGE_SIZE]; WAL_SECTOR_COUNT]
    }

    fn seal_into_ring(
        flash: &mut [[u8; PAGE_SIZE]; WAL_SECTOR_COUNT],
        slot: usize,
        seq: u64,
        data: &[u8],
    ) {
        let mut page = [0u8; PAGE_SIZE];
        page[WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + data.len()].copy_from_slice(data);
        seal_page(&mut page, seq, data.len()).unwrap();
        flash[slot % WAL_SECTOR_COUNT] = page;
    }

    #[test]
    fn ring_scan_locates_active_slot_and_replays_across_wrap() {
        let mut flash = ring_flash();
        for seq in 0..20u64 {
            seal_into_ring(&mut flash, seq as usize, seq, b"bundle");
        }
        let mut seen = [0u64; WAL_SECTOR_COUNT];
        let mut count = 0usize;
        let report = scan_wal_ring(
            |slot, page| {
                page.copy_from_slice(&flash[slot]);
                true
            },
            &mut |_, s, _| {
                seen[count] = s;
                count += 1;
            },
        );
        assert_eq!(report.active_slot, 3); // 19 % 16
        assert_eq!(report.max_seq, 19);
        assert_eq!(report.next_write_slot, 4);
        assert_eq!(report.pages_valid, 16);
        assert_eq!(report.pages_discarded, 0);
        assert_eq!(count, 16);
        for (i, value) in seen.iter().enumerate() {
            assert_eq!(*value, 4 + i as u64);
        }
    }

    #[test]
    fn ring_scan_skips_torn_sector_and_blank_ring() {
        let mut flash = ring_flash();
        for slot in 0..5 {
            seal_into_ring(&mut flash, slot, slot as u64, b"x");
        }
        flash[2][0] = 0xff; // torn write: invalid magic
        let mut count = 0usize;
        let report = scan_wal_ring(
            |slot, page| {
                page.copy_from_slice(&flash[slot]);
                true
            },
            &mut |_, _, _| count += 1,
        );
        assert_eq!(report.active_slot, 4);
        assert_eq!(report.next_write_slot, 5);
        assert_eq!(report.pages_valid, 4);
        assert_eq!(report.pages_discarded, 12); // 1 torn + 11 blank
        assert_eq!(count, 4);

        let blank = ring_flash();
        let report = scan_wal_ring(
            |slot, page| {
                page.copy_from_slice(&blank[slot]);
                true
            },
            &mut |_, _, _| {},
        );
        assert_eq!(report.active_slot, 0);
        assert_eq!(report.next_write_slot, 0);
        assert_eq!(report.max_seq, 0);
        assert_eq!(report.pages_valid, 0);
    }

    #[test]
    fn wal_sequence_and_release_support_ring_restart() {
        let mut wal = DirectWal::<1>::new();
        wal.set_next_sequence(42);
        wal.append(b"fresh").unwrap();
        assert_eq!(wal.next_sequence(), 43);
        let mut page = [0u8; PAGE_SIZE];
        assert!(wal.write_first_sealed_page(&mut page));
        assert_eq!(read_page(&page).unwrap().0, 42);
        wal.release_ready();
        assert_eq!(wal.full_pages(), 0);
        assert_eq!(wal.next_sequence(), 43); // not reset to zero
        assert!(wal.append(b"again").is_ok());
        assert_eq!(wal.next_sequence(), 44);
    }
}
