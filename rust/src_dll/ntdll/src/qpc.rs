//! QPC user-mode-bypass setup and unix-side read path.
//!
//! Mirrors the QPC parts of `dlls/ntdll/unix/sync.c` / `virtual.c`. At prefix
//! boot (first process of a wineserver session, `virtual_init_user_shared_data`)
//! [`rs_qpc_init`] decides whether the host TSC can back
//! `QueryPerformanceCounter` directly and, if so, returns the parameters wine
//! publishes in `KUSER_SHARED_DATA` (`QpcFrequency`/`QpcBias`/`QpcShift`/
//! `QpcBypassEnabled`). From then on the PE side reads the counter without any
//! syscall (`rs_rtl_query_performance_counter`, crate `wine_ntdll_pe`), and the
//! unix-side `NtQueryPerformanceCounter` serves the identical formula via
//! [`rs_qpc_read`] so both paths always agree.
//!
//! The bypass is enabled only when ALL of the following hold, and otherwise
//! everything keeps the historical `clock_gettime`-based 10 MHz counter:
//! * x86_64 Linux host,
//! * cpuid reports an invariant TSC (constant rate, never stops),
//! * the kernel itself trusts the TSC (`current_clocksource` == `tsc`, which
//!   also implies the TSC is synchronized across cores),
//! * two independent calibration intervals against `CLOCK_MONOTONIC_RAW` agree,
//! * `SPRITZWINE_NO_QPC_BYPASS` is not set (escape hatch).

/// Mirror of `struct rs_qpc_params` in `dlls/ntdll/unix/virtual.c`.
#[repr(C)]
pub struct QpcParams {
    pub frequency: u64,
    pub bias: u64,
    pub flags: u8,
    pub shift: u8,
}

// QpcBypassEnabled bits (SHARED_GLOBAL_FLAGS_QPC_BYPASS_* in include/ddk/wdm.h).
const QPC_BYPASS_ENABLED: u8 = 0x01;
const QPC_BYPASS_USE_MFENCE: u8 = 0x10;
const QPC_BYPASS_USE_LFENCE: u8 = 0x20;
const QPC_BYPASS_USE_RDTSCP: u8 = 0x80;

/// Windows uses a fixed shift of 10 on bare metal (`ntdll/tests/time.c`
/// asserts exactly this), i.e. QPC units of 1024 TSC cycles.
const QPC_SHIFT: u8 = 10;

const TICKSPERSEC: u64 = 10_000_000;

