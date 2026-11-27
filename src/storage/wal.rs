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
//! DTN forwarding records use a fixed 28-byte [`WalEnvelope`] in front of the
//! raw bundle:
//!
//! ```text
//! [state: u8 | reserved: [u8; 3] | bundle_uid: [u8; 16] | next_hop: u64 LE]
//! ```
//!
//! States are `0x01 = Pending`, `0x02 = Delivered` and `0x03 = PendingRoute`.
//! `bundle_uid` is the first 16 bytes of SHA-256 over the canonical BPv7
//! identity (source EID, creation timestamp, sequence number and fragment
//! coordinates), so it is stable even if a retransmission is re-encoded.
//! `Delivered` records are **compact tombstones**: only the 28-byte envelope
//! is written, no bundle bytes. Boot replay skips every `Pending` record whose
//! `bundle_uid` has a later `Delivered` tombstone, and re-evaluates the contact
//! plan for `PendingRoute` records instead of inventing a next hop.
//! CRC32C protects only the physical WAL page; it is never a bundle ID or a
//! BPSec context.
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
use crate::parser::primary_block::{parse_primary_block, ParsedBundleHeader};
use crate::storage::crc32c::crc32c;

/// Size of a flash sector / WAL page in bytes.
pub const PAGE_SIZE: usize = 4096;

/// Magic bytes identifying a sealed WAL page.
pub const WAL_PAGE_MAGIC: [u8; 4] = *b"DTNW";

/// Size of the WAL page header in bytes.
pub const WAL_PAGE_HEADER_LEN: usize = 24;

/// Maximum number of payload bytes stored in one WAL page.
pub const WAL_PAYLOAD_CAPACITY: usize = PAGE_SIZE - WAL_PAGE_HEADER_LEN;

/// Size of the fixed [`WalEnvelope`] prefix in a DTN WAL record.
pub const WAL_ENVELOPE_LEN: usize = 28;
/// Size of the 128-bit canonical BPv7 bundle identifier.
pub const BUNDLE_UID_LEN: usize = 16;
/// Envelope state for a bundle that still needs forwarding.
pub const WAL_STATE_PENDING: u8 = 0x01;
/// Envelope state for a bundle already acknowledged by the next hop.
pub const WAL_STATE_DELIVERED: u8 = 0x02;
/// Envelope state for a valid bundle stored while no contact was available.
pub const WAL_STATE_PENDING_ROUTE: u8 = 0x03;
/// Persisted `next_hop` for a bundle that had no active contact when sealed.
pub const WAL_NEXT_HOP_NONE: u64 = u64::MAX;

/// Maximum bytes of canonical identity material hashed into [`bundle_uid`].
const CANONICAL_IDENTITY_CAPACITY: usize = 128;
/// FNV-1a 128-bit offset basis, used only when `bpsec` (and `sha2`) is disabled.
const FNV1A_128_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
/// FNV-1a 128-bit prime, used only when `bpsec` (and `sha2`) is disabled.
const FNV1A_128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// Fixed routing/state metadata prepended to a forwarded BPv7 bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalEnvelope {
    /// `WAL_STATE_PENDING`, `WAL_STATE_DELIVERED` or `WAL_STATE_PENDING_ROUTE`.
    pub state: u8,
    /// First 16 bytes of SHA-256 over the canonical BPv7 bundle identity.
    pub bundle_uid: [u8; BUNDLE_UID_LEN],
    /// Next hop selected by the Contact Plan Scheduler when the record was sealed.
    pub next_hop: u64,
}

impl WalEnvelope {
    /// Creates an envelope; callers normally use [`wal_pending`],
    /// [`wal_pending_route`] or [`wal_delivered_uid`].
    pub const fn new(state: u8, bundle_uid: [u8; BUNDLE_UID_LEN], next_hop: u64) -> Self {
        Self { state, bundle_uid, next_hop }
    }

    /// Returns `true` for a `Pending` record.
    pub const fn is_pending(&self) -> bool {
        self.state == WAL_STATE_PENDING
    }

