//! The HidP_* value engine: caps walk + bounded bit-field read/write.
//!
//! Mirrors `dlls/hid/hidp.c`'s `enum_value_caps` / `copy_bits` / `get_usage_value`
//! / `set_usage_value` cluster, but every access into the untrusted report buffer
//! is bounds-checked (see the crate docs for the trust model). Return codes,
//! sign-extension, the scaled-value formula and `MulDiv`'s rounding are all
//! reproduced so output is bit-identical to the C for well-formed input.

#![allow(clippy::missing_safety_doc)]

use core::mem::size_of;

// ---- HIDP_STATUS codes (include/ddk/hidpi.h) --------------------------------
// HIDP_ERROR_CODES(sev, code) = (sev << 28) | (FACILITY_HID_ERROR_CODE << 16) | code
const fn hidp_status(sev: u32, code: u32) -> i32 {
    ((sev << 28) | (0x11 << 16) | code) as i32
}
const HIDP_STATUS_SUCCESS: i32 = hidp_status(0x0, 0x00);
const HIDP_STATUS_NULL: i32 = hidp_status(0x8, 0x01);
const HIDP_STATUS_INVALID_PREPARSED_DATA: i32 = hidp_status(0xc, 0x01);
const HIDP_STATUS_INVALID_REPORT_TYPE: i32 = hidp_status(0xc, 0x02);
const HIDP_STATUS_INVALID_REPORT_LENGTH: i32 = hidp_status(0xc, 0x03);
const HIDP_STATUS_USAGE_NOT_FOUND: i32 = hidp_status(0xc, 0x04);
const HIDP_STATUS_VALUE_OUT_OF_RANGE: i32 = hidp_status(0xc, 0x05);
const HIDP_STATUS_BAD_LOG_PHY_VALUES: i32 = hidp_status(0xc, 0x06);
const HIDP_STATUS_BUFFER_TOO_SMALL: i32 = hidp_status(0xc, 0x07);
const HIDP_STATUS_INCOMPATIBLE_REPORT_ID: i32 = hidp_status(0xc, 0x0a);
const HIDP_STATUS_NOT_VALUE_ARRAY: i32 = hidp_status(0xc, 0x0b);

// ---- report types (ddk/hidpi.h HIDP_REPORT_TYPE) ----------------------------
const HIDP_INPUT: u32 = 0;
const HIDP_OUTPUT: u32 = 1;
const HIDP_FEATURE: u32 = 2;

// ---- value-caps flags (include/wine/hid.h) ----------------------------------
const HID_VALUE_CAPS_IS_BUTTON: u32 = 0x04;
const HID_VALUE_CAPS_IS_RANGE: u32 = 0x10;

const USAGE_MASK: u32 = 0xffff;
const USAGE_ANY: u32 = 0x10000;

/// `struct hid_value_caps` (include/wine/hid.h). 104 bytes. Only the fields the
/// value engine touches are named; the rest is covered by `_padN` so the layout
/// (and every field offset the C reads) is byte-exact.
#[repr(C)]
#[derive(Clone, Copy)]
struct HidValueCaps {
    usage_page: u16,
    report_id: u8,
    start_bit: u8,
    bit_size: u16,
    report_count: u16,
    start_byte: u16,
    _total_bits: u16,
    bit_field: u32,
    _end_byte: u16,
    link_collection: u16,
    _link_usage_page: u16,
    _link_usage: u16,
    flags: u32,
    _padding: [u32; 8],
    usage_min: u16,
    usage_max: u16,
    _string_min: u16,
    _string_max: u16,
    _designator_min: u16,
    _designator_max: u16,
    _data_index_min: u16,
    _data_index_max: u16,
    _null_value: u16,
    _unknown: u16,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    _units: i32,
    _units_exp: i32,
}

const _: () = assert!(size_of::<HidValueCaps>() == 104);
const _: () = assert!(core::mem::align_of::<HidValueCaps>() == 4);

