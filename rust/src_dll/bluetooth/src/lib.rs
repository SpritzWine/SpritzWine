//! Bluetooth SDP client for Wine's Unix side (libwine_bluetooth.so), no libbluetooth.
//!
//! C contract (dlls/ntdll/unix/socket.c, rfcomm_resolve_channel): `rs_bt_resolve_rfcomm_channel`
//! runs synchronously on the calling thread, blocks at most `timeout_ms` (0 = 15 s, capped at
//! 60 s), owns one L2CAP socket for the duration of the call, starts no threads, installs no
//! signal handlers and keeps no global state, so any number of threads may call it concurrently
//! and ntdll.so can carry it in every process. Returns the RFCOMM channel (1..=30), 0 if the
//! peer has no matching record, or -errno (Linux values; see error.rs). Panics never cross the
//! boundary. Layering: uuid/element/record/pdu are pure codecs, client.rs drives the
//! continuation loop over a `Transport`, l2cap.rs/sys.rs are the only host-facing modules.

use std::panic::{AssertUnwindSafe, catch_unwind};

mod client;
mod errno;
mod error;
mod l2cap;
mod sys;

// The codecs carry the record surface winebth.sys / bluetoothapis will need (names,
// handles, class lists, raw record bytes); ntdll's channel lookup uses part of it.
#[allow(dead_code)]
mod element;
#[allow(dead_code)]
mod pdu;
#[allow(dead_code)]
mod record;
#[allow(dead_code)]
mod uuid;
#[cfg(test)]
mod vectors;

use error::SdpError;
use l2cap::{Deadline, DEFAULT_TOTAL_MS, MAX_TOTAL_MS};
use sys::Sys;

/// `int rs_bt_resolve_rfcomm_channel( UINT64 bdaddr, const GUID *service_class, unsigned int timeout_ms )`
///
/// `bdaddr` is the Windows BTH_ADDR (low 48 bits; its little-endian bytes are the kernel's
/// bdaddr_t). `service_class` points at a GUID in Windows memory layout; it comes out of a
/// pack(1) SOCKADDR_BTH, so it may be unaligned and is read with read_unaligned. Null -> -EINVAL.
#[unsafe(no_mangle)]
pub extern "C" fn rs_bt_resolve_rfcomm_channel(bdaddr: u64, service_class: *const [u8; 16], timeout_ms: u32) -> i32 {
    if service_class.is_null() {
        return -errno::EINVAL;
    }
    let guid = read_guid(service_class);
    // A panic must never unwind into Wine's C. -EIO is unknown to sock_errno_to_status, which
    // prints a FIXME: a bug should be loud. The C side then reports it as unreachable.
    catch_unwind(AssertUnwindSafe(|| resolve(&sys::HostSys, bdaddr, &guid, timeout_ms))).unwrap_or(-errno::EIO)
}

fn read_guid(p: *const [u8; 16]) -> [u8; 16] {
    // SAFETY: non-null (checked by the caller), and ws2_32's own copy of the sockaddr keeps
    // the 16 bytes alive for the duration of the call; read_unaligned tolerates any alignment.
    unsafe { core::ptr::read_unaligned(p) }
}

/// Pure orchestration, testable end to end with sys::fake::FakeSys.
fn resolve<S: Sys>(sys: &S, bdaddr: u64, guid: &[u8; 16], timeout_ms: u32) -> i32 {
    let Some(addr) = l2cap::bdaddr_from_windows(bdaddr) else { return -errno::EINVAL };
    let total = if timeout_ms == 0 { DEFAULT_TOTAL_MS } else { timeout_ms.min(MAX_TOTAL_MS) };
    let deadline = Deadline::after(sys, total);
    let class = uuid::Uuid::from_guid(guid);
    let outcome: Result<Option<u8>, SdpError> = l2cap::connect(sys, addr, deadline)
        .and_then(|mut session| client::resolve_channel(&mut session, class, 1, &client::DEFAULT_LIMITS));
    // The session (and its fd) is dropped here, before control returns to C.
    match outcome {
        Ok(Some(channel)) => channel as i32,
        Ok(None) => 0,
        Err(e) => -e.errno(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errno::EINPROGRESS;
    use crate::sys::fake::{FakeSys, Step};
    use crate::sys::{POLLIN, POLLOUT};
    use crate::vectors::{EMPTY_LIST_RESPONSE, RESPONSE_B, SPP_GUID_BYTES, VECTOR_A};

    const WIN_ADDR: u64 = 0x0011_2233_4455;

    /// Connect, send the request, receive `reply`.
    fn exchange(reply: &[u8]) -> Vec<Step> {
        vec![
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Poll(Ok(POLLOUT)),
            Step::SoError(Ok(0)),
            Step::Send(Ok(20)),
            Step::Poll(Ok(POLLIN)),
            Step::Recv(Ok(reply.to_vec())),
        ]
    }

    #[test]
    fn ffi_null_guid_is_einval() {
        assert_eq!(rs_bt_resolve_rfcomm_channel(0, core::ptr::null(), 0), -22);
    }

    #[test]
    fn ffi_bad_bdaddr_is_einval() {
        // An empty script: any host call would panic.
        assert_eq!(resolve(&FakeSys::new(vec![]), 1 << 48, &SPP_GUID_BYTES, 0), -22);
    }

    #[test]
    fn ffi_unaligned_guid_pointer_is_read() {
        let mut buf = [0u8; 17];
        buf[1..].copy_from_slice(&SPP_GUID_BYTES);
        assert_eq!(read_guid(buf[1..].as_ptr() as *const [u8; 16]), SPP_GUID_BYTES);
    }

    #[test]
    fn resolve_end_to_end_vector_b() {
        let fake = FakeSys::new(exchange(RESPONSE_B));
        assert_eq!(resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 0), 5);
        assert_eq!(*fake.sent.borrow(), [VECTOR_A]);
        assert_eq!(*fake.closed.borrow(), [100]);
    }

    #[test]
    fn resolve_end_to_end_no_record() {
        let fake = FakeSys::new(exchange(EMPTY_LIST_RESPONSE));
        assert_eq!(resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 0), 0);
        assert_eq!(*fake.closed.borrow(), [100]);
    }

    #[test]
    fn resolve_end_to_end_error_response() {
        let fake = FakeSys::new(exchange(&[0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03]));
        assert_eq!(resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 0), -74);
    }

    #[test]
    fn resolve_default_timeout_is_15s() {
        // 14 s pass while the connect completes, leaving 1 s of the default 15 for the PDU.
        let fake = FakeSys::new(vec![
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Poll(Ok(POLLOUT)),
            Step::Advance(14_000),
            Step::SoError(Ok(0)),
            Step::Send(Ok(20)),
            Step::Poll(Ok(0)),
        ]);
        assert_eq!(resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 0), -110);
        assert_eq!(*fake.polls.borrow(), [10_000, 1_000]);
    }

    #[test]
    fn resolve_clamps_timeout() {
        // 120 s asked, 60 s granted: 55 s in, the connect stage has 5 s left.
        let fake = FakeSys::new(vec![
            Step::Advance(55_000),
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Poll(Ok(0)),
        ]);
        assert_eq!(resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 120_000), -113);
        assert_eq!(fake.polls.borrow().last(), Some(&5_000));
    }

    #[test]
    fn resolve_honours_a_short_timeout() {
        let fake = FakeSys::new(vec![Step::Socket(Ok(())), Step::Connect(Err(EINPROGRESS)), Step::Poll(Ok(0))]);
        assert_eq!(resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 2_500), -113);
        assert_eq!(*fake.polls.borrow(), [2_500]);
    }
}
