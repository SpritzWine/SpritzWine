//! `RtlStringFromGUIDEx` — the Rust-side handle on ntdll's GUID formatter.
//!
//! The C implementation in `dlls/ntdll/rtlstr.c` is the single source of truth;
//! this module only declares the import and wraps it so Rust code in the crate
//! can format a GUID without building a `UNICODE_STRING` by hand. It is an
//! option kept for later use and is deliberately minimal: the non-allocating
//! form only (the allocating form would need `RtlFreeUnicodeString` and an
//! owner type nothing asks for yet).
//!
//! `RtlStringFromGUIDEx(guid, string, allocate)` writes the 38-WCHAR
//! `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` form. With `allocate == FALSE` it
//! formats into the caller's `string.Buffer` bounded by `string.MaximumLength`,
//! sets `string.Length` on success and returns `STATUS_BUFFER_TOO_SMALL` when
//! the buffer cannot hold it. Whether it also NUL-terminates when there is room,
//! and the exact `MaximumLength` threshold (38 or 39 WCHARs), are the C side's
//! call: the wrapper never depends on either -- it always offers room for a NUL
//! and only reads back the `Length` the callee reports.

use crate::status::{STATUS_SUCCESS, STATUS_UNSUCCESSFUL};

/// A `GUID`, laid out as the Windows struct.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// `UNICODE_STRING`. Both lengths are byte counts, not character counts.
#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

/// Length in UTF-16 units of the formatted string, `{` through `}` inclusive.
pub const GUID_STRING_LEN: usize = 38;

// Resolved against ntdll itself at the final link; WINAPI (stdcall on i686), so
// the i386 import decorates to `_RtlStringFromGUIDEx@12` as the spec declares.
#[cfg(not(test))]
unsafe extern "system" {
    fn RtlStringFromGUIDEx(guid: *const Guid, string: *mut UnicodeString, allocate: u8) -> i32;
}

/// Host stand-in for the ntdll import, honouring the documented contract so the
/// wrapper's buffer handling and error surfacing can be exercised without ntdll:
/// refuses (buffer untouched) when `MaximumLength` cannot hold the 38 units,
/// otherwise writes them, sets `Length`, and NUL-terminates only if there is
/// room. Windows formats the hex digits in uppercase.
#[cfg(test)]
#[allow(non_snake_case)]
unsafe fn RtlStringFromGUIDEx(guid: *const Guid, string: *mut UnicodeString, allocate: u8) -> i32 {
    use crate::status::STATUS_BUFFER_TOO_SMALL;

    assert_eq!(allocate, 0, "the wrapper only uses the non-allocating form");
    // SAFETY: the wrapper passes a valid reference and a descriptor of a live
    // buffer; this mock is only ever reached through it.
    let (guid, string) = unsafe { (&*guid, &mut *string) };
    let d = &guid.data4;
    let text = format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        guid.data1, guid.data2, guid.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    );
    let units: Vec<u16> = text.encode_utf16().collect();
    let capacity = usize::from(string.maximum_length) / 2;
    if capacity < units.len() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: `capacity` units is exactly what MaximumLength promises is there.
    let buf = unsafe { core::slice::from_raw_parts_mut(string.buffer, capacity) };
    buf[..units.len()].copy_from_slice(&units);
    if capacity > units.len() {
        buf[units.len()] = 0;
    }
    string.length = (units.len() * 2) as u16;
    STATUS_SUCCESS
}

/// Format `guid` into `buf` via ntdll, returning the number of UTF-16 units
/// written (always [`GUID_STRING_LEN`] on success).
///
/// `buf.len()` becomes the `MaximumLength` handed to the callee, so the C side
/// is bounded by the caller's real allocation and can never write past it.
/// Offer at least `GUID_STRING_LEN + 1` units to be safe against either
/// NUL-termination behaviour; a buffer the callee deems too short surfaces as
/// `Err(STATUS_BUFFER_TOO_SMALL)` from the callee, never as anything worse.
/// Any other failure is returned as the callee's `NTSTATUS`.
pub fn string_from_guid_into(guid: &Guid, buf: &mut [u16]) -> Result<usize, i32> {
    // MaximumLength is a u16 byte count; clamp so a huge slice cannot wrap it
    // into a small (or odd) value and fool the callee about its room.
    let max_bytes = buf.len().saturating_mul(2).min(usize::from(u16::MAX) & !1) as u16;
    let mut string = UnicodeString {
        length: 0,
        maximum_length: max_bytes,
        buffer: buf.as_mut_ptr(),
    };
    // SAFETY: `guid` is a valid reference; `string` describes exactly `buf`
    // (pointer + byte capacity), which is live and writable for the call.
    let status = unsafe { RtlStringFromGUIDEx(guid, &mut string, 0) };
    if status != STATUS_SUCCESS {
        return Err(status);
    }
    let written = usize::from(string.length) / 2;
    // A success that did not produce the full 38 units, or that claims more
    // than the buffer holds, is a contract violation on the C side; do not hand
    // back a truncated or fabricated string.
    if written != GUID_STRING_LEN || written > buf.len() {
        return Err(STATUS_UNSUCCESSFUL);
    }
    Ok(written)
}

