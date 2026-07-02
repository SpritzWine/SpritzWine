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
const HIDP_STATUS_IS_VALUE_ARRAY: i32 = hidp_status(0xc, 0x0c);
const HIDP_STATUS_BUTTON_NOT_PRESSED: i32 = hidp_status(0xc, 0x0f);
const HIDP_STATUS_REPORT_DOES_NOT_EXIST: i32 = hidp_status(0xc, 0x10);

// ---- report types (ddk/hidpi.h HIDP_REPORT_TYPE) ----------------------------
const HIDP_INPUT: u32 = 0;
const HIDP_OUTPUT: u32 = 1;
const HIDP_FEATURE: u32 = 2;

// ---- value-caps flags (include/wine/hid.h) ----------------------------------
const HID_VALUE_CAPS_ARRAY_HAS_MORE: u32 = 0x01;
const HID_VALUE_CAPS_IS_BUTTON: u32 = 0x04;
const HID_VALUE_CAPS_IS_ABSOLUTE: u32 = 0x08;
const HID_VALUE_CAPS_IS_RANGE: u32 = 0x10;
const HID_VALUE_CAPS_IS_STRING_RANGE: u32 = 0x40;
const HID_VALUE_CAPS_IS_DESIGNATOR_RANGE: u32 = 0x80;

impl HidValueCaps {
    /// `HID_VALUE_CAPS_HAS_NULL` (bit_field & 0x40).
    fn has_null(&self) -> bool {
        self.bit_field & 0x40 != 0
    }
    /// `HID_VALUE_CAPS_IS_ARRAY` (bit_field & 2 == 0).
    fn is_array(&self) -> bool {
        self.bit_field & 2 == 0
    }
}

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
    link_usage_page: u16,
    link_usage: u16,
    flags: u32,
    _padding: [u32; 8],
    usage_min: u16,
    usage_max: u16,
    string_min: u16,
    string_max: u16,
    designator_min: u16,
    designator_max: u16,
    data_index_min: u16,
    data_index_max: u16,
    _null_value: u16,
    _unknown: u16,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    units: i32,
    units_exp: i32,
}

const _: () = assert!(size_of::<HidValueCaps>() == 104);
const _: () = assert!(core::mem::align_of::<HidValueCaps>() == 4);

/// Offset of `value_caps[0]` inside `struct hid_preparsed_data`, i.e. the 44-byte
/// header. The header fields the engine needs are read individually below.
const HEADER_SIZE: usize = 44;
/// Byte offsets of the header fields we read (see include/wine/hid.h layout).
const OFF_USAGE: usize = 8;
const OFF_USAGE_PAGE: usize = 10;
const OFF_INPUT_CAPS_START: usize = 16;
const OFF_INPUT_CAPS_COUNT: usize = 18;
const OFF_INPUT_REPORT_LEN: usize = 22;
const OFF_OUTPUT_CAPS_START: usize = 24;
const OFF_OUTPUT_CAPS_COUNT: usize = 26;
const OFF_OUTPUT_REPORT_LEN: usize = 30;
const OFF_FEATURE_CAPS_START: usize = 32;
const OFF_FEATURE_CAPS_COUNT: usize = 34;
const OFF_FEATURE_REPORT_LEN: usize = 38;
const OFF_CAPS_SIZE: usize = 40;
const OFF_NUMBER_LINK_NODES: usize = 42;

