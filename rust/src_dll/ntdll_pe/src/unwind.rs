//! x86_64 `.pdata` UNWIND_INFO opcode interpreter -- the core of Wine's
//! `RtlVirtualUnwind2` (dlls/ntdll/unwind.c), moved to Rust (task #35).
//!
//! This is the attacker-facing part of the unwinder: it walks a module's
//! `.pdata` UNWIND_INFO opcode stream and dereferences stack slots to restore
//! the caller's nonvolatile registers. Two reasons to do it in Rust:
//!
//! * **bounds-checked opcode parsing** -- a malformed/truncated opcode stream
//!   can no longer read past the opcode array; it aborts with
//!   `STATUS_BAD_FUNCTION_TABLE`.
//! * **proper stack limits** -- Wine's C `RtlVirtualUnwind2` *ignored* its
//!   `limit_low`/`limit_high` parameters (FIXME "limits not supported") and
//!   dereferenced whatever the opcodes pointed at. A frame whose `.pdata`
//!   doesn't match its real stack (e.g. VMProtect VM code) made the unwinder
//!   fault on a wild read and recurse to a stack overflow during exception
//!   dispatch. Here every stack read is validated against `[limit_low,
//!   limit_high)` when the caller supplies the limits; an out-of-bounds read
//!   aborts the unwind cleanly. (Callers that pass NULL limits keep the old
//!   best-effort behavior, matching Windows before stack limits existed.)
//!
//! The C `RtlVirtualUnwind2` keeps the surrounding glue (leaf functions,
//! prolog/epilog handling, chained-info following, handler lookup) and calls
//! [`rs_rtl_unwind_run_codes`] once per UNWIND_INFO to process its opcodes.

use crate::status::{STATUS_BAD_FUNCTION_TABLE, STATUS_SUCCESS};

/// Index of RSP in the `&context->Rax` register array
/// (Rax, Rcx, Rdx, Rbx, **Rsp**, Rbp, Rsi, Rdi, R8..R15).
const REG_RSP: usize = 4;

// UWOP opcode codes (see [MS-XCA] / winnt.h), matching dlls/ntdll/unwind.c.
const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_NONVOL_FAR: u8 = 5;
const UWOP_EPILOG: u8 = 6;
const UWOP_SAVE_XMM128: u8 = 8;
const UWOP_SAVE_XMM128_FAR: u8 = 9;
const UWOP_PUSH_MACHFRAME: u8 = 10;

const UNW_FLAG_CHAININFO: u8 = 0x4;

/// `~0u32`: sentinel the caller passes for `prolog_offset` when not in a prolog.
const NOT_IN_PROLOG: u32 = u32::MAX;

/// Opcode entry size in 2-byte slots, mirroring Wine's `get_opcode_size`.
fn opcode_size(code: u8, info: u8) -> usize {
    match code {
        UWOP_ALLOC_LARGE => 2 + (info != 0) as usize,
        UWOP_SAVE_NONVOL | UWOP_SAVE_XMM128 | UWOP_EPILOG => 2,
        UWOP_SAVE_NONVOL_FAR | UWOP_SAVE_XMM128_FAR => 3,
        _ => 1,
    }
}

/// Validate the start of a stack read against the caller's limits. With limits,
/// the address must lie in `[lo, hi)`; with no limits, anything is accepted
/// (matching the C's behavior for callers that don't pass bounds).
#[inline]
fn stack_in_bounds(addr: u64, limits: Option<(u64, u64)>) -> bool {
    match limits {
        Some((lo, hi)) => addr >= lo && addr < hi,
        None => true,
    }
}