    /// Returns `true` for a compact `Delivered` tombstone.
    pub const fn is_delivered(&self) -> bool {
        self.state == WAL_STATE_DELIVERED
    }

    /// Returns `true` for a bundle stored while no route was available.
    pub const fn is_pending_route(&self) -> bool {
        self.state == WAL_STATE_PENDING_ROUTE
    }

    /// Returns `true` when this record still represents a bundle to forward.
    pub const fn needs_forwarding(&self) -> bool {
        self.is_pending() || self.is_pending_route()
    }

    /// Encodes `[envelope | bundle]` into `out` and returns the record length.
    pub fn encode(&self, bundle: &[u8], out: &mut [u8]) -> DtnResult<usize> {
        if !self.needs_forwarding() && !self.is_delivered() {
            return Err(DtnError::InvalidLength);
        }
        if self.is_delivered() && !bundle.is_empty() {
            return Err(DtnError::InvalidLength); // tombstones must stay compact
        }
        let total = WAL_ENVELOPE_LEN.checked_add(bundle.len())
            .ok_or(DtnError::LengthOverflow)?;
        let out = out.get_mut(..total).ok_or(DtnError::BufferTooSmall)?;
        out[0] = self.state;
        out[1..4].fill(0);
        out[4..4 + BUNDLE_UID_LEN].copy_from_slice(&self.bundle_uid);
        out[20..28].copy_from_slice(&self.next_hop.to_le_bytes());
        out[WAL_ENVELOPE_LEN..].copy_from_slice(bundle);
        Ok(total)
    }

    /// Decodes a record into `(envelope, raw_bundle_bytes)`.
    pub fn decode(record: &[u8]) -> DtnResult<(Self, &[u8])> {
        let header = record.get(..WAL_ENVELOPE_LEN).ok_or(DtnError::Truncated)?;
        let state = header[0];
        if state != WAL_STATE_PENDING
            && state != WAL_STATE_DELIVERED
            && state != WAL_STATE_PENDING_ROUTE
        {
            return Err(DtnError::InvalidLength);
        }
        if header[1..4].iter().any(|&b| b != 0) {
            return Err(DtnError::InvalidLength);
        }
        let mut uid = [0u8; BUNDLE_UID_LEN];
        uid.copy_from_slice(&header[4..4 + BUNDLE_UID_LEN]);
        let mut hop = [0u8; 8];
        hop.copy_from_slice(&header[20..28]);
        Ok((
            Self::new(state, uid, u64::from_le_bytes(hop)),
            &record[WAL_ENVELOPE_LEN..],
        ))
    }
}

/// Builds the canonical, allocation-free identity material of a bundle.
///
/// Mutable forwarding state (block flags other than the fragment bit, CRC
/// fields and block ordering) is deliberately excluded. Long text EIDs are
/// truncated to a bounded prefix with their original length recorded, so the
/// hash stays deterministic without a heap.
fn canonical_bundle_identity(
    header: &ParsedBundleHeader<'_>,
    out: &mut [u8; CANONICAL_IDENTITY_CAPACITY],
) -> DtnResult<usize> {
    fn push(
        bytes: &[u8],
        out: &mut [u8; CANONICAL_IDENTITY_CAPACITY],
        cursor: &mut usize,
    ) -> DtnResult<()> {
        let end = cursor.checked_add(bytes.len()).ok_or(DtnError::LengthOverflow)?;
        let dst = out.get_mut(*cursor..end).ok_or(DtnError::BufferTooSmall)?;
        dst.copy_from_slice(bytes);
        *cursor = end;
        Ok(())
    }

    let mut cursor = 0usize;
    push(b"BPv1", out, &mut cursor)?;
    push(&header.version.to_le_bytes(), out, &mut cursor)?;
    push(&header.creation_ts_sec.unwrap_or(0).to_le_bytes(), out, &mut cursor)?;
    push(&header.creation_seq.unwrap_or(0).to_le_bytes(), out, &mut cursor)?;
    push(&[u8::from(header.is_fragment())], out, &mut cursor)?;
    push(&header.fragment_offset.unwrap_or(0).to_le_bytes(), out, &mut cursor)?;
    push(&header.total_app_data_len.unwrap_or(0).to_le_bytes(), out, &mut cursor)?;
    match (header.source, header.source_node_id) {
        (Some(text), _) => {
            push(&[1u8], out, &mut cursor)?;
            let original = u16::try_from(text.len()).map_err(|_| DtnError::LengthOverflow)?;
            push(&original.to_le_bytes(), out, &mut cursor)?;
            push(&text.as_bytes()[..text.len().min(96)], out, &mut cursor)?;
        }
        (None, Some(node)) => {
            push(&[2u8], out, &mut cursor)?;
            push(&node.to_le_bytes(), out, &mut cursor)?;
        }
        (None, None) => push(&[0u8], out, &mut cursor)?,
    }
    Ok(cursor)
}

