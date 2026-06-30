//! HID report-descriptor parser, ported from `dlls/hidparse.sys/main.c`
//! (`parse_descriptor` and the helper cluster it drives).
//!
//! The descriptor is an untrusted, device-supplied byte stream. The C item loop
//! walked it with raw pointer arithmetic; this port walks a `&[u8]` with checked
//! indexing, which removes two latent out-of-bounds reads on malformed input:
//!
//!  * the C bounds check `ptr + size > end` is off-by-one -- a 4-byte item value
//!    ending exactly on `end` was read one byte past the buffer, and
//!  * the loop terminator `ptr != end` let that same item step `ptr` *over* `end`
//!    and run away.
//!
//! Both become a single `pos + 1 + size > len` check plus a `pos < len` loop here.
//! Every integer truncation / wrap the C relied on (USHORT/UCHAR fields, the
//! `count = -1` sentinel) is reproduced exactly with `as`/`wrapping_*`, so the
//! produced `hid_preparsed_data` is byte-identical for well-formed descriptors.

use alloc::vec::Vec;
use core::cmp::max;
use core::mem::size_of;

// ---- on-the-wire structs (must match include/wine/hid.h byte-for-byte) -------

/// `struct hid_value_caps` (include/wine/hid.h). 104 bytes, 4-aligned.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HidValueCaps {
    usage_page: u16,
    report_id: u8,
    start_bit: u8,
    bit_size: u16,
    report_count: u16,
    start_byte: u16,
    total_bits: u16,
    bit_field: u32,
    end_byte: u16,
    link_collection: u16,
    link_usage_page: u16,
    link_usage: u16,
    flags: u32,
    padding: [u32; 8],
    usage_min: u16,
    usage_max: u16,
    string_min: u16,
    string_max: u16,
    designator_min: u16,
    designator_max: u16,
    data_index_min: u16,
    data_index_max: u16,
    null_value: u16,
    unknown: u16,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    units: i32,
    units_exp: i32,
}

/// `struct hid_collection_node` (include/wine/hid.h). 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HidCollectionNode {
    usage: u16,
    usage_page: u16,
    parent: u16,
    number_of_children: u16,
    next_sibling: u16,
    first_child: u16,
    collection_type: u32,
}

/// Header of `struct hid_preparsed_data` (include/wine/hid.h), i.e. everything
/// before the trailing `value_caps[]` array -- 44 bytes. The value-caps and
/// collection-node arrays are written after it via computed offsets, matching the
/// `HID_*_VALUE_CAPS` / `HID_COLLECTION_NODES` macros.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HidPreparsedDataHeader {
    magic: [u8; 8],
    usage: u16,
    usage_page: u16,
    unknown: [u16; 2],
    input_caps_start: u16,
    input_caps_count: u16,
    input_caps_end: u16,
    input_report_byte_length: u16,
    output_caps_start: u16,
    output_caps_count: u16,
    output_caps_end: u16,
    output_report_byte_length: u16,
    feature_caps_start: u16,
    feature_caps_count: u16,
    feature_caps_end: u16,
    feature_report_byte_length: u16,
    caps_size: u16,
    number_link_collection_nodes: u16,
}

const HEADER_SIZE: usize = size_of::<HidPreparsedDataHeader>();

// Layout guards: the C side reads these structs at the same offsets, so any drift
// (field reorder, padding) must fail the build rather than corrupt parsing.
const _: () = assert!(size_of::<HidValueCaps>() == 104);
const _: () = assert!(core::mem::align_of::<HidValueCaps>() == 4);
const _: () = assert!(size_of::<HidCollectionNode>() == 16);
const _: () = assert!(HEADER_SIZE == 44);

// ---- item-tag decoding (from the C enums + SHORT_ITEM macro) -----------------

const fn short_item(tag: u8, ty: u8) -> u8 {
    (tag << 4) | (ty << 2)
}

