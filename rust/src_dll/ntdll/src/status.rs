//! NTSTATUS values, mirrored by hand from Wine's `include/winternl.h` / ntstatus
//! (kept here so the crate stays free of any dependency on the Wine headers).

#![allow(dead_code)]

pub const STATUS_SUCCESS: i32 = 0x00000000;
pub const STATUS_INVALID_PARAMETER: i32 = 0xC000000Du32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: i32 = 0xC0000023u32 as i32;