/// Hashes identity material to 128 bits with the configured backend.
fn hash_identity(bytes: &[u8]) -> [u8; BUNDLE_UID_LEN] {
    #[cfg(feature = "bpsec")]
    {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        let mut uid = [0u8; BUNDLE_UID_LEN];
        uid.copy_from_slice(&digest[..BUNDLE_UID_LEN]);
        uid
    }
    #[cfg(not(feature = "bpsec"))]
    {
        bundle_uid_fallback(bytes)
    }
}

/// Deterministic 128-bit FNV-1a fallback used when `bpsec` is disabled.
///
/// This is **not** cryptographic; it keeps `bundle_uid` stable and 128 bits
/// wide in builds that do not link the optional `sha2` dependency.
pub fn bundle_uid_fallback(bytes: &[u8]) -> [u8; BUNDLE_UID_LEN] {
    let mut hash = FNV1A_128_OFFSET;
    for &byte in bytes {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(FNV1A_128_PRIME);
    }
    hash.to_le_bytes()
}

/// Computes the 128-bit canonical BPv7 identity of a bundle.
///
/// Uses SHA-256 when the `bpsec` feature is enabled (the default and the
/// firmware configuration), otherwise the deterministic
/// [`bundle_uid_fallback`]. Malformed bundles fall back to hashing the raw
/// bytes so replay never panics on hostile input.
pub fn bundle_uid(bundle: &[u8]) -> [u8; BUNDLE_UID_LEN] {
    let mut canonical = [0u8; CANONICAL_IDENTITY_CAPACITY];
    let identity = match parse_primary_block(bundle) {
        Ok(header) => match canonical_bundle_identity(&header, &mut canonical) {
            Ok(len) => &canonical[..len],
            Err(_) => bundle,
        },
        Err(_) => bundle,
    };
    hash_identity(identity)
}

/// Builds a `Pending` envelope record for `bundle` and `next_hop`.
pub fn wal_pending(next_hop: u64, bundle: &[u8], out: &mut [u8]) -> DtnResult<usize> {
    WalEnvelope::new(WAL_STATE_PENDING, bundle_uid(bundle), next_hop)
        .encode(bundle, out)
}

/// Builds a `PendingRoute` record for a valid bundle with no active contact.
pub fn wal_pending_route(next_hop: u64, bundle: &[u8], out: &mut [u8]) -> DtnResult<usize> {
    WalEnvelope::new(WAL_STATE_PENDING_ROUTE, bundle_uid(bundle), next_hop)
        .encode(bundle, out)
}

/// Writes a compact `Delivered` tombstone (envelope only, no bundle bytes).
pub fn wal_delivered_uid(
    next_hop: u64,
    uid: [u8; BUNDLE_UID_LEN],
    out: &mut [u8],
) -> DtnResult<usize> {
    WalEnvelope::new(WAL_STATE_DELIVERED, uid, next_hop).encode(&[], out)
}

/// Computes `bundle_uid` and writes a compact `Delivered` tombstone.
pub fn wal_delivered(next_hop: u64, bundle: &[u8], out: &mut [u8]) -> DtnResult<usize> {
    wal_delivered_uid(next_hop, bundle_uid(bundle), out)
}