// MAIN (type 0)
const M_INPUT: u8 = short_item(0x08, 0);
const M_OUTPUT: u8 = short_item(0x09, 0);
const M_FEATURE: u8 = short_item(0x0B, 0);
const M_COLLECTION: u8 = short_item(0x0A, 0);
const M_END_COLLECTION: u8 = short_item(0x0C, 0);
// GLOBAL (type 1)
const G_USAGE_PAGE: u8 = short_item(0, 1);
const G_LOGICAL_MIN: u8 = short_item(1, 1);
const G_LOGICAL_MAX: u8 = short_item(2, 1);
const G_PHYSICAL_MIN: u8 = short_item(3, 1);
const G_PHYSICAL_MAX: u8 = short_item(4, 1);
const G_UNIT_EXPONENT: u8 = short_item(5, 1);
const G_UNIT: u8 = short_item(6, 1);
const G_REPORT_SIZE: u8 = short_item(7, 1);
const G_REPORT_ID: u8 = short_item(8, 1);
const G_REPORT_COUNT: u8 = short_item(9, 1);
const G_PUSH: u8 = short_item(10, 1);
const G_POP: u8 = short_item(11, 1);
// LOCAL (type 2)
const L_USAGE: u8 = short_item(0, 2);
const L_USAGE_MIN: u8 = short_item(1, 2);
const L_USAGE_MAX: u8 = short_item(2, 2);
const L_DESIGNATOR_INDEX: u8 = short_item(3, 2);
const L_DESIGNATOR_MIN: u8 = short_item(4, 2);
const L_DESIGNATOR_MAX: u8 = short_item(5, 2);
const L_STRING_INDEX: u8 = short_item(7, 2);
const L_STRING_MIN: u8 = short_item(8, 2);
const L_STRING_MAX: u8 = short_item(9, 2);
const L_DELIMITER: u8 = short_item(10, 2);

// INPUT/OUTPUT/FEATURE bit-field flags (Device Class Definition for HID)
const INPUT_DATA_CONST: u32 = 0x01;
const INPUT_ARRAY_VAR: u32 = 0x02;
const INPUT_ABS_REL: u32 = 0x04;
const INPUT_NULL: u32 = 0x40;

// hid_value_caps::flags bits (include/wine/hid.h)
const HID_VALUE_CAPS_ARRAY_HAS_MORE: u32 = 0x01;
const HID_VALUE_CAPS_IS_CONSTANT: u32 = 0x02;
const HID_VALUE_CAPS_IS_BUTTON: u32 = 0x04;
const HID_VALUE_CAPS_IS_ABSOLUTE: u32 = 0x08;
const HID_VALUE_CAPS_IS_RANGE: u32 = 0x10;
const HID_VALUE_CAPS_IS_STRING_RANGE: u32 = 0x40;
const HID_VALUE_CAPS_IS_DESIGNATOR_RANGE: u32 = 0x80;

/// `HID_VALUE_CAPS_IS_ARRAY(c)`: `(bit_field & 2) == 0`.
#[inline]
fn is_array(bit_field: u32) -> bool {
    bit_field & INPUT_ARRAY_VAR == 0
}

// ---- helpers on hid_value_caps ----------------------------------------------

fn copy_global_items(dst: &mut HidValueCaps, src: &HidValueCaps) {
    dst.usage_page = src.usage_page;
    dst.logical_min = src.logical_min;
    dst.logical_max = src.logical_max;
    dst.physical_min = src.physical_min;
    dst.physical_max = src.physical_max;
    dst.units_exp = src.units_exp;
    dst.units = src.units;
    dst.bit_size = src.bit_size;
    dst.report_id = src.report_id;
    dst.report_count = src.report_count;
}

fn copy_collection_items(dst: &mut HidValueCaps, src: &HidValueCaps) {
    dst.link_collection = src.link_collection;
    dst.link_usage_page = src.link_usage_page;
    dst.link_usage = src.link_usage;
}

/// `array_reserve`: faithful port -- a *single* growth step per call (32, then
/// *3/2), returning `false` if that one step still cannot hold `index`. The
/// stepwise contract matches the C, including its failure mode for a usage list
/// larger than the current capacity allows.
fn array_reserve(vec: &mut Vec<HidValueCaps>, cap: &mut u32, index: u32) -> bool {
    if index < *cap {
        return true;
    }
    *cap = if *cap != 0 {
        ((*cap as u64) * 3 / 2) as u32
    } else {
        32
    };
    if *cap <= index {
        return false;
    }
    vec.resize(*cap as usize, HidValueCaps::default());
    true
}

// ---- parser state ------------------------------------------------------------

/// Port of `struct hid_parser_state`. The growable C arrays (`stack`,
/// `collections`, `values[3]`) become `Vec`s paired with the original capacity
/// counters so `array_reserve`'s semantics carry over unchanged. `stack` is
/// shared between the global push/pop and collection push/pop indices exactly as
/// in C (they write disjoint field sets, so the sharing is safe).
struct HidParserState {
    usage: u16,
    usage_page: u16,
    number_link_collection_nodes: u16,

    usages_page: [u16; 256],
    usages_min: [u16; 256],
    usages_max: [u16; 256],
    usages_size: u32,

    items: HidValueCaps,