/// Interpret one UNWIND_INFO's opcode stream, updating the register file in
/// `int_regs`/`xmm_regs` exactly as Wine's `RtlVirtualUnwind2` inner loop does.
///
/// Parameters mirror the C call site:
/// * `int_regs` -> `&context->Rax`: 16 integer registers (Rsp is `[REG_RSP]`).
/// * `xmm_regs` -> `&context->Xmm0`: 16 16-byte XMM registers.
/// * `ctx_int` / `ctx_float` -> `ctx_ptr->IntegerContext` / `FloatingContext`
///   (16-pointer arrays recording where each register was saved), or NULL.
/// * `unwind_info` -> the `struct UNWIND_INFO`.
/// * `frame` -> the established frame base (Rsp- or frame-register-derived).
/// * `prolog_offset` -> `~0` if not inside the prolog, else the prolog offset.
/// * `limit_low`/`limit_high` -> stack bounds, or NULL for unchecked.
/// * `frame_ret` -> in/out: `*frame_ret` (UWOP_SET_FPREG writes it).
/// * `mach_frame` -> out: set to 1 if a machine frame was popped.
///
/// Returns `STATUS_SUCCESS`, or `STATUS_BAD_FUNCTION_TABLE` on an out-of-bounds
/// stack read or a malformed opcode stream.
///
/// # Safety
/// `int_regs` (>=17 u64), `xmm_regs` (>=16*16 bytes), `unwind_info` (a valid
/// UNWIND_INFO), `frame_ret` and `mach_frame` must be valid; `ctx_int`,
/// `ctx_float`, `limit_low`, `limit_high` are valid or NULL. Wine's ntdll
/// guarantees this at the single call site.
#[unsafe(no_mangle)]
pub extern "C" fn rs_rtl_unwind_run_codes(
    int_regs: *mut u64,
    xmm_regs: *mut u8,
    ctx_int: *mut *mut u64,
    ctx_float: *mut *mut u8,
    unwind_info: *const u8,
    frame: u64,
    prolog_offset: u32,
    limit_low: *const u64,
    limit_high: *const u64,
    frame_ret: *mut u64,
    mach_frame: *mut i32,
) -> i32 {
    let limits = if limit_low.is_null() || limit_high.is_null() {
        None
    } else {
        // SAFETY: both non-null per the contract.
        Some(unsafe { (*limit_low, *limit_high) })
    };

    // UNWIND_INFO header: [0]=version:3|flags:5, [1]=prolog, [2]=count,
    // [3]=frame_reg:4|frame_offset:4, then `count` 2-byte opcode slots at [4..].
    // SAFETY: `unwind_info` points at a valid UNWIND_INFO with `count` opcodes.
    let header = unsafe { core::slice::from_raw_parts(unwind_info, 4) };
    let version = header[0] & 0x7;
    let flags = header[0] >> 3;
    let count = header[2] as usize;
    // SAFETY: the opcode array is `count` 2-byte entries immediately after the header.
    let opcodes = unsafe { core::slice::from_raw_parts(unwind_info.add(4), count * 2) };

    // Helpers reading little-endian extra-data out of the opcode array. `slot`
    // is a 2-byte opcode index; bounds are guaranteed by the `i + size <= count`
    // check below before any extra slot is touched.
    let word_at = |slot: usize| -> u16 {
        u16::from_le_bytes([opcodes[slot * 2], opcodes[slot * 2 + 1]])
    };
    let dword_at = |slot: usize| -> u32 {
        u32::from_le_bytes([
            opcodes[slot * 2],
            opcodes[slot * 2 + 1],
            opcodes[slot * 2 + 2],
            opcodes[slot * 2 + 3],
        ])
    };

    // SAFETY helpers over the live register file.
    let get_rsp = || unsafe { *int_regs.add(REG_RSP) };
    let set_rsp = |v: u64| unsafe { *int_regs.add(REG_RSP) = v };

    let mut mach = 0i32;

    let mut i = 0usize;
    while i < count {
        let offset = opcodes[i * 2];
        let byte1 = opcodes[i * 2 + 1];
        let code = byte1 & 0x0f;
        let info = byte1 >> 4;

        let size = opcode_size(code, info);
        // A well-formed entry (with its extra slots) must fit in `count`.
        if i + size > count {
            return STATUS_BAD_FUNCTION_TABLE;
        }

        // Skip opcodes that belong to a not-yet-executed part of the prolog.
        if prolog_offset < offset as u32 {
            i += size;
            continue;
        }

        match code {
            UWOP_PUSH_NONVOL => {
                // pushq %reg: pop the saved register off the stack.
                let rsp = get_rsp();
                if !stack_in_bounds(rsp, limits) {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                // SAFETY: `rsp` validated against the limits (or unchecked by contract).
                let val = unsafe { (rsp as *const u64).read_unaligned() };
                set_int_reg(int_regs, ctx_int, info as usize, rsp as *mut u64, val);
                set_rsp(rsp + 8);
            }
            UWOP_ALLOC_LARGE => {
                // subq $nn,%rsp
                let n = if info != 0 {
                    dword_at(i + 1) as u64
                } else {
                    word_at(i + 1) as u64 * 8
                };
                set_rsp(get_rsp() + n);
            }
            UWOP_ALLOC_SMALL => {
                // subq $n,%rsp
                set_rsp(get_rsp() + (info as u64 + 1) * 8);
            }
            UWOP_SET_FPREG => {
                // leaq nn(%rsp),%framereg : Rsp := *frame_ret := frame
                set_rsp(frame);
                // SAFETY: `frame_ret` is valid per the contract.
                unsafe { *frame_ret = frame };
            }
            UWOP_SAVE_NONVOL => {
                // movq %reg,n(%rsp)
                let off = frame + word_at(i + 1) as u64 * 8;
                if !stack_in_bounds(off, limits) {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                // SAFETY: `off` validated against the limits.
                let val = unsafe { (off as *const u64).read_unaligned() };
                set_int_reg(int_regs, ctx_int, info as usize, off as *mut u64, val);
            }
            UWOP_SAVE_NONVOL_FAR => {
                // movq %reg,nn(%rsp)
                let off = frame + dword_at(i + 1) as u64;
                if !stack_in_bounds(off, limits) {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                // SAFETY: `off` validated against the limits.
                let val = unsafe { (off as *const u64).read_unaligned() };
                set_int_reg(int_regs, ctx_int, info as usize, off as *mut u64, val);
            }
            UWOP_SAVE_XMM128 => {
                // movaps %xmmreg,n(%rsp)
                let off = frame + word_at(i + 1) as u64 * 16;
                if !stack_in_bounds(off, limits) {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                set_float_reg(xmm_regs, ctx_float, info as usize, off);
            }
            UWOP_SAVE_XMM128_FAR => {
                // movaps %xmmreg,nn(%rsp)
                let off = frame + dword_at(i + 1) as u64;
                if !stack_in_bounds(off, limits) {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                set_float_reg(xmm_regs, ctx_float, info as usize, off);
            }
            UWOP_PUSH_MACHFRAME => {
                // Must be the last opcode and not chained (Wine FIXMEs and skips
                // otherwise; mirror that by leaving the frame unchanged).
                if (flags & UNW_FLAG_CHAININFO) == 0 && i + size >= count {
                    let mut rsp = get_rsp();
                    if info != 0 {
                        rsp += 0x8;
                    }
                    if !stack_in_bounds(rsp, limits) || !stack_in_bounds(rsp + 24, limits) {
                        return STATUS_BAD_FUNCTION_TABLE;
                    }
                    // machine frame: { Rip, Cs, EFlags, Rsp, Ss } at [rsp..].
                    // SAFETY: both reads validated against the limits.
                    let rip = unsafe { (rsp as *const u64).read_unaligned() };
                    let new_rsp = unsafe { ((rsp + 24) as *const u64).read_unaligned() };
                    // Rip lives right after R15 in CONTEXT -> register index 16.
                    unsafe { *int_regs.add(16) = rip };
                    set_rsp(new_rsp);
                    mach = 1;
                }
            }
            UWOP_EPILOG => {
                // v2 epilog markers carry no unwind action; older versions fall
                // through to the unknown-code path (as in Wine).
                if version != 2 {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
            }
            _ => return STATUS_BAD_FUNCTION_TABLE, // unknown opcode
        }

        i += size;
    }

    // SAFETY: `mach_frame` is valid per the contract.
    unsafe { *mach_frame = mach };
    STATUS_SUCCESS
}

/// `set_int_reg`: write `val` into integer register `reg` and, if a context-
/// pointers array was supplied, record the stack address `loc` it came from.
#[inline]
fn set_int_reg(int_regs: *mut u64, ctx_int: *mut *mut u64, reg: usize, loc: *mut u64, val: u64) {
    // SAFETY: `reg` is a 4-bit field (0..15); `int_regs` holds >=16 regs.
    unsafe { *int_regs.add(reg) = val };
    if !ctx_int.is_null() {
        // SAFETY: `ctx_int` is a 16-pointer array per the contract.
        unsafe { *ctx_int.add(reg) = loc };
    }
}

/// `set_float_reg`: copy the 16-byte XMM value at stack address `off` into XMM
/// register `reg`, recording the location in `ctx_float` when supplied.
#[inline]
fn set_float_reg(xmm_regs: *mut u8, ctx_float: *mut *mut u8, reg: usize, off: u64) {
    // SAFETY: `off` was bounds-checked by the caller; `reg` is a 4-bit field and
    // `xmm_regs` holds 16 16-byte registers.
    unsafe {
        core::ptr::copy_nonoverlapping(off as *const u8, xmm_regs.add(reg * 16), 16);
    }
    if !ctx_float.is_null() {
        // SAFETY: `ctx_float` is a 16-pointer array per the contract.
        unsafe { *ctx_float.add(reg) = off as *mut u8 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal UNWIND_INFO: version 1, no flags, prolog 0, the given
    /// opcode bytes (already encoded as 2-byte slots), `count` = slots.
    fn unwind_info(opcode_bytes: &[u8]) -> alloc::vec::Vec<u8> {
        assert!(opcode_bytes.len() % 2 == 0);
        let count = (opcode_bytes.len() / 2) as u8;
        let mut v = alloc::vec::Vec::new();
        v.push(0x01); // version 1, flags 0
        v.push(0x00); // prolog
        v.push(count); // count
        v.push(0x00); // frame_reg/offset
        v.extend_from_slice(opcode_bytes);
        v
    }

    extern crate alloc;

    fn run(
        regs: &mut [u64; 17],
        info: &[u8],
        frame: u64,
        limits: Option<(u64, u64)>,
    ) -> (i32, i32) {
        let mut xmm = [0u8; 16 * 16];
        let mut frame_ret = 0u64;
        let mut mach = 0i32;
        let (lo, hi) = match limits {
            Some((l, h)) => (Some(l), Some(h)),
            None => (None, None),
        };
        let status = rs_rtl_unwind_run_codes(
            regs.as_mut_ptr(),
            xmm.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            info.as_ptr(),
            frame,
            NOT_IN_PROLOG,
            lo.as_ref().map_or(core::ptr::null(), |x| x),
            hi.as_ref().map_or(core::ptr::null(), |x| x),
            &mut frame_ret,
            &mut mach,
        );
        (status, mach)
    }

    #[test]
    fn push_nonvol_pops_register_and_advances_rsp() {
        // A 16-byte stack: slot0 = 0xAAAA..., slot1 = 0xBBBB...
        let stack = [0xAAAAAAAAAAAAAAAAu64, 0xBBBBBBBBBBBBBBBBu64];
        let base = stack.as_ptr() as u64;
        let mut regs = [0u64; 17];
        regs[REG_RSP] = base;
        // pushq %rbp (reg 5): opcode offset=0, code=0(PUSH_NONVOL), info=5.
        let info = unwind_info(&[0x00, 0x50]);
        let lim = Some((base, base + 16));
        let (status, _) = run(&mut regs, &info, 0, lim);
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(regs[5], 0xAAAAAAAAAAAAAAAA); // rbp restored from slot0
        assert_eq!(regs[REG_RSP], base + 8); // rsp advanced
    }

    #[test]
    fn out_of_bounds_read_aborts_cleanly() {
        // RSP points far outside the declared limits -> the PUSH_NONVOL read must
        // be rejected, never dereferenced (this is the VMProtect/stack-overflow fix).
        let mut regs = [0u64; 17];
        regs[REG_RSP] = 0xdb4e_ec4b_fe20_e508; // the non-canonical garbage from the AFOP crash
        let info = unwind_info(&[0x00, 0x50]); // pushq %rbp
        let lim = Some((0x7fff_fe11_0000, 0x7fff_fe21_0000));
        let (status, _) = run(&mut regs, &info, 0, lim);
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
        assert_eq!(regs[5], 0); // rbp untouched -- no wild read happened
    }

    #[test]
    fn alloc_small_and_large_adjust_rsp() {
        let mut regs = [0u64; 17];
        regs[REG_RSP] = 0x1000;
        // ALLOC_SMALL info=3 -> (3+1)*8 = 32.
        let info = unwind_info(&[0x00, 0x32]); // code=2, info=3
        let (status, _) = run(&mut regs, &info, 0, None);
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(regs[REG_RSP], 0x1000 + 32);

        // ALLOC_LARGE info=0 -> USHORT slot * 8. slot = 100 -> 800.
        let mut regs2 = [0u64; 17];
        regs2[REG_RSP] = 0x2000;
        let info2 = unwind_info(&[0x00, 0x01, 100, 0x00]); // op + 1 extra slot (the count)
        let (s2, _) = run(&mut regs2, &info2, 0, None);
        assert_eq!(s2, STATUS_SUCCESS);
        assert_eq!(regs2[REG_RSP], 0x2000 + 800);
    }

    #[test]
    fn truncated_opcode_stream_is_rejected() {
        // ALLOC_LARGE info=1 needs 3 slots but only declares 1 -> malformed.
        let info = unwind_info(&[0x00, 0x11]); // code=1, info=1, count=1
        let mut regs = [0u64; 17];
        let (status, _) = run(&mut regs, &info, 0, None);
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
    }

    #[test]
    fn save_nonvol_reads_from_frame_offset() {
        let stack = [0u64, 0u64, 0xCAFEF00DD15EA5E5u64]; // slot2 holds the saved reg
        let frame = stack.as_ptr() as u64;
        let mut regs = [0u64; 17];
        // SAVE_NONVOL reg=3(rbx), offset slot = 2 -> frame + 2*8 = slot2.
        let info = unwind_info(&[0x00, 0x34, 0x02, 0x00]);
        let lim = Some((frame, frame + 24));
        let (status, _) = run(&mut regs, &info, frame, lim);
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(regs[3], 0xCAFEF00DD15EA5E5);
    }
}
