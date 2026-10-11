//! SDP UUIDs in their three wire sizes, the Bluetooth base reduction, and the
//! Windows GUID memory image (mixed endian) on either side of the FFI.

#![forbid(unsafe_code)]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uuid {
    U16(u16),
    U32(u32),
    U128([u8; 16]),
}

impl Uuid {
    /// 00000000-0000-1000-8000-00805F9B34FB in wire (big-endian textual) order.
    pub const BASE: [u8; 16] = [0, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5f, 0x9b, 0x34, 0xfb];
    pub const RFCOMM: Uuid = Uuid::U16(0x0003);
    pub const L2CAP: Uuid = Uuid::U16(0x0100);
    pub const PUBLIC_BROWSE_ROOT: Uuid = Uuid::U16(0x1002);

    /// Windows GUID memory image: Data1 u32 LE, Data2 u16 LE, Data3 u16 LE, Data4[8].
    /// A base-derived GUID that fits 16 bits goes on the wire as uuid16 (some SDP servers never
    /// match an expanded 128-bit form against their 16-bit records). Anything else stays
    /// 128-bit, never uuid32: that is what the libbluetooth-based code sent, and the form
    /// device firmware is least exercised on.
    pub fn from_guid(g: &[u8; 16]) -> Uuid {
        let mut w = [0u8; 16];
        w[0..4].copy_from_slice(&u32::from_le_bytes([g[0], g[1], g[2], g[3]]).to_be_bytes());
        w[4..6].copy_from_slice(&u16::from_le_bytes([g[4], g[5]]).to_be_bytes());
        w[6..8].copy_from_slice(&u16::from_le_bytes([g[6], g[7]]).to_be_bytes());
        w[8..16].copy_from_slice(&g[8..16]);
        match Uuid::U128(w).reduce() {
            short @ Uuid::U16(_) => short,
            _ => Uuid::U128(w),
        }
    }

    pub fn to_guid(self) -> [u8; 16] {
        let w = self.expand();
        let mut g = [0u8; 16];
        g[0..4].copy_from_slice(&u32::from_be_bytes([w[0], w[1], w[2], w[3]]).to_le_bytes());
        g[4..6].copy_from_slice(&u16::from_be_bytes([w[4], w[5]]).to_le_bytes());
        g[6..8].copy_from_slice(&u16::from_be_bytes([w[6], w[7]]).to_le_bytes());
        g[8..16].copy_from_slice(&w[8..16]);
        g
    }

    /// Full 128-bit wire form; the only basis for equality.
    pub fn expand(&self) -> [u8; 16] {
        let mut w = Self::BASE;
        match *self {
            Uuid::U16(v) => w[2..4].copy_from_slice(&v.to_be_bytes()),
            Uuid::U32(v) => w[0..4].copy_from_slice(&v.to_be_bytes()),
            Uuid::U128(b) => w = b,
        }
        w
    }

    /// Shortest form: a base-derived uuid128 becomes U32, a U32 <= 0xffff becomes U16.
    pub fn reduce(self) -> Uuid {
        let v = match self {
            Uuid::U128(b) if b[4..] == Self::BASE[4..] => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            Uuid::U32(v) => v,
            other => return other,
        };
        if v <= 0xffff { Uuid::U16(v as u16) } else { Uuid::U32(v) }
    }

    pub fn matches(&self, other: &Uuid) -> bool {
        self.expand() == other.expand()
    }

    /// Wire element: 0x19 + 2, 0x1a + 4, 0x1c + 16 bytes, big-endian.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match *self {
            Uuid::U16(v) => {
                out.push(0x19);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Uuid::U32(v) => {
                out.push(0x1a);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Uuid::U128(b) => {
                out.push(0x1c);
                out.extend_from_slice(&b);
            }
        }
    }

    /// From an element payload of 2, 4 or 16 bytes; anything else is None.
    pub fn from_payload(p: &[u8]) -> Option<Uuid> {
        match p.len() {
            2 => Some(Uuid::U16(u16::from_be_bytes([p[0], p[1]]))),
            4 => Some(Uuid::U32(u32::from_be_bytes([p[0], p[1], p[2], p[3]]))),
            16 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(p);
                Some(Uuid::U128(b))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::{RFCOMM_GUID_BYTES, SPP_GUID_BYTES};

    const RFCOMM_WIRE: [u8; 16] = [0, 0, 0, 3, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5f, 0x9b, 0x34, 0xfb];

    #[test]
    fn guid_base_derived_reduces_to_u16() {
        assert_eq!(Uuid::from_guid(&SPP_GUID_BYTES), Uuid::U16(0x1101));
    }

    #[test]
    fn guid_rfcomm_reduces_to_u16_3() {
        assert_eq!(Uuid::from_guid(&RFCOMM_GUID_BYTES), Uuid::U16(0x0003));
    }

    #[test]
    fn guid_non_base_stays_u128() {
        let mut g = SPP_GUID_BYTES;
        g[0..4].copy_from_slice(&0x12345678u32.to_le_bytes());
        let u = Uuid::from_guid(&g);
        assert!(matches!(u, Uuid::U128(_)));
        assert_eq!(u.expand()[0..4], [0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn guid_base_tail_mismatch_stays_u128() {
        let mut g = SPP_GUID_BYTES;
        g[15] = 0xfc;
        assert!(matches!(Uuid::from_guid(&g), Uuid::U128(_)));
    }

    #[test]
    fn u32_above_ffff_not_reduced() {
        assert_eq!(Uuid::U32(0x0001_0000).reduce(), Uuid::U32(0x0001_0000));
        assert_eq!(Uuid::U32(0x1101).reduce(), Uuid::U16(0x1101));
    }

    #[test]
    fn uuid128_rfcomm_matches_u16() {
        let u = Uuid::U128(RFCOMM_WIRE);
        assert!(u.matches(&Uuid::U16(3)));
        assert!(!u.matches(&Uuid::U16(0x1101)));
    }

    #[test]
    fn guid_roundtrip() {
        let mut non_base = SPP_GUID_BYTES;
        non_base[0..4].copy_from_slice(&0x12345678u32.to_le_bytes());
        for g in [SPP_GUID_BYTES, RFCOMM_GUID_BYTES, non_base] {
            assert_eq!(Uuid::from_guid(&g).to_guid(), g);
        }
    }

    #[test]
    fn encode_wire_forms() {
        let mut out = Vec::new();
        Uuid::U16(0x1101).encode(&mut out);
        assert_eq!(out, [0x19, 0x11, 0x01]);
        out.clear();
        Uuid::U32(0x0001_0000).encode(&mut out);
        assert_eq!(out, [0x1a, 0x00, 0x01, 0x00, 0x00]);
        out.clear();
        Uuid::U128(RFCOMM_WIRE).encode(&mut out);
        assert_eq!(out[0], 0x1c);
        assert_eq!(out[1..], RFCOMM_WIRE);
        assert_eq!(out.len(), 17);
    }

    #[test]
    fn from_payload_sizes() {
        assert_eq!(Uuid::from_payload(&[0x11, 0x01]), Some(Uuid::U16(0x1101)));
        assert_eq!(Uuid::from_payload(&[0, 1, 0, 0]), Some(Uuid::U32(0x0001_0000)));
        assert_eq!(Uuid::from_payload(&RFCOMM_WIRE), Some(Uuid::U128(RFCOMM_WIRE)));
        for n in [1usize, 3, 8] {
            assert_eq!(Uuid::from_payload(&vec![0u8; n]), None);
        }
    }
}