    stack: Vec<HidValueCaps>,
    stack_cap: u32,
    global_idx: u32,
    collection_idx: u32,

    collections: Vec<HidValueCaps>,
    collections_cap: u32,

    values: [Vec<HidValueCaps>; 3],
    values_cap: [u32; 3],

    bit_size: [[u32; 256]; 3],
    byte_length: [u16; 3],
    caps_count: [u16; 3],
    empty_caps: [u16; 3],
    data_count: [u16; 3],
}

impl HidParserState {
    fn new() -> Self {
        Self {
            usage: 0,
            usage_page: 0,
            number_link_collection_nodes: 0,
            usages_page: [0; 256],
            usages_min: [0; 256],
            usages_max: [0; 256],
            usages_size: 0,
            items: HidValueCaps::default(),
            stack: Vec::new(),
            stack_cap: 0,
            global_idx: 0,
            collection_idx: 0,
            collections: Vec::new(),
            collections_cap: 0,
            values: [Vec::new(), Vec::new(), Vec::new()],
            values_cap: [0; 3],
            bit_size: [[0; 256]; 3],
            byte_length: [0; 3],
            caps_count: [0; 3],
            empty_caps: [0; 3],
            data_count: [0; 3],
        }
    }

    fn reset_local_items(&mut self) {
        let mut tmp = HidValueCaps::default();
        copy_global_items(&mut tmp, &self.items);
        copy_collection_items(&mut tmp, &self.items);
        self.items = HidValueCaps::default();
        copy_global_items(&mut self.items, &tmp);
        copy_collection_items(&mut self.items, &tmp);
        self.usages_page = [0; 256];
        self.usages_min = [0; 256];
        self.usages_max = [0; 256];
        self.usages_size = 0;
    }

    fn parse_global_push(&mut self) -> bool {
        if !array_reserve(&mut self.stack, &mut self.stack_cap, self.global_idx) {
            return false;
        }
        let items = self.items;
        copy_global_items(&mut self.stack[self.global_idx as usize], &items);
        self.global_idx += 1;
        true
    }

    fn parse_global_pop(&mut self) -> bool {
        if self.global_idx == 0 {
            return false;
        }
        self.global_idx -= 1;
        let src = self.stack[self.global_idx as usize];
        copy_global_items(&mut self.items, &src);
        true
    }

    fn parse_local_usage(&mut self, mut usage_page: u16, usage: u16) -> bool {
        if usage_page == 0 {
            usage_page = self.items.usage_page;
        }
        if self.items.flags & HID_VALUE_CAPS_IS_RANGE != 0 {
            self.usages_size = 0;
        }
        let idx = self.usages_size as usize;
        // C writes usages_page[usages_size] before the post-increment overflow
        // check; that index is always <= 255 there because the previous overflow
        // aborts parsing. Guard defensively so a logic slip can never index OOB.
        if idx >= 256 {
            return false;
        }
        self.usages_page[idx] = usage_page;
        self.usages_min[idx] = usage;
        self.usages_max[idx] = usage;
        self.items.usage_min = usage;
        self.items.usage_max = usage;
        self.items.flags &= !HID_VALUE_CAPS_IS_RANGE;
        self.usages_size += 1;
        self.usages_size <= 255
    }

    fn parse_local_usage_min(&mut self, mut usage_page: u16, usage: u16) {
        if usage_page == 0 {
            usage_page = self.items.usage_page;
        }
        if self.items.flags & HID_VALUE_CAPS_IS_RANGE == 0 {
            self.usages_max[0] = 0;
        }
        self.usages_page[0] = usage_page;
        self.usages_min[0] = usage;
        self.items.usage_min = usage;
        self.items.flags |= HID_VALUE_CAPS_IS_RANGE;
        self.usages_size = 1;
    }

    fn parse_local_usage_max(&mut self, mut usage_page: u16, usage: u16) {
        if usage_page == 0 {
            usage_page = self.items.usage_page;
        }
        if self.items.flags & HID_VALUE_CAPS_IS_RANGE == 0 {
            self.usages_min[0] = 0;
        }
        self.usages_page[0] = usage_page;
        self.usages_max[0] = usage;
        self.items.usage_max = usage;
        self.items.flags |= HID_VALUE_CAPS_IS_RANGE;
        self.usages_size = 1;
    }

