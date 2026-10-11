#![forbid(unsafe_code)]
use crate::element::{self, Tree};
use crate::error::SdpError;
use crate::pdu::{self, AttrId, Response, SearchAttrRequest};
use crate::record;
use crate::uuid::Uuid;

/// The I/O seam: one SDP transaction. The implementation owns the deadline and the receive
/// buffer; the returned slice lives until the next call.
pub trait Transport {
    /// Sends `req` as one SDU and returns the next SDU received.
    fn exchange(&mut self, req: &[u8]) -> Result<&[u8], SdpError>;
}

pub struct Limits { pub max_rounds: u32, pub max_attr_bytes: usize }
pub const DEFAULT_LIMITS: Limits = Limits { max_rounds: 64, max_attr_bytes: element::MAX_INPUT };

/// One ServiceSearchAttribute transaction including continuation. Returns the reassembled
/// AttributeLists bytes (one DES of records), still unparsed.
pub fn search_attr<T: Transport>(t: &mut T, req: &SearchAttrRequest, first_tid: u16, limits: &Limits)
    -> Result<Vec<u8>, SdpError>
{
    let mut acc: Vec<u8> = Vec::new();
    let mut tx: Vec<u8> = Vec::with_capacity(64);
    let mut cstate = [0u8; pdu::MAX_CSTATE];
    let mut cstate_len = 0usize;
    let mut tid = first_tid;
    // A server may answer with zero bytes and a fresh continuation state forever;
    // the round cap, not the byte cap, ends that.
    for _ in 0..limits.max_rounds {
        // Rebuilt from `req` every round: identical body, new tid, echoed state (vector c2).
        pdu::encode_search_attr_request(req, tid, &cstate[..cstate_len], &mut tx)?;
        let rsp = t.exchange(&tx)?;
        match pdu::decode_response(rsp, tid)? {
            Response::Error(code) => return Err(SdpError::ErrorResponse(code)),
            Response::SearchAttr { attr_lists, cstate: next } => {
                if acc.len() + attr_lists.len() > limits.max_attr_bytes {
                    return Err(SdpError::TooLarge);
                }
                acc.try_reserve(attr_lists.len()).map_err(|_| SdpError::NoMemory)?;
                acc.extend_from_slice(attr_lists);
                if next.is_empty() {
                    return Ok(acc);
                }
                cstate_len = next.len(); // <= MAX_CSTATE, checked by decode_response
                cstate[..cstate_len].copy_from_slice(next);
            }
        }
        tid = tid.wrapping_add(1);
    }
    Err(SdpError::TooLarge)
}