/// `struct hid_collection_node` (include/wine/hid.h), 16 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct HidCollectionNode {
    usage: u16,
    usage_page: u16,
    parent: u16,
    number_of_children: u16,
    next_sibling: u16,
    first_child: u16,
    collection_type: u32,
}
const _: () = assert!(size_of::<HidCollectionNode>() == 16);

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
    F: FnMut(&Preparsed, usize, &HidValueCaps) -> i32,
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

    // Matches the C `for (...; status == HIDP_STATUS_SUCCESS && caps != end; caps++)`:
    // the walk stops as soon as a callback returns non-SUCCESS. The value callbacks
    // return HIDP_STATUS_NULL, which ends the walk after a single match; the caps /
    // usage / data callbacks return SUCCESS and are applied to every match up to the
    // buffer limit.
    for i in 0..caps_count {
        if status != HIDP_STATUS_SUCCESS {
            break;
        }
        let idx = caps_start as usize + i as usize;
        let caps = unsafe { pp.cap(idx) };
        let is_range = caps.flags & HID_VALUE_CAPS_IS_RANGE != 0;
        if !match_value_caps(&caps, filter) {
            continue;
        }
        if filter.report_id != 0 && caps.report_id != filter.report_id {
            incompatible = true;
        } else if filter.array && (is_range || caps.report_count <= 1) {
            return HIDP_STATUS_NOT_VALUE_ARRAY;
        } else {
            let take = remaining > 0; // C `remaining-- > 0`: decrement regardless
            remaining -= 1;
            if take {
                status = op(pp, idx, &caps);
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
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |_, _, caps| {
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
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |_, _, caps| {
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
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |_, _, caps| {
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
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |_, _, caps| {
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
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |_, _, caps| {
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
        enum_value_caps(&pp, report_type, report_len, &filter, &mut count, |_, _, caps| {
            set_usage_value(caps, usage as u32, true, report, value)
        })
    }
}

// ============================================================================
//  Caps enumeration, usage/data read-write, collection nodes.
//  These complete the hidp.c migration; the shared enum_value_caps / copy_bits
//  above are reused so the whole file's untrusted-report handling is bounded.
// ============================================================================

/// `end - caps + 1` from the C array callbacks: 1 + the run of caps (starting at
/// `abs_index`) whose flags carry ARRAY_HAS_MORE.
unsafe fn array_index_min(pp: &Preparsed, abs_index: usize) -> u32 {
    let mut end = abs_index;
    while unsafe { pp.cap(end) }.flags & HID_VALUE_CAPS_ARRAY_HAS_MORE != 0 {
        end += 1;
    }
    (end - abs_index + 1) as u32
}

// ---- output structs (ddk/hidpi.h), byte-exact ------------------------------

#[repr(C)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_report_byte_length: u16,
    output_report_byte_length: u16,
    feature_report_byte_length: u16,
    reserved: [u16; 17],
    number_link_collection_nodes: u16,
    number_input_button_caps: u16,
    number_input_value_caps: u16,
    number_input_data_indices: u16,
    number_output_button_caps: u16,
    number_output_value_caps: u16,
    number_output_data_indices: u16,
    number_feature_button_caps: u16,
    number_feature_value_caps: u16,
    number_feature_data_indices: u16,
}
const _: () = assert!(size_of::<HidpCaps>() == 64);

/// HIDP_BUTTON_CAPS / HIDP_VALUE_CAPS share a trailing 8-`USHORT` union (Range vs
/// NotRange overlap at the same offsets). Indices: 0 Usage/UsageMin, 1 UsageMax,
/// 2 StringMin/StringIndex, 3 StringMax, 4 DesignatorMin/Index, 5 DesignatorMax,
/// 6 DataIndexMin/DataIndex, 7 DataIndexMax.
#[repr(C)]
struct HidpButtonCaps {
    usage_page: u16,
    report_id: u8,
    is_alias: u8,
    bit_field: u16,
    link_collection: u16,
    link_usage: u16,
    link_usage_page: u16,
    is_range: u8,
    is_string_range: u8,
    is_designator_range: u8,
    is_absolute: u8,
    reserved: [u32; 10],
    union_area: [u16; 8],
}
const _: () = assert!(size_of::<HidpButtonCaps>() == 72);

#[repr(C)]
struct HidpValueCaps {
    usage_page: u16,
    report_id: u8,
    is_alias: u8,
    bit_field: u16,
    link_collection: u16,
    link_usage: u16,
    link_usage_page: u16,
    is_range: u8,
    is_string_range: u8,
    is_designator_range: u8,
    is_absolute: u8,
    has_null: u8,
    reserved: u8,
    bit_size: u16,
    report_count: u16,
    reserved2: [u16; 5],
    units_exp: u32,
    units: u32,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    union_area: [u16; 8],
}
const _: () = assert!(size_of::<HidpValueCaps>() == 72);

#[repr(C)]
#[derive(Clone, Copy)]
struct HidpData {
    data_index: u16,
    reserved: u16,
    value: u32, // union { ULONG RawValue; BOOLEAN On; }
}
const _: () = assert!(size_of::<HidpData>() == 8);

#[repr(C)]
struct UsageAndPage {
    usage: u16,
    usage_page: u16,
}

#[repr(C)]
struct HidpLinkCollectionNode {
    link_usage: u16,
    link_usage_page: u16,
    parent: u16,
    number_of_children: u16,
    next_sibling: u16,
    first_child: u16,
    bits: u32, // CollectionType:8 | IsAlias:1 | Reserved:23
    user_context: *mut core::ffi::c_void,
}

fn bool_u8(b: bool) -> u8 {
    if b { 1 } else { 0 }
}

fn write_button_caps(dst: &mut HidpButtonCaps, caps: &HidValueCaps) {
    dst.usage_page = caps.usage_page;
    dst.report_id = caps.report_id;
    dst.link_collection = caps.link_collection;
    dst.link_usage_page = caps.link_usage_page;
    dst.link_usage = caps.link_usage;
    dst.bit_field = caps.bit_field as u16;
    dst.is_alias = 0;
    dst.is_absolute = bool_u8(caps.flags & HID_VALUE_CAPS_IS_ABSOLUTE != 0);
    dst.is_range = bool_u8(caps.flags & HID_VALUE_CAPS_IS_RANGE != 0);
    if dst.is_range == 0 {
        dst.union_area[0] = caps.usage_min; // NotRange.Usage
        dst.union_area[6] = caps.data_index_min; // NotRange.DataIndex
    } else {
        dst.union_area[0] = caps.usage_min; // Range.UsageMin
        dst.union_area[1] = caps.usage_max; // Range.UsageMax
        dst.union_area[6] = caps.data_index_min;
        dst.union_area[7] = caps.data_index_max;
    }
    dst.is_string_range = bool_u8(caps.flags & HID_VALUE_CAPS_IS_STRING_RANGE != 0);
    if dst.is_string_range == 0 {
        dst.union_area[2] = caps.string_min; // NotRange.StringIndex
    } else {
        dst.union_area[2] = caps.string_min;
        dst.union_area[3] = caps.string_max;
    }
    dst.is_designator_range = bool_u8(caps.flags & HID_VALUE_CAPS_IS_DESIGNATOR_RANGE != 0);
    if dst.is_designator_range == 0 {
        dst.union_area[4] = caps.designator_min; // NotRange.DesignatorIndex
    } else {
        dst.union_area[4] = caps.designator_min;
        dst.union_area[5] = caps.designator_max;
    }
}

fn write_value_caps(dst: &mut HidpValueCaps, caps: &HidValueCaps) {
    dst.usage_page = caps.usage_page;
    dst.report_id = caps.report_id;
    dst.link_collection = caps.link_collection;
    dst.link_usage_page = caps.link_usage_page;
    dst.link_usage = caps.link_usage;
    dst.bit_field = caps.bit_field as u16;
    dst.is_alias = 0;
    dst.is_absolute = bool_u8(caps.flags & HID_VALUE_CAPS_IS_ABSOLUTE != 0);
    dst.has_null = bool_u8(caps.has_null());
    dst.bit_size = caps.bit_size;
    dst.units_exp = caps.units_exp as u32;
    dst.units = caps.units as u32;
    dst.logical_min = caps.logical_min;
    dst.logical_max = caps.logical_max;
    dst.physical_min = caps.physical_min;
    dst.physical_max = caps.physical_max;
    dst.is_range = bool_u8(caps.flags & HID_VALUE_CAPS_IS_RANGE != 0);
    if dst.is_range == 0 {
        dst.report_count = caps.report_count;
        dst.union_area[0] = caps.usage_min;
        dst.union_area[6] = caps.data_index_min;
    } else {
        dst.report_count = 1;
        dst.union_area[0] = caps.usage_min;
        dst.union_area[1] = caps.usage_max;
        dst.union_area[6] = caps.data_index_min;
        dst.union_area[7] = caps.data_index_max;
    }
    dst.is_string_range = bool_u8(caps.flags & HID_VALUE_CAPS_IS_STRING_RANGE != 0);
    if dst.is_string_range == 0 {
        dst.union_area[2] = caps.string_min;
    } else {
        dst.union_area[2] = caps.string_min;
        dst.union_area[3] = caps.string_max;
    }
    dst.is_designator_range = bool_u8(caps.flags & HID_VALUE_CAPS_IS_DESIGNATOR_RANGE != 0);
    if dst.is_designator_range == 0 {
        dst.union_area[4] = caps.designator_min;
    } else {
        dst.union_area[4] = caps.designator_min;
        dst.union_area[5] = caps.designator_max;
    }
}

/// (button_caps, value_caps, data_indices) for a caps range, per HidP_GetCaps.
unsafe fn count_caps(pp: &Preparsed, start: u16, count: u16) -> (u16, u16, u16) {
    let (mut btn, mut val, mut idx): (u16, u16, u16) = (0, 0, 0);
    for i in 0..count {
        let c = unsafe { pp.cap(start as usize + i as usize) };
        if c.usage_min == 0 && c.usage_max == 0 {
            continue;
        }
        if c.flags & HID_VALUE_CAPS_IS_BUTTON != 0 {
            btn += 1;
        } else {
            val += 1;
        }
        if c.flags & HID_VALUE_CAPS_IS_RANGE == 0 {
            idx += 1;
        } else {
            idx += c.data_index_max - c.data_index_min + 1;
        }
    }
    (btn, val, idx)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_caps(preparsed: *const u8, caps_out: *mut u8) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    let caps = unsafe { &mut *(caps_out as *mut HidpCaps) };
    *caps = unsafe { core::mem::zeroed() };
    caps.usage = unsafe { pp.u16_at(OFF_USAGE) };
    caps.usage_page = unsafe { pp.u16_at(OFF_USAGE_PAGE) };
    caps.input_report_byte_length = unsafe { pp.u16_at(OFF_INPUT_REPORT_LEN) };
    caps.output_report_byte_length = unsafe { pp.u16_at(OFF_OUTPUT_REPORT_LEN) };
    caps.feature_report_byte_length = unsafe { pp.u16_at(OFF_FEATURE_REPORT_LEN) };
    caps.number_link_collection_nodes = unsafe { pp.u16_at(OFF_NUMBER_LINK_NODES) };

    let (is, ic) = unsafe { (pp.u16_at(OFF_INPUT_CAPS_START), pp.u16_at(OFF_INPUT_CAPS_COUNT)) };
    let (os, oc) = unsafe { (pp.u16_at(OFF_OUTPUT_CAPS_START), pp.u16_at(OFF_OUTPUT_CAPS_COUNT)) };
    let (fs, fc) = unsafe { (pp.u16_at(OFF_FEATURE_CAPS_START), pp.u16_at(OFF_FEATURE_CAPS_COUNT)) };
    (caps.number_input_button_caps, caps.number_input_value_caps, caps.number_input_data_indices) =
        unsafe { count_caps(&pp, is, ic) };
    (caps.number_output_button_caps, caps.number_output_value_caps, caps.number_output_data_indices) =
        unsafe { count_caps(&pp, os, oc) };
    (caps.number_feature_button_caps, caps.number_feature_value_caps, caps.number_feature_data_indices) =
        unsafe { count_caps(&pp, fs, fc) };
    HIDP_STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_specific_button_caps(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    caps_out: *mut u8,
    caps_count: *mut u16,
    preparsed: *const u8,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    let cap = usize::from(unsafe { *caps_count });
    let out = unsafe { core::slice::from_raw_parts_mut(caps_out as *mut HidpButtonCaps, cap) };
    let filter = Filter {
        buttons: true,
        values: false,
        array: false,
        usage_page: usage_page as u32 | USAGE_ANY,
        collection,
        usage: usage as u32 | USAGE_ANY,
        report_id: 0,
    };
    let mut count = unsafe { *caps_count };
    let mut written = 0usize;
    let status = unsafe {
        enum_value_caps(&pp, report_type, 0, &filter, &mut count, |_, _, caps| {
            write_button_caps(&mut out[written], caps);
            written += 1;
            HIDP_STATUS_SUCCESS
        })
    };
    unsafe { *caps_count = count };
    status
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_specific_value_caps(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usage: u16,
    caps_out: *mut u8,
    caps_count: *mut u16,
    preparsed: *const u8,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    let cap = usize::from(unsafe { *caps_count });
    let out = unsafe { core::slice::from_raw_parts_mut(caps_out as *mut HidpValueCaps, cap) };
    let filter = Filter {
        buttons: false,
        values: true,
        array: false,
        usage_page: usage_page as u32 | USAGE_ANY,
        collection,
        usage: usage as u32 | USAGE_ANY,
        report_id: 0,
    };
    let mut count = unsafe { *caps_count };
    let mut written = 0usize;
    let status = unsafe {
        enum_value_caps(&pp, report_type, 0, &filter, &mut count, |_, _, caps| {
            write_value_caps(&mut out[written], caps);
            written += 1;
            HIDP_STATUS_SUCCESS
        })
    };
    unsafe { *caps_count = count };
    status
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_link_collection_nodes(
    nodes_out: *mut u8,
    nodes_len: *mut u32,
    preparsed: *const u8,
) -> i32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return HIDP_STATUS_INVALID_PREPARSED_DATA,
    };
    let capacity = unsafe { *nodes_len };
    let count = unsafe { pp.u16_at(OFF_NUMBER_LINK_NODES) } as u32;
    unsafe { *nodes_len = count };
    if capacity < count {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    let caps_size = unsafe { pp.u16_at(OFF_CAPS_SIZE) } as usize;
    let base_off = HEADER_SIZE + caps_size; // (char*)value_caps + caps_size
    let nodes = unsafe { core::slice::from_raw_parts_mut(nodes_out as *mut HidpLinkCollectionNode, count as usize) };
    for (i, node) in nodes.iter_mut().enumerate() {
        let src: HidCollectionNode =
            unsafe { core::ptr::read_unaligned(pp.base.add(base_off + i * size_of::<HidCollectionNode>()) as *const HidCollectionNode) };
        node.link_usage_page = src.usage_page;
        node.link_usage = src.usage;
        node.parent = src.parent;
        node.number_of_children = src.number_of_children;
        node.next_sibling = src.next_sibling;
        node.first_child = src.first_child;
        node.bits = src.collection_type & 0xff; // CollectionType (8 bits), IsAlias = 0
        node.user_context = core::ptr::null_mut();
    }
    HIDP_STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_initialize_report_for_id(
    report_type: u32,
    report_id: u8,
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
    let (start, count, rlen) = match unsafe { pp.caps_range(report_type) } {
        Some(v) => v,
        None => return HIDP_STATUS_INVALID_REPORT_TYPE,
    };
    if rlen as u32 != report_len {
        return HIDP_STATUS_INVALID_REPORT_LENGTH;
    }
    let mut found = false;
    for i in 0..count {
        let c = unsafe { pp.cap(start as usize + i as usize) };
        if c.report_id == report_id && (c.usage_min != 0 || c.usage_max != 0) {
            found = true;
            break;
        }
    }
    if !found {
        return HIDP_STATUS_REPORT_DOES_NOT_EXIST;
    }
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    report.iter_mut().for_each(|b| *b = 0);
    report[0] = report_id;
    HIDP_STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_max_usage_list_length(report_type: u32, usage_page: u16, preparsed: *const u8) -> u32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return 0,
    };
    let filter = Filter {
        buttons: true,
        values: false,
        array: false,
        usage_page: usage_page as u32 | USAGE_ANY,
        collection: 0,
        usage: USAGE_ANY,
        report_id: 0,
    };
    let mut count: u32 = 0;
    let mut limit: u16 = u16::MAX;
    unsafe {
        enum_value_caps(&pp, report_type, 0, &filter, &mut limit, |_, _, caps| {
            count += caps.report_count as u32;
            HIDP_STATUS_SUCCESS
        })
    };
    count
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_max_data_list_length(report_type: u32, preparsed: *const u8) -> u32 {
    let pp = match unsafe { Preparsed::new(preparsed) } {
        Some(p) => p,
        None => return 0,
    };
    let filter = Filter {
        buttons: false,
        values: false,
        array: false,
        usage_page: USAGE_ANY,
        collection: 0,
        usage: USAGE_ANY,
        report_id: 0,
    };
    let mut count: u32 = 0;
    let mut limit: u16 = u16::MAX;
    unsafe {
        enum_value_caps(&pp, report_type, 0, &filter, &mut limit, |_, _, caps| {
            let is_button = caps.flags & HID_VALUE_CAPS_IS_BUTTON != 0;
            let is_range = caps.flags & HID_VALUE_CAPS_IS_RANGE != 0;
            count += if is_range || is_button { caps.report_count as u32 } else { 1 };
            HIDP_STATUS_SUCCESS
        })
    };
    count
}

/// Shared button read: appends pressed usages to `out` (up to `cap`), returning
/// the total that would be written (for overflow detection). `emit(usage_page,
/// usage, index)` writes one entry. Mirrors get_usage / get_usage_and_page.
fn walk_pressed_buttons(
    pp: &Preparsed,
    abs_index: usize,
    caps: &HidValueCaps,
    report: &[u8],
    total: &mut usize,
    mut emit: impl FnMut(usize, u16, u16),
) {
    let base = caps.start_byte as usize;
    if caps.is_array() {
        let index_min = unsafe { array_index_min(pp, abs_index) };
        let index_max = index_min + (caps.usage_max as u32 - caps.usage_min as u32);
        let mut bit = caps.start_bit as u32;
        let last = bit + caps.report_count as u32 * caps.bit_size as u32 - 1;
        while bit <= last {
            let index = get8(report, base + (bit / 8) as usize);
            if index != 0 && index >= index_min && index <= index_max {
                emit(*total, caps.usage_page, (caps.usage_min as u32 + index - index_min) as u16);
                *total += 1;
            }
            bit += 8;
        }
    } else {
        let mut bit = caps.start_bit as u32;
        let last = bit + (caps.usage_max as u32 - caps.usage_min as u32);
        while bit <= last {
            if get8(report, base + (bit / 8) as usize) & (1 << (bit % 8)) != 0 {
                emit(*total, caps.usage_page, (caps.usage_min as u32 + bit - caps.start_bit as u32) as u16);
                *total += 1;
            }
            bit += 1;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_usages(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usages_out: *mut u16,
    usages_len: *mut u32,
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
    let cap = unsafe { *usages_len } as usize;
    let out = unsafe { core::slice::from_raw_parts_mut(usages_out, cap) };
    let report = unsafe { core::slice::from_raw_parts(report, report_len as usize) };
    let filter = Filter {
        buttons: true,
        values: false,
        array: false,
        usage_page: usage_page as u32,
        collection,
        usage: USAGE_ANY,
        report_id: report[0],
    };
    let mut total = 0usize;
    let mut limit = u16::MAX;
    let status = unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut limit, |pp, idx, caps| {
            walk_pressed_buttons(pp, idx, caps, report, &mut total, |pos, _page, usage| {
                if pos < cap {
                    out[pos] = usage;
                }
            });
            HIDP_STATUS_SUCCESS
        })
    };
    unsafe { *usages_len = total as u32 };
    if status != HIDP_STATUS_SUCCESS {
        return status;
    }
    if total > cap {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    status
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_usages_ex(
    report_type: u32,
    collection: u16,
    usages_out: *mut u8,
    usages_len: *mut u32,
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
    let cap = unsafe { *usages_len } as usize;
    let out = unsafe { core::slice::from_raw_parts_mut(usages_out as *mut UsageAndPage, cap) };
    let report = unsafe { core::slice::from_raw_parts(report, report_len as usize) };
    let filter = Filter {
        buttons: true,
        values: false,
        array: false,
        usage_page: USAGE_ANY,
        collection,
        usage: USAGE_ANY,
        report_id: report[0],
    };
    let mut total = 0usize;
    let mut limit = u16::MAX;
    let status = unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut limit, |pp, idx, caps| {
            walk_pressed_buttons(pp, idx, caps, report, &mut total, |pos, page, usage| {
                if pos < cap {
                    out[pos].usage = usage;
                    out[pos].usage_page = page;
                }
            });
            HIDP_STATUS_SUCCESS
        })
    };
    unsafe { *usages_len = total as u32 };
    if status != HIDP_STATUS_SUCCESS {
        return status;
    }
    if total > cap {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    status
}

/// set_usage: light one button (array slot or bit). Bounded report writes.
fn op_set_usage(pp: &Preparsed, abs_index: usize, caps: &HidValueCaps, usage: u32, report: &mut [u8]) -> i32 {
    let base = caps.start_byte as usize;
    if caps.is_array() {
        let index_min = unsafe { array_index_min(pp, abs_index) };
        let mut bit = caps.start_bit as u32;
        let last = bit + caps.report_count as u32 * caps.bit_size as u32 - 1;
        while bit <= last {
            if get8(report, base + (bit / 8) as usize) != 0 {
                bit += 8;
                continue;
            }
            set8(report, base + (bit / 8) as usize, (index_min + usage - caps.usage_min as u32) as u8);
            return HIDP_STATUS_NULL;
        }
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    let bit = caps.start_bit as u32 + usage - caps.usage_min as u32;
    let i = base + (bit / 8) as usize;
    set8(report, i, (get8(report, i) | (1 << (bit % 8))) as u8);
    HIDP_STATUS_NULL
}

/// unset_usage: clear one button. Sets `found` when it was present.
fn op_unset_usage(pp: &Preparsed, abs_index: usize, caps: &HidValueCaps, usage: u32, report: &mut [u8], found: &mut bool) -> i32 {
    let base = caps.start_byte as usize;
    if caps.is_array() {
        let index_min = unsafe { array_index_min(pp, abs_index) };
        let index_max = index_min + (caps.usage_max as u32 - caps.usage_min as u32);
        let mut bit = caps.start_bit as u32;
        let last = bit + caps.report_count as u32 * caps.bit_size as u32 - 1;
        while bit <= last {
            let index = get8(report, base + (bit / 8) as usize);
            if index != 0 && index >= index_min && index <= index_max {
                set8(report, base + (bit / 8) as usize, 0);
                *found = true;
                break;
            }
            bit += 8;
        }
        return HIDP_STATUS_NULL;
    }
    let bit = caps.start_bit as u32 + usage - caps.usage_min as u32;
    let i = base + (bit / 8) as usize;
    let mask = 1u32 << (bit % 8);
    if get8(report, i) & mask != 0 {
        *found = true;
    }
    set8(report, i, (get8(report, i) & !mask) as u8);
    HIDP_STATUS_NULL
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_set_usages(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usages_in: *const u16,
    usage_count: *mut u32,
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
    let count = unsafe { *usage_count } as usize;
    let usages = unsafe { core::slice::from_raw_parts(usages_in, count) };
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    let report0 = report[0];
    for &usage in usages {
        let filter = Filter {
            buttons: true,
            values: false,
            array: false,
            usage_page: usage_page as u32,
            collection,
            usage: usage as u32,
            report_id: report0,
        };
        let mut limit: u16 = 1;
        let status = unsafe {
            enum_value_caps(&pp, report_type, report_len, &filter, &mut limit, |pp, idx, caps| {
                op_set_usage(pp, idx, caps, usage as u32, report)
            })
        };
        if status != HIDP_STATUS_SUCCESS {
            return status;
        }
    }
    HIDP_STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_unset_usages(
    report_type: u32,
    usage_page: u16,
    collection: u16,
    usages_in: *const u16,
    usage_count: *mut u32,
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
    let count = unsafe { *usage_count } as usize;
    let usages = unsafe { core::slice::from_raw_parts(usages_in, count) };
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    let report0 = report[0];
    let mut found = false;
    for &usage in usages {
        let filter = Filter {
            buttons: true,
            values: false,
            array: false,
            usage_page: usage_page as u32,
            collection,
            usage: usage as u32,
            report_id: report0,
        };
        let mut limit: u16 = 1;
        let status = unsafe {
            enum_value_caps(&pp, report_type, report_len, &filter, &mut limit, |pp, idx, caps| {
                op_unset_usage(pp, idx, caps, usage as u32, report, &mut found)
            })
        };
        if status != HIDP_STATUS_SUCCESS {
            return status;
        }
    }
    if !found {
        return HIDP_STATUS_BUTTON_NOT_PRESSED;
    }
    HIDP_STATUS_SUCCESS
}

/// find_all_data callback (HidP_GetData): append pressed buttons + single values.
fn op_find_all_data(pp: &Preparsed, abs_index: usize, caps: &HidValueCaps, report: &[u8], out: &mut [HidpData], total: &mut usize) -> i32 {
    if caps.bit_size == 0 {
        return HIDP_STATUS_SUCCESS;
    }
    let base = caps.start_byte as usize;
    if caps.is_array() {
        let index_min = unsafe { array_index_min(pp, abs_index) };
        let index_max = index_min + (caps.usage_max as u32 - caps.usage_min as u32);
        let mut bit = caps.start_bit as u32;
        let last = bit + caps.report_count as u32 * caps.bit_size as u32 - 1;
        while bit <= last {
            let index = get8(report, base + (bit / 8) as usize);
            if index != 0 && index >= index_min && index <= index_max {
                if *total < out.len() {
                    out[*total].data_index = (caps.data_index_min as u32 + index - index_min) as u16;
                    unsafe { (&mut out[*total].value as *mut u32 as *mut u8).write(1) };
                }
                *total += 1;
            }
            bit += 8;
        }
    } else if caps.flags & HID_VALUE_CAPS_IS_BUTTON != 0 {
        let mut bit = caps.start_bit as u32;
        let last = bit + (caps.usage_max as u32 - caps.usage_min as u32);
        while bit <= last {
            if get8(report, base + (bit / 8) as usize) & (1 << (bit % 8)) != 0 {
                if *total < out.len() {
                    out[*total].data_index = (caps.data_index_min as u32 + bit - caps.start_bit as u32) as u16;
                    unsafe { (&mut out[*total].value as *mut u32 as *mut u8).write(1) };
                }
                *total += 1;
            }
            bit += 1;
        }
    } else if caps.report_count == 1 {
        if *total < out.len() {
            let bit_count = caps.bit_size as i32 * caps.report_count as i32;
            if (bit_count as usize).div_ceil(8) > core::mem::size_of::<u32>() {
                return HIDP_STATUS_BUFFER_TOO_SMALL;
            }
            out[*total].data_index = caps.data_index_min;
            let mut raw = [0u8; 4];
            let src = report.get(base..).unwrap_or(&[]);
            copy_bits(&mut raw, src, bit_count, -(caps.start_bit as i32));
            out[*total].value = u32::from_le_bytes(raw);
        }
        *total += 1;
    }
    HIDP_STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_get_data(
    report_type: u32,
    data_out: *mut u8,
    data_len: *mut u32,
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
    let cap = unsafe { *data_len } as usize;
    let out = unsafe { core::slice::from_raw_parts_mut(data_out as *mut HidpData, cap) };
    let report = unsafe { core::slice::from_raw_parts(report, report_len as usize) };
    let filter = Filter {
        buttons: false,
        values: false,
        array: false,
        usage_page: USAGE_ANY,
        collection: 0,
        usage: USAGE_ANY,
        report_id: report[0],
    };
    let mut total = 0usize;
    let mut limit = u16::MAX;
    let status = unsafe {
        enum_value_caps(&pp, report_type, report_len, &filter, &mut limit, |pp, idx, caps| {
            op_find_all_data(pp, idx, caps, report, out, &mut total)
        })
    };
    unsafe { *data_len = total as u32 };
    if status != HIDP_STATUS_SUCCESS {
        return status;
    }
    if total > cap {
        return HIDP_STATUS_BUFFER_TOO_SMALL;
    }
    HIDP_STATUS_SUCCESS
}

/// set_data callback (HidP_SetData): write one HIDP_DATA into the report.
fn op_set_data(pp: &Preparsed, abs_index: usize, caps: &HidValueCaps, data: &HidpData, report: &mut [u8], found: &mut bool) -> i32 {
    if caps.bit_size == 0 {
        return HIDP_STATUS_SUCCESS;
    }
    if data.data_index < caps.data_index_min || data.data_index > caps.data_index_max {
        return HIDP_STATUS_SUCCESS;
    }
    *found = true;
    let base = caps.start_byte as usize;
    let raw_value = data.value;
    let on = (data.value & 0xff) as u8;

    if caps.is_array() {
        if caps.flags & HID_VALUE_CAPS_IS_RANGE == 0 {
            return HIDP_STATUS_IS_VALUE_ARRAY;
        }
        let index_min = unsafe { array_index_min(pp, abs_index) };
        let index_max = index_min + (caps.usage_max as u32 - caps.usage_min as u32);
        let lookup = index_min + data.data_index as u32 - caps.data_index_min as u32;
        let mut bit = caps.start_bit as u32;
        let last = bit + caps.report_count as u32 * caps.bit_size as u32 - 1;
        while bit <= last {
            let index = get8(report, base + (bit / 8) as usize);
            if index == 0 || index < index_min || index > index_max {
                break;
            }
            if raw_value == 0 && index == lookup {
                break;
            }
            bit += 8;
        }
        if bit > last {
            return if raw_value != 0 {
                HIDP_STATUS_BUFFER_TOO_SMALL
            } else {
                HIDP_STATUS_BUTTON_NOT_PRESSED
            };
        }
        let bi = base + (bit / 8) as usize;
        if raw_value != 0 {
            set8(report, bi, lookup as u8);
        } else if get8(report, bi) != lookup {
            return HIDP_STATUS_BUTTON_NOT_PRESSED;
        } else {
            // shift the array down over the removed slot (C advances bit by 1)
            let mut b = bit;
            while b < last {
                let byte = base + (b / 8) as usize;
                set8(report, byte, get8(report, byte + 1) as u8);
                b += 1;
            }
            set8(report, base + (b / 8) as usize, 0);
        }
    } else if caps.flags & HID_VALUE_CAPS_IS_BUTTON != 0 {
        let bit = caps.start_bit as u32 + (data.data_index as u32 - caps.data_index_min as u32) * caps.bit_size as u32;
        let mask = 1u32 << (bit % 8);
        let bi = base + (bit / 8) as usize;
        if on != 0 {
            set8(report, bi, (get8(report, bi) | mask) as u8);
        } else if get8(report, bi) & mask == 0 {
            return HIDP_STATUS_BUTTON_NOT_PRESSED;
        } else {
            set8(report, bi, (get8(report, bi) & !mask) as u8);
        }
    } else if caps.report_count == 1 {
        let bit_count = caps.bit_size as i32 * caps.report_count as i32;
        let dst = report.get_mut(base..).unwrap_or(&mut []);
        copy_bits(dst, &raw_value.to_le_bytes(), bit_count, -(caps.start_bit as i32));
    }
    HIDP_STATUS_SUCCESS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hidp_set_data(
    report_type: u32,
    data_in: *mut u8,
    data_len: *mut u32,
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
    let count = unsafe { *data_len } as usize;
    let data = unsafe { core::slice::from_raw_parts(data_in as *const HidpData, count) };
    let report = unsafe { core::slice::from_raw_parts_mut(report, report_len as usize) };
    let report0 = report[0];
    let mut status = HIDP_STATUS_SUCCESS;
    let mut found_any = false;
    let mut consumed = 0usize;
    for d in data {
        let mut found = false;
        let filter = Filter {
            buttons: false,
            values: false,
            array: false,
            usage_page: USAGE_ANY,
            collection: 0,
            usage: USAGE_ANY,
            report_id: report0,
        };
        let mut limit = u16::MAX;
        status = unsafe {
            enum_value_caps(&pp, report_type, report_len, &filter, &mut limit, |pp, idx, caps| {
                op_set_data(pp, idx, caps, d, report, &mut found)
            })
        };
        found_any |= found;
        if status != HIDP_STATUS_SUCCESS || !found {
            break;
        }
        consumed += 1;
    }
    unsafe { *data_len = consumed as u32 };
    if !found_any {
        return HIDP_STATUS_REPORT_DOES_NOT_EXIST;
    }
    status
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
        put(&mut blob, OFF_CAPS_SIZE, (caps.len() * size_of::<HidValueCaps>()) as u16);
        for (i, c) in caps.iter().enumerate() {
            let off = HEADER_SIZE + i * size_of::<HidValueCaps>();
            let bytes = unsafe { core::slice::from_raw_parts(c as *const _ as *const u8, size_of::<HidValueCaps>()) };
            blob[off..off + size_of::<HidValueCaps>()].copy_from_slice(bytes);
        }
        blob
    }

    /// A variable (non-array) button cap spanning usages `usage_min..=usage_max`
    /// as consecutive bits from `start_byte`/`start_bit`.
    fn button_cap(report_id: u8, usage_min: u16, usage_max: u16, start_byte: u16, start_bit: u8, data_index_min: u16) -> HidValueCaps {
        let mut c = zeroed_cap();
        c.report_id = report_id;
        c.usage_page = 9; // Button page
        c.usage_min = usage_min;
        c.usage_max = usage_max;
        c.start_byte = start_byte;
        c.start_bit = start_bit;
        c.bit_size = 1;
        c.report_count = usage_max - usage_min + 1;
        c.bit_field = 2; // Var -> not an array
        c.flags = HID_VALUE_CAPS_IS_BUTTON | HID_VALUE_CAPS_IS_ABSOLUTE;
        if usage_min != usage_max {
            c.flags |= HID_VALUE_CAPS_IS_RANGE;
        }
        c.data_index_min = data_index_min;
        c.data_index_max = data_index_min + (usage_max - usage_min);
        c
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
        c.bit_field = 2; // Var (not an array) -- real value caps set this
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

    // ---- the newly migrated functions ---------------------------------------

    #[test]
    fn get_caps_counts_buttons_and_values() {
        let caps = [value_cap(1, 0x30, 1, 0, 16), button_cap(1, 1, 3, 3, 0, 1)];
        let pp = mk_preparsed(&caps, 4);
        let mut out = [0u8; 64];
        let st = unsafe { rs_hidp_get_caps(pp.as_ptr(), out.as_mut_ptr()) };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        let caps = unsafe { &*(out.as_ptr() as *const HidpCaps) };
        assert_eq!(caps.number_input_value_caps, 1);
        assert_eq!(caps.number_input_button_caps, 1);
        assert_eq!(caps.input_report_byte_length, 4);
        // 1 value (not range) + a 3-button range => 1 + 3 data indices
        assert_eq!(caps.number_input_data_indices, 4);
    }

    #[test]
    fn get_usages_reports_pressed_buttons() {
        // buttons 1..=3 at byte 1 bit 0..2; press buttons 1 and 3
        let caps = [button_cap(1, 1, 3, 1, 0, 1)];
        let pp = mk_preparsed(&caps, 2);
        let report = [1u8, 0b0000_0101]; // bits 0 and 2 set -> usages 1 and 3
        let mut usages = [0u16; 4];
        let mut len: u32 = 4;
        let st = unsafe {
            rs_hidp_get_usages(HIDP_INPUT, 9, 0, usages.as_mut_ptr(), &mut len, pp.as_ptr(), report.as_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(len, 2);
        assert_eq!(&usages[..2], &[1, 3]);
    }

    #[test]
    fn set_then_unset_usage_roundtrip() {
        let caps = [button_cap(1, 1, 8, 1, 0, 1)];
        let pp = mk_preparsed(&caps, 2);
        let mut report = [1u8, 0];
        // set button 5
        let mut one = [5u16];
        let mut n: u32 = 1;
        let st = unsafe {
            rs_hidp_set_usages(HIDP_INPUT, 9, 0, one.as_ptr(), &mut n, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(report[1], 1 << 4); // button 5 => bit 4

        // GetUsages sees it
        let mut usages = [0u16; 4];
        let mut len: u32 = 4;
        unsafe {
            rs_hidp_get_usages(HIDP_INPUT, 9, 0, usages.as_mut_ptr(), &mut len, pp.as_ptr(), report.as_ptr(), report.len() as u32)
        };
        assert_eq!(&usages[..len as usize], &[5]);

        // unset it
        n = 1;
        let st = unsafe {
            rs_hidp_unset_usages(HIDP_INPUT, 9, 0, one.as_mut_ptr(), &mut n, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(report[1], 0);
        // unsetting again => BUTTON_NOT_PRESSED
        n = 1;
        let st = unsafe {
            rs_hidp_unset_usages(HIDP_INPUT, 9, 0, one.as_mut_ptr(), &mut n, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_BUTTON_NOT_PRESSED);
    }

    #[test]
    fn get_data_returns_buttons_and_values() {
        // value at byte1 (16 bits, data_index 0) + buttons 1..=2 at byte3 (data_index 1..2)
        let mut val = value_cap(1, 0x30, 1, 0, 16);
        val.data_index_min = 0;
        val.data_index_max = 0;
        let caps = [val, button_cap(1, 1, 2, 3, 0, 1)];
        let pp = mk_preparsed(&caps, 4);
        let report = [1u8, 0x34, 0x12, 0b10]; // value 0x1234, button 2 pressed
        let mut data = [HidpData { data_index: 0, reserved: 0, value: 0 }; 8];
        let mut len: u32 = 8;
        let st = unsafe {
            rs_hidp_get_data(HIDP_INPUT, data.as_mut_ptr() as *mut u8, &mut len, pp.as_ptr(), report.as_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(len, 2);
        // first: the value cap
        assert_eq!(data[0].data_index, 0);
        assert_eq!(data[0].value, 0x1234);
        // second: button 2 -> data_index 2, On
        assert_eq!(data[1].data_index, 2);
        assert_eq!(data[1].value & 0xff, 1);
    }

    #[test]
    fn get_specific_value_caps_fields() {
        let mut c = value_cap(1, 0x31, 1, 0, 12);
        c.logical_min = -100;
        c.logical_max = 100;
        c.report_count = 1;
        let pp = mk_preparsed(&[c], 3);
        let mut out = [0u8; 72];
        let mut n: u16 = 1;
        let st = unsafe {
            rs_hidp_get_specific_value_caps(HIDP_INPUT, 0, 0, 0, out.as_mut_ptr(), &mut n, pp.as_ptr())
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(n, 1);
        let vc = unsafe { &*(out.as_ptr() as *const HidpValueCaps) };
        assert_eq!(vc.usage_page, 1);
        assert_eq!(vc.bit_size, 12);
        assert_eq!(vc.logical_min, -100);
        assert_eq!(vc.logical_max, 100);
        assert_eq!(vc.is_range, 0);
        assert_eq!(vc.union_area[0], 0x31); // NotRange.Usage
    }

    #[test]
    fn max_usage_list_length_sums_report_counts() {
        let caps = [button_cap(1, 1, 8, 1, 0, 1)];
        let pp = mk_preparsed(&caps, 2);
        let n = unsafe { rs_hidp_max_usage_list_length(HIDP_INPUT, 9, pp.as_ptr()) };
        assert_eq!(n, 8);
    }

    #[test]
    fn initialize_report_writes_id_and_zeroes() {
        let caps = [value_cap(7, 0x30, 1, 0, 8)];
        let pp = mk_preparsed(&caps, 3);
        let mut report = [0xffu8; 3];
        let st = unsafe {
            rs_hidp_initialize_report_for_id(HIDP_INPUT, 7, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_SUCCESS);
        assert_eq!(report, [7, 0, 0]);
        // unknown report id => REPORT_DOES_NOT_EXIST
        let st = unsafe {
            rs_hidp_initialize_report_for_id(HIDP_INPUT, 9, pp.as_ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        assert_eq!(st, HIDP_STATUS_REPORT_DOES_NOT_EXIST);
    }
}