    fn parse_new_collection(&mut self) -> bool {
        if !array_reserve(&mut self.stack, &mut self.stack_cap, self.collection_idx) {
            return false;
        }
        if !array_reserve(
            &mut self.collections,
            &mut self.collections_cap,
            self.number_link_collection_nodes as u32,
        ) {
            return false;
        }

        let items = self.items;
        copy_collection_items(&mut self.stack[self.collection_idx as usize], &items);
        self.collection_idx += 1;

        self.items.usage_min = self.usages_min[0];
        self.items.usage_max = self.usages_max[0];

        let n = self.number_link_collection_nodes as usize;
        let it = self.items;
        self.collections[n] = it;
        self.items.link_collection = self.number_link_collection_nodes;
        self.items.link_usage_page = self.items.usage_page;
        self.items.link_usage = self.items.usage_min;
        if self.number_link_collection_nodes == 0 {
            self.usage_page = self.items.usage_page;
            self.usage = self.items.usage_min;
        }
        self.number_link_collection_nodes = self.number_link_collection_nodes.wrapping_add(1);

        self.reset_local_items();
        true
    }

    fn parse_end_collection(&mut self) -> bool {
        if self.collection_idx == 0 {
            return false;
        }
        self.collection_idx -= 1;
        let src = self.stack[self.collection_idx as usize];
        copy_collection_items(&mut self.items, &src);
        self.reset_local_items();
        true
    }

    /// `i` is the destination index into the type's value-caps run (the value the
    /// C caller passes); the usage arrays are read at `usages_size - 1 - i`.
    fn add_new_value_caps(&mut self, t: usize, base: usize, i: usize, start_bit: u32) {
        let usages_size = max(1, self.usages_size);

        self.items.start_byte = (start_bit / 8) as u16;
        self.items.start_bit = (start_bit % 8) as u8;
        self.items.total_bits =
            (self.items.report_count as u32).wrapping_mul(self.items.bit_size as u32) as u16;
        self.items.end_byte = (start_bit
            .wrapping_add(self.items.total_bits as u32)
            .wrapping_add(7)
            / 8) as u16;

        let uidx = (usages_size - 1 - i as u32) as usize;
        self.items.usage_page = self.usages_page[uidx];
        self.items.usage_min = self.usages_min[uidx];
        self.items.usage_max = self.usages_max[uidx];

        // count = -1 sentinel (stored unsigned) when both usages are zero, else the
        // signed difference -- replicated exactly so data_index_max wraps as in C.
        let count: u32 = if self.items.usage_max == 0 && self.items.usage_min == 0 {
            u32::MAX
        } else {
            (self.items.usage_max as i32 - self.items.usage_min as i32) as u32
        };
        self.items.data_index_min = self.items.data_index_max.wrapping_add(1);
        self.items.data_index_max = self.items.data_index_min.wrapping_add(count as u16);

        let it = self.items;
        self.values[t][base + i] = it;

        let cap = &mut self.values[t][base + i];
        if cap.flags & HID_VALUE_CAPS_IS_BUTTON != 0 {
            if !is_array(cap.bit_field) {
                cap.logical_min = 0;
            } else {
                cap.logical_min = cap.logical_max;
            }
            cap.logical_max = 0;
            cap.physical_min = 0;
            cap.physical_max = 0;
        }
    }

