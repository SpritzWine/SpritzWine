//! Sanity scan of the untrusted, device-supplied HID report descriptor.
//!
//! Before the C side splices Wine's synthetic haptics + PID reports into the
//! hidraw passthrough descriptor it must know that (a) the descriptor is
//! well-formed enough to splice into — every item in bounds, ending on the
//! `END_COLLECTION` of the application collection — and (b) none of the
//! device's own report IDs fall into the range the synthetic reports will
//! use. Anything suspicious fails the scan and the device stays a plain
//! passthrough, exactly as before.

/// Walks the descriptor's items. Returns true when the descriptor ends with
/// `END_COLLECTION`, all items lie inside the buffer, and no `Report ID`
/// item declares an ID in the exclusive range `(base, base_max)`.
pub fn check_descriptor(desc: &[u8], base: u8, base_max: u8) -> bool {
    if desc.is_empty() || *desc.last().unwrap() != 0xc0 {
        return false;
    }

    let mut pos = 0usize;
    while pos < desc.len() {
        let prefix = desc[pos];
        let (header, size) = if prefix == 0xfe {
            // long item: [0xfe][data size][tag][data...]
            if pos + 2 >= desc.len() {
                return false;
            }
            (3usize, desc[pos + 1] as usize)
        } else {
            let size = match prefix & 0x03 {
                3 => 4,
                s => s as usize,
            };
            (1usize, size)
        };

        if pos + header + size > desc.len() {
            return false;
        }

        // global item "Report ID" (tag 8, type 1): prefix 0x84 | size
        if prefix & 0xfc == 0x84 && size >= 1 {
            let id = desc[pos + header];
            if id == 0 || (id > base && id < base_max) {
                return false;
            }
        }

        pos += header + size;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::check_descriptor;

    /// Minimal but real structure: gamepad application collection with a
    /// report ID, one button-ish input, closed collection.
    const GOOD: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x05, // Usage (Gamepad)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x01, //   Report ID (1)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x04, //   Report Count (4)
        0x81, 0x02, //   Input (Data,Var,Abs)
        0x85, 0x05, //   Report ID (5)
        0x91, 0x02, //   Output (Data,Var,Abs)
        0xc0,       // End Collection
    ];

    #[test]
    fn accepts_regular_descriptor() {
        assert!(check_descriptor(GOOD, 0x40, 0x50));
    }

    #[test]
    fn rejects_report_id_in_synthetic_range() {
        let mut desc = GOOD.to_vec();
        desc[7] = 0x42; // Report ID (1) -> Report ID (0x42)
        assert!(!check_descriptor(&desc, 0x40, 0x50));
        // the base itself is never allocated (IDs start at base + 1)
        desc[7] = 0x40;
        assert!(check_descriptor(&desc, 0x40, 0x50));
        desc[7] = 0x50;
        assert!(check_descriptor(&desc, 0x40, 0x50));
    }

    #[test]
    fn rejects_missing_end_collection() {
        assert!(!check_descriptor(&GOOD[..GOOD.len() - 1], 0x40, 0x50));
        assert!(!check_descriptor(&[], 0x40, 0x50));
    }

    #[test]
    fn rejects_truncated_item() {
        // 4-byte item announced but only the trailing END_COLLECTION follows
        let desc = &[0x07, 0x01, 0xc0];
        assert!(!check_descriptor(desc, 0x40, 0x50));
        // long item header running past the end
        let desc = &[0xfe, 0x10, 0x00, 0xc0];
        assert!(!check_descriptor(desc, 0x40, 0x50));
    }

    #[test]
    fn rejects_report_id_zero() {
        let mut desc = GOOD.to_vec();
        desc[7] = 0x00;
        assert!(!check_descriptor(&desc, 0x40, 0x50));
    }
}
