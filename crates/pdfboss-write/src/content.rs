//! Content-stream serialization: `pdfboss_core::content::Op` values back to
//! operator syntax. The writer emits from the same IR the reader parses
//! into, so `parse_content(serialize_ops(ops)) == ops` is the module's
//! defining property — every variant of [`Op`] must round-trip.

use pdfboss_core::content::Op;

/// Serializes a sequence of content operators. Inline-image dictionaries
/// are written with their canonical (unabbreviated) keys, which the parser
/// passes through unchanged.
pub fn serialize_ops(ops: &[Op]) -> Vec<u8> {
    todo!("serialize {} ops", ops.len())
}
