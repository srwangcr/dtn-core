//! Explicit, allocation-free error type shared by every `dtn-core` module.
//!
//! The library is `#![no_std]` and therefore does not use `std::error::Error`.
//! Every fallible operation in the hot path returns [`crate::error::DtnResult`] instead of a
//! bare string, so callers can match on a stable, `Copy` discriminant without
//! parsing text or allocating.

use core::fmt;

/// Discriminant returned by parsing, storage, framing and processing APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DtnError {
    /// The supplied slice contains no data at all.
    BufferEmpty,
    /// A CBOR item or frame announced more data than the slice contains.
    Truncated,
    /// The CBOR major type does not match the item being decoded.
    InvalidMajorType,
    /// An invalid CBOR additional-information value was encountered.
    InvalidAdditionalInfo,
    /// A length or count field is outside the allowed range.
    InvalidLength,
    /// A `u64` value cannot be represented in `usize` on this target.
    LengthOverflow,
    /// A definite-length encoding was required but an indefinite one was used.
    IndefiniteLengthForbidden,
    /// A CBOR text string is not valid UTF-8.
    InvalidUtf8,
    /// The caller-provided output buffer is smaller than required.
    BufferTooSmall,
    /// A COBS frame is malformed (bad code byte or embedded delimiter).
    InvalidFrame,
    /// A fragment overlaps bytes that were already received in the same slot.
    OverlappingFragment,
    /// A fragment's offset/length exceeds the declared total length.
    FragmentOutOfRange,
    /// A fragment disagrees with the total length of the open reassembly slot.
    LengthMismatch,
    /// A slot is already complete and cannot accept further fragments.
    AlreadyComplete,
    /// The fixed fragment-range table of a reassembly slot is exhausted.
    FragmentTableFull,
    /// A hop-count block reached or exceeded its hop limit.
    HopLimitExceeded,
    /// A hop-count block is malformed (`count > limit` or out of `u8` range).
    InvalidHopCount,
    /// A bundle-age value exceeds the configured maximum age.
    BundleAgeExceeded,
    /// A stored page failed its CRC32C integrity check or is a torn write.
    CrcMismatch,
    /// A recovered page does not carry the expected WAL magic.
    InvalidPageMagic,
    /// A recovered page sequence number is not strictly increasing.
    InvalidSequence,
    /// The write-ahead log has no capacity left for another record.
    WalFull,
    /// The sink rejected a page write.
    Io,
    /// A required BPSec Block Integrity Block was not present.
    MissingBib,
    /// The BPSec BIB is malformed, duplicated or uses an unknown context.
    InvalidBib,
    /// The BPSec BIB result does not match the recomputed MAC.
    BibVerificationFailed,
    /// A bounded queue or a single in-flight ARQ slot has no capacity left.
    QueueFull,
    /// A control frame was requested without a data frame awaiting commit.
    NoPendingCommit,
    /// A persistence state transition was requested without a pending queue entry.
    NoPendingPersist,
    /// A reliable serial frame is shorter than the 3-byte link header.
    InvalidLinkFrame,
    /// A reliable serial frame announced an unknown frame type.
    UnknownFrameType,
}

impl DtnError {
    /// Returns a stable, human-readable description of the error.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BufferEmpty => "empty buffer",
            Self::Truncated => "truncated input",
            Self::InvalidMajorType => "unexpected CBOR major type",
            Self::InvalidAdditionalInfo => "invalid CBOR additional information",
            Self::InvalidLength => "invalid length field",
            Self::LengthOverflow => "length exceeds addressable range",
            Self::IndefiniteLengthForbidden => "indefinite-length item is not allowed",
            Self::InvalidUtf8 => "invalid UTF-8 text string",
            Self::BufferTooSmall => "output buffer too small",
            Self::InvalidFrame => "invalid COBS frame",
            Self::OverlappingFragment => "overlapping fragment",
            Self::FragmentOutOfRange => "fragment outside declared total length",
            Self::LengthMismatch => "fragment total length mismatch",
            Self::AlreadyComplete => "reassembly slot already complete",
            Self::FragmentTableFull => "fragment range table full",
            Self::HopLimitExceeded => "hop limit exceeded",
            Self::InvalidHopCount => "invalid hop count block",
            Self::BundleAgeExceeded => "bundle age exceeded",
            Self::CrcMismatch => "CRC32C mismatch / torn page",
            Self::InvalidPageMagic => "invalid WAL page magic",
            Self::InvalidSequence => "non-monotonic WAL sequence number",
            Self::WalFull => "write-ahead log full",
            Self::Io => "I/O error on storage sink",
            Self::MissingBib => "required BPSec BIB is missing",
            Self::InvalidBib => "malformed or duplicated BPSec BIB",
            Self::BibVerificationFailed => "BPSec BIB integrity verification failed",
            Self::QueueFull => "bounded queue / in-flight ARQ slot is full",
            Self::NoPendingCommit => "no received frame is awaiting commit",
            Self::NoPendingPersist => "no outbound entry is awaiting verified persistence",
            Self::InvalidLinkFrame => "reliable serial frame shorter than link header",
            Self::UnknownFrameType => "unknown reliable serial frame type",
        }
    }
}

impl fmt::Display for DtnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Convenience alias for fallible operations in this crate.
pub type DtnResult<T> = Result<T, DtnError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_copy_and_descriptive() {
        let err = DtnError::CrcMismatch;
        let copy = err;
        assert_eq!(err, copy);
        assert_eq!(err.as_str(), "CRC32C mismatch / torn page");
        assert_eq!(alloc_free_format(err), "CRC32C mismatch / torn page");
    }

    fn alloc_free_format(err: DtnError) -> &'static str {
        err.as_str()
    }
}
