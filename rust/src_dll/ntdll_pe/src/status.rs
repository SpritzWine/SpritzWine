//! NTSTATUS values used by this crate, mirrored by hand from Wine's headers
//! (kept local so the crate stays free of any dependency on the Wine headers).

#![allow(dead_code)]

pub const STATUS_SUCCESS: i32 = 0x00000000;
pub const STATUS_BUFFER_TOO_SMALL: i32 = 0xC0000023u32 as i32;
pub const STATUS_BAD_COMPRESSION_BUFFER: i32 = 0xC0000242u32 as i32;
pub const STATUS_BAD_FUNCTION_TABLE: i32 = 0xC00000FFu32 as i32;
