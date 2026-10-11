//! SDP data element codec: a strict header decoder and an arena tree of byte
//! ranges over the source buffer. Nothing is copied out; accessors decode on
//! demand so a later PE-side crate can `#[path]` this module in unchanged.

#![forbid(unsafe_code)]

use std::borrow::Cow;

/// Bytes of one parse input (the reassembled AttributeLists is bounded by this).
pub const MAX_INPUT: usize = 1 << 20;
/// Container nesting levels including the root; legitimate records need 5.
pub const MAX_DEPTH: usize = 16;
/// Elements per tree; one element costs >= 1 byte, so this only matters near MAX_INPUT.
pub const MAX_NODES: usize = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,          // input > MAX_INPUT, nodes > MAX_NODES, or node allocation failed
    Truncated,         // descriptor or its length bytes run past the enclosing end
    BadDescriptor(u8), // reserved type 9..31, or an illegal (type, size index) pair
    Overflow,          // declared payload runs past the enclosing element's end
    TooDeep,           // nesting deeper than MAX_DEPTH
    Trailing,          // bytes after the root element
    BadRecord,         // record.rs: not a seq of (uint16 id, value) pairs
    BadRecordList,     // record.rs: a child of the AttributeLists seq is not a seq
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Type {
    Nil = 0,
    UInt = 1,
    Int = 2,
    Uuid = 3,
    Text = 4,
    Bool = 5,
    Seq = 6,
    Alt = 7,
    Url = 8,
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub ty: Type,
    pub size_index: u8,
    pub header_len: usize,
    pub payload_len: usize,
}

/// Decodes the descriptor at `buf[pos]`. `end` is the enclosing element's end (<= buf.len());
/// every read and the payload bound are checked against it, never against buf.len().
pub fn decode_header(buf: &[u8], pos: usize, end: usize) -> Result<Header, ParseError> {
    debug_assert!(end <= buf.len());
    if pos >= end {
        return Err(ParseError::Truncated);
    }
    let d = buf[pos];
    let ty = match d >> 3 {
        0 => Type::Nil,
        1 => Type::UInt,
        2 => Type::Int,
        3 => Type::Uuid,
        4 => Type::Text,
        5 => Type::Bool,
        6 => Type::Seq,
        7 => Type::Alt,
        8 => Type::Url,
        _ => return Err(ParseError::BadDescriptor(d)),
    };
    let si = d & 7;
    let legal = match ty {
        Type::Nil | Type::Bool => si == 0,
        Type::UInt | Type::Int => si <= 4,
        Type::Uuid => matches!(si, 1 | 2 | 4),
        Type::Text | Type::Url | Type::Seq | Type::Alt => si >= 5,
    };
    if !legal {
        return Err(ParseError::BadDescriptor(d));
    }
    let header_len = 1 + match si {
        5 => 1,
        6 => 2,
        7 => 4,
        _ => 0,
    };
    if end - pos < header_len {
        return Err(ParseError::Truncated);
    }
    let payload_len = match si {
        0 => {
            if ty == Type::Nil {
                0
            } else {
                1
            }
        }
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        5 => buf[pos + 1] as usize,
        6 => u16::from_be_bytes([buf[pos + 1], buf[pos + 2]]) as usize,
        _ => u32::from_be_bytes([buf[pos + 1], buf[pos + 2], buf[pos + 3], buf[pos + 4]]) as usize,
    };
    match (pos + header_len).checked_add(payload_len) {
        Some(e) if e <= end => Ok(Header { ty, size_index: si, header_len, payload_len }),
        _ => Err(ParseError::Overflow),
    }
}

/// 0x35 + u8 when <= 0xff, 0x36 + u16 BE when <= 0xffff, 0x37 + u32 BE otherwise.
pub fn encode_seq_header(payload_len: usize, out: &mut Vec<u8>) {
    if payload_len <= 0xff {
        out.push(0x35);
        out.push(payload_len as u8);
    } else if payload_len <= 0xffff {
        out.push(0x36);
        out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    } else {
        out.push(0x37);
        out.extend_from_slice(&(payload_len as u32).to_be_bytes());
    }
}

/// One element in pre-order. Offsets are absolute in the source buffer; u32 because
/// MAX_INPUT < 4 GiB keeps the node 20 bytes.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub ty: Type,
    pub size_index: u8,
    pub start: u32, // descriptor byte
    pub data: u32,  // first payload byte
    pub end: u32,   // one past the payload == start of the next sibling
    pub next: u32,  // index of the first node after this subtree
}

