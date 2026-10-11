//! L2CAP transport for the SDP client: one non-blocking SOCK_SEQPACKET socket to PSM 1,
//! driven synchronously on the caller's thread with poll(2) under one overall deadline.
//! All host calls go through `Sys` so the state machine runs against a fake in tests.

#![forbid(unsafe_code)]

use crate::client::Transport;
use crate::errno;
use crate::error::{ProtoFault, SdpError};
use crate::sys::{SockaddrL2, Sys};

pub const DEFAULT_TOTAL_MS: u32 = 15_000;
pub const MAX_TOTAL_MS: u32 = 60_000;
/// Must stay above the HCI page timeout (5.12 s) so a powered-off peer surfaces as the
/// kernel's EHOSTDOWN; if the adapter is slower than that, ConnectTimeout reports the same
/// EHOSTUNREACH class, never ETIMEDOUT.
pub const CONNECT_MS: u32 = 10_000;
pub const PDU_MS: u32 = 5_000;
pub const BUSY_SLEEP_MS: u32 = 100;
pub const BUSY_WINDOW_MS: u64 = 3_000;
/// Largest L2CAP SDU; with MSG_TRUNC a longer recv() result means data was dropped.
pub const RECV_BUF: usize = 65_535;

/// Windows BTH_ADDR (u64, low 48 bits) -> bdaddr_t: the 6 low LE bytes, no swap (C reference memcpy).
pub fn bdaddr_from_windows(addr: u64) -> Option<[u8; 6]> {
    if addr >> 48 != 0 { return None; }
    let b = addr.to_le_bytes();
    Some([b[0], b[1], b[2], b[3], b[4], b[5]])
}

/// Absolute point on the Sys monotonic clock.
#[derive(Clone, Copy)]
pub struct Deadline { end_ms: u64 }

impl Deadline {
    pub fn after(sys: &impl Sys, ms: u32) -> Self { Self { end_ms: sys.now_ms().saturating_add(ms as u64) } }
    /// Milliseconds left, 0 once expired, clamped to i32 for poll().
    pub fn left(&self, sys: &impl Sys) -> u32 {
        let now = sys.now_ms();
        if now >= self.end_ms { 0 } else { (self.end_ms - now).min(i32::MAX as u64) as u32 }
    }
    /// A per-stage deadline: `cap` from now, but never past the overall one.
    pub fn stage(&self, sys: &impl Sys, cap: u32) -> Self { Deadline::after(sys, self.left(sys).min(cap)) }
}

/// Owned socket: closed exactly once on every path out of scope, including unwinding.
pub struct SockFd<'s, S: Sys> { fd: i32, sys: &'s S }

impl<'s, S: Sys> SockFd<'s, S> {
    pub fn open(sys: &'s S) -> Result<Self, SdpError> {
        sys.socket().map(|fd| Self { fd, sys }).map_err(SdpError::Os)
    }
    pub fn raw(&self) -> i32 { self.fd }
}

impl<S: Sys> Drop for SockFd<'_, S> {
    fn drop(&mut self) { self.sys.close(self.fd); }
}

/// Ok(true) = ready (POLLERR/POLLHUP included: SO_ERROR or recv report the cause),
/// Ok(false) = stage deadline reached. poll is never auto-restarted by SA_RESTART, so EINTR
/// (Wine's SIGUSR1 suspend) re-enters with the remaining time; the deadline is absolute, so a
/// storm of signals cannot extend the budget.
fn wait<S: Sys>(sys: &S, fd: i32, events: i16, stage: &Deadline) -> Result<bool, SdpError> {
    loop {
        let left = stage.left(sys);
        if left == 0 {
            return Ok(false);
        }
        match sys.poll1(fd, events, left as i32) {
            Ok(0) => return Ok(false),
            Ok(_) => return Ok(true),
            Err(errno::EINTR) => continue,
            Err(e) => return Err(SdpError::Os(e)),
        }
    }
}