    fn parse_new_value_caps(&mut self, t: usize) -> bool {
        let usage_page = self.items.usage_page;
        let report_count = self.items.report_count;
        let usages_size = max(1, self.usages_size);

        let rid = self.items.report_id as usize;
        let mut bs = self.bit_size[t][rid];
        if bs == 0 {
            bs = 8;
        }
        bs = bs.wrapping_add((self.items.bit_size as u32).wrapping_mul(self.items.report_count as u32));
        self.bit_size[t][rid] = bs;
        let nb = bs.wrapping_add(7) / 8;
        if nb > self.byte_length[t] as u32 {
            self.byte_length[t] = nb as u16;
        }
        let mut start_bit = bs;

        if self.items.report_count == 0 {
            self.empty_caps[t] = self.empty_caps[t].wrapping_add(usages_size as u16);
            self.reset_local_items();
            return true;
        }

        if !array_reserve(
            &mut self.values[t],
            &mut self.values_cap[t],
            self.caps_count[t] as u32 + usages_size,
        ) {
            return false;
        }
        let base = self.caps_count[t] as usize;

        let arr = is_array(self.items.bit_field);
        if !arr {
            self.items.report_count = self.items.report_count.wrapping_sub((usages_size - 1) as u16);
        } else {
            start_bit = start_bit
                .wrapping_sub((self.items.report_count as u32).wrapping_mul(self.items.bit_size as u32));
        }

        if self.items.bit_field & INPUT_ABS_REL == 0 {
            self.items.flags |= HID_VALUE_CAPS_IS_ABSOLUTE;
        }
        if self.items.bit_field & INPUT_DATA_CONST != 0 {
            self.items.flags |= HID_VALUE_CAPS_IS_CONSTANT;
        }
        if self.items.bit_size == 1 || arr {
            self.items.flags |= HID_VALUE_CAPS_IS_BUTTON;
        }

        if arr {
            self.items.null_value = self.items.logical_min as u16;
        } else if self.items.bit_field & INPUT_NULL == 0 {
            self.items.null_value = 0;
        } else {
            self.items.null_value = 1;
        }

        self.items.data_index_max = self.data_count[t].wrapping_sub(1);
        for i in 0..usages_size as usize {
            if !arr {
                start_bit = start_bit.wrapping_sub(
                    (self.items.report_count as u32).wrapping_mul(self.items.bit_size as u32),
                );
            } else if i != 0 {
                self.items.flags |= HID_VALUE_CAPS_ARRAY_HAS_MORE;
            } else {
                self.items.flags &= !HID_VALUE_CAPS_ARRAY_HAS_MORE;
            }
            let dst_i = if arr { usages_size as usize - i - 1 } else { i };
            self.add_new_value_caps(t, base, dst_i, start_bit);
            if !arr {
                self.items.report_count = 1;
            }
        }
        self.caps_count[t] = self.caps_count[t].wrapping_add(usages_size as u16);
        self.data_count[t] = self.items.data_index_max.wrapping_add(1);

        self.items.usage_page = usage_page;
        self.items.report_count = report_count;
        self.reset_local_items();
        true
    }

    // ---- serialization into the preparsed-data buffer ------------------------

    fn preparsed_size(&self) -> usize {
        let total_caps = self.caps_count[0] as usize
            + self.caps_count[1] as usize
            + self.caps_count[2] as usize
            + self.empty_caps[0] as usize
            + self.empty_caps[1] as usize
            + self.empty_caps[2] as usize;
        let caps_size = total_caps * size_of::<HidValueCaps>();
        HEADER_SIZE + caps_size + self.number_link_collection_nodes as usize * size_of::<HidCollectionNode>()
    }

    /// Write the `hid_preparsed_data` image into `out`, which must point at
    /// `self.preparsed_size()` writable, zero-or-garbage bytes (the buffer is
    /// zeroed here first, matching the C `memset`).
    ///
    /// # Safety
    /// `out` must be valid for `self.preparsed_size()` bytes and suitably aligned
    /// (pool/`alloc` blocks satisfy the 4-byte requirement).
    unsafe fn write_preparsed(&self, out: *mut u8) {
        let total_caps = self.caps_count[0] as usize
            + self.caps_count[1] as usize
            + self.caps_count[2] as usize
            + self.empty_caps[0] as usize
            + self.empty_caps[1] as usize
            + self.empty_caps[2] as usize;
        let caps_size = total_caps * size_of::<HidValueCaps>();
        let size = self.preparsed_size();

        unsafe { core::ptr::write_bytes(out, 0, size) };

        let input_caps_count = self.caps_count[0].wrapping_add(self.empty_caps[0]);
        let input_caps_end = self.caps_count[0]; // input_caps_start (0) + caps_count[0]
        let output_caps_start = input_caps_end;
        let output_caps_count = self.caps_count[1].wrapping_add(self.empty_caps[1]);
        let output_caps_end = output_caps_start.wrapping_add(self.caps_count[1]);
        let feature_caps_start = output_caps_end;
        let feature_caps_count = self.caps_count[2].wrapping_add(self.empty_caps[2]);
        let feature_caps_end = feature_caps_start.wrapping_add(self.caps_count[2]);

        let hdr = HidPreparsedDataHeader {
            magic: *b"HidP KDR",
            usage: self.usage,
            usage_page: self.usage_page,
            unknown: [0; 2],
            input_caps_start: 0,
            input_caps_count,
            input_caps_end,
            input_report_byte_length: self.byte_length[0],
            output_caps_start,
            output_caps_count,
            output_caps_end,
            output_report_byte_length: self.byte_length[1],
            feature_caps_start,
            feature_caps_count,
            feature_caps_end,
            feature_report_byte_length: self.byte_length[2],
            caps_size: caps_size as u16,
            number_link_collection_nodes: self.number_link_collection_nodes,
        };
        unsafe { core::ptr::write(out as *mut HidPreparsedDataHeader, hdr) };

        // value_caps[] starts right after the header; the three runs are packed
        // [input][output][feature] at their *_caps_start indices.
        let caps_ptr = unsafe { out.add(HEADER_SIZE) } as *mut HidValueCaps;
        let starts = [0usize, output_caps_start as usize, feature_caps_start as usize];
        for t in 0..3 {
            let count = self.caps_count[t] as usize;
            if count != 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        self.values[t].as_ptr(),
                        caps_ptr.add(starts[t]),
                        count,
                    );
                }
            }
        }

        // collection nodes follow the value-caps array.
        let n = self.number_link_collection_nodes as usize;
        let nodes_ptr = unsafe { out.add(HEADER_SIZE + caps_size) } as *mut HidCollectionNode;
        for i in 0..n {
            let src = self.collections[i];
            let node = unsafe { &mut *nodes_ptr.add(i) };
            node.usage_page = src.usage_page;
            node.usage = src.usage_min;
            node.parent = src.link_collection;
            node.collection_type = src.bit_field;
            node.first_child = 0;
            node.next_sibling = 0;
            node.number_of_children = 0;

            if i > 0 {
                let parent = src.link_collection as usize;
                // parent is an earlier node index for well-formed state; bound it
                // so corrupt state cannot index past the node array.
                if parent < i {
                    let pnode = unsafe { &mut *nodes_ptr.add(parent) };
                    node.next_sibling = pnode.first_child;
                    pnode.first_child = i as u16;
                    pnode.number_of_children = pnode.number_of_children.wrapping_add(1);
                }
            }
        }
    }
}

