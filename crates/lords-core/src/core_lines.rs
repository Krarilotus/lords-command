//! Core lines (design: Entry metadata, `start` and `core`). Tests first: only the limit the tests name.

/// The longest core line, in UTF-8 bytes: the drafted lines' longest is 433 (31 Oct, after check r1).
pub const LINE_MAX_BYTES: usize = 450;