fn connect_once<S: Sys>(sys: &S, sock: &SockFd<S>, addr: &SockaddrL2, stage: &Deadline) -> Result<(), SdpError> {
    loop {
        match sys.connect(sock.raw(), addr) {
            Ok(()) => return Ok(()),
            Err(errno::EINTR) => continue,
            Err(errno::EINPROGRESS) | Err(errno::EAGAIN) => break,
            Err(e) => return Err(SdpError::Os(e)),
        }
    }
    // Completion is POLLOUT (or POLLERR/POLLHUP on failure); either way SO_ERROR holds the verdict.
    if !wait(sys, sock.raw(), crate::sys::POLLOUT, stage)? {
        return Err(SdpError::ConnectTimeout);
    }
    match sys.so_error(sock.raw()) {
        Ok(0) => Ok(()),
        Ok(e) => Err(SdpError::Os(e)),
        Err(e) => Err(SdpError::Os(e)),
    }
}

pub struct Session<'s, S: Sys> { sock: SockFd<'s, S>, sys: &'s S, deadline: Deadline, rx: Vec<u8> }

/// Connect to the peer's SDP server. EBUSY (an ACL/L2CAP setup to this bdaddr is already in
/// flight, e.g. the app's own RFCOMM socket on another thread) is retried on a FRESH socket
/// every 100 ms for up to 3 s, never past the overall deadline; BlueZ spins without sleeping.
pub fn connect<'s, S: Sys>(sys: &'s S, bdaddr: [u8; 6], deadline: Deadline) -> Result<Session<'s, S>, SdpError> {
    let addr = SockaddrL2::sdp(bdaddr);
    let busy_until = sys.now_ms().saturating_add(BUSY_WINDOW_MS);
    loop {
        let sock = SockFd::open(sys)?;
        let stage = deadline.stage(sys, CONNECT_MS);
        match connect_once(sys, &sock, &addr, &stage) {
            Ok(()) => {
                // Heap, not stack: the hook runs on an app thread of unknown stack size, and an
                // allocation failure must report instead of aborting the process.
                let mut rx = Vec::new();
                rx.try_reserve_exact(RECV_BUF).map_err(|_| SdpError::NoMemory)?; // sock dropped -> closed
                rx.resize(RECV_BUF, 0);
                return Ok(Session { sock, sys, deadline, rx });
            }
            Err(SdpError::Os(errno::EBUSY)) => {
                drop(sock); // a socket whose connect failed is not reused
                if sys.now_ms() >= busy_until || deadline.left(sys) <= BUSY_SLEEP_MS {
                    return Err(SdpError::Os(errno::EBUSY));
                }
                sys.sleep_ms(BUSY_SLEEP_MS);
            }
            Err(e) => return Err(e), // sock dropped -> closed
        }
    }
}

