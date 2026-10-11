#![forbid(unsafe_code)]
use std::borrow::Cow;
use crate::element::{Elem, ParseError, Tree, Type};
use crate::uuid::Uuid;

pub const ATTR_SERVICE_RECORD_HANDLE: u16 = 0x0000;
pub const ATTR_SERVICE_CLASS_ID_LIST: u16 = 0x0001;
pub const ATTR_PROTOCOL_DESCRIPTOR_LIST: u16 = 0x0004;
pub const ATTR_BROWSE_GROUP_LIST: u16 = 0x0005;
pub const ATTR_ADDITIONAL_PROTOCOL_DESCRIPTOR_LISTS: u16 = 0x000d;
pub const ATTR_SERVICE_NAME: u16 = 0x0100;
pub const RFCOMM_CHANNEL_MIN: u16 = 1;
pub const RFCOMM_CHANNEL_MAX: u16 = 30;

/// A service record: a Seq whose children are (uint16 attribute id, value) pairs.
/// Validated once in `new`; every accessor afterwards is infallible.
#[derive(Clone, Copy)]
pub struct Record<'t, 'a> { root: Elem<'t, 'a> }

impl<'t, 'a> Record<'t, 'a> {
    pub fn new(root: Elem<'t, 'a>) -> Result<Self, ParseError> {
        if root.ty() != Type::Seq || !root.child_count().is_multiple_of(2) {
            return Err(ParseError::BadRecord);
        }
        let mut it = root.children();
        while let Some(id) = it.next() {
            if id.as_u16().is_none() || it.next().is_none() {
                return Err(ParseError::BadRecord);
            }
        }
        Ok(Record { root })
    }
    /// Raw record DES: what BluetoothSdpEnumAttributes consumes.
    pub fn bytes(&self) -> &'a [u8] { self.root.bytes() }
    pub fn attributes(&self) -> impl Iterator<Item = (u16, Elem<'t, 'a>)> + '_ {
        let mut it = self.root.children();
        core::iter::from_fn(move || {
            let id = it.next()?.as_u16()?; // always Some after new()
            Some((id, it.next()?))
        })
    }
    /// First occurrence wins; ids are not required to be ascending (BlueZ does not enforce it).
    pub fn attribute(&self, id: u16) -> Option<Elem<'t, 'a>> {
        self.attributes().find(|(i, _)| *i == id).map(|(_, v)| v)
    }
    pub fn handle(&self) -> Option<u32> { self.attribute(ATTR_SERVICE_RECORD_HANDLE)?.as_u32() }
    pub fn service_class_ids(&self) -> Vec<Uuid> {
        self.attribute(ATTR_SERVICE_CLASS_ID_LIST)
            .filter(|e| e.ty() == Type::Seq)
            .map(|e| e.children().filter_map(|c| c.as_uuid()).collect())
            .unwrap_or_default()
    }
    pub fn service_name_lossy(&self) -> Option<Cow<'a, str>> { self.attribute(ATTR_SERVICE_NAME)?.as_text_lossy() }
    /// Protocol stacks of attribute `attr`: a Seq is one stack; an Alt is one stack per child.
    pub fn protocol_stacks(&self, attr: u16) -> Vec<Elem<'t, 'a>> {
        match self.attribute(attr) {
            Some(v) if v.ty() == Type::Seq => vec![v],
            Some(v) if v.ty() == Type::Alt => v.children().filter(|s| s.ty() == Type::Seq).collect(),
            _ => Vec::new(),
        }
    }
    /// BlueZ sdp_get_proto_port over attribute `attr`: for each stack, for each protocol
    /// descriptor Seq whose first element is a UUID matching `proto`, the next element yields
    /// the port if it is a uint8 or uint16. `accept` filters candidates, so an out-of-range
    /// value is skipped and the scan continues instead of ending.
    pub fn proto_port_in(&self, attr: u16, proto: Uuid, accept: impl Fn(u16) -> bool) -> Option<u16> {
        for stack in self.protocol_stacks(attr) {
            for desc in stack.children().filter(|d| d.ty() == Type::Seq) {
                let mut it = desc.children();
                let Some(first) = it.next() else { continue };
                let Some(uuid) = first.as_uuid() else { continue };
                if !uuid.matches(&proto) {
                    continue;
                }
                let port = match it.next() {
                    Some(p) if p.size_index() == 0 => p.as_u8().map(u16::from),
                    Some(p) if p.size_index() == 1 => p.as_u16(),
                    _ => None,
                };
                if let Some(port) = port.filter(|p| accept(*p)) {
                    return Some(port);
                }
            }
        }
        None
    }
    /// The channel Windows would connect to: ProtocolDescriptorList (0x0004) only (the C
    /// reference and Windows never consult 0x000d, which lists secondary channels), RFCOMM
    /// 0x0003 with 128-bit forms reduced, uint8 or uint16, 1..=30.
    pub fn rfcomm_channel(&self) -> Option<u8> {
        self.proto_port_in(ATTR_PROTOCOL_DESCRIPTOR_LIST, Uuid::RFCOMM,
                           |p| (RFCOMM_CHANNEL_MIN..=RFCOMM_CHANNEL_MAX).contains(&p))
            .map(|p| p as u8)
    }
}

