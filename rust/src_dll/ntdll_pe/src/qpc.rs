//! `RtlQueryPerformanceCounter` / `RtlQueryPerformanceFrequency` — the QPC
//! user-mode bypass.
//!
//! Windows implements QueryPerformanceCounter entirely in user mode when the
//! kernel publishes TSC parameters in `KUSER_SHARED_DATA`: if `QpcBypassEnabled`
//! has `SHARED_GLOBAL_FLAGS_QPC_BYPASS_ENABLED` set, the counter is
//! `(rdtsc + QpcBias) >> QpcShift` and the frequency is `QpcFrequency` — no
//! syscall involved. Wine's unix side (`rs_qpc_init` in the sibling `wine_ntdll`
//! crate) decides once per prefix session whether the host TSC is trustworthy
//! and publishes those fields; this module is the PE-side reader. When the
//! bypass is off, the exports fall back to the `NtQueryPerformanceCounter`
//! syscall, which serves the same parameters from the same page, so both paths
//! always agree.

/// `KUSER_SHARED_DATA` is mapped read-only at this fixed address in every
/// process, for both 32-bit and 64-bit (`dlls/ntdll/unix/virtual.c`).
const USD_BASE: usize = 0x7ffe_0000;

// Field offsets within KUSER_SHARED_DATA (see include/ddk/wdm.h; the C side
// cross-checks these with compile-time asserts in dlls/ntdll/unix/virtual.c).
const OFF_QPC_FREQUENCY: usize = 0x300;
const OFF_QPC_BIAS: usize = 0x3b8;
const OFF_QPC_BYPASS_ENABLED: usize = 0x3c6;
const OFF_QPC_SHIFT: usize = 0x3c7;

// QpcBypassEnabled bits (SHARED_GLOBAL_FLAGS_QPC_BYPASS_* in wdm.h).
const QPC_BYPASS_ENABLED: u8 = 0x01;
const QPC_BYPASS_USE_HV_PAGE: u8 = 0x02;
#[cfg(target_arch = "x86")]
const QPC_BYPASS_DISABLE_32BIT: u8 = 0x04;
const QPC_BYPASS_USE_MFENCE: u8 = 0x10;
const QPC_BYPASS_USE_LFENCE: u8 = 0x20;
const QPC_BYPASS_USE_RDTSCP: u8 = 0x80;

const TICKSPERSEC: i64 = 10_000_000;

/// The published counter formula. `QpcBias` is in TSC units and is added
/// before the shift.
#[inline]
pub fn counter_from_tsc(tsc: u64, bias: u64, shift: u8) -> i64 {
    (tsc.wrapping_add(bias) >> shift) as i64
}

#[cfg(all(not(test), any(target_arch = "x86", target_arch = "x86_64")))]
mod real {
    use super::*;
    #[cfg(target_arch = "x86")]
    use core::arch::x86::{__rdtscp, _mm_lfence, _mm_mfence, _rdtsc};
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{__rdtscp, _mm_lfence, _mm_mfence, _rdtsc};

    unsafe extern "system" {
        fn NtQueryPerformanceCounter(counter: *mut i64, frequency: *mut i64) -> i32;
    }

    /// Volatile `KUSER_SHARED_DATA` field read. The Qpc fields are written once
    /// at prefix boot and never change afterwards (wine does not re-bias on
    /// resume the way Windows does), so a torn 64-bit read on i386 cannot
    /// actually observe a mixed value.
    #[inline]
    fn usd_read<T: Copy>(off: usize) -> T {
        // SAFETY: KUSER_SHARED_DATA is guaranteed mapped readable at USD_BASE
        // for the lifetime of the process.
        unsafe { core::ptr::read_volatile((USD_BASE + off) as *const T) }
    }

    /// Read the TSC honoring the serialization flavor the kernel picked.
    #[inline]
    fn read_tsc(flags: u8) -> u64 {
        // SAFETY: flag-gated instructions; the unix side only publishes
        // QPC_BYPASS_USE_RDTSCP after verifying cpuid support, and
        // lfence/mfence/rdtsc are baseline on every supported x86.
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

    #[inline]
    fn bypass_flags() -> Option<u8> {
        let flags: u8 = usd_read(OFF_QPC_BYPASS_ENABLED);
        if flags & QPC_BYPASS_ENABLED == 0 {
            return None;
        }
        // The hypervisor-page form is never published by wine; if it ever
        // appears, take the syscall path rather than misinterpret the fields.
        if flags & QPC_BYPASS_USE_HV_PAGE != 0 {
            return None;
        }
        #[cfg(target_arch = "x86")]
        if flags & QPC_BYPASS_DISABLE_32BIT != 0 {
            return None;
        }
        Some(flags)
    }

    /// PE-side body of `RtlQueryPerformanceCounter` (the C export in
    /// `dlls/ntdll/time.c` is a `DECLSPEC_HOTPATCH` shim around this).
    ///
    /// # Safety
    /// `counter` must point to writable memory; like on Windows, a bad pointer
    /// faults in user mode here (the caller-visible access violation).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn rs_rtl_query_performance_counter(counter: *mut i64) -> u8 {
        match bypass_flags() {
            Some(flags) => {
                let bias: u64 = usd_read(OFF_QPC_BIAS);
                let shift: u8 = usd_read(OFF_QPC_SHIFT);
                let value = counter_from_tsc(read_tsc(flags), bias, shift);
                // SAFETY: caller contract; unaligned store is free on x86.
                unsafe { counter.write_unaligned(value) };
            }
            // SAFETY: forwarding the caller's pointer to the syscall.
            None => unsafe {
                NtQueryPerformanceCounter(counter, core::ptr::null_mut());
            },
        }
        1
    }

    /// PE-side body of `RtlQueryPerformanceFrequency`.
    ///
    /// # Safety
    /// `frequency` must point to writable memory (see above).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn rs_rtl_query_performance_frequency(frequency: *mut i64) -> u8 {
        let value = if bypass_flags().is_some() {
            usd_read::<i64>(OFF_QPC_FREQUENCY)
        } else {
            // Matches the unix-side fallback (monotonic_counter at 100 ns).
            TICKSPERSEC
        };
        // SAFETY: caller contract.
        unsafe { frequency.write_unaligned(value) };
        1
    }
}

#[cfg(test)]
mod tests {
    use super::counter_from_tsc;

    #[test]
    fn bias_is_added_before_the_shift() {
        // The documented Windows formula: (tsc + bias) >> shift.
        assert_eq!(counter_from_tsc(1 << 10, 0, 10), 1);
        assert_eq!(counter_from_tsc(0, 1 << 10, 10), 1);
        assert_eq!(counter_from_tsc(3 << 10, 1 << 10, 10), 4);
    }

    #[test]
    fn negative_bias_wraps_like_the_hardware() {
        // bias = -tsc0 as u64: counter starts at 0 at tsc0 and counts up.
        let tsc0: u64 = 0xdead_beef_0000;
        let bias = 0u64.wrapping_sub(tsc0);
        assert_eq!(counter_from_tsc(tsc0, bias, 10), 0);
        assert_eq!(counter_from_tsc(tsc0 + (5 << 10), bias, 10), 5);
    }
}
