//! Host syscall surface for the SDP client, declared by hand (the workspace has no libc
//! crate; std links libc anyway so the symbols resolve). All of the crate's externs live here,
//! behind the `Sys` trait so l2cap.rs runs against a fake in tests.

use std::ffi::{c_int, c_void};

pub const AF_BLUETOOTH: c_int = 31;          // /usr/include/bits/socket.h:75 (PF_BLUETOOTH), :127 (AF_ = PF_); /usr/include/bluetooth/bluetooth.h:29
pub const BTPROTO_L2CAP: c_int = 0;          // /usr/include/bluetooth/bluetooth.h:33
pub const SDP_PSM: u16 = 0x0001;             // /usr/include/bluetooth/sdp.h:34
pub const BDADDR_BREDR: u8 = 0x00;           // /usr/include/bluetooth/bluetooth.h:427
pub const SOCK_SEQPACKET: c_int = 5;         // /usr/include/bits/socket_type.h:36
pub const SOCK_CLOEXEC: c_int = 0o2000000;   // /usr/include/bits/socket_type.h:49 (= 0x80000)
pub const SOCK_NONBLOCK: c_int = 0o4000;     // /usr/include/bits/socket_type.h:52 (= 0x800)
pub const SOL_SOCKET: c_int = 1;             // /usr/include/asm-generic/socket.h:9
pub const SO_ERROR: c_int = 4;               // /usr/include/asm-generic/socket.h:14
pub const MSG_TRUNC: c_int = 0x20;           // /usr/include/bits/socket.h:223
pub const MSG_NOSIGNAL: c_int = 0x4000;      // /usr/include/bits/socket.h:241
pub const POLLIN: i16 = 0x001;               // /usr/include/bits/poll.h:25
pub const POLLOUT: i16 = 0x004;              // /usr/include/bits/poll.h:27
// POLLERR 0x008, POLLHUP 0x010, POLLNVAL 0x020 (bits/poll.h:47-49) arrive in revents regardless
// of `events`; any non-zero revents means "go look" and SO_ERROR / recv give the verdict.
pub const CLOCK_MONOTONIC: c_int = 1;        // /usr/include/bits/time.h:48

/// struct pollfd (/usr/include/sys/poll.h:36-41).
#[repr(C)]
pub struct PollFd { pub fd: c_int, pub events: i16, pub revents: i16 }
const _: () = assert!(core::mem::size_of::<PollFd>() == 8);

/// struct timespec on x86_64: time_t = long, tv_nsec = long (/usr/include/bits/types/struct_timespec.h).
#[repr(C)]
pub struct Timespec { pub tv_sec: i64, pub tv_nsec: i64 }
const _: () = assert!(core::mem::size_of::<Timespec>() == 16);

/// struct sockaddr_l2 (/usr/include/bluetooth/l2cap.h:28-34): sa_family_t (unsigned short,
/// /usr/include/bits/sockaddr.h:28), unsigned short psm, bdaddr_t (6 packed bytes,
/// bluetooth.h:422-424), unsigned short cid, uint8_t bdaddr_type, 1 byte tail padding.
#[repr(C)]
pub struct SockaddrL2 {
    pub l2_family: u16,       // @0  AF_BLUETOOTH
    pub l2_psm: u16,          // @2  host order == __le16 on x86: 1 for SDP
    pub l2_bdaddr: [u8; 6],   // @4  b[0] = least significant byte of the BTH_ADDR
    pub l2_cid: u16,          // @10 0
    pub l2_bdaddr_type: u8,   // @12 BDADDR_BREDR
}
const _: () = assert!(core::mem::size_of::<SockaddrL2>() == 14);
const _: () = assert!(core::mem::offset_of!(SockaddrL2, l2_bdaddr) == 4);
const _: () = assert!(core::mem::offset_of!(SockaddrL2, l2_cid) == 10);
const _: () = assert!(core::mem::offset_of!(SockaddrL2, l2_bdaddr_type) == 12);
/// The kernel expects the padded size.
pub const SOCKADDR_L2_LEN: u32 = 14;