/// Run the descriptor item loop / state machine. Returns the populated parser
/// state, or `None` if the descriptor is malformed (truncated item value, a
/// stack/usage overflow, an unbalanced collection, or a delimiter) -- the same
/// conditions under which the C `parse_descriptor` took its `goto done` /
/// returned NULL, but without ever reading out of bounds.
fn run_parser(desc: &[u8]) -> Option<HidParserState> {
    let mut state = HidParserState::new();
    let len = desc.len();
    let mut pos = 0usize;

    while pos < len {
        let prefix = desc[pos];
        let mut size = (prefix & 0x03) as usize;
        if size == 3 {
            size = 4;
        }
        // Need `size` data bytes after the 1-byte prefix. The off-by-one in the C
        // check (`ptr + size > end`) and its `ptr != end` runaway are both fixed
        // by requiring the whole item to fit before reading any of it.
        if pos + 1 + size > len {
            return None;
        }

        let (value, signed_value): (u32, i32) = match size {
            0 => (0, 0),
            1 => {
                let v = desc[pos + 1] as u32;
                (v, v as u8 as i8 as i32)
            }
            2 => {
                let v = u16::from_le_bytes([desc[pos + 1], desc[pos + 2]]) as u32;
                (v, v as u16 as i16 as i32)
            }
            4 => {
                let v = u32::from_le_bytes([
                    desc[pos + 1],
                    desc[pos + 2],
                    desc[pos + 3],
                    desc[pos + 4],
                ]);
                (v, v as i32)
            }
            _ => unreachable!(),
        };

        state.items.bit_field = value;

        match prefix & short_item(0xf, 0x3) {
            M_INPUT => {
                if !state.parse_new_value_caps(0) {
                    return None;
                }
            }
            M_OUTPUT => {
                if !state.parse_new_value_caps(1) {
                    return None;
                }
            }
            M_FEATURE => {
                if !state.parse_new_value_caps(2) {
                    return None;
                }
            }
            M_COLLECTION => {
                if !state.parse_new_collection() {
                    return None;
                }
            }
            M_END_COLLECTION => {
                if !state.parse_end_collection() {
                    return None;
                }
            }

            G_USAGE_PAGE => state.items.usage_page = value as u16,
            G_LOGICAL_MIN => state.items.logical_min = signed_value,
            G_LOGICAL_MAX => state.items.logical_max = signed_value,
            G_PHYSICAL_MIN => state.items.physical_min = signed_value,
            G_PHYSICAL_MAX => state.items.physical_max = signed_value,
            G_UNIT_EXPONENT => state.items.units_exp = signed_value,
            G_UNIT => state.items.units = signed_value,
            G_REPORT_SIZE => state.items.bit_size = value as u16,
            G_REPORT_ID => state.items.report_id = value as u8,
            G_REPORT_COUNT => state.items.report_count = value as u16,
            G_PUSH => {
                if !state.parse_global_push() {
                    return None;
                }
            }
            G_POP => {
                if !state.parse_global_pop() {
                    return None;
                }
            }

            L_USAGE => {
                if !state.parse_local_usage((value >> 16) as u16, (value & 0xffff) as u16) {
                    return None;
                }
            }
            L_USAGE_MIN => state.parse_local_usage_min((value >> 16) as u16, (value & 0xffff) as u16),
            L_USAGE_MAX => state.parse_local_usage_max((value >> 16) as u16, (value & 0xffff) as u16),
            L_DESIGNATOR_INDEX => {
                state.items.designator_min = value as u16;
                state.items.designator_max = value as u16;
                state.items.flags &= !HID_VALUE_CAPS_IS_DESIGNATOR_RANGE;
            }
            L_DESIGNATOR_MIN => {
                state.items.designator_min = value as u16;
                state.items.flags |= HID_VALUE_CAPS_IS_DESIGNATOR_RANGE;
            }
            L_DESIGNATOR_MAX => {
                state.items.designator_max = value as u16;
                state.items.flags |= HID_VALUE_CAPS_IS_DESIGNATOR_RANGE;
            }
            L_STRING_INDEX => {
                state.items.string_min = value as u16;
                state.items.string_max = value as u16;
                state.items.flags &= !HID_VALUE_CAPS_IS_STRING_RANGE;
            }
            L_STRING_MIN => {
                state.items.string_min = value as u16;
                state.items.flags |= HID_VALUE_CAPS_IS_STRING_RANGE;
            }
            L_STRING_MAX => {
                state.items.string_max = value as u16;
                state.items.flags |= HID_VALUE_CAPS_IS_STRING_RANGE;
            }
            L_DELIMITER => return None, // delimiter not implemented (matches C goto done)

            // Unknown / reserved item type: the C parser warns and keeps going.
            _ => {}
        }

        pos += size + 1;
    }

    Some(state)
}