pub struct Tree<'a> {
    src: &'a [u8],
    nodes: Vec<Node>,
}

#[derive(Clone, Copy)]
pub struct Elem<'t, 'a> {
    tree: &'t Tree<'a>,
    idx: usize,
}

pub struct Children<'t, 'a> {
    tree: &'t Tree<'a>,
    next: usize,
    end: usize,
}

impl<'a> Tree<'a> {
    /// Strict: exactly one root element that ends at src.len().
    pub fn parse(src: &'a [u8]) -> Result<Tree<'a>, ParseError> {
        if src.len() > MAX_INPUT {
            return Err(ParseError::TooLarge);
        }
        let mut t = Tree { src, nodes: Vec::new() };
        let end = t.parse_one(0, src.len(), 0)?;
        if end != src.len() {
            return Err(ParseError::Trailing);
        }
        Ok(t)
    }

    /// Parses the element at `pos` bounded by `end`, appending its subtree in pre-order.
    /// Returns the element's end offset. Recursion depth is capped by MAX_DEPTH.
    fn parse_one(&mut self, pos: usize, end: usize, depth: usize) -> Result<usize, ParseError> {
        if depth >= MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(ParseError::TooLarge);
        }
        let h = decode_header(self.src, pos, end)?;
        let data = pos + h.header_len;
        let elem_end = data + h.payload_len; // in bounds: decode_header checked it
        let idx = self.nodes.len();
        // Rust OOM aborts instead of panicking, so growth is checked here, not left to push.
        self.nodes.try_reserve(1).map_err(|_| ParseError::TooLarge)?;
        self.nodes.push(Node {
            ty: h.ty,
            size_index: h.size_index,
            start: pos as u32,
            data: data as u32,
            end: elem_end as u32,
            next: 0,
        });
        if matches!(h.ty, Type::Seq | Type::Alt) {
            let mut p = data;
            while p < elem_end {
                p = self.parse_one(p, elem_end, depth + 1)?; // children bounded by the parent, not the buffer
            }
        }
        self.nodes[idx].next = self.nodes.len() as u32;
        Ok(elem_end)
    }

    pub fn root(&self) -> Elem<'_, 'a> {
        Elem { tree: self, idx: 0 }
    }
    pub fn src(&self) -> &'a [u8] {
        self.src
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
}

impl<'t, 'a> Iterator for Children<'t, 'a> {
    type Item = Elem<'t, 'a>;
    fn next(&mut self) -> Option<Elem<'t, 'a>> {
        if self.next >= self.end {
            return None;
        }
        let idx = self.next;
        self.next = self.tree.nodes[idx].next as usize;
        Some(Elem { tree: self.tree, idx })
    }
}