/// First record (in server order) carrying an RFCOMM channel for `class`; Ok(None) = no record
/// or no channel (the C reference's "no RFCOMM channel" case).
pub fn resolve_channel<T: Transport>(t: &mut T, class: Uuid, first_tid: u16, limits: &Limits)
    -> Result<Option<u8>, SdpError>
{
    let pattern = [class]; // as chosen by Uuid::from_guid; not reduced further
    let req = SearchAttrRequest { pattern: &pattern, max_attr_bytes: 0xffff, attr_ids: &[AttrId::Range(0x0000, 0xffff)] };
    let bytes = search_attr(t, &req, first_tid, limits)?;
    let tree = Tree::parse(&bytes).map_err(SdpError::Malformed)?;
    let records = record::parse_record_list(&tree).map_err(SdpError::Malformed)?;
    Ok(records.iter().find_map(|r| r.rfcomm_channel()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::ParseError;
    use crate::errno;
    use crate::error::ProtoFault;
    use crate::vectors::*;
    use std::collections::VecDeque;

    enum Reply {
        Pdu(Vec<u8>),
        Fail(SdpError),
    }

    struct FakeTransport {
        replies: VecDeque<Reply>,
        sent: Vec<Vec<u8>>,
        echo_tid: bool,
        buf: Vec<u8>,
    }

    impl FakeTransport {
        fn new(replies: Vec<Reply>) -> Self {
            FakeTransport { replies: replies.into(), sent: Vec::new(), echo_tid: false, buf: Vec::new() }
        }
        fn pdus(pdus: &[&[u8]]) -> Self {
            Self::new(pdus.iter().map(|p| Reply::Pdu(p.to_vec())).collect())
        }
    }

    impl Transport for FakeTransport {
        fn exchange(&mut self, req: &[u8]) -> Result<&[u8], SdpError> {
            self.sent.push(req.to_vec());
            match self.replies.pop_front() {
                Some(Reply::Pdu(mut p)) => {
                    if self.echo_tid && p.len() >= 3 {
                        p[1..3].copy_from_slice(&req[1..3]);
                    }
                    self.buf = p;
                    Ok(&self.buf)
                }
                Some(Reply::Fail(e)) => Err(e),
                None => panic!("unexpected exchange"),
            }
        }
    }

    /// `07 tid plen | count | attr_lists | 00`: a complete answer in one PDU.
    fn wrap_attr_lists(tid: u16, lists: &[u8]) -> Vec<u8> {
        let mut v = vec![0x07];
        v.extend_from_slice(&tid.to_be_bytes());
        v.extend_from_slice(&((lists.len() + 3) as u16).to_be_bytes());
        v.extend_from_slice(&(lists.len() as u16).to_be_bytes());
        v.extend_from_slice(lists);
        v.push(0x00);
        v
    }

    fn wrap_single_record(tid: u16, record: &[u8]) -> Vec<u8> {
        let mut lists = vec![0x35, record.len() as u8];
        lists.extend_from_slice(record);
        wrap_attr_lists(tid, &lists)
    }

    const SPP: Uuid = Uuid::U16(0x1101);
    const ALL: &[AttrId] = &[AttrId::Range(0x0000, 0xffff)];

    fn search(t: &mut FakeTransport, first_tid: u16) -> Result<Vec<u8>, SdpError> {
        let req = SearchAttrRequest { pattern: &[SPP], max_attr_bytes: 0xffff, attr_ids: ALL };
        search_attr(t, &req, first_tid, &DEFAULT_LIMITS)
    }

    #[test]
    fn wrap_helper_reproduces_vector_b() {
        assert_eq!(wrap_single_record(1, RECORD_B), RESPONSE_B);
    }

    #[test]
    fn search_single_response_b() {
        let mut t = FakeTransport::pdus(&[RESPONSE_B]);
        assert_eq!(search(&mut t, 1).unwrap(), ATTR_LISTS_B);
        assert_eq!(t.sent, [VECTOR_A]);
    }

    #[test]
    fn search_continuation_c1_c3() {
        let mut t = FakeTransport::pdus(&[RESPONSE_C1, RESPONSE_C3]);
        assert_eq!(search(&mut t, 1).unwrap(), ATTR_LISTS_B);
        assert_eq!(t.sent, [VECTOR_A, REQUEST_C2]);
    }

    #[test]
    fn search_error_response_d() {
        let mut t = FakeTransport::pdus(&[ERROR_D3]);
        let err = search(&mut t, 3).unwrap_err();
        assert_eq!(err, SdpError::ErrorResponse(3));
        assert_eq!(err.errno(), errno::EBADMSG);
    }

    #[test]
    fn search_tid_mismatch_f9() {
        let mut t = FakeTransport::pdus(&[&[0x07, 0x00, 0x02, 0x00, 0x05, 0x00, 0x02, 0x35, 0x00, 0x00]]);
        assert_eq!(search(&mut t, 1), Err(SdpError::Protocol(ProtoFault::TidMismatch)));
        assert_eq!(t.sent.len(), 1);
    }

    #[test]
    fn search_round_cap() {
        let empty: &[u8] = &[0x07, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x02, 0x00, 0x14];
        let mut t = FakeTransport::pdus(&[empty; 200]);
        t.echo_tid = true;
        assert_eq!(search(&mut t, 1), Err(SdpError::TooLarge));
        assert_eq!(t.sent.len() as u32, DEFAULT_LIMITS.max_rounds);
        assert_eq!(t.sent.len(), 64);
    }

    #[test]
    fn search_byte_cap() {
        const FRAGMENT: usize = 60_000;
        let mut pdu = vec![0x07, 0x00, 0x00];
        pdu.extend_from_slice(&((FRAGMENT + 5) as u16).to_be_bytes());
        pdu.extend_from_slice(&(FRAGMENT as u16).to_be_bytes());
        pdu.resize(pdu.len() + FRAGMENT, 0x08);
        pdu.extend_from_slice(&[0x02, 0x01, 0xaa]);
        let mut t = FakeTransport::new((0..40).map(|_| Reply::Pdu(pdu.clone())).collect());
        t.echo_tid = true;
        assert_eq!(search(&mut t, 1), Err(SdpError::TooLarge));
        // 17 fragments fit under 1 MiB; the 18th is the one refused.
        assert_eq!(t.sent.len(), DEFAULT_LIMITS.max_attr_bytes / FRAGMENT + 1);
        assert_eq!(t.sent.len(), 18);
    }

    #[test]
    fn search_timeout_propagates() {
        let mut t = FakeTransport::new(vec![Reply::Fail(SdpError::Timeout)]);
        let err = search(&mut t, 1).unwrap_err();
        assert_eq!((err, err.errno()), (SdpError::Timeout, 110));
        let mut t = FakeTransport::new(vec![Reply::Fail(SdpError::Os(104))]);
        assert_eq!(search(&mut t, 1), Err(SdpError::Os(104)));
    }

    #[test]
    fn search_oversized_cstate_from_server_f7() {
        let mut pdu = vec![0x07, 0x00, 0x01, 0x00, 0x16, 0x00, 0x02, 0x35, 0x00, 0x11];
        pdu.extend(0u8..17);
        let mut t = FakeTransport::pdus(&[&pdu]);
        assert_eq!(search(&mut t, 1), Err(SdpError::Protocol(ProtoFault::BadContinuation)));
        assert_eq!(t.sent.len(), 1);
    }

    #[test]
    fn resolve_channel_b() {
        let mut t = FakeTransport::pdus(&[RESPONSE_B]);
        assert_eq!(resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS), Ok(Some(5)));
    }

    #[test]
    fn resolve_channel_continuation() {
        let mut t = FakeTransport::pdus(&[RESPONSE_C1, RESPONSE_C3]);
        assert_eq!(resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS), Ok(Some(5)));
    }

    #[test]
    fn resolve_no_records_empty_list() {
        let mut t = FakeTransport::pdus(&[EMPTY_LIST_RESPONSE]);
        assert_eq!(resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS), Ok(None));
    }

    #[test]
    fn resolve_device_id_record_no_channel() {
        let mut t = FakeTransport::pdus(&[&wrap_single_record(1, WINE_DEVICE_ID_RECORD)]);
        assert_eq!(resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS), Ok(None));
    }

    #[test]
    fn resolve_uuid128_uint16_e() {
        let mut t = FakeTransport::pdus(&[RESPONSE_E]);
        assert_eq!(resolve_channel(&mut t, SPP, 4, &DEFAULT_LIMITS), Ok(Some(5)));
        assert_eq!(t.sent[0][1..3], [0x00, 0x04]);
    }

    #[test]
    fn resolve_first_record_with_channel_wins() {
        let body = [WINE_DEVICE_ID_RECORD, RECORD_B].concat();
        let mut lists = vec![0x35, body.len() as u8];
        lists.extend_from_slice(&body);
        let mut t = FakeTransport::pdus(&[&wrap_attr_lists(1, &lists)]);
        assert_eq!(resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS), Ok(Some(5)));
    }

    #[test]
    fn resolve_malformed_attribute_lists_f13() {
        let pdu: &[u8] = &[
            0x07, 0x00, 0x01, 0x00, 0x0d, 0x00, 0x0a, 0x35, 0x40, 0x09, 0x00, 0x00, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00,
        ];
        let mut t = FakeTransport::pdus(&[pdu]);
        let err = resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS).unwrap_err();
        assert_eq!(err, SdpError::Malformed(ParseError::Overflow));
        assert_eq!(err.errno(), errno::EBADMSG);
    }

    #[test]
    fn resolve_empty_attribute_lists_is_malformed() {
        let mut t = FakeTransport::pdus(&[&[0x07, 0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00]]);
        assert_eq!(
            resolve_channel(&mut t, SPP, 1, &DEFAULT_LIMITS),
            Err(SdpError::Malformed(ParseError::Truncated))
        );
    }

    #[test]
    fn resolve_from_guid_bytes_reduces() {
        let mut t = FakeTransport::pdus(&[RESPONSE_B]);
        let class = Uuid::from_guid(&SPP_GUID_BYTES);
        assert_eq!(resolve_channel(&mut t, class, 1, &DEFAULT_LIMITS), Ok(Some(5)));
        assert_eq!(t.sent[0], VECTOR_A);
    }

    #[test]
    fn resolve_wide_guid_goes_out_as_uuid128() {
        let mut guid = SPP_GUID_BYTES;
        guid[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let mut t = FakeTransport::pdus(&[EMPTY_LIST_RESPONSE]);
        assert_eq!(resolve_channel(&mut t, Uuid::from_guid(&guid), 1, &DEFAULT_LIMITS), Ok(None));
        assert_eq!(t.sent[0][5..12], [0x35, 0x11, 0x1c, 0x12, 0x34, 0x56, 0x78]);
    }
}
