//! Failure classes of one SDP lookup and their single errno each. The C side
//! (dlls/ntdll/unix/socket.c, rfcomm_errno_to_status) has an explicit case for
//! every value `errno()` can return, so the set here is closed.

#![forbid(unsafe_code)]

use crate::element::ParseError;
use crate::errno;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtoFault {
    ShortPdu,
    LengthMismatch,
    TidMismatch,
    UnexpectedPdu(u8),
    BadByteCount,
    BadContinuation,
    ShortSend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdpError {
    InvalidArgument,       // EINVAL 22: bdaddr with bits above 48, empty pattern, cstate > 16
    Os(i32),               // errno of the failing socket/connect/SO_ERROR/poll/send/recv/getsockopt, verbatim;
                           // also EBUSY after the retry window and ECONNRESET for recv() == 0
    ConnectTimeout,        // EHOSTUNREACH 113: our connect budget expired before the kernel's verdict
    Timeout,               // ETIMEDOUT 110: no PDU within the per-transaction budget
    Protocol(ProtoFault),  // EPROTO 71: framing, tid, pdu id, byte count, continuation state, short send
    ErrorResponse(u16),    // EBADMSG 74: server sent SDP_ERROR_RSP (code kept for tests/diagnostics)
    Malformed(ParseError), // EBADMSG 74: element / record syntax in the AttributeLists
    TooLarge,              // EMSGSIZE 90: SDU > 65535, > max_attr_bytes, > max_rounds
    NoMemory,              // ENOMEM 12: try_reserve failed
}

impl SdpError {
    /// The one number that crosses the FFI (negated by the shim). Every value has an explicit
    /// case in rfcomm_errno_to_status() or in sock_errno_to_status() (dlls/ntdll/unix/socket.c).
    pub fn errno(&self) -> i32 {
        match *self {
            SdpError::InvalidArgument => errno::EINVAL,
            SdpError::Os(e) if e > 0 => e,
            SdpError::Os(_) => errno::EIO,
            SdpError::ConnectTimeout => errno::EHOSTUNREACH,
            SdpError::Timeout => errno::ETIMEDOUT,
            SdpError::Protocol(_) => errno::EPROTO,
            SdpError::ErrorResponse(_) | SdpError::Malformed(_) => errno::EBADMSG,
            SdpError::TooLarge => errno::EMSGSIZE,
            SdpError::NoMemory => errno::ENOMEM,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_mapping_table() {
        let table = [
            (SdpError::InvalidArgument, 22),
            (SdpError::Os(113), 113),
            (SdpError::Os(0), 5),
            (SdpError::Os(-7), 5),
            (SdpError::ConnectTimeout, 113),
            (SdpError::Timeout, 110),
            (SdpError::Protocol(ProtoFault::ShortPdu), 71),
            (SdpError::Protocol(ProtoFault::UnexpectedPdu(0x42)), 71),
            (SdpError::ErrorResponse(3), 74),
            (SdpError::Malformed(ParseError::Overflow), 74),
            (SdpError::TooLarge, 90),
            (SdpError::NoMemory, 12),
        ];
        for (e, want) in table {
            assert_eq!(e.errno(), want, "{e:?}");
        }
    }
}