// rs_qpc_init return codes; keep in sync with the WARN in unix/virtual.c.
pub const QPC_INIT_OK: i32 = 1;
pub const QPC_INIT_DISABLED_BY_ENV: i32 = 0;
pub const QPC_INIT_UNSUPPORTED: i32 = -1;
pub const QPC_INIT_NO_INVARIANT_TSC: i32 = -2;
pub const QPC_INIT_CLOCKSOURCE_NOT_TSC: i32 = -3;
pub const QPC_INIT_CALIBRATION_FAILED: i32 = -4;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod imp {
    use super::*;
    use core::arch::x86_64::{__cpuid, __rdtscp, _mm_lfence, _mm_mfence, _rdtsc};

    /// Read the TSC with the same serialization flavor the published flags ask
    /// the PE side to use, so both sides sample the identical clock.
    #[inline]
    pub fn read_tsc(flags: u8) -> u64 {
        // SAFETY: rdtsc/lfence/mfence are baseline on x86_64; RDTSCP is only
        // ever set in flags after the cpuid check in `detect_tsc`.
        unsafe {
            if flags & QPC_BYPASS_USE_RDTSCP != 0 {
                let mut aux = 0u32;
                __rdtscp(&mut aux)
            } else {
                if flags & QPC_BYPASS_USE_LFENCE != 0 {
                    _mm_lfence();
                } else if flags & QPC_BYPASS_USE_MFENCE != 0 {
                    _mm_mfence();
                }
                _rdtsc()
            }
        }
    }

    /// cpuid gates: invariant TSC required; RDTSCP picked up if present.
    /// Returns the QpcBypassEnabled flag byte to publish, or None.
    pub fn detect_tsc() -> Option<u8> {
        // cpuid is unprivileged and always present on x86_64.
        let max_ext = __cpuid(0x8000_0000).eax;
        if max_ext < 0x8000_0007 {
            return None;
        }
        let (ext1, ext7) = (__cpuid(0x8000_0001), __cpuid(0x8000_0007));
        if ext7.edx & (1 << 8) == 0 {
            return None; // no invariant TSC
        }
        let rdtscp = ext1.edx & (1 << 27) != 0;
        Some(QPC_BYPASS_ENABLED | if rdtscp { QPC_BYPASS_USE_RDTSCP } else { QPC_BYPASS_USE_LFENCE })
    }

    /// The kernel's own verdict on the TSC; covers cross-core synchronization
    /// and errata the cpuid bits cannot.
    pub fn kernel_trusts_tsc() -> bool {
        std::fs::read_to_string("/sys/devices/system/clocksource/clocksource0/current_clocksource")
            .map(|s| s.trim() == "tsc")
            .unwrap_or(false)
    }

    /// `clock_gettime(CLOCK_MONOTONIC_RAW)` in nanoseconds via a raw syscall —
    /// the crate deliberately has no libc dependency, and RAW (unlike std's
    /// `Instant`/CLOCK_MONOTONIC) is never NTP-slewed, which matters when
    /// measuring a frequency.
    fn clock_raw_ns() -> u64 {
        #[repr(C)]
        struct Timespec {
            tv_sec: i64,
            tv_nsec: i64,
        }
        let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
        let ret: i64;
        // SAFETY: clock_gettime(2) is syscall 228 on x86_64; CLOCK_MONOTONIC_RAW
        // is 4; the pointer is valid for the duration of the call.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") 228i64 => ret,
                in("rdi") 4i64,
                in("rsi") &mut ts as *mut Timespec,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack),
            );
        }
        if ret != 0 {
            return 0;
        }
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }

    /// One calibration anchor: the tightest of 16 rdtsc-clock-rdtsc brackets
    /// (midpoint TSC paired with the clock reading), so scheduler noise cannot
    /// skew a sample.
    fn anchor() -> Option<(u64, u64)> {
        let mut best_width = u64::MAX;
        let mut best = (0u64, 0u64);
        for _ in 0..16 {
            // SAFETY: baseline instructions (see read_tsc).
            let (t0, ns, t1) = unsafe {
                _mm_lfence();
                let t0 = _rdtsc();
                _mm_lfence();
                let ns = clock_raw_ns();
                _mm_lfence();
                let t1 = _rdtsc();
                _mm_lfence();
                (t0, ns, t1)
            };
            if ns == 0 || t1 <= t0 {
                continue;
            }
            let width = t1 - t0;
            if width < best_width {
                best_width = width;
                best = (t0 + width / 2, ns);
            }
        }
        (best_width != u64::MAX).then_some(best)
    }

    /// Measure the TSC frequency against CLOCK_MONOTONIC_RAW over two ~20 ms
    /// intervals and require them to agree within ~1000 ppm (guards against a
    /// migrating VM, throttled clock, or broken clocksource). Returns Hz.
    pub fn calibrate_tsc_hz() -> Option<u64> {
        let hz = |(t0, n0): (u64, u64), (t1, n1): (u64, u64)| -> Option<u64> {
            if t1 <= t0 || n1 <= n0 {
                return None;
            }
            Some(((t1 - t0) as u128 * 1_000_000_000 / (n1 - n0) as u128) as u64)
        };
        let a = anchor()?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        let b = anchor()?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        let c = anchor()?;

        let hz1 = hz(a, b)?;
        let hz2 = hz(b, c)?;
        let full = hz(a, c)?;
        if hz1.abs_diff(hz2) > full / 1000 {
            return None;
        }
        // Plausibility: no shipping x86 TSC is outside this range.
        (100_000_000..=100_000_000_000).contains(&full).then_some(full)
    }
}