impl SockaddrL2 {
    pub fn sdp(bdaddr: [u8; 6]) -> Self {
        Self { l2_family: AF_BLUETOOTH as u16, l2_psm: SDP_PSM, l2_bdaddr: bdaddr, l2_cid: 0, l2_bdaddr_type: BDADDR_BREDR }
    }
}

unsafe extern "C" {
    fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int;                       // /usr/include/sys/socket.h:102
    fn connect(fd: c_int, addr: *const SockaddrL2, len: u32) -> c_int;                   // sys/socket.h:126; socklen_t = u32 (bits/types.h:210 __U32_TYPE)
    fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;           // sys/socket.h:138
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;             // sys/socket.h:145
    fn getsockopt(fd: c_int, level: c_int, name: c_int, val: *mut c_void, len: *mut u32) -> c_int; // sys/socket.h:255
    fn poll(fds: *mut PollFd, nfds: u64, timeout_ms: c_int) -> c_int;                    // /usr/include/sys/poll.h:54; nfds_t = unsigned long (sys/poll.h:33)
    fn close(fd: c_int) -> c_int;                                                        // /usr/include/unistd.h:358
    fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> c_int;                     // /usr/include/time.h:290
    fn clock_gettime(clk: c_int, tp: *mut Timespec) -> c_int;                            // /usr/include/time.h:297; clockid_t = int
    fn __errno_location() -> *mut c_int;                                                 // /usr/include/errno.h:37
}

/// Every host call the transport makes. Err(errno) everywhere; errno is read immediately
/// after the failing call, before anything else runs.
pub trait Sys {
    fn socket(&self) -> Result<i32, i32>;                                   // AF_BLUETOOTH, SEQPACKET|CLOEXEC|NONBLOCK, L2CAP
    fn connect(&self, fd: i32, addr: &SockaddrL2) -> Result<(), i32>;
    fn poll1(&self, fd: i32, events: i16, timeout_ms: i32) -> Result<i16, i32>; // Ok(0) = timed out, Ok(revents) otherwise
    fn so_error(&self, fd: i32) -> Result<i32, i32>;                        // Ok(pending errno or 0)
    fn send(&self, fd: i32, buf: &[u8]) -> Result<usize, i32>;              // MSG_NOSIGNAL
    fn recv(&self, fd: i32, buf: &mut [u8]) -> Result<usize, i32>;          // MSG_TRUNC: may return > buf.len()
    fn close(&self, fd: i32);
    fn sleep_ms(&self, ms: u32);                                            // nanosleep, finishes the full interval across EINTR
    fn now_ms(&self) -> u64;                                                // CLOCK_MONOTONIC milliseconds
}

pub struct HostSys;

impl HostSys {
    fn errno() -> i32 {
        // SAFETY: __errno_location always returns a valid thread-local pointer.
        unsafe { *__errno_location() }
    }
}

