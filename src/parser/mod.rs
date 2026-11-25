//! Zero-copy CBOR decoders for Bundle Protocol version 7 (RFC 9171).
//!
//! All functions borrow from caller-owned byte slices and report malformed
//! input through [`crate::error::DtnError`].

/// Low-level CBOR primitive helpers (major types, lengths, item skipping).
pub mod cbor;
/// RFC 9171 canonical extension and payload block parsers.
pub mod extension_blocks;
/// Compatibility alias of [`extension_blocks`].
pub mod extension_block;
/// RFC 9171 primary block parser.
pub mod primary_block;
