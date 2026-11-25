//! Backwards-compatible alias for [`crate::parser::extension_blocks`].
//!
//! New code should import the plural module directly; this shim keeps older
//! call sites (`use dtn_core::parser::extension_block::BlockType`) working.

pub use crate::parser::extension_blocks::*;
