#![forbid(unsafe_code)]
use crate::element::encode_seq_header;
use crate::error::{ProtoFault, SdpError};
use crate::uuid::Uuid;

pub const PDU_ERROR_RSP: u8 = 0x01;
pub const PDU_SVC_SEARCH_ATTR_REQ: u8 = 0x06;
pub const PDU_SVC_SEARCH_ATTR_RSP: u8 = 0x07;
pub const MAX_CSTATE: usize = 16;
pub const MAX_PDU: usize = 65535; // an L2CAP SDU never exceeds the 16-bit MTU

#[derive(Clone, Copy, Debug)]
pub enum AttrId { Single(u16), Range(u16, u16) }

pub struct SearchAttrRequest<'r> {
    pub pattern: &'r [Uuid],      // 1..=12 UUIDs, already reduced by the caller
    pub max_attr_bytes: u16,      // >= 7; we send 0xffff like BlueZ
    pub attr_ids: &'r [AttrId],   // ascending, non-overlapping; Range(0, 0xffff) = everything
}

#[derive(Debug, PartialEq, Eq)]
pub enum Response<'p> {
    SearchAttr { attr_lists: &'p [u8], cstate: &'p [u8] },
    Error(u16),
}

pub fn encode_search_attr_request(req: &SearchAttrRequest, tid: u16, cstate: &[u8], out: &mut Vec<u8>)
    -> Result<(), SdpError>
{
    if req.pattern.is_empty() || req.pattern.len() > 12 || req.attr_ids.is_empty()
        || req.max_attr_bytes < 7 || cstate.len() > MAX_CSTATE
    {
        return Err(SdpError::InvalidArgument);
    }
    out.clear();
    out.push(PDU_SVC_SEARCH_ATTR_REQ);
    out.extend_from_slice(&tid.to_be_bytes());
    out.extend_from_slice(&[0, 0]); // plen, patched below

    let mut pattern = Vec::with_capacity(12 * 17);
    for u in req.pattern {
        u.encode(&mut pattern);
    }
    encode_seq_header(pattern.len(), out);
    out.extend_from_slice(&pattern);

    out.extend_from_slice(&req.max_attr_bytes.to_be_bytes());

    let mut ids = Vec::with_capacity(req.attr_ids.len() * 5);
    for a in req.attr_ids {
        match *a {
            AttrId::Single(id) => { ids.push(0x09); ids.extend_from_slice(&id.to_be_bytes()); }
            AttrId::Range(lo, hi) => {
                ids.push(0x0a);
                ids.extend_from_slice(&lo.to_be_bytes());
                ids.extend_from_slice(&hi.to_be_bytes());
            }
        }
    }
    encode_seq_header(ids.len(), out);
    out.extend_from_slice(&ids);

    out.push(cstate.len() as u8);
    out.extend_from_slice(cstate);

    let plen = out.len() - 5;
    if plen > 0xffff {
        return Err(SdpError::InvalidArgument);
    }
    out[3..5].copy_from_slice(&(plen as u16).to_be_bytes());
    Ok(())
}