impl<S: Sys> Transport for Session<'_, S> {
    fn exchange(&mut self, req: &[u8]) -> Result<&[u8], SdpError> {
        let stage = self.deadline.stage(self.sys, PDU_MS);
        let fd = self.sock.raw();
        // SEQPACKET send is all-or-nothing; a short count would mean a second, different SDU
        // if we looped, so it is a protocol fault instead.
        loop {
            match self.sys.send(fd, req) {
                Ok(n) if n == req.len() => break,
                Ok(_) => return Err(SdpError::Protocol(ProtoFault::ShortSend)),
                Err(errno::EINTR) => continue,
                Err(errno::EAGAIN) => {
                    if !wait(self.sys, fd, crate::sys::POLLOUT, &stage)? {
                        return Err(SdpError::Timeout);
                    }
                }
                Err(e) => return Err(SdpError::Os(e)),
            }
        }
        loop {
            if !wait(self.sys, fd, crate::sys::POLLIN, &stage)? {
                return Err(SdpError::Timeout);
            }
            match self.sys.recv(fd, &mut self.rx) {
                Ok(n) if n > RECV_BUF => return Err(SdpError::TooLarge),       // MSG_TRUNC: real SDU length
                Ok(0) => return Err(SdpError::Os(errno::ECONNRESET)),          // orderly close by the SDP server
                Ok(n) => return Ok(&self.rx[..n]),
                Err(errno::EINTR) | Err(errno::EAGAIN) => continue,            // spurious wakeup; stage still bounds us
                Err(e) => return Err(SdpError::Os(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errno::*;
    use crate::sys::fake::{FakeSys, Step};
    use crate::sys::{POLLIN, POLLOUT};
    use crate::vectors::{RESPONSE_B, SPP_GUID_BYTES, VECTOR_A};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    const POLLERR: i16 = 0x008;
    const BDADDR: [u8; 6] = [0x55, 0x44, 0x33, 0x22, 0x11, 0x00];
    const WIN_ADDR: u64 = 0x0011_2233_4455;

    fn happy() -> Vec<Step> {
        vec![Step::Socket(Ok(())), Step::Connect(Err(EINPROGRESS)), Step::Poll(Ok(POLLOUT)), Step::SoError(Ok(0))]
    }

    fn script(parts: Vec<Vec<Step>>) -> FakeSys {
        FakeSys::new(parts.into_iter().flatten().collect())
    }

    /// Outcome of `connect` under an overall budget of `total` ms.
    fn connect_err(fake: &FakeSys, total: u32) -> Option<SdpError> {
        connect(fake, BDADDR, Deadline::after(fake, total)).err()
    }

    fn session(fake: &FakeSys, total: u32) -> Session<'_, FakeSys> {
        connect(fake, BDADDR, Deadline::after(fake, total)).unwrap_or_else(|e| panic!("connect failed: {e:?}"))
    }

    fn all_closed(fake: &FakeSys) {
        let closed = fake.closed.borrow();
        assert_eq!(closed.len() as u32, fake.opened.get(), "every opened fd is closed");
        let mut seen = closed.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), closed.len(), "no fd closed twice");
        assert!(closed.iter().all(|fd| (100..100 + fake.opened.get() as i32).contains(fd)));
        assert_eq!(fake.live_fd.get(), None);
    }

    #[test]
    fn sockaddr_l2_layout() {
        let fake = FakeSys::new(happy());
        drop(session(&fake, DEFAULT_TOTAL_MS));
        assert_eq!(
            fake.connects.borrow()[0],
            [0x1f, 0x00, 0x01, 0x00, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn bdaddr_windows_order() {
        assert_eq!(bdaddr_from_windows(WIN_ADDR), Some(BDADDR));
    }

    #[test]
    fn bdaddr_rejects_high_bits() {
        assert_eq!(bdaddr_from_windows(0x0001_0000_0000_0000), None);
        assert_eq!(bdaddr_from_windows(0x0000_ffff_ffff_ffff), Some([0xff; 6]));
    }

    #[test]
    fn deadline_left_saturates() {
        let fake = FakeSys::new(vec![]);
        let d = Deadline::after(&fake, 1000);
        assert_eq!(d.left(&fake), 1000);
        assert_eq!(d.stage(&fake, 300).left(&fake), 300);
        assert_eq!(d.stage(&fake, 5000).left(&fake), 1000);
        fake.now.set(2000);
        assert_eq!(d.left(&fake), 0);
        assert_eq!(d.stage(&fake, 300).left(&fake), 0);
        let far = Deadline::after(&fake, u32::MAX);
        assert_eq!(far.left(&fake), i32::MAX as u32);
    }

    #[test]
    fn no_bluetooth_is_os_eafnosupport() {
        let fake = FakeSys::new(vec![Step::Socket(Err(EAFNOSUPPORT))]);
        assert_eq!(connect_err(&fake, DEFAULT_TOTAL_MS), Some(SdpError::Os(97)));
        assert!(fake.closed.borrow().is_empty());
    }

    #[test]
    fn connect_sync_error_closes_fd() {
        let fake = FakeSys::new(vec![Step::Socket(Ok(())), Step::Connect(Err(EHOSTUNREACH))]);
        assert_eq!(connect_err(&fake, DEFAULT_TOTAL_MS), Some(SdpError::Os(113)));
        assert_eq!(*fake.closed.borrow(), [100]);
    }

    #[test]
    fn connect_happy_then_drop_closes() {
        let fake = FakeSys::new(happy());
        let s = session(&fake, DEFAULT_TOTAL_MS);
        assert!(fake.closed.borrow().is_empty());
        drop(s);
        assert_eq!(*fake.closed.borrow(), [100]);
        assert_eq!(fake.opened.get(), 1);
    }

    #[test]
    fn connect_sync_success_needs_no_poll() {
        let fake = FakeSys::new(vec![Step::Socket(Ok(())), Step::Connect(Ok(()))]);
        drop(session(&fake, DEFAULT_TOTAL_MS));
        assert!(fake.polls.borrow().is_empty());
    }

    #[test]
    fn connect_soerror_ehostdown() {
        let fake = FakeSys::new(vec![
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Poll(Ok(POLLOUT)),
            Step::SoError(Ok(EHOSTDOWN)),
        ]);
        assert_eq!(connect_err(&fake, DEFAULT_TOTAL_MS), Some(SdpError::Os(112)));
        assert_eq!(*fake.closed.borrow(), [100]);
    }

    #[test]
    fn connect_pollerr_still_reads_so_error() {
        let fake = FakeSys::new(vec![
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Poll(Ok(POLLERR)),
            Step::SoError(Ok(ECONNREFUSED)),
        ]);
        assert_eq!(connect_err(&fake, DEFAULT_TOTAL_MS), Some(SdpError::Os(111)));
    }

    #[test]
    fn connect_poll_timeout_is_connect_timeout() {
        let fake = FakeSys::new(vec![Step::Socket(Ok(())), Step::Connect(Err(EINPROGRESS)), Step::Poll(Ok(0))]);
        let err = connect_err(&fake, DEFAULT_TOTAL_MS).unwrap();
        assert_eq!(err, SdpError::ConnectTimeout);
        assert_eq!(err.errno(), 113);
        assert_eq!(*fake.polls.borrow(), [10000]);
        all_closed(&fake);
    }

    #[test]
    fn connect_timeout_honours_overall_budget() {
        let fake = FakeSys::new(vec![Step::Socket(Ok(())), Step::Connect(Err(EINPROGRESS)), Step::Poll(Ok(0))]);
        assert_eq!(connect_err(&fake, 3000), Some(SdpError::ConnectTimeout));
        assert_eq!(*fake.polls.borrow(), [3000]);
    }

    #[test]
    fn eintr_retries_with_remaining_time() {
        // The clock jump is applied by the interrupted poll itself.
        let fake = FakeSys::new(vec![
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Advance(400),
            Step::Poll(Err(EINTR)),
            Step::Poll(Ok(POLLOUT)),
            Step::SoError(Ok(0)),
        ]);
        drop(session(&fake, DEFAULT_TOTAL_MS));
        assert_eq!(*fake.polls.borrow(), [10000, 9600]);
    }

    #[test]
    fn connect_eintr_retries_connect() {
        let fake = script(vec![
            vec![Step::Socket(Ok(())), Step::Connect(Err(EINTR))],
            happy().into_iter().skip(1).collect(),
        ]);
        drop(session(&fake, DEFAULT_TOTAL_MS));
        assert_eq!(fake.connects.borrow().len(), 2);
        assert_eq!(fake.opened.get(), 1);
    }

    fn busy() -> Vec<Step> {
        vec![Step::Socket(Ok(())), Step::Connect(Err(EBUSY))]
    }

    #[test]
    fn ebusy_retries_then_succeeds() {
        let fake = script(vec![busy(), busy(), busy(), happy()]);
        let s = session(&fake, DEFAULT_TOTAL_MS);
        assert_eq!(fake.opened.get(), 4);
        assert_eq!(*fake.closed.borrow(), [100, 101, 102]);
        assert_eq!(fake.slept_ms.get(), 300);
        drop(s);
        assert_eq!(*fake.closed.borrow(), [100, 101, 102, 103]);
    }

    #[test]
    fn ebusy_exhausts_after_window() {
        let fake = script((0..40).map(|_| busy()).collect());
        assert_eq!(connect_err(&fake, DEFAULT_TOTAL_MS), Some(SdpError::Os(EBUSY)));
        assert_eq!(fake.now.get(), BUSY_WINDOW_MS);
        assert_eq!(fake.opened.get(), 31);
        all_closed(&fake);
    }

    #[test]
    fn ebusy_bounded_by_overall_deadline() {
        let fake = script((0..40).map(|_| busy()).collect());
        assert_eq!(connect_err(&fake, 1000), Some(SdpError::Os(EBUSY)));
        assert!(fake.slept_ms.get() <= 1000);
        all_closed(&fake);
    }

    /// A connected session plus the scripted steps of one exchange.
    fn exchange_with(steps: Vec<Step>, total: u32) -> (FakeSys, Result<Vec<u8>, SdpError>) {
        let fake = script(vec![happy(), steps]);
        let result = {
            let mut s = session(&fake, total);
            s.exchange(VECTOR_A).map(<[u8]>::to_vec)
        };
        (fake, result)
    }

    fn reply() -> Vec<Step> {
        vec![Step::Poll(Ok(POLLIN)), Step::Recv(Ok(RESPONSE_B.to_vec()))]
    }

    #[test]
    fn exchange_happy_vector_b() {
        let (fake, r) = exchange_with([vec![Step::Send(Ok(20))], reply()].concat_steps(), DEFAULT_TOTAL_MS);
        assert_eq!(r.unwrap(), RESPONSE_B);
        assert_eq!(*fake.sent.borrow(), [VECTOR_A]);
        assert_eq!(fake.polls.borrow().last(), Some(&5000));
        all_closed(&fake);
    }

    #[test]
    fn send_eagain_waits_pollout() {
        let steps = [vec![Step::Send(Err(EAGAIN)), Step::Poll(Ok(POLLOUT)), Step::Send(Ok(20))], reply()];
        let (fake, r) = exchange_with(steps.concat_steps(), DEFAULT_TOTAL_MS);
        assert_eq!(r.unwrap(), RESPONSE_B);
        assert_eq!(fake.sent.borrow().len(), 2);
    }

    #[test]
    fn send_eintr_retries() {
        let steps = [vec![Step::Send(Err(EINTR)), Step::Send(Ok(20))], reply()];
        assert_eq!(exchange_with(steps.concat_steps(), DEFAULT_TOTAL_MS).1.unwrap(), RESPONSE_B);
    }

    #[test]
    fn short_send_is_protocol_error() {
        let (_, r) = exchange_with(vec![Step::Send(Ok(19))], DEFAULT_TOTAL_MS);
        assert_eq!(r, Err(SdpError::Protocol(ProtoFault::ShortSend)));
    }

    #[test]
    fn recv_truncated_is_too_large() {
        let steps = vec![Step::Send(Ok(20)), Step::Poll(Ok(POLLIN)), Step::Recv(Ok(vec![0; 70_000]))];
        assert_eq!(exchange_with(steps, DEFAULT_TOTAL_MS).1, Err(SdpError::TooLarge));
    }

    #[test]
    fn recv_zero_is_connreset() {
        let steps = vec![Step::Send(Ok(20)), Step::Poll(Ok(POLLIN)), Step::Recv(Ok(vec![]))];
        assert_eq!(exchange_with(steps, DEFAULT_TOTAL_MS).1, Err(SdpError::Os(ECONNRESET)));
    }

    #[test]
    fn recv_eagain_spurious_wakeup_loops() {
        let steps = [vec![Step::Send(Ok(20)), Step::Poll(Ok(POLLIN)), Step::Recv(Err(EAGAIN))], reply()];
        assert_eq!(exchange_with(steps.concat_steps(), DEFAULT_TOTAL_MS).1.unwrap(), RESPONSE_B);
    }

    #[test]
    fn pdu_stage_timeout() {
        let (fake, r) = exchange_with(vec![Step::Send(Ok(20)), Step::Poll(Ok(0))], DEFAULT_TOTAL_MS);
        assert_eq!(r, Err(SdpError::Timeout));
        assert_eq!(SdpError::Timeout.errno(), 110);
        assert_eq!(fake.polls.borrow().last(), Some(&5000));
    }

    #[test]
    fn pdu_stage_bounded_by_overall_budget() {
        // 1500 ms pass while the connect completes; 500 of the 2000 are left for the PDU.
        let fake = FakeSys::new(vec![
            Step::Socket(Ok(())),
            Step::Connect(Err(EINPROGRESS)),
            Step::Poll(Ok(POLLOUT)),
            Step::Advance(1500),
            Step::SoError(Ok(0)),
            Step::Send(Ok(20)),
            Step::Poll(Ok(0)),
        ]);
        let mut s = session(&fake, 2000);
        assert_eq!(s.exchange(VECTOR_A).err(), Some(SdpError::Timeout));
        assert_eq!(fake.polls.borrow().last(), Some(&500));
    }

    trait ConcatSteps {
        fn concat_steps(self) -> Vec<Step>;
    }
    impl<const N: usize> ConcatSteps for [Vec<Step>; N] {
        fn concat_steps(self) -> Vec<Step> {
            self.into_iter().flatten().collect()
        }
    }

    #[test]
    fn fd_closed_on_every_error_path() {
        let malformed = vec![
            0x07, 0x00, 0x01, 0x00, 0x0d, 0x00, 0x0a, 0x35, 0x40, 0x09, 0x00, 0x00, 0x0a, 0x00, 0x01, 0x00, 0x00, 0x00,
        ];
        let pending = || vec![Step::Socket(Ok(())), Step::Connect(Err(EINPROGRESS))];
        let sent = || [happy(), vec![Step::Send(Ok(20)), Step::Poll(Ok(POLLIN))]].concat_steps();
        let cases: Vec<(&str, Vec<Step>, i32)> = vec![
            ("socket error", vec![Step::Socket(Err(EAFNOSUPPORT))], -EAFNOSUPPORT),
            ("sync connect error", vec![Step::Socket(Ok(())), Step::Connect(Err(EHOSTUNREACH))], -EHOSTUNREACH),
            ("poll error", [pending(), vec![Step::Poll(Err(ENOMEM))]].concat_steps(), -ENOMEM),
            ("connect poll timeout", [pending(), vec![Step::Poll(Ok(0))]].concat_steps(), -EHOSTUNREACH),
            (
                "SO_ERROR non-zero",
                [pending(), vec![Step::Poll(Ok(POLLOUT)), Step::SoError(Ok(EHOSTDOWN))]].concat_steps(),
                -EHOSTDOWN,
            ),
            (
                "so_error syscall error",
                [pending(), vec![Step::Poll(Ok(POLLOUT)), Step::SoError(Err(ENOTSOCK))]].concat_steps(),
                -ENOTSOCK,
            ),
            ("send error", [happy(), vec![Step::Send(Err(ECONNRESET))]].concat_steps(), -ECONNRESET),
            ("short send", [happy(), vec![Step::Send(Ok(19))]].concat_steps(), -EPROTO),
            ("recv error", [sent(), vec![Step::Recv(Err(ECONNRESET))]].concat_steps(), -ECONNRESET),
            ("truncation", [sent(), vec![Step::Recv(Ok(vec![0; 70_000]))]].concat_steps(), -EMSGSIZE),
            ("PDU timeout", [happy(), vec![Step::Send(Ok(20)), Step::Poll(Ok(0))]].concat_steps(), -ETIMEDOUT),
            ("parse failure", [sent(), vec![Step::Recv(Ok(malformed))]].concat_steps(), -EBADMSG),
            ("success", [sent(), vec![Step::Recv(Ok(RESPONSE_B.to_vec()))]].concat_steps(), 5),
        ];
        for (name, steps, want) in cases {
            let fake = FakeSys::new(steps);
            assert_eq!(crate::resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 0), want, "{name}");
            all_closed(&fake);
            assert!(fake.steps.borrow().is_empty(), "{name}: script fully consumed");
        }
    }

    #[test]
    fn fd_closed_on_panic() {
        let fake = script(vec![happy(), vec![Step::Send(Ok(20)), Step::Poll(Ok(POLLIN)), Step::Panic]]);
        let outcome = catch_unwind(AssertUnwindSafe(|| crate::resolve(&fake, WIN_ADDR, &SPP_GUID_BYTES, 0)));
        assert!(outcome.is_err());
        all_closed(&fake);
    }
}