/// Offset of `value_caps[0]` inside `struct hid_preparsed_data`, i.e. the 44-byte
/// header. The header fields the engine needs are read individually below.
const HEADER_SIZE: usize = 44;
/// Byte offsets of the header fields we read (see include/wine/hid.h layout).
const OFF_INPUT_CAPS_START: usize = 16;
const OFF_INPUT_CAPS_COUNT: usize = 18;
const OFF_INPUT_REPORT_LEN: usize = 22;
const OFF_OUTPUT_CAPS_START: usize = 24;
const OFF_OUTPUT_CAPS_COUNT: usize = 26;
const OFF_OUTPUT_REPORT_LEN: usize = 30;
const OFF_FEATURE_CAPS_START: usize = 32;
const OFF_FEATURE_CAPS_COUNT: usize = 34;
const OFF_FEATURE_REPORT_LEN: usize = 38;

/// A magic-gated view over the preparsed blob. Per the trust model the blob comes
/// from our own hardened parser, so its declared cap counts are trusted (as the C
/// does); the report buffer, not this, is the untrusted surface.
struct Preparsed {
    base: *const u8,
}

impl Preparsed {
    /// # Safety
    /// `base` must point at a valid `struct hid_preparsed_data` (header + caps).
    unsafe fn new(base: *const u8) -> Option<Self> {
        if base.is_null() {
            return None;
        }
        // magic "HidP KDR" at offset 0
        let magic = b"HidP KDR";
        for (i, &m) in magic.iter().enumerate() {
            if unsafe { core::ptr::read(base.add(i)) } != m {
                return None;
            }
        }
        Some(Self { base })
    }

    unsafe fn u16_at(&self, off: usize) -> u16 {
        unsafe { core::ptr::read_unaligned(self.base.add(off) as *const u16) }
    }

    unsafe fn cap(&self, index: usize) -> HidValueCaps {
        let off = HEADER_SIZE + index * size_of::<HidValueCaps>();
        unsafe { core::ptr::read_unaligned(self.base.add(off) as *const HidValueCaps) }
    }

    /// Returns (caps_start, caps_count, report_byte_length) for the report type,
    /// or None for an invalid type.
    unsafe fn caps_range(&self, report_type: u32) -> Option<(u16, u16, u16)> {
        let (s, c, l) = match report_type {
            HIDP_INPUT => (OFF_INPUT_CAPS_START, OFF_INPUT_CAPS_COUNT, OFF_INPUT_REPORT_LEN),
            HIDP_OUTPUT => (OFF_OUTPUT_CAPS_START, OFF_OUTPUT_CAPS_COUNT, OFF_OUTPUT_REPORT_LEN),
            HIDP_FEATURE => (OFF_FEATURE_CAPS_START, OFF_FEATURE_CAPS_COUNT, OFF_FEATURE_REPORT_LEN),
            _ => return None,
        };
        Some(unsafe { (self.u16_at(s), self.u16_at(c), self.u16_at(l)) })
    }
}

/// Mirror of `struct caps_filter`. `usage`/`usage_page` use the `USAGE_ANY` /
/// `USAGE_MASK` encoding from the C.
struct Filter {
    buttons: bool,
    values: bool,
    array: bool,
    usage_page: u32,
    collection: u16,
    usage: u32,
    report_id: u8,
}

fn match_value_caps(caps: &HidValueCaps, f: &Filter) -> bool {
    if caps.usage_min == 0 && caps.usage_max == 0 {
        return false;
    }
    if f.buttons && (caps.flags & HID_VALUE_CAPS_IS_BUTTON) == 0 {
        return false;
    }
    if f.values && (caps.flags & HID_VALUE_CAPS_IS_BUTTON) != 0 {
        return false;
    }
    if f.usage_page != USAGE_ANY && (f.usage_page & USAGE_MASK) != caps.usage_page as u32 {
        return false;
    }
    if f.collection != 0 && f.collection != caps.link_collection {
        return false;
    }
    if f.usage == USAGE_ANY {
        return true;
    }
    caps.usage_min as u32 <= (f.usage & USAGE_MASK) && caps.usage_max as u32 >= (f.usage & USAGE_MASK)
}