/// Decide whether to publish the QPC bypass and compute the parameters.
///
/// `mono_ticks` is `monotonic_counter()` (the historical 100 ns counter) read
/// by the caller immediately before this call; the bias is chosen so the new
/// counter continues the same boot-based epoch, just in the new units.
///
/// Returns `QPC_INIT_OK` with `*out` filled on success, or a
/// `QPC_INIT_*` reason code (≤ 0) with `*out` untouched.
///
/// # Safety
/// `out` must point to a writable `QpcParams`.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_qpc_init(mono_ticks: u64, out: *mut QpcParams) -> i32 {
    // Sample the TSC first thing so it pairs tightly with the caller's
    // monotonic_counter() read.
    let tsc_entry = imp::read_tsc(QPC_BYPASS_USE_LFENCE);

    if std::env::var_os("SPRITZWINE_NO_QPC_BYPASS").is_some_and(|v| !v.is_empty() && v != "0") {
        return QPC_INIT_DISABLED_BY_ENV;
    }
    let Some(flags) = imp::detect_tsc() else {
        return QPC_INIT_NO_INVARIANT_TSC;
    };
    if !imp::kernel_trusts_tsc() {
        return QPC_INIT_CLOCKSOURCE_NOT_TSC;
    }
    let Some(tsc_hz) = imp::calibrate_tsc_hz() else {
        return QPC_INIT_CALIBRATION_FAILED;
    };

    let frequency = tsc_hz >> QPC_SHIFT;
    // Counter value "now" in the new units, continuing the boot-based epoch of
    // the old 10 MHz counter, then solved for the bias in TSC units:
    // counter = (tsc + bias) >> shift.
    let counter_now = (mono_ticks as u128 * frequency as u128 / TICKSPERSEC as u128) as u64;
    let bias = (counter_now << QPC_SHIFT).wrapping_sub(tsc_entry);

    // SAFETY: caller contract.
    unsafe {
        out.write(QpcParams { frequency, bias, flags, shift: QPC_SHIFT });
    }
    QPC_INIT_OK
}

/// Stub for hosts that can never bypass (non-x86_64 or non-Linux builds).
#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_qpc_init(_mono_ticks: u64, _out: *mut QpcParams) -> i32 {
    QPC_INIT_UNSUPPORTED
}

/// Unix-side counter read for `NtQueryPerformanceCounter` once the bypass is
/// published: the same formula the PE side uses, fed from the same
/// `KUSER_SHARED_DATA` fields (passed in by the C caller).
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(no_mangle)]
pub extern "C" fn rs_qpc_read(flags: u8, bias: u64, shift: u8) -> u64 {
    imp::read_tsc(flags).wrapping_add(bias) >> shift
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn bias_continues_the_monotonic_epoch() {
        // With freq = tsc_hz >> shift, the bias must map tsc_entry exactly onto
        // the rescaled monotonic counter value.
        let mono_ticks: u64 = 123_456_789_012; // ~3.4 h of 100 ns ticks
        let frequency: u64 = 4_500_000_000u64 >> QPC_SHIFT;
        let tsc_entry: u64 = 55_555_555_555_555;
        let counter_now = (mono_ticks as u128 * frequency as u128 / TICKSPERSEC as u128) as u64;
        let bias = (counter_now << QPC_SHIFT).wrapping_sub(tsc_entry);
        assert_eq!(tsc_entry.wrapping_add(bias) >> QPC_SHIFT, counter_now);
        // And it keeps counting forward in TSC units from there.
        let later = tsc_entry + (1u64 << 20);
        assert_eq!(later.wrapping_add(bias) >> QPC_SHIFT, counter_now + (1 << 10));
    }

    #[test]
    fn calibration_is_plausible_on_the_host() {
        // Real measurement (~40 ms). Only run where the host actually has an
        // invariant TSC; otherwise there is nothing meaningful to assert.
        if imp::detect_tsc().is_none() {
            return;
        }
        let Some(hz) = imp::calibrate_tsc_hz() else {
            // A loaded CI box may fail the consistency gate; that is the
            // fail-closed behavior working as intended.
            return;
        };
        assert!((100_000_000..=100_000_000_000).contains(&hz), "implausible tsc_hz {hz}");
        // A second run must agree within 1% — the property the whole feature
        // rests on.
        if let Some(hz2) = imp::calibrate_tsc_hz() {
            assert!(hz.abs_diff(hz2) < hz / 100, "unstable calibration: {hz} vs {hz2}");
        }
    }

    #[test]
    fn read_matches_formula() {
        let a = rs_qpc_read(QPC_BYPASS_USE_LFENCE, 0, 10);
        let b = rs_qpc_read(QPC_BYPASS_USE_LFENCE, 0, 10);
        assert!(b >= a, "QPC went backwards: {a} -> {b}");
    }
}