/// Parse a HID report descriptor into a freshly allocated `hid_preparsed_data`
/// buffer, returning a pointer the C caller owns and frees with `ExFreePool`
/// (or `NULL` on a malformed descriptor or allocation failure). Drop-in for the
/// C `parse_descriptor`.
///
/// # Safety
/// `descriptor` must point at `length` readable bytes (or be null when `length`
/// is 0). `pool_type` is a `POOL_TYPE`.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_hid_parse_descriptor(
    descriptor: *const u8,
    length: u32,
    pool_type: u32,
) -> *mut core::ffi::c_void {
    let desc: &[u8] = if length == 0 || descriptor.is_null() {
        &[]
    } else {
        unsafe { core::slice::from_raw_parts(descriptor, length as usize) }
    };

    let state = match run_parser(desc) {
        Some(s) => s,
        None => return core::ptr::null_mut(),
    };

    let size = state.preparsed_size();
    let out = unsafe { crate::heap::ExAllocatePool(pool_type, size) };
    if out.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { state.write_preparsed(out as *mut u8) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the preparsed image into a host `Vec` (the real entry point allocates
    /// it from the kernel pool instead).
    fn parse_to_buffer(desc: &[u8]) -> Option<Vec<u8>> {
        let state = run_parser(desc)?;
        let size = state.preparsed_size();
        let mut buf = alloc::vec![0u8; size];
        unsafe { state.write_preparsed(buf.as_mut_ptr()) };
        Some(buf)
    }

    fn header(buf: &[u8]) -> HidPreparsedDataHeader {
        assert!(buf.len() >= HEADER_SIZE);
        unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const HidPreparsedDataHeader) }
    }

    // A standard 2-axis, 8-button joystick (Generic Desktop / Joystick).
    const JOYSTICK: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x04, // Usage (Joystick)
        0xA1, 0x01, // Collection (Application)
        0x09, 0x30, //   Usage (X)
        0x09, 0x31, //   Usage (Y)
        0x15, 0x81, //   Logical Minimum (-127)
        0x25, 0x7F, //   Logical Maximum (127)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x02, //   Report Count (2)
        0x81, 0x02, //   Input (Data,Var,Abs)
        0x05, 0x09, //   Usage Page (Button)
        0x19, 0x01, //   Usage Minimum (1)
        0x29, 0x08, //   Usage Maximum (8)
        0x15, 0x00, //   Logical Minimum (0)
        0x25, 0x01, //   Logical Maximum (1)
        0x75, 0x01, //   Report Size (1)
        0x95, 0x08, //   Report Count (8)
        0x81, 0x02, //   Input (Data,Var,Abs)
        0xC0, //       End Collection
    ];

    #[test]
    fn parses_joystick_top_level() {
        let buf = parse_to_buffer(JOYSTICK).expect("well-formed descriptor parses");
        let h = header(&buf);
        assert_eq!(&h.magic, b"HidP KDR");
        assert_eq!(h.usage_page, 0x01); // Generic Desktop
        assert_eq!(h.usage, 0x04); // Joystick
        assert_eq!(h.number_link_collection_nodes, 1);
        // 2 axis caps (X, Y) + 1 button-range cap = 3 input value caps.
        assert_eq!(h.input_caps_start, 0);
        assert_eq!(h.input_caps_end, 3);
        // initial report-id byte (8b) + 2*8b axes + 8*1b buttons = 32b = 4 bytes.
        assert_eq!(h.input_report_byte_length, 4);
        assert_eq!(h.output_report_byte_length, 0);
        assert_eq!(h.feature_report_byte_length, 0);
        // buffer is exactly header + caps + nodes.
        let expected = HEADER_SIZE
            + (h.input_caps_count as usize) * size_of::<HidValueCaps>()
            + size_of::<HidCollectionNode>();
        assert_eq!(buf.len(), expected);
    }

    #[test]
    fn empty_descriptor_builds_empty() {
        let buf = parse_to_buffer(&[]).expect("empty descriptor is valid");
        let h = header(&buf);
        assert_eq!(&h.magic, b"HidP KDR");
        assert_eq!(h.number_link_collection_nodes, 0);
        assert_eq!(h.input_caps_end, 0);
        assert_eq!(buf.len(), HEADER_SIZE);
    }

    #[test]
    fn rejects_truncated_one_byte_value() {
        // 0x05 = Usage Page, item size 1, but no data byte follows. The C parser
        // read desc[1] (OOB) and then ran `ptr` past `end`; here it is rejected.
        assert!(run_parser(&[0x05]).is_none());
    }

    #[test]
    fn rejects_truncated_trailing_four_byte_value() {
        // A valid descriptor followed by a 4-byte item (0x27, size==4) with only
        // one of its four value bytes present -- the exact off-by-one/runaway shape.
        let mut d = JOYSTICK.to_vec();
        d.extend_from_slice(&[0x27, 0x01]);
        assert!(run_parser(&d).is_none());
    }

    #[test]
    fn rejects_truncated_two_byte_value() {
        // 0x06 = Usage Page (size 2), only one data byte present.
        assert!(run_parser(&[0x06, 0x01]).is_none());
    }

    #[test]
    fn rejects_unbalanced_end_collection() {
        // End Collection with no open collection -> collection stack underflow.
        assert!(run_parser(&[0xC0]).is_none());
    }

    #[test]
    fn ignores_unknown_item_type_and_continues() {
        // 0x38 = a reserved/long-ish tag the C `default` arm warns about and skips
        // (here: size 0, no data) -- parsing must continue, not fail.
        let mut d = vec![0x00u8]; // a 0-size unknown main-ish item
        d.extend_from_slice(JOYSTICK);
        let buf = parse_to_buffer(&d).expect("unknown items are skipped");
        let h = header(&buf);
        assert_eq!(h.usage, 0x04);
    }

    #[test]
    fn nested_collections_link_to_parents() {
        // Application collection containing a Physical collection.
        let desc: &[u8] = &[
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x05, // Usage (Game Pad)
            0xA1, 0x01, //   Collection (Application)
            0x09, 0x01, //     Usage (Pointer)
            0xA1, 0x00, //     Collection (Physical)
            0x09, 0x30, //       Usage (X)
            0x75, 0x08, //       Report Size (8)
            0x95, 0x01, //       Report Count (1)
            0x81, 0x02, //       Input (Data,Var,Abs)
            0xC0, //          End Collection
            0xC0, //        End Collection
        ];
        let buf = parse_to_buffer(desc).expect("nested collections parse");
        let h = header(&buf);
        assert_eq!(h.usage_page, 0x01);
        assert_eq!(h.usage, 0x05); // Game Pad
        assert_eq!(h.number_link_collection_nodes, 2);
        // node[1] (Physical) parent links back to node[0] (Application).
        let nodes_off = HEADER_SIZE + h.caps_size as usize;
        let node1 = unsafe {
            core::ptr::read_unaligned(
                buf.as_ptr().add(nodes_off + size_of::<HidCollectionNode>())
                    as *const HidCollectionNode,
            )
        };
        assert_eq!(node1.parent, 0);
        let node0 = unsafe {
            core::ptr::read_unaligned(buf.as_ptr().add(nodes_off) as *const HidCollectionNode)
        };
        assert_eq!(node0.number_of_children, 1);
        assert_eq!(node0.first_child, 1);
    }
}