/// Walks the caps for `report_type`, invoking `op` on each match, replicating
/// `enum_value_caps`'s count / status bookkeeping exactly. `count` is in/out.
///
/// # Safety
/// `pp` must be a valid preparsed view.
unsafe fn enum_value_caps<F>(
    pp: &Preparsed,
    report_type: u32,
    report_len: u32,
    filter: &Filter,
    count: &mut u16,
    mut op: F,
) -> i32
where
    F: FnMut(&HidValueCaps) -> i32,
{
    let (caps_start, caps_count, rlen) = match unsafe { pp.caps_range(report_type) } {
        Some(v) => v,
        None => return HIDP_STATUS_INVALID_REPORT_TYPE,
    };
    if report_len != 0 && report_len != rlen as u32 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }

    let mut remaining: i32 = *count as i32;
    let mut incompatible = false;
    let mut status = HIDP_STATUS_SUCCESS;

    for i in 0..caps_count {
        let caps = unsafe { pp.cap(caps_start as usize + i as usize) };
        let is_range = caps.flags & HID_VALUE_CAPS_IS_RANGE != 0;
        if !match_value_caps(&caps, filter) {
            continue;
        }
        if filter.report_id != 0 && caps.report_id != filter.report_id {
            incompatible = true;
        } else if filter.array && (is_range || caps.report_count <= 1) {
            return HIDP_STATUS_NOT_VALUE_ARRAY;
        } else {
            let take = remaining > 0;
            remaining -= 1;
            if take {
                status = op(&caps);
                if status != HIDP_STATUS_SUCCESS && status != HIDP_STATUS_NULL {
                    return status;
                }
            }
        }
    }

    if status == HIDP_STATUS_NULL {
        status = HIDP_STATUS_SUCCESS;
    }
    if status != HIDP_STATUS_SUCCESS {
        return status;
    }

    *count = (*count as i32 - remaining) as u16;
    if *count == 0 {
        return if incompatible {
            HIDP_STATUS_INCOMPATIBLE_REPORT_ID
        } else {
            HIDP_STATUS_USAGE_NOT_FOUND
        };
    }
    if remaining < 0 {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    HIDP_STATUS_SUCCESS
}

// ---- the bounded bit copier -------------------------------------------------

#[inline]
fn get8(s: &[u8], i: usize) -> u32 {
    s.get(i).copied().unwrap_or(0) as u32
}
#[inline]
fn rd16(s: &[u8], i: usize) -> u32 {
    get8(s, i) | (get8(s, i + 1) << 8)
}
#[inline]
fn set8(d: &mut [u8], i: usize, v: u8) {
    if let Some(b) = d.get_mut(i) {
        *b = v;
    }
}

/// Port of `copy_bits`: copy `count` bits between `src` and `dst`, with a signed
/// `shift` (source bit offset when < 0, destination bit offset when > 0). Reads
/// past `src`/writes past `dst` resolve to zero / are dropped instead of running
/// off the buffer; the trailing bytes the C over-reads are masked off anyway, so
/// in-bounds results are identical. `bits` is kept in a u32 but masked to 8 bits
/// exactly where the C truncates to `unsigned char`.
fn copy_bits(dst: &mut [u8], src: &[u8], mut count: i32, shift: i32) {
    if count <= 0 {
        return;
    }
    let src_shift: u32 = if shift < 0 { ((-shift) & 7) as u32 } else { 0 };
    let dst_shift: u32 = if shift > 0 { (shift & 7) as u32 } else { 0 };
    let mut si: usize = if shift < 0 { ((-shift) / 8) as usize } else { 0 };
    let mut di: usize = if shift > 0 { (shift / 8) as usize } else { 0 };

    if src_shift == 0 && dst_shift == 0 {
        let n = (count / 8) as usize;
        for k in 0..n {
            set8(dst, di + k, get8(src, si + k) as u8);
        }
        di += n;
        si += n;
        count &= 7;
    }
    if count == 0 {
        return;
    }

    let mut bits: u32 = ((get8(dst, di) << (8 - dst_shift)) & 0xff) as u32;
    count += dst_shift as i32;

    while count > 8 {
        set8(dst, di, (bits >> (8 - dst_shift)) as u8);
        bits = (rd16(src, si) >> src_shift) & 0xff;
        si += 1;
        let cur = get8(dst, di);
        set8(dst, di, (cur | ((bits << dst_shift) & 0xff)) as u8);
        di += 1;
        count -= 8;
    }

    bits >>= 8 - dst_shift;
    if count <= (8 - src_shift) as i32 {
        bits = (bits | ((get8(src, si) >> src_shift) << dst_shift)) & 0xff;
    } else {
        bits = (bits | ((rd16(src, si) >> src_shift) << dst_shift)) & 0xff;
    }

    let mask: u32 = (1u32 << count) - 1;
    let cur = get8(dst, di);
    set8(dst, di, ((bits & mask) | (cur & !mask)) as u8);
}