impl Sys for HostSys {
    fn socket(&self) -> Result<i32, i32> {
        // SAFETY: plain syscall wrapper, no pointers.
        let fd = unsafe { socket(AF_BLUETOOTH, SOCK_SEQPACKET | SOCK_CLOEXEC | SOCK_NONBLOCK, BTPROTO_L2CAP) };
        if fd < 0 { Err(Self::errno()) } else { Ok(fd) }
    }
    fn connect(&self, fd: i32, addr: &SockaddrL2) -> Result<(), i32> {
        // SAFETY: addr is a live repr(C) struct of exactly SOCKADDR_L2_LEN bytes.
        let r = unsafe { connect(fd, addr, SOCKADDR_L2_LEN) };
        if r < 0 { Err(Self::errno()) } else { Ok(()) }
    }
    fn poll1(&self, fd: i32, events: i16, timeout_ms: i32) -> Result<i16, i32> {
        let mut p = PollFd { fd, events, revents: 0 };
        // SAFETY: one pollfd, nfds = 1.
        let r = unsafe { poll(&mut p, 1, timeout_ms) };
        if r < 0 { Err(Self::errno()) } else if r == 0 { Ok(0) } else { Ok(p.revents) }
    }
    fn so_error(&self, fd: i32) -> Result<i32, i32> {
        let mut err: c_int = 0;
        let mut len: u32 = core::mem::size_of::<c_int>() as u32;
        // SAFETY: val/len point at live locals of the advertised size.
        let r = unsafe { getsockopt(fd, SOL_SOCKET, SO_ERROR, &mut err as *mut c_int as *mut c_void, &mut len) };
        if r < 0 { Err(Self::errno()) } else { Ok(err) }
    }
    fn send(&self, fd: i32, buf: &[u8]) -> Result<usize, i32> {
        // SAFETY: buf is a live slice. MSG_NOSIGNAL: no SIGPIPE from a vanished peer.
        let n = unsafe { send(fd, buf.as_ptr() as *const c_void, buf.len(), MSG_NOSIGNAL) };
        if n < 0 { Err(Self::errno()) } else { Ok(n as usize) }
    }
    fn recv(&self, fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
        // SAFETY: buf is a live mutable slice; the kernel writes at most buf.len() bytes even
        // when MSG_TRUNC makes it report the full SDU length.
        let n = unsafe { recv(fd, buf.as_mut_ptr() as *mut c_void, buf.len(), MSG_TRUNC) };
        if n < 0 { Err(Self::errno()) } else { Ok(n as usize) }
    }
    fn close(&self, fd: i32) {
        // SAFETY: fd is one we created. EINTR from close is not retried: Linux has already
        // released the descriptor, retrying could close a reused fd.
        unsafe { close(fd); }
    }
    fn sleep_ms(&self, ms: u32) {
        let mut req = Timespec { tv_sec: (ms / 1000) as i64, tv_nsec: ((ms % 1000) as i64) * 1_000_000 };
        let mut rem = Timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: both pointers are live locals. nanosleep is never auto-restarted (SA_RESTART
        // does not apply), so Wine's SIGUSR1 suspends resume with the remainder.
        while unsafe { nanosleep(&req, &mut rem) } < 0 && Self::errno() == crate::errno::EINTR {
            req = Timespec { tv_sec: rem.tv_sec, tv_nsec: rem.tv_nsec };
        }
    }
    fn now_ms(&self) -> u64 {
        let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: ts is a live local; CLOCK_MONOTONIC cannot fail on Linux.
        if unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) } != 0 { return 0; }
        (ts.tv_sec as u64).saturating_mul(1000).saturating_add((ts.tv_nsec / 1_000_000) as u64)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::{SockaddrL2, Sys};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    pub enum Step {
        Socket(Result<(), i32>),      // Ok: hand out next_fd
        Connect(Result<(), i32>),
        Poll(Result<i16, i32>),       // Ok(0) = timeout: the fake advances its clock by the requested timeout
        SoError(Result<i32, i32>),
        Send(Result<usize, i32>),     // Ok(n): bytes accepted (n == buf.len() normally)
        Recv(Result<Vec<u8>, i32>),   // Ok(bytes): copies min(bytes.len(), buf.len()), returns bytes.len() (MSG_TRUNC)
        Advance(u64),                 // clock jump, consumed transparently by the next host call
        Panic,                        // panics inside the host call (fd_closed_on_panic)
    }

    #[derive(Default)]
    pub struct FakeSys {
        pub steps: RefCell<VecDeque<Step>>,
        pub now: Cell<u64>,
        pub next_fd: Cell<i32>,
        pub opened: Cell<u32>,
        pub closed: RefCell<Vec<i32>>,
        pub live_fd: Cell<Option<i32>>,   // every fd-taking call asserts it is this one
        pub slept_ms: Cell<u64>,
        pub polls: RefCell<Vec<i32>>,     // timeout_ms of every poll1 call
        pub sent: RefCell<Vec<Vec<u8>>>,
        pub connects: RefCell<Vec<SockaddrL2Bytes>>, // [u8; 14] images, for the layout test
    }
    pub type SockaddrL2Bytes = [u8; 14];