/// Format `guid` as its 38-unit `{...}` string, with no allocation.
pub fn string_from_guid(guid: &Guid) -> Result<[u16; GUID_STRING_LEN], i32> {
    // One spare unit so the callee may NUL-terminate if it does.
    let mut buf = [0u16; GUID_STRING_LEN + 1];
    string_from_guid_into(guid, &mut buf)?;
    let mut out = [0u16; GUID_STRING_LEN];
    out.copy_from_slice(&buf[..GUID_STRING_LEN]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::STATUS_BUFFER_TOO_SMALL;

    // A real-world sample: an anti-cheat module class id, whose session object
    // name is what the game-side caller of RtlStringFromGUIDEx builds from it.
    const SAMPLE: Guid = Guid {
        data1: 0x0b9c0968,
        data2: 0xc6e4,
        data3: 0x4ce1,
        data4: [0x87, 0xd7, 0x28, 0xe0, 0x45, 0x54, 0x73, 0xea],
    };
    const SAMPLE_TEXT: &str = "{0B9C0968-C6E4-4CE1-87D7-28E0455473EA}";

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn formats_the_documented_shape() {
        let out = string_from_guid(&SAMPLE).unwrap();
        assert_eq!(out.len(), GUID_STRING_LEN);
        assert_eq!(&out[..], &units(SAMPLE_TEXT)[..]);
        assert_eq!(out[0], u16::from(b'{'));
        assert_eq!(out[GUID_STRING_LEN - 1], u16::from(b'}'));
    }

    #[test]
    fn into_reports_units_written() {
        let mut buf = [0u16; GUID_STRING_LEN + 1];
        assert_eq!(string_from_guid_into(&SAMPLE, &mut buf), Ok(GUID_STRING_LEN));
        assert_eq!(&buf[..GUID_STRING_LEN], &units(SAMPLE_TEXT)[..]);
    }

    #[test]
    fn exact_fit_is_the_callees_call_not_the_wrappers() {
        // The wrapper encodes no threshold of its own: a 38-unit buffer is
        // passed through as-is and it is the callee that accepts or refuses it.
        let mut buf = [0u16; GUID_STRING_LEN];
        assert_eq!(string_from_guid_into(&SAMPLE, &mut buf), Ok(GUID_STRING_LEN));
        assert_eq!(&buf[..], &units(SAMPLE_TEXT)[..]);
    }

    #[test]
    fn too_small_buffer_surfaces_the_status_and_stays_untouched() {
        let mut buf = [0u16; GUID_STRING_LEN - 1];
        assert_eq!(string_from_guid_into(&SAMPLE, &mut buf), Err(STATUS_BUFFER_TOO_SMALL));
        assert!(buf.iter().all(|&u| u == 0));
    }

    #[test]
    fn empty_buffer_is_an_error_not_a_write() {
        let mut buf: [u16; 0] = [];
        assert_eq!(string_from_guid_into(&SAMPLE, &mut buf), Err(STATUS_BUFFER_TOO_SMALL));
    }

    #[test]
    fn huge_buffer_clamps_maximum_length_instead_of_wrapping() {
        // 32768 units = 65536 bytes: a naive `as u16` would wrap to 0 and make
        // the callee refuse a buffer that is in fact enormous.
        let mut buf = vec![0u16; 32768];
        assert_eq!(string_from_guid_into(&SAMPLE, &mut buf), Ok(GUID_STRING_LEN));
        assert_eq!(&buf[..GUID_STRING_LEN], &units(SAMPLE_TEXT)[..]);
    }
}