// ---- value get/set ----------------------------------------------------------

fn sign_extend(value: u32, caps: &HidValueCaps) -> i32 {
    let sign = 1u32 << (caps.bit_size - 1);
    if sign <= 1 || caps.logical_min >= 0 {
        return value as i32;
    }
    (value.wrapping_sub((value & sign) << 1)) as i32
}

// MulDiv: use the Win32 import in the PE build so hid.dll shares the one
// implementation (dlls/kernelbase/main.c) and, importantly, avoids emitting a
// 64-bit division that would pull Rust's non-SafeSEH `__divdi3` into the i386 PE
// link. The host-test build has no import, so it uses a faithful reimplementation.
#[cfg(not(test))]
unsafe extern "system" {
    safe fn MulDiv(a: i32, b: i32, c: i32) -> i32;
}

#[cfg(test)]
fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    if c == 0 {
        return -1;
    }
    let (mut a, mut c) = (a as i64, c as i64);
    let b = b as i64;
    if c < 0 {
        a = -a;
        c = -c;
    }
    let ret = if (a < 0 && b < 0) || (a >= 0 && b >= 0) {
        (a * b + c / 2) / c
    } else {
        (a * b - c / 2) / c
    };
    if ret > 2147483647 || ret < -2147483647 {
        return -1;
    }
    ret as i32
}

#[cfg(not(test))]
#[inline]
fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    MulDiv(a, b, c)
}