/// The reassembled AttributeLists: a Seq whose children are records.
pub fn parse_record_list<'t, 'a>(tree: &'t Tree<'a>) -> Result<Vec<Record<'t, 'a>>, ParseError> {
    let root = tree.root();
    if root.ty() != Type::Seq {
        return Err(ParseError::BadRecordList);
    }
    root.children()
        .map(|c| if c.ty() == Type::Seq { Record::new(c) } else { Err(ParseError::BadRecordList) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::{ATTR_LISTS_B, RECORD_B, RECORD_E, WINE_DEVICE_ID_RECORD};

    /// Data element with an 8-bit length: `desc len parts...` (0x35 Seq, 0x3d Alt).
    fn des(desc: u8, parts: &[&[u8]]) -> Vec<u8> {
        let body: Vec<u8> = parts.concat();
        assert!(body.len() <= 0xff);
        let mut v = vec![desc, body.len() as u8];
        v.extend_from_slice(&body);
        v
    }

    /// A record holding the single attribute `id` with the given value element.
    fn record_with(id: u16, value: &[u8]) -> Vec<u8> {
        let id = id.to_be_bytes();
        des(0x35, &[&[0x09, id[0], id[1]], value])
    }

    fn channel_of(record: &[u8]) -> Option<u8> {
        let tree = Tree::parse(record).unwrap();
        Record::new(tree.root()).unwrap().rfcomm_channel()
    }

    const L2CAP_DESC: &[u8] = &[0x35, 0x03, 0x19, 0x01, 0x00];

    #[test]
    fn record_b_fields() {
        let tree = Tree::parse(RECORD_B).unwrap();
        let r = Record::new(tree.root()).unwrap();
        let ids: Vec<u16> = r.attributes().map(|(id, _)| id).collect();
        assert_eq!(ids, [0x0000, 0x0001, 0x0004, 0x0100]);
        assert_eq!(r.handle(), Some(0x0001_0000));
        assert_eq!(r.service_class_ids(), [Uuid::U16(0x1101)]);
        assert_eq!(r.rfcomm_channel(), Some(5));
        assert_eq!(r.service_name_lossy().as_deref(), Some("Serial Port"));
        assert_eq!(r.bytes().len(), 51);
    }

    #[test]
    fn wine_device_id_record_has_no_channel() {
        let tree = Tree::parse(WINE_DEVICE_ID_RECORD).unwrap();
        let r = Record::new(tree.root()).unwrap();
        assert_eq!(r.attributes().count(), 10);
        assert_eq!(r.service_class_ids(), [Uuid::U16(0x1200)]);
        assert_eq!(r.attribute(0x0204).and_then(|e| e.as_bool()), Some(true));
        assert!(r.attribute(ATTR_PROTOCOL_DESCRIPTOR_LIST).is_none());
        assert_eq!(r.attribute(ATTR_BROWSE_GROUP_LIST).map(|e| e.child_count()), Some(1));
        assert_eq!(r.rfcomm_channel(), None);
    }

    #[test]
    fn record_e_uuid128_and_uint16_channel() {
        assert_eq!(channel_of(RECORD_E), Some(5));
    }

    #[test]
    fn channel_out_of_range_skipped() {
        for (value, want) in [(0x00, None), (0x1f, None), (0x1e, Some(30)), (0x01, Some(1))] {
            let mut rec = RECORD_B.to_vec();
            assert_eq!(rec[34], 0x05);
            rec[34] = value;
            assert_eq!(channel_of(&rec), want, "channel byte {value:#04x}");
        }
    }

    #[test]
    fn uint16_channel_out_of_range_skipped() {
        let mut rec = RECORD_E.to_vec();
        let n = rec.len();
        assert_eq!(rec[n - 3..], [0x09, 0x00, 0x05]);
        rec[n - 2] = 0x01;
        rec[n - 1] = 0x00;
        assert_eq!(channel_of(&rec), None);
    }

    #[test]
    fn channel_in_alternative_second_stack() {
        let first = des(0x35, &[L2CAP_DESC]);
        let second = des(0x35, &[L2CAP_DESC, &[0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x07]]);
        let alt = des(0x3d, &[&first, &second]);
        assert_eq!(channel_of(&record_with(ATTR_PROTOCOL_DESCRIPTOR_LIST, &alt)), Some(7));
    }

    #[test]
    fn non_rfcomm_protocol_ignored() {
        let stack = des(0x35, &[&[0x35, 0x05, 0x19, 0x00, 0x08, 0x08, 0x09]]);
        assert_eq!(channel_of(&record_with(ATTR_PROTOCOL_DESCRIPTOR_LIST, &stack)), None);
    }

    #[test]
    fn rfcomm_followed_by_non_integer_skipped() {
        let bad: &[u8] = &[0x35, 0x06, 0x19, 0x00, 0x03, 0x19, 0x01, 0x00];
        let stack = des(0x35, &[bad]);
        assert_eq!(channel_of(&record_with(ATTR_PROTOCOL_DESCRIPTOR_LIST, &stack)), None);
        let stack = des(0x35, &[bad, &[0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x04]]);
        assert_eq!(channel_of(&record_with(ATTR_PROTOCOL_DESCRIPTOR_LIST, &stack)), Some(4));
    }

    #[test]
    fn rfcomm_without_parameter_skipped() {
        let stack = des(0x35, &[&[0x35, 0x03, 0x19, 0x00, 0x03]]);
        assert_eq!(channel_of(&record_with(ATTR_PROTOCOL_DESCRIPTOR_LIST, &stack)), None);
    }

    #[test]
    fn additional_lists_not_scanned() {
        // 0x000d is a list of stacks; the one inside carries RFCOMM channel 9.
        let stack = des(0x35, &[L2CAP_DESC, &[0x35, 0x05, 0x19, 0x00, 0x03, 0x08, 0x09]]);
        let lists = des(0x35, &[&stack]);
        let rec = record_with(ATTR_ADDITIONAL_PROTOCOL_DESCRIPTOR_LISTS, &lists);
        assert_eq!(channel_of(&rec), None);
        // The same stack as the primary list is found: only the attribute decides.
        assert_eq!(channel_of(&record_with(ATTR_PROTOCOL_DESCRIPTOR_LIST, &stack)), Some(9));
    }

    #[test]
    fn f4_odd_element_count() {
        let tree = Tree::parse(&[0x35, 0x03, 0x09, 0x00, 0x00]).unwrap();
        assert_eq!(Record::new(tree.root()).err(), Some(ParseError::BadRecord));
    }

    #[test]
    fn f4_attribute_id_not_uint16() {
        let tree = Tree::parse(&[0x35, 0x07, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x28, 0x01]).unwrap();
        assert_eq!(Record::new(tree.root()).err(), Some(ParseError::BadRecord));
    }

    #[test]
    fn record_list_b() {
        let tree = Tree::parse(ATTR_LISTS_B).unwrap();
        let records = parse_record_list(&tree).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].bytes(), RECORD_B);
    }

    #[test]
    fn record_list_empty() {
        let tree = Tree::parse(&[0x35, 0x00]).unwrap();
        assert_eq!(parse_record_list(&tree).unwrap().len(), 0);
    }

    #[test]
    fn record_list_child_not_seq() {
        let tree = Tree::parse(&[0x35, 0x02, 0x08, 0x01]).unwrap();
        assert_eq!(parse_record_list(&tree).err(), Some(ParseError::BadRecordList));
    }

    #[test]
    fn record_list_root_not_seq() {
        let tree = Tree::parse(&[0x08, 0x01]).unwrap();
        assert_eq!(parse_record_list(&tree).err(), Some(ParseError::BadRecordList));
    }

    #[test]
    fn f13_reassembled_length_mismatch() {
        let mut buf = vec![0x35, 0x40];
        buf.extend_from_slice(&RECORD_B[..10]);
        assert_eq!(Tree::parse(&buf).err(), Some(ParseError::Overflow));
    }
}