    impl FakeSys {
        pub fn new(steps: Vec<Step>) -> Self {
            let s = FakeSys { next_fd: Cell::new(100), ..Default::default() };
            *s.steps.borrow_mut() = steps.into();
            s
        }
        fn step(&self, what: &str) -> Step {
            loop {
                match self.steps.borrow_mut().pop_front() {
                    Some(Step::Advance(ms)) => self.now.set(self.now.get() + ms),
                    Some(Step::Panic) => panic!("scripted panic in {what}"),
                    Some(s) => return s,
                    None => panic!("unexpected host call: {what}"),
                }
            }
        }
        fn check_fd(&self, fd: i32) { assert_eq!(Some(fd), self.live_fd.get(), "call on a foreign or closed fd"); }
    }

    impl Sys for FakeSys {
        fn socket(&self) -> Result<i32, i32> {
            match self.step("socket") {
                Step::Socket(Ok(())) => {
                    assert!(self.live_fd.get().is_none(), "second socket while one is open");
                    let fd = self.next_fd.get();
                    self.next_fd.set(fd + 1);
                    self.opened.set(self.opened.get() + 1);
                    self.live_fd.set(Some(fd));
                    Ok(fd)
                }
                Step::Socket(Err(e)) => Err(e),
                _ => panic!("expected Socket step"),
            }
        }
        fn connect(&self, fd: i32, addr: &SockaddrL2) -> Result<(), i32> {
            self.check_fd(fd);
            let mut img = [0u8; 14];
            img[0..2].copy_from_slice(&addr.l2_family.to_ne_bytes());
            img[2..4].copy_from_slice(&addr.l2_psm.to_ne_bytes());
            img[4..10].copy_from_slice(&addr.l2_bdaddr);
            img[10..12].copy_from_slice(&addr.l2_cid.to_ne_bytes());
            img[12] = addr.l2_bdaddr_type;
            self.connects.borrow_mut().push(img);
            match self.step("connect") { Step::Connect(r) => r, _ => panic!("expected Connect step") }
        }
        fn poll1(&self, fd: i32, _events: i16, timeout_ms: i32) -> Result<i16, i32> {
            self.check_fd(fd);
            self.polls.borrow_mut().push(timeout_ms);
            match self.step("poll") {
                Step::Poll(Ok(0)) => { self.now.set(self.now.get() + timeout_ms as u64); Ok(0) }
                Step::Poll(r) => r,
                _ => panic!("expected Poll step"),
            }
        }
        fn so_error(&self, fd: i32) -> Result<i32, i32> {
            self.check_fd(fd);
            match self.step("so_error") { Step::SoError(r) => r, _ => panic!("expected SoError step") }
        }
        fn send(&self, fd: i32, buf: &[u8]) -> Result<usize, i32> {
            self.check_fd(fd);
            self.sent.borrow_mut().push(buf.to_vec());
            match self.step("send") { Step::Send(r) => r, _ => panic!("expected Send step") }
        }
        fn recv(&self, fd: i32, buf: &mut [u8]) -> Result<usize, i32> {
            self.check_fd(fd);
            match self.step("recv") {
                Step::Recv(Ok(bytes)) => {
                    let n = bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&bytes[..n]);
                    Ok(bytes.len())
                }
                Step::Recv(Err(e)) => Err(e),
                _ => panic!("expected Recv step"),
            }
        }
        fn close(&self, fd: i32) {
            self.check_fd(fd);
            self.live_fd.set(None);
            self.closed.borrow_mut().push(fd);
        }
        fn sleep_ms(&self, ms: u32) {
            self.now.set(self.now.get() + ms as u64);
            self.slept_ms.set(self.slept_ms.get() + ms as u64);
        }
        fn now_ms(&self) -> u64 { self.now.get() }
    }
}