/// `get_usage_value` callback: extract a field into `value_buf` (already zeroed
/// by the caller path). `report` is the whole report, bounded.
fn get_usage_value(caps: &HidValueCaps, usage: u32, array: bool, report: &[u8], value_buf: &mut [u8]) -> i32 {
    let mut bit_count = caps.bit_size as i32;
    let mut bit_offset: u32 = 0;
    if array {
        bit_count *= caps.report_count as i32;
    } else {
        bit_offset = (usage.wrapping_sub(caps.usage_min as u32)) * caps.bit_size as u32;
    }

    if (bit_count as usize).div_ceil(8) > value_buf.len() {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    value_buf.iter_mut().for_each(|b| *b = 0);

    let src_start = caps.start_byte as usize + (bit_offset / 8) as usize;
    let src = report.get(src_start..).unwrap_or(&[]);
    let shift = -((caps.start_bit as i32) + (bit_offset % 8) as i32);
    copy_bits(value_buf, src, bit_count, shift);
    HIDP_STATUS_NULL
}

/// `set_usage_value` callback: write `value_buf` into the report field. Refuses
/// (INVALID_REPORT_LENGTH) if the field would run past the report -- the C would
/// write out of bounds here.
fn set_usage_value(caps: &HidValueCaps, usage: u32, array: bool, report: &mut [u8], value_buf: &[u8]) -> i32 {
    let mut bit_count = caps.bit_size as i32;
    let mut bit_offset: u32 = 0;
    if array {
        bit_count *= caps.report_count as i32;
    } else {
        bit_offset = (usage.wrapping_sub(caps.usage_min as u32)) * caps.bit_size as u32;
    }

    if (bit_count as usize).div_ceil(8) > value_buf.len() {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }

    let start_bit = caps.start_bit as u32 + bit_offset % 8;
    let dst_start = caps.start_byte as usize + (bit_offset / 8) as usize;
    // the field occupies bits [start_bit .. start_bit + bit_count) of report[dst_start..]
    let last_byte = dst_start + ((start_bit + bit_count as u32 + 7) / 8) as usize;
    if last_byte > report.len() {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }

    let dst = &mut report[dst_start..];
    copy_bits(dst, value_buf, bit_count, start_bit as i32);
    HIDP_STATUS_NULL
}

/// `get_scaled_usage_value`: raw extract then logical→physical scaling.
fn get_scaled_usage_value(caps: &HidValueCaps, usage: u32, report: &[u8], out: &mut i32) -> i32 {
    let mut raw_buf = [0u8; 4];
    let status = get_usage_value(caps, usage, false, report, &mut raw_buf);
    if status != HIDP_STATUS_NULL {
        return status;
    }
    let unsigned_value = u32::from_le_bytes(raw_buf);
    let signed_value = sign_extend(unsigned_value, caps);

    if caps.logical_min > caps.logical_max || caps.physical_min > caps.physical_max {
        return HIDP_STATUS_BAD_LOG_PHY_VALUES;
    }
    if caps.logical_min > signed_value || caps.logical_max < signed_value {
        return HIDP_STATUS_VALUE_OUT_OF_RANGE;
    }

    *out = if caps.physical_min == 0 && caps.physical_max == 0 {
        signed_value
    } else {
        caps.physical_min
            + mul_div(
                signed_value - caps.logical_min,
                caps.physical_max - caps.physical_min,
                caps.logical_max - caps.logical_min,
            )
    };
    HIDP_STATUS_NULL
}

/// `set_scaled_usage_value`: physical→logical then write.
fn set_scaled_usage_value(caps: &HidValueCaps, usage: u32, report: &mut [u8], value: i32) -> i32 {
    if caps.logical_min > caps.logical_max || caps.physical_min > caps.physical_max {
        return HIDP_STATUS_BAD_LOG_PHY_VALUES;
    }
    let mut v = value;
    if caps.physical_min != 0 || caps.physical_max != 0 {
        let log_range = (caps.logical_max - caps.logical_min + 1) / 2;
        let phy_range = (caps.physical_max - caps.physical_min + 1) / 2;
        v -= caps.physical_min;
        v = (log_range * v) / phy_range;
        v = caps.logical_min + v;
    }
    set_usage_value(caps, usage, false, report, &v.to_le_bytes())
}

// ---- FFI entry points -------------------------------------------------------
//
// Each mirrors one HidP_*UsageValue* function: build the filter, then walk the
// caps. `report_type` uses the HIDP_REPORT_TYPE ints; `report`/`value` come with
// explicit lengths; `preparsed` is the (magic-gated) blob pointer.

unsafe fn value_filter(usage_page: u16, collection: u16, usage: u16, array: bool, report0: u8) -> Filter {
    Filter {
        buttons: false,
        values: true,
        array,
        usage_page: usage_page as u32,
        collection,
        usage: usage as u32,
        report_id: report0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_usage_value(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    value_out: *mut u32,
    preparsed: *const u8,
    report: *const u8,
    report_len: u32,
) -> i32 {
    // matches the C: *value is only written on the success path (unlike the
    // scaled variant, which zeroes up front).
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    if report_len == 0 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let report = unsafe { core::slice::from_raw_parts(report, report_len as usize) };
    let filter = unsafe { value_filter(usage_page, collection, usage, false, report[0]) };
    let mut count: u16 = 1;
    let mut out = [0u8; 4];
    let status = unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |caps| {
            get_usage_value(caps, usage as u32, false, report, &mut out)
        })
    };
    if status == HIDP_STATUS_SUCCESS {
        unsafe { *value_out = u32::from_le_bytes(out) };
    }
    status
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_scaled_usage_value(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    value_out: *mut i32,
    preparsed: *const u8,
    report: *const u8,
    report_len: u32,
) -> i32 {
    unsafe { *value_out = 0 };
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    if report_len == 0 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let report = unsafe { core::slice::from_raw_parts(report, report_len as usize) };
    let filter = unsafe { value_filter(usage_page, collection, usage, false, report[0]) };
    let mut count: u16 = 1;
    let mut out: i32 = 0;
    let status = unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |caps| {
            get_scaled_usage_value(caps, usage as u32, report, &mut out)
        })
    };
    if status == HIDP_STATUS_SUCCESS {
        unsafe { *value_out = out };
    }
    status
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_usage_value_array(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    value_buf: *mut u8,
    value_len: u16,
    preparsed: *const u8,
    report: *const u8,
    report_len: u32,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    if report_len == 0 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let report = unsafe { core::slice::from_raw_parts(report, report_len as usize) };
    let value = unsafe { core::slice::from_raw_parts_mut(value_buf, value_len as usize) };
    let filter = unsafe { value_filter(usage_page, collection, usage, true, report[0]) };
    let mut count: u16 = 1;
    unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |caps| {
            get_usage_value(caps, usage as u32, true, report, value)
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_set_usage_value(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    value: u32,
    preparsed: *const u8,
    report: *mut u8,
    report_len: u32,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    if report_len == 0 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    let report0 = report[0];
    let val = value.to_le_bytes();
    let filter = unsafe { value_filter(usage_page, collection, usage, false, report0) };
    let mut count: u16 = 1;
    unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |caps| {
            set_usage_value(caps, usage as u32, false, report, &val)
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_set_scaled_usage_value(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    value: i32,
    preparsed: *const u8,
    report: *mut u8,
    report_len: u32,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    if report_len == 0 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    let report0 = report[0];
    let filter = unsafe { value_filter(usage_page, collection, usage, false, report0) };
    let mut count: u16 = 1;
    unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |caps| {
            set_scaled_usage_value(caps, usage as u32, report, value)
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_set_usage_value_array(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    value_buf: *const u8,
    value_len: u16,
    preparsed: *const u8,
    report: *mut u8,
    report_len: u32,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    if report_len == 0 {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    let value = unsafe { core::slice::from_raw_parts(value_buf, value_len as usize) };
    let report0 = report[0];
    let filter = unsafe { value_filter(usage_page, collection, usage, true, report0) };
    let mut count: u16 = 1;
    unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |caps| {
            set_usage_value(caps, usage as u32, true, report, value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed_cap() -> HidValueCaps {
        // repr(C), all-integer -> an all-zero bit pattern is a valid value.
        unsafe { core::mem::zeroed() }
    }

    /// Build a preparsed blob: 44-byte header + `caps` value-caps, all in the
    /// Input report. `report_len` is the declared input report byte length.
    fn mk_preparsed(caps: &[HidValueCaps], report_len: u16) -> Vec<u8> {
        let mut blob = vec![0u8; HEADER_SIZE + caps.len() * size_of::<HidValueCaps>()];
        blob[..8].copy_from_slice(b"HidP KDR");
        let put = |b: &mut [u8], off: usize, v: u16| b[off..off + 2].copy_from_slice(&v.to_le_bytes());
        put(&mut blob, OFF_INPUT_CAPS_START, 0);
        put(&mut blob, OFF_INPUT_CAPS_COUNT, caps.len() as u16);
        put(&mut blob, OFF_INPUT_REPORT_LEN, report_len);
        for (i, c) in caps.iter().enumerate() {
            let off = HEADER_SIZE + i * size_of::<HidValueCaps>();
            let bytes = unsafe { core::slice::from_raw_parts(c as *const _ as *const u8, size_of::<HidValueCaps>()) };
            blob[off..off + size_of::<HidValueCaps>()].copy_from_slice(bytes);
        }
        blob
    }

    /// A plain value cap: `size` bits at byte/bit offset, single usage `usage`.
    fn value_cap(report_id: u8, usage: u16, start_byte: u16, start_bit: u8, bit_size: u16) -> HidValueCaps {
        let mut c = zeroed_cap();
        c.report_id = report_id;
        c.usage_page = 1;
        c.usage_min = usage;
        c.usage_max = usage;
        c.start_byte = start_byte;
        c.start_bit = start_bit;
        c.bit_size = bit_size;
        c.report_count = 1;
        c.logical_min = 0;
        c.logical_max = (1i64 << bit_size).wrapping_sub(1) as i32;
        c
    }

    fn get_value(pp: &[u8], report: &[u8], usage: u16) -> (i32, u32) {
        let mut out: u32 = 0xdead;
        let st = unsafe {
            rs_hidp_get_usage_value(HIDP_INPUT, 1, 0, usage, &mut out, pp.as_ptr(), report.as_ptr(), report.len() as u32)
        };
        (st, out)
    }

    #[test]
    fn extract_byte_aligned_value() {
        let caps = [value_cap(1, 0x30, 1, 0, 8)];
        let pp = mk_preparsed(&caps, 3);
        let report = [1u8, 0xab, 0x00]; // report id 1, field byte = 0xab
        let (st, v) = get_value(&pp, &report, 0x30);
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(v, 0xab);
    }

    #[test]
    fn extract_value_with_bit_offset() {
        // 12-bit field starting at byte 1, bit 4
        let caps = [value_cap(1, 0x31, 1, 4, 12)];
        let pp = mk_preparsed(&caps, 4);
        // bits: byte1 = 0x50 (low nibble ignored, high nibble 0x5), byte2 = 0x63
        // field = ((0x63 << 4) | (0x50 >> 4)) & 0xfff = 0x635
        let report = [1u8, 0x50, 0x63, 0x00];
        let (st, v) = get_value(&pp, &report, 0x31);
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(v, 0x635);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let caps = [value_cap(1, 0x30, 1, 0, 16)];
        let pp = mk_preparsed(&caps, 3);
        let mut report = [1u8, 0, 0];
        let st = unsafe {
            rs_hidp_set_usage_value(HIDP_INPUT, 1, 0, 0x30, 0x1234, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(&report[1..], &[0x34, 0x12]); // little-endian in the report
        let (st, v) = get_value(&pp, &report, 0x30);
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(v, 0x1234);
    }

    #[test]
    fn usage_not_found() {
        let caps = [value_cap(1, 0x30, 1, 0, 8)];
        let pp = mk_preparsed(&caps, 3);
        let report = [1u8, 0xab, 0];
        let (st, _) = get_value(&pp, &report, 0x99); // no such usage
        assert_eq!(st, HIDP_STATUS_USAGE_NOT_FOUND);
    }

    #[test]
    fn incompatible_report_id() {
        let caps = [value_cap(7, 0x30, 1, 0, 8)]; // cap belongs to report id 7
        let pp = mk_preparsed(&caps, 3);
        let report = [1u8, 0xab, 0]; // report id 1
        let (st, _) = get_value(&pp, &report, 0x30);
        assert_eq!(st, HIDP_STATUS_INCOMPATIBLE_REPORT_ID);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut pp = mk_preparsed(&[value_cap(1, 0x30, 1, 0, 8)], 3);
        pp[0] = b'X';
        let report = [1u8, 0xab, 0];
        let (st, _) = get_value(&pp, &report, 0x30);
        assert_eq!(st, HIDP_STATUS_INVALID_PREPARSED_DATA);
    }

    #[test]
    fn invalid_report_length_rejected() {
        let caps = [value_cap(1, 0x30, 1, 0, 8)];
        let pp = mk_preparsed(&caps, 3); // declares length 3
        let mut out: u32 = 0;
        let report = [1u8, 0xab, 0, 0]; // length 4 != declared 3
        let st = unsafe {
            rs_hidp_get_usage_value(HIDP_INPUT, 1, 0, 0x30, &mut out, pp.as_ptr(), report.as_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_INVALID_REPORT_LENGTH);
    }

    // ---- the safety cases: crafted caps that point past the report ----------

    #[test]
    fn get_field_past_report_is_safe() {
        // cap claims a 32-bit field at byte 100, but the report is only 3 bytes.
        // enum_value_caps validates report_len == declared, so declare a huge
        // report length and pass a truthfully-sized short buffer: the C would
        // read at report+100. We must not, and must not crash.
        let caps = [value_cap(1, 0x30, 100, 0, 32)];
        let pp = mk_preparsed(&caps, 8); // declared input length 8
        // report_len must equal 8 to pass the length gate, but the field at
        // byte 100 is still out of even an 8-byte buffer.
        let report = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let (st, v) = get_value(&pp, &report, 0x30);
        // no panic, no OOB; out-of-range bytes read as zero
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(v, 0);
    }

    #[test]
    fn set_field_past_report_refused() {
        let caps = [value_cap(1, 0x30, 100, 0, 32)];
        let pp = mk_preparsed(&caps, 8);
        let mut report = [1u8, 0, 0, 0, 0, 0, 0, 0];
        let st = unsafe {
            rs_hidp_set_usage_value(HIDP_INPUT, 1, 0, 0x30, 0xffffffff, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        // must refuse rather than write out of bounds
        assert_eq!(st, HIDP_STATUS_INVALID_REPORT_LENGTH);
        assert_eq!(report, [1u8, 0, 0, 0, 0, 0, 0, 0]); // untouched
    }

    #[test]
    fn get_field_at_report_end_no_overread() {
        // 8-bit field exactly at the last byte: copy_bits' trailing 16-bit read
        // would touch one byte past; must stay safe and correct.
        let caps = [value_cap(1, 0x30, 2, 0, 8)];
        let pp = mk_preparsed(&caps, 3);
        let report = [1u8, 0, 0x7e]; // last byte
        let (st, v) = get_value(&pp, &report, 0x30);
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(v, 0x7e);
    }

    #[test]
    fn scaled_value_maps_logical_to_physical() {
        // logical 0..255 -> physical 0..1000; raw 128 -> ~502
        let mut c = value_cap(1, 0x30, 1, 0, 8);
        c.logical_min = 0;
        c.logical_max = 255;
        c.physical_min = 0;
        c.physical_max = 1000;
        let pp = mk_preparsed(&[c], 3);
        let report = [1u8, 128, 0];
        let mut out: i32 = 0;
        let st = unsafe {
            rs_hidp_get_scaled_usage_value(HIDP_INPUT, 1, 0, 0x30, &mut out, pp.as_ptr(), report.as_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(out, mul_div(128, 1000, 255)); // exact same formula as the C
    }

    #[test]
    fn buffer_too_small_for_array() {
        // array of 4x 8-bit = 32 bits, value buffer only 2 bytes
        let mut c = value_cap(1, 0x30, 1, 0, 8);
        c.report_count = 4;
        let pp = mk_preparsed(&[c], 6);
        let report = [1u8, 0x11, 0x22, 0x33, 0x44, 0];
        let mut buf = [0u8; 2];
        let st = unsafe {
            rs_hidp_get_usage_value_array(
                HIDP_INPUT, 1, 0, 0x30, buf.as_mut_ptr(), buf.len() as u16,
                pp.as_ptr(), report.as_ptr(), report.len() as u32,
            )
        };
        assert_eq!(st, HIDP_STATUS_BUFFER_TOO_SMALL);
    }

    #[test]
    fn get_usage_value_array_extracts_all() {
        let mut c = value_cap(1, 0x30, 1, 0, 8);
        c.report_count = 4;
        let pp = mk_preparsed(&[c], 6);
        let report = [1u8, 0x11, 0x22, 0x33, 0x44, 0];
        let mut buf = [0u8; 4];
        let st = unsafe {
            rs_hidp_get_usage_value_array(
                HIDP_INPUT, 1, 0, 0x30, buf.as_mut_ptr(), buf.len() as u16,
                pp.as_ptr(), report.as_ptr(), report.len() as u32,
            )
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);
    }
}