/// Decodes a WAL record; alias of [`WalEnvelope::decode`].
pub fn decode_wal_record(record: &[u8]) -> DtnResult<(WalEnvelope, &[u8])> {
    WalEnvelope::decode(record)
}

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

    /// Appends one DTN record: `[WalEnvelope | raw_bundle]` in a sealed page.
    ///
    /// This is the forwarding path. `uid` lets the caller seal a compact
    /// `Delivered` tombstone for a bundle whose bytes are no longer available.
    /// A `WAL_STATE_DELIVERED` record must carry an empty `bundle`.
    /// Returns the payload length written.
    pub fn append_record(
        &mut self,
        state: u8,
        uid: [u8; BUNDLE_UID_LEN],
        next_hop: u64,
        bundle: &[u8],
    ) -> DtnResult<usize> {
        if state == WAL_STATE_DELIVERED && !bundle.is_empty() {
            return Err(DtnError::InvalidLength);
        }
        let payload_len = WAL_ENVELOPE_LEN.checked_add(bundle.len())
            .ok_or(DtnError::LengthOverflow)?;
        if payload_len > WAL_PAYLOAD_CAPACITY || self.ready_pages >= PAGES {
            return Err(DtnError::WalFull);
        }
        let index = self.ready_pages;
        let page = &mut self.buf.0[index];
        for byte in page.iter_mut() {
            *byte = 0;
        }
        let payload = &mut page[WAL_PAGE_HEADER_LEN..WAL_PAGE_HEADER_LEN + payload_len];
        let written = WalEnvelope::new(state, uid, next_hop).encode(bundle, payload)?;
        seal_page(page, self.sequence, written)?;
        self.sequence = self.sequence.wrapping_add(1);
        self.ready_pages += 1;
        Ok(written)
    }

    /// Convenience: derives the 128-bit [`bundle_uid`] from `bundle` and seals
    /// the record with `append_record`.
    pub fn append_bundle(
        &mut self,
        state: u8,
        next_hop: u64,
        bundle: &[u8],
    ) -> DtnResult<usize> {
        self.append_record(state, bundle_uid(bundle), next_hop, bundle)
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

    const VALID_BUNDLE: &[u8] = &[
        0x9f, 0x88, 0x07, 0x00, 0x00,
        0x82, 0x01, 0x00, 0x82, 0x01, 0x00, 0x82, 0x01, 0x00,
        0x82, 0x00, 0x01, 0x19, 0xea, 0x60,
        0x85, 0x01, 0x01, 0x00, 0x00, 0x41, 0x58, 0xff,
    ];

    #[test]
    fn bundle_uid_is_canonical_stable_and_identity_sensitive() {
        let uid = bundle_uid(VALID_BUNDLE);
        assert_eq!(uid.len(), BUNDLE_UID_LEN);
        assert_eq!(uid, bundle_uid(VALID_BUNDLE));

        // Changing the creation timestamp sequence number changes the identity.
        let mut other = [0u8; VALID_BUNDLE.len()];
        other.copy_from_slice(VALID_BUNDLE);
        other[15] = 0x02; // creation timestamp sequence [0, 1] -> [0, 2]
        assert_ne!(uid, bundle_uid(&other));

        // A malformed bundle still yields a deterministic 128-bit identity.
        assert_eq!(bundle_uid(b"garbage"), bundle_uid(b"garbage"));
        assert_eq!(bundle_uid_fallback(b"garbage").len(), BUNDLE_UID_LEN);
    }

    #[test]
    fn envelope_round_trip_keeps_uid_state_and_next_hop() {
        let mut record = [0u8; 64];
        let len = wal_pending(42, VALID_BUNDLE, &mut record).unwrap();
        assert_eq!(len, WAL_ENVELOPE_LEN + VALID_BUNDLE.len());
        assert_eq!(&record[..4], &[WAL_STATE_PENDING, 0, 0, 0]);
        assert_eq!(&record[4..20], &bundle_uid(VALID_BUNDLE));
        let (env, payload) = decode_wal_record(&record[..len]).unwrap();
        assert!(env.is_pending());
        assert!(env.needs_forwarding());
        assert_eq!(env.next_hop, 42);
        assert_eq!(env.bundle_uid, bundle_uid(VALID_BUNDLE));
        assert_eq!(payload, VALID_BUNDLE);

        // A tombstone stores only the 28-byte envelope: no bundle bytes.
        let mut tombstone = [0u8; 64];
        let tlen = wal_delivered(42, VALID_BUNDLE, &mut tombstone).unwrap();
        assert_eq!(tlen, WAL_ENVELOPE_LEN);
        let (tenv, tpayload) = decode_wal_record(&tombstone[..tlen]).unwrap();
        assert!(tenv.is_delivered());
        assert!(!tenv.needs_forwarding());
        assert_eq!(tenv.bundle_uid, env.bundle_uid);
        assert_eq!(tenv.next_hop, 42);
        assert!(tpayload.is_empty());
    }

    #[test]
    fn pending_route_state_round_trips() {
        let mut record = [0u8; 64];
        let len = wal_pending_route(WAL_NEXT_HOP_NONE, VALID_BUNDLE, &mut record).unwrap();
        let (env, payload) = decode_wal_record(&record[..len]).unwrap();
        assert!(env.is_pending_route());
        assert!(env.needs_forwarding());
        assert!(!env.is_pending());
        assert_eq!(env.next_hop, WAL_NEXT_HOP_NONE);
        assert_eq!(payload, VALID_BUNDLE);
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let mut record = [0u8; WAL_ENVELOPE_LEN + 1];
        assert_eq!(
            wal_pending(1, b"x", &mut [0u8; WAL_ENVELOPE_LEN]),
            Err(DtnError::BufferTooSmall)
        );
        record[0] = 0x7f;
        assert!(decode_wal_record(&record).is_err());
        record[0] = WAL_STATE_PENDING;
        record[1] = 0x01; // reserved bytes must be zero
        assert!(decode_wal_record(&record).is_err());
        record[1] = 0;
        // Delivered records must not carry bundle bytes.
        record[0] = WAL_STATE_DELIVERED;
        assert!(WalEnvelope::new(WAL_STATE_DELIVERED, [0u8; 16], 0)
            .encode(b"x", &mut record)
            .is_err());
    }

    #[test]
    fn append_record_seals_envelope_in_page() {
        let mut wal = DirectWal::<1>::new();
        assert_eq!(
            wal.append_bundle(WAL_STATE_PENDING, 7, VALID_BUNDLE),
            Ok(WAL_ENVELOPE_LEN + VALID_BUNDLE.len())
        );
        let mut page = [0u8; PAGE_SIZE];
        assert!(wal.write_first_sealed_page(&mut page));
        let (seq, payload) = read_page(&page).unwrap();
        assert_eq!(seq, 0);
        let (env, recovered) = decode_wal_record(payload).unwrap();
        assert_eq!(env.next_hop, 7);
        assert_eq!(env.bundle_uid, bundle_uid(VALID_BUNDLE));
        assert_eq!(recovered, VALID_BUNDLE);
    }

    #[test]
    fn compact_tombstone_uses_only_the_envelope_payload() {
        let mut wal = DirectWal::<1>::new();
        let uid = bundle_uid(VALID_BUNDLE);
        assert_eq!(
            wal.append_record(WAL_STATE_DELIVERED, uid, 7, &[]),
            Ok(WAL_ENVELOPE_LEN)
        );
        let mut page = [0u8; PAGE_SIZE];
        assert!(wal.write_first_sealed_page(&mut page));
        let (_, payload) = read_page(&page).unwrap();
        assert_eq!(payload.len(), WAL_ENVELOPE_LEN);
        let (env, bundle) = decode_wal_record(payload).unwrap();
        assert!(env.is_delivered());
        assert_eq!(env.bundle_uid, uid);
        assert!(bundle.is_empty());
        // Supplying bundle bytes for a tombstone is refused atomically.
        assert!(wal
            .append_record(WAL_STATE_DELIVERED, uid, 7, b"must-not-happen")
            .is_err());
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
