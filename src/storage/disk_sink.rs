//! Sink de disco para persistencia Store-and-Forward (Direct I/O).

use crate::storage::wal::{PAGE_SIZE, WalWriter};

/// Sink en memoria estática o simulación de bloque en disco de tamaño fijo.
pub struct DiskBlockStore<const MAX_PAGES: usize> {
    blocks: [[u8; PAGE_SIZE]; MAX_PAGES],
    written_pages: usize,
}

impl<const MAX_PAGES: usize> DiskBlockStore<MAX_PAGES> {
    pub const fn new() -> Self {
        Self {
            blocks: [[0u8; PAGE_SIZE]; MAX_PAGES],
            written_pages: 0,
        }
    }

    pub fn written_pages(&self) -> usize {
        self.written_pages
    }

    pub fn get_page(&self, index: usize) -> Option<&[u8; PAGE_SIZE]> {
        if index < self.written_pages {
            Some(&self.blocks[index])
        } else {
            None
        }
    }
}

impl<const MAX_PAGES: usize> Default for DiskBlockStore<MAX_PAGES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX_PAGES: usize> WalWriter for DiskBlockStore<MAX_PAGES> {
    fn write_page(&mut self, page: &[u8]) -> Result<(), &'static str> {
        if self.written_pages >= MAX_PAGES {
            return Err("disk block store full");
        }
        self.blocks[self.written_pages].copy_from_slice(page);
        self.written_pages += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_store_writer() {
        let mut store = DiskBlockStore::<4>::new();
        let page_data = [0xEEu8; PAGE_SIZE];

        assert!(store.write_page(&page_data).is_ok());
        assert_eq!(store.written_pages(), 1);
        assert_eq!(store.get_page(0).unwrap()[0], 0xEE);
    }
}