//! Static in-memory simulation of a block device for store-and-forward WALs.
//!
//! On a real firmware target the same page bytes would be handed to an
//! `O_DIRECT` write or a flash driver. Keeping the sink as fixed arrays makes
//! power-loss scenarios reproducible under Miri without any system calls.

use crate::error::DtnResult;
use crate::storage::wal::{
    read_page, recover_pages, seal_page, PageAlignedBuffer, RecoveryReport, WalWriter, PAGE_SIZE,
    WAL_PAGE_HEADER_LEN, WAL_PAYLOAD_CAPACITY,
};

/// Fixed-capacity disk/flash block store used for tests and simulations.
pub struct DiskBlockStore<const MAX_PAGES: usize> {
    blocks: PageAlignedBuffer<MAX_PAGES>,
    written_pages: usize,
}

impl<const MAX_PAGES: usize> DiskBlockStore<MAX_PAGES> {
    /// Creates an empty store.
    pub const fn new() -> Self {
        Self { blocks: PageAlignedBuffer::new(), written_pages: 0 }
    }

    /// Number of pages currently written.
    pub const fn written_pages(&self) -> usize {
        self.written_pages
    }

    /// Borrows one written page.
    pub fn get_page(&self, index: usize) -> Option<&[u8; PAGE_SIZE]> {
        self.blocks.0.get(index).filter(|_| index < self.written_pages)
    }

    /// Mutably borrows one written page (used to inject torn writes/corruption).
    pub fn get_page_mut(&mut self, index: usize) -> Option<&mut [u8; PAGE_SIZE]> {
        if index < self.written_pages {
            self.blocks.0.get_mut(index)
        } else {
            None
        }
    }

    /// Borrows all written pages as a contiguous slice.
    pub fn pages(&self) -> &[[u8; PAGE_SIZE]] {
        &self.blocks.0[..self.written_pages]
    }

    /// Validates all pages and calls `visit` for each recovered record.
    ///
    /// CRC failures and non-monotonic sequence numbers terminate the scan;
    /// earlier valid records are still reported.
    pub fn recover<F: FnMut(&[u8])>(&self, visit: F) -> RecoveryReport {
        recover_pages(self.pages(), visit)
    }

    /// Validates pages, then drops the inconsistent suffix from the store.
    ///
    /// Returns the same report as [`Self::recover`]; `written_pages` becomes
    /// the number of valid pages, so subsequent appends overwrite the torn
    /// region instead of preserving it.
    pub fn recover_and_compact<F: FnMut(&[u8])>(&mut self, visit: F) -> RecoveryReport {
        let report = self.recover(visit);
        self.written_pages = report.pages_valid;
        report
    }

    /// Writes a sealed page constructed from `sequence` and `payload`.
    ///
    /// This bypasses [`WalWriter`] and is handy for tests that emulate a
    /// producer writing a record directly to flash.
    pub fn append_record(&mut self, sequence: u64, payload: &[u8]) -> DtnResult<()> {
        if self.written_pages >= MAX_PAGES {
            return Err(crate::error::DtnError::WalFull);
        }
        let mut page = [0u8; PAGE_SIZE];
        let len = payload.len().min(WAL_PAYLOAD_CAPACITY);
        page[WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + len].copy_from_slice(&payload[..len]);
        seal_page(&mut page, sequence, len)?;
        self.write_page(&page)?;
        Ok(())
    }

    /// Reads and validates a single page.
    pub fn read_record(&self, index: usize) -> DtnResult<(u64, &[u8])> {
        let page = self.get_page(index).ok_or(crate::error::DtnError::Truncated)?;
        read_page(page)
    }
}

impl<const MAX_PAGES: usize> Default for DiskBlockStore<MAX_PAGES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX_PAGES: usize> WalWriter for DiskBlockStore<MAX_PAGES> {
    fn write_page(&mut self, page: &[u8]) -> DtnResult<()> {
        if self.written_pages >= MAX_PAGES {
            return Err(crate::error::DtnError::Io);
        }
        self.blocks.0[self.written_pages].copy_from_slice(page);
        self.written_pages += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DtnError;

    #[test]
    fn disk_store_writer() {
        let mut store = DiskBlockStore::<4>::new();
        let page_data = [0xeeu8; PAGE_SIZE];
        assert!(store.write_page(&page_data).is_ok());
        assert_eq!(store.written_pages(), 1);
        assert_eq!(store.get_page(0).unwrap()[0], 0xee);
    }

    #[test]
    fn recovers_intact_records_after_power_loss() {
        let mut store = DiskBlockStore::<4>::new();
        store.append_record(1, b"bundle-A").unwrap();
        store.append_record(2, b"bundle-B").unwrap();
        store.append_record(3, b"bundle-C").unwrap();

        // Power loss during page 2: only the magic prefix survived.
        store.get_page_mut(2).unwrap()[..4].copy_from_slice(b"DTN\x00");
        // Page 3 belongs to the same generation but is beyond the torn page.
        store.append_record(4, b"bundle-D").unwrap();

        let mut seen = [[0u8; 16]; 4];
        let mut lens = [0usize; 4];
        let report = store.recover(|data| {
            let slot = lens.iter().position(|&n| n == 0).unwrap_or(3);
            let n = data.len().min(seen[slot].len());
            seen[slot][..n].copy_from_slice(&data[..n]);
            lens[slot] = n;
        });
        assert_eq!(report.pages_valid, 2);
        assert_eq!(report.bytes_recovered, 16);
        assert_eq!(&seen[0][..8], b"bundle-A");
        assert_eq!(&seen[1][..8], b"bundle-B");
    }

    #[test]
    fn bitflip_is_detected_and_suffix_discarded() {
        let mut store = DiskBlockStore::<3>::new();
        store.append_record(10, b"keep me").unwrap();
        store.append_record(11, b"corrupt").unwrap();
        // Flip a byte inside the sealed payload region covered by CRC32C.
        store.get_page_mut(1).unwrap()[WAL_PAGE_HEADER_LEN] ^= 0x01;
        store.append_record(12, b"after").unwrap();

        let mut count = 0;
        let report = store.recover_and_compact(|_| count += 1);
        assert_eq!(report.pages_valid, 1);
        assert_eq!(report.pages_discarded, 2);
        assert_eq!(count, 1);
        assert_eq!(store.written_pages(), 1);
        assert_eq!(store.read_record(0).unwrap().1, b"keep me");
        assert!(matches!(store.read_record(1), Err(DtnError::Truncated)));
    }
}