pub fn decode_response(pdu: &[u8], expected_tid: u16) -> Result<Response<'_>, SdpError> {
    use ProtoFault::*;
    if pdu.len() < 5 {
        return Err(SdpError::Protocol(ShortPdu));
    }
    let tid = u16::from_be_bytes([pdu[1], pdu[2]]);
    let plen = u16::from_be_bytes([pdu[3], pdu[4]]) as usize;
    if plen != pdu.len() - 5 {
        return Err(SdpError::Protocol(LengthMismatch)); // f10: strict both ways
    }
    if tid != expected_tid {
        return Err(SdpError::Protocol(TidMismatch)); // f9, checked before the pdu id
    }
    let params = &pdu[5..];
    match pdu[0] {
        PDU_ERROR_RSP => {
            if plen != 2 {
                return Err(SdpError::Protocol(LengthMismatch)); // f12
            }
            Ok(Response::Error(u16::from_be_bytes([params[0], params[1]])))
        }
        PDU_SVC_SEARCH_ATTR_RSP => {
            if plen < 3 {
                return Err(SdpError::Protocol(LengthMismatch));
            }
            let count = u16::from_be_bytes([params[0], params[1]]) as usize;
            if count > plen - 3 {
                return Err(SdpError::Protocol(BadByteCount)); // f8: room for count bytes + the cstate length byte
            }
            let cs_len = params[2 + count] as usize;
            if cs_len > MAX_CSTATE {
                return Err(SdpError::Protocol(BadContinuation)); // f7
            }
            if 3 + count + cs_len != plen {
                return Err(SdpError::Protocol(LengthMismatch)); // trailing or missing cstate bytes
            }
            Ok(Response::SearchAttr {
                attr_lists: &params[2..2 + count],
                cstate: &params[3 + count..3 + count + cs_len],
            })
        }
        other => Err(SdpError::Protocol(UnexpectedPdu(other))), // f11
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::*;

    const ALL: &[AttrId] = &[AttrId::Range(0x0000, 0xffff)];

    fn encode(pattern: &[Uuid], max: u16, tid: u16, cstate: &[u8]) -> Result<Vec<u8>, SdpError> {
        let mut out = Vec::new();
        let req = SearchAttrRequest { pattern, max_attr_bytes: max, attr_ids: ALL };
        encode_search_attr_request(&req, tid, cstate, &mut out).map(|()| out)
    }

    fn fault(pdu: &[u8], tid: u16) -> ProtoFault {
        match decode_response(pdu, tid) {
            Err(SdpError::Protocol(f)) => f,
            other => panic!("expected a protocol fault, got {other:?}"),
        }
    }

    #[test]
    fn request_matches_vector_a() {
        assert_eq!(encode(&[Uuid::U16(0x1101)], 0xffff, 1, &[]).unwrap(), VECTOR_A);
    }

    #[test]
    fn request_from_windows_guid_matches_vector_a() {
        assert_eq!(encode(&[Uuid::from_guid(&SPP_GUID_BYTES)], 0xffff, 1, &[]).unwrap(), VECTOR_A);
    }

    #[test]
    fn resend_matches_vector_c2() {
        assert_eq!(encode(&[Uuid::U16(0x1101)], 0xffff, 2, &[0x00, 0x14]).unwrap(), REQUEST_C2);
    }

    #[test]
    fn request_uuid128_pattern() {
        let mut wire = Uuid::BASE;
        wire[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        wire[15] = 0xfc;
        let out = encode(&[Uuid::U128(wire)], 0xffff, 1, &[]).unwrap();
        assert_eq!(out[3..5], [0x00, 0x1d]);
        assert_eq!(out[5..8], [0x35, 0x11, 0x1c]);
        assert_eq!(out[8..24], wire);
    }

    #[test]
    fn request_single_attribute_id() {
        let mut out = Vec::new();
        let req = SearchAttrRequest {
            pattern: &[Uuid::U16(0x1101)],
            max_attr_bytes: 0xffff,
            attr_ids: &[AttrId::Single(0x0004)],
        };
        encode_search_attr_request(&req, 1, &[], &mut out).unwrap();
        assert_eq!(out[12..], [0x35, 0x03, 0x09, 0x00, 0x04, 0x00]);
        assert!(out.len() <= MAX_PDU);
    }

    #[test]
    fn request_rejects_bad_args() {
        let one = [Uuid::U16(0x1101)];
        assert_eq!(encode(&[], 0xffff, 1, &[]).err(), Some(SdpError::InvalidArgument));
        assert_eq!(encode(&[Uuid::U16(1); 13], 0xffff, 1, &[]).err(), Some(SdpError::InvalidArgument));
        assert_eq!(encode(&one, 0xffff, 1, &[0u8; 17]).err(), Some(SdpError::InvalidArgument));
        assert_eq!(encode(&one, 6, 1, &[]).err(), Some(SdpError::InvalidArgument));
        assert!(encode(&[Uuid::U16(1); 12], 7, 1, &[0u8; 16]).is_ok());
    }

    #[test]
    fn response_b_decodes() {
        assert_eq!(decode_response(RESPONSE_B, 1), Ok(Response::SearchAttr { attr_lists: ATTR_LISTS_B, cstate: &[] }));
    }

    #[test]
    fn response_c1_decodes() {
        assert_eq!(
            decode_response(RESPONSE_C1, 1),
            Ok(Response::SearchAttr { attr_lists: &ATTR_LISTS_B[..20], cstate: &[0x00, 0x14] })
        );
    }

    #[test]
    fn response_c3_decodes() {
        let Ok(Response::SearchAttr { attr_lists: c3, cstate }) = decode_response(RESPONSE_C3, 2) else {
            panic!("c3 did not decode");
        };
        assert_eq!((c3.len(), cstate.len()), (33, 0));
        let Ok(Response::SearchAttr { attr_lists: c1, .. }) = decode_response(RESPONSE_C1, 1) else {
            panic!("c1 did not decode");
        };
        assert_eq!([c1, c3].concat(), ATTR_LISTS_B);
    }

    #[test]
    fn response_e_decodes() {
        let Ok(Response::SearchAttr { attr_lists, cstate }) = decode_response(RESPONSE_E, 4) else {
            panic!("e did not decode");
        };
        assert_eq!((attr_lists.len(), cstate.len()), (52, 0));
        assert_eq!(attr_lists[2..], *RECORD_E);
    }

    #[test]
    fn error_response_d() {
        assert_eq!(decode_response(ERROR_D3, 3), Ok(Response::Error(0x0003)));
        assert_eq!(decode_response(ERROR_D2, 3), Ok(Response::Error(0x0002)));
    }

    #[test]
    fn f7_cstate_len_17() {
        let mut pdu = vec![0x07, 0x00, 0x01, 0x00, 0x16, 0x00, 0x02, 0x35, 0x00, 0x11];
        pdu.extend(0u8..17);
        assert_eq!(fault(&pdu, 1), ProtoFault::BadContinuation);
    }

    #[test]
    fn f8_byte_count_too_large() {
        assert_eq!(fault(&[0x07, 0x00, 0x01, 0x00, 0x05, 0x00, 0x10, 0x35, 0x00, 0x00], 1), ProtoFault::BadByteCount);
        assert_eq!(fault(&[0x07, 0x00, 0x01, 0x00, 0x04, 0x00, 0x02, 0x35, 0x00], 1), ProtoFault::BadByteCount);
    }

    #[test]
    fn f9_tid_mismatch() {
        assert_eq!(fault(&[0x07, 0x00, 0x02, 0x00, 0x05, 0x00, 0x02, 0x35, 0x00, 0x00], 1), ProtoFault::TidMismatch);
    }

    #[test]
    fn f10_plen_mismatch() {
        assert_eq!(fault(&[0x07, 0x00, 0x01, 0x00, 0x09, 0x00, 0x02, 0x35, 0x00, 0x00], 1), ProtoFault::LengthMismatch);
        assert_eq!(fault(&[0x07, 0x00, 0x01, 0x00, 0x03, 0x00, 0x02, 0x35, 0x00, 0x00], 1), ProtoFault::LengthMismatch);
        assert_eq!(fault(&[0x07, 0x00, 0x01], 1), ProtoFault::ShortPdu);
    }

    #[test]
    fn f11_wrong_pdu_id() {
        assert_eq!(
            fault(&[0x03, 0x00, 0x01, 0x00, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], 1),
            ProtoFault::UnexpectedPdu(0x03)
        );
        assert_eq!(fault(&[0x42, 0x00, 0x01, 0x00, 0x00], 1), ProtoFault::UnexpectedPdu(0x42));
    }

    #[test]
    fn f12_error_response_bad_length() {
        assert_eq!(fault(&[0x01, 0x00, 0x01, 0x00, 0x01, 0x00], 1), ProtoFault::LengthMismatch);
        assert_eq!(fault(&[0x01, 0x00, 0x01, 0x00, 0x00], 1), ProtoFault::LengthMismatch);
    }

    #[test]
    fn search_attr_response_too_short_for_its_fields() {
        assert_eq!(fault(&[0x07, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00], 1), ProtoFault::LengthMismatch);
    }

    #[test]
    fn cstate_trailing_bytes_rejected() {
        let pdu = [0x07, 0x00, 0x01, 0x00, 0x07, 0x00, 0x02, 0x35, 0x00, 0x01, 0xaa, 0xbb];
        assert_eq!(fault(&pdu, 1), ProtoFault::LengthMismatch);
    }

    #[test]
    fn zero_byte_fragment_with_cstate_is_legal() {
        assert_eq!(
            decode_response(&[0x07, 0x00, 0x01, 0x00, 0x05, 0x00, 0x00, 0x02, 0x00, 0x14], 1),
            Ok(Response::SearchAttr { attr_lists: &[], cstate: &[0x00, 0x14] })
        );
    }

    #[test]
    fn error_with_foreign_tid_is_tid_mismatch() {
        assert_eq!(fault(&[0x01, 0x00, 0x09, 0x00, 0x02, 0x00, 0x03], 1), ProtoFault::TidMismatch);
    }
}