impl<'t, 'a> Elem<'t, 'a> {
    fn node(&self) -> &'t Node {
        &self.tree.nodes[self.idx]
    }
    pub fn ty(&self) -> Type {
        self.node().ty
    }
    pub fn size_index(&self) -> u8 {
        self.node().size_index
    }
    /// start..end, descriptor included.
    pub fn range(&self) -> core::ops::Range<usize> {
        let n = self.node();
        n.start as usize..n.end as usize
    }
    /// src[start..end]: what SDP_ELEMENT_DATA.sequence.value and the EnumAttributes stream want.
    pub fn bytes(&self) -> &'a [u8] {
        &self.tree.src[self.range()]
    }
    /// src[data..end]: what string/url value wants.
    pub fn payload(&self) -> &'a [u8] {
        let n = self.node();
        &self.tree.src[n.data as usize..n.end as usize]
    }
    pub fn children(&self) -> Children<'t, 'a> {
        Children { tree: self.tree, next: self.idx + 1, end: self.node().next as usize }
    }
    pub fn child_count(&self) -> usize {
        self.children().count()
    }
    /// Any UInt size, big-endian.
    pub fn as_uint(&self) -> Option<u128> {
        if self.ty() != Type::UInt {
            return None;
        }
        Some(self.payload().iter().fold(0u128, |v, &b| (v << 8) | b as u128))
    }
    pub fn as_u8(&self) -> Option<u8> {
        (self.ty() == Type::UInt && self.size_index() == 0).then(|| self.payload()[0])
    }
    pub fn as_u16(&self) -> Option<u16> {
        (self.ty() == Type::UInt && self.size_index() == 1)
            .then(|| u16::from_be_bytes([self.payload()[0], self.payload()[1]]))
    }
    pub fn as_u32(&self) -> Option<u32> {
        (self.ty() == Type::UInt && self.size_index() == 2).then(|| {
            let p = self.payload();
            u32::from_be_bytes([p[0], p[1], p[2], p[3]])
        })
    }
    /// Any Int size, big-endian two's complement.
    pub fn as_int(&self) -> Option<i128> {
        if self.ty() != Type::Int {
            return None;
        }
        let p = self.payload();
        let seed: i128 = if p[0] & 0x80 != 0 { -1 } else { 0 };
        Some(p.iter().fold(seed, |v, &b| (v << 8) | b as i128))
    }
    pub fn as_uuid(&self) -> Option<crate::uuid::Uuid> {
        if self.ty() != Type::Uuid {
            return None;
        }
        crate::uuid::Uuid::from_payload(self.payload())
    }
    pub fn as_bool(&self) -> Option<bool> {
        (self.ty() == Type::Bool).then(|| self.payload()[0] != 0)
    }
    /// Text/Url; invalid UTF-8 is replaced, never an error (f15).
    pub fn as_text_lossy(&self) -> Option<Cow<'a, str>> {
        matches!(self.ty(), Type::Text | Type::Url).then(|| String::from_utf8_lossy(self.payload()))
    }
    /// Windows SDP_SPECIFICTYPE: (size_index << 8) | (type << 4) for UInt/Int/Uuid, else 0.
    pub fn specific_type(&self) -> u16 {
        match self.ty() {
            Type::UInt | Type::Int | Type::Uuid => ((self.size_index() as u16) << 8) | ((self.ty() as u16) << 4),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::RECORD_B;

    fn root_of(buf: &[u8]) -> Result<(Tree<'_>, usize), ParseError> {
        let t = Tree::parse(buf)?;
        let n = t.len();
        Ok((t, n))
    }

    #[test]
    fn uint_fixed_sizes() {
        let (t, _) = root_of(&[0x08, 0x05]).unwrap();
        let e = t.root();
        assert_eq!(e.as_uint(), Some(5));
        assert_eq!(e.as_u8(), Some(5));
        assert_eq!((e.as_u16(), e.as_u32()), (None, None));

        let (t, _) = root_of(&[0x09, 0x00, 0x05]).unwrap();
        let e = t.root();
        assert_eq!(e.as_uint(), Some(5));
        assert_eq!(e.as_u16(), Some(5));
        assert_eq!((e.as_u8(), e.as_u32()), (None, None));

        let (t, _) = root_of(&[0x0a, 0x00, 0x01, 0x00, 0x00]).unwrap();
        let e = t.root();
        assert_eq!(e.as_uint(), Some(0x10000));
        assert_eq!(e.as_u32(), Some(0x10000));
        assert_eq!((e.as_u8(), e.as_u16()), (None, None));

        let mut b64 = vec![0x0b];
        b64.extend_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());
        let (t, _) = root_of(&b64).unwrap();
        assert_eq!(t.root().as_uint(), Some(0x0102_0304_0506_0708));
        assert_eq!((t.root().as_u8(), t.root().as_u16(), t.root().as_u32()), (None, None, None));

        let mut b128 = vec![0x0c];
        b128.extend_from_slice(&[0xff; 16]);
        let (t, _) = root_of(&b128).unwrap();
        assert_eq!(t.root().as_uint(), Some(u128::MAX));
    }

    #[test]
    fn int_sign_extends() {
        assert_eq!(Tree::parse(&[0x10, 0xff]).unwrap().root().as_int(), Some(-1));
        assert_eq!(Tree::parse(&[0x11, 0x80, 0x00]).unwrap().root().as_int(), Some(-32768));
        assert_eq!(Tree::parse(&[0x10, 0x7f]).unwrap().root().as_int(), Some(127));
    }

    #[test]
    fn strings_8_16_32() {
        let cases: [(&[u8], usize); 3] = [
            (&[0x25, 0x06, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72], 8),
            (&[0x26, 0x00, 0x06, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72], 9),
            (&[0x27, 0x00, 0x00, 0x00, 0x06, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72], 11),
        ];
        for (buf, len) in cases {
            let t = Tree::parse(buf).unwrap();
            let e = t.root();
            assert_eq!(e.ty(), Type::Text);
            assert_eq!(e.payload(), b"foobar");
            assert_eq!(e.as_text_lossy().as_deref(), Some("foobar"));
            assert_eq!(e.range().len(), len);
        }
    }

    #[test]
    fn sequences_8_16_32() {
        let cases: [&[u8]; 3] = [
            &[0x35, 0x06, 0x08, 0xde, 0x08, 0xad, 0x08, 0xbe],
            &[0x36, 0x00, 0x06, 0x08, 0xde, 0x08, 0xad, 0x08, 0xbe],
            &[0x37, 0x00, 0x00, 0x00, 0x06, 0x08, 0xde, 0x08, 0xad, 0x08, 0xbe],
        ];
        for buf in cases {
            let t = Tree::parse(buf).unwrap();
            assert_eq!(t.len(), 4);
            assert_eq!(t.root().ty(), Type::Seq);
            let v: Vec<u8> = t.root().children().map(|c| c.as_u8().unwrap()).collect();
            assert_eq!(v, [0xde, 0xad, 0xbe]);
        }
    }

    #[test]
    fn empty_seq_and_string_are_legal() {
        for buf in [&[0x35, 0x00][..], &[0x25, 0x00][..]] {
            let t = Tree::parse(buf).unwrap();
            assert_eq!(t.len(), 1);
            assert!(t.root().payload().is_empty());
            assert_eq!(t.root().child_count(), 0);
        }
    }

    #[test]
    fn truncated_seq_errors() {
        assert_eq!(Tree::parse(&[0x35, 0x08, 0xde]).err(), Some(ParseError::Overflow));
    }

    #[test]
    fn f1_truncated_descriptor_lengths() {
        assert_eq!(Tree::parse(&[0x36, 0x00]).err(), Some(ParseError::Truncated));
        assert_eq!(Tree::parse(&[0x35]).err(), Some(ParseError::Truncated));
        assert_eq!(Tree::parse(&[0x27, 0x00, 0x00]).err(), Some(ParseError::Truncated));
    }

    #[test]
    fn f2_declared_length_exceeds_buffer() {
        assert_eq!(Tree::parse(&[0x35, 0x10, 0x09, 0x00, 0x00]).err(), Some(ParseError::Overflow));
        assert_eq!(Tree::parse(&[0x25, 0x05, 0x41, 0x42]).err(), Some(ParseError::Overflow));
    }

    #[test]
    fn f3_nested_overflows_parent() {
        // Inner bound is the parent's end (6), not the buffer end (7).
        assert_eq!(Tree::parse(&[0x35, 0x04, 0x35, 0x05, 0x09, 0x00, 0x01]).err(), Some(ParseError::Overflow));
    }

    #[test]
    fn f5_illegal_descriptors() {
        let cases: [&[u8]; 7] = [
            &[0x18, 0x05],
            &[0x1b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03],
            &[0x1d, 0x02, 0x11, 0x01],
            &[0x0d, 0x02, 0x00, 0x05],
            &[0x0f, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x05],
            &[0x01],
            &[0x29, 0x00, 0x01],
        ];
        for buf in cases {
            assert_eq!(Tree::parse(buf).err(), Some(ParseError::BadDescriptor(buf[0])), "{buf:02x?}");
        }
    }

    /// Level k is `35 (2*(d-1-k))`. The u8 cast wraps for d > 128, but every level loses the
    /// same multiple of 256, so the first MAX_DEPTH levels stay mutually consistent, which is
    /// all the depth cap needs to see before any length matters.
    fn chain8(d: usize) -> Vec<u8> {
        (0..d).flat_map(|k| [0x35, (2 * (d - 1 - k)) as u8]).collect()
    }

    fn chain32(d: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for k in 0..d {
            v.push(0x37);
            v.extend_from_slice(&((5 * (d - 1 - k)) as u32).to_be_bytes());
        }
        v
    }

    /// Proves the parser does not recurse past MAX_DEPTH frames: a 64 KiB stack is far too
    /// small for 200 levels of anything, so a missing cap would overflow it.
    fn on_small_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn f6_depth_bomb_seq8() {
        assert_eq!(chain8(3), [0x35, 0x04, 0x35, 0x02, 0x35, 0x00]);
        on_small_stack(|| {
            assert_eq!(Tree::parse(&chain8(200)).err(), Some(ParseError::TooDeep));
            assert!(Tree::parse(&chain8(16)).is_ok());
            assert_eq!(Tree::parse(&chain8(17)).err(), Some(ParseError::TooDeep));
        });
    }

    #[test]
    fn f6_depth_bomb_seq32() {
        on_small_stack(|| {
            assert_eq!(Tree::parse(&chain32(200)).err(), Some(ParseError::TooDeep));
            assert!(Tree::parse(&chain32(16)).is_ok());
            assert_eq!(Tree::parse(&chain32(17)).err(), Some(ParseError::TooDeep));
        });
    }

    #[test]
    fn f14_length_overflow() {
        assert_eq!(Tree::parse(&[0x37, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00]).err(), Some(ParseError::Overflow));
    }

    #[test]
    fn f15_non_utf8_text_kept() {
        let t = Tree::parse(&[0x25, 0x02, 0xff, 0xfe]).unwrap();
        assert_eq!(t.root().payload(), [0xff, 0xfe]);
        assert_eq!(t.root().as_text_lossy().as_deref(), Some("\u{fffd}\u{fffd}"));
    }

    #[test]
    fn root_must_consume_whole_buffer() {
        assert_eq!(Tree::parse(&[0x35, 0x00, 0x00]).err(), Some(ParseError::Trailing));
    }

    #[test]
    fn reserved_type_rejected() {
        assert_eq!(Tree::parse(&[0x48, 0x00]).err(), Some(ParseError::BadDescriptor(0x48)));
        assert_eq!(Tree::parse(&[0xf8]).err(), Some(ParseError::BadDescriptor(0xf8)));
    }

    #[test]
    fn empty_input_is_truncated() {
        assert_eq!(Tree::parse(&[]).err(), Some(ParseError::Truncated));
    }

    #[test]
    fn input_over_max_rejected() {
        assert_eq!(Tree::parse(&vec![0u8; MAX_INPUT + 1]).err(), Some(ParseError::TooLarge));
    }

    #[test]
    fn spans_are_absolute_in_record_b() {
        let t = Tree::parse(RECORD_B).unwrap();
        let kids: Vec<Elem> = t.root().children().collect();
        assert_eq!(kids.len(), 8);
        let pdl = kids[5];
        assert_eq!(pdl.bytes(), &RECORD_B[21..35]);
        assert_eq!(pdl.bytes()[..3], [0x35, 0x0c, 0x35]);
        assert_eq!(pdl.range(), 21..35);
        let name = kids[7];
        assert_eq!(name.payload(), b"Serial Port");
        assert_eq!(name.bytes()[0], 0x25);
    }

    #[test]
    fn specific_type_matches_windows() {
        let cases: [(&[u8], u16); 6] = [
            (&[0x08, 0x01], 0x0010),
            (&[0x09, 0x00, 0x01], 0x0110),
            (&[0x0a, 0x00, 0x00, 0x00, 0x01], 0x0210),
            (&[0x19, 0x11, 0x01], 0x0130),
            (&[0x1c, 0, 0, 0, 3, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5f, 0x9b, 0x34, 0xfb], 0x0430),
            (&[0x25, 0x01, 0x41], 0),
        ];
        for (buf, want) in cases {
            assert_eq!(Tree::parse(buf).unwrap().root().specific_type(), want, "{buf:02x?}");
        }
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    #[test]
    fn fuzz_lcg_no_panic() {
        let mut rng = Lcg(0x5eed_1234_abcd_0001);
        for i in 0..4096 {
            let buf: Vec<u8> = if i % 2 == 0 {
                let len = (rng.next() % 97) as usize;
                (0..len).map(|_| rng.next() as u8).collect()
            } else {
                let mut b = RECORD_B.to_vec();
                for _ in 0..1 + rng.next() % 3 {
                    let at = (rng.next() as usize) % b.len();
                    b[at] = rng.next() as u8;
                }
                b
            };
            if let Ok(tree) = Tree::parse(&buf) {
                for n in tree.nodes() {
                    assert!(n.start < n.data && n.data <= n.end && n.end as usize <= buf.len());
                }
                if let Ok(records) = crate::record::parse_record_list(&tree) {
                    for r in &records {
                        let _ = (r.rfcomm_channel(), r.service_class_ids(), r.handle());
                    }
                }
            }
        }
    }
}
