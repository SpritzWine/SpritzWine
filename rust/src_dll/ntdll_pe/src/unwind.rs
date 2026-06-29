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
/// Index of RIP in the same array: it sits right after R15 in `CONTEXT`.
const REG_RIP: usize = 16;

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

// --- Reject diagnostics --------------------------------------------------
// When the unwinder aborts a frame it reports *why* and *where* to the C
// caller, which logs it on the `unwind` debug channel. This is the line that
// was missing when triaging the VMProtect/Denuvo crashes (AFOP 2840770, WDL
// 2239550): the trace showed the frames but never said which read fell outside
// the stack window, or that the opcode stream was malformed.

/// No rejection (set on success / left untouched).
pub const UNWIND_REJECT_NONE: u32 = 0;
/// A stack read fell outside the caller-supplied `[limit_low, limit_high)`.
pub const UNWIND_REJECT_OOB_READ: u32 = 1;
/// The opcode stream was truncated/overlong (an entry ran past `count`).
pub const UNWIND_REJECT_MALFORMED: u32 = 2;
/// An unknown UWOP opcode or unsupported UNWIND_INFO version.
pub const UNWIND_REJECT_UNKNOWN_OPCODE: u32 = 3;
/// A SEH SCOPE_TABLE entry (count or a filter/__finally/jump RVA) pointed
/// outside the module image -- `__C_specific_handler` would call a wild pointer.
pub const UNWIND_REJECT_BAD_SCOPE: u32 = 4;

/// Diagnostic written by the unwinder on an aborted frame. `#[repr(C)]` so the
/// C `RtlVirtualUnwind2` can read it back (mirrored as `struct unwind_reject`
/// in dlls/ntdll/unwind.c). Field order keeps it padding-free (8+4+4).
#[repr(C)]
pub struct UnwindReject {
    /// Offending stack address (for `OOB_READ`), else 0.
    pub addr: u64,
    /// One of the `UNWIND_REJECT_*` codes.
    pub reason: u32,
    /// Opcode slot index at the failure, or `~0` when not opcode-related.
    pub opcode_index: u32,
}

/// Read the optional `[lo, hi)` stack bounds from the caller's pointers.
#[inline]
fn read_limits(limit_low: *const u64, limit_high: *const u64) -> Option<(u64, u64)> {
    if limit_low.is_null() || limit_high.is_null() {
        None
    } else {
        // SAFETY: both non-null per the contract.
        Some(unsafe { (*limit_low, *limit_high) })
    }
}

/// Record a rejection diagnostic (no-op when the caller passes NULL).
#[inline]
fn set_reject(reject: *mut UnwindReject, reason: u32, addr: u64, opcode_index: u32) {
    if !reject.is_null() {
        // SAFETY: `reject` is a valid UnwindReject per the contract.
        unsafe {
            (*reject).reason = reason;
            (*reject).addr = addr;
            (*reject).opcode_index = opcode_index;
        }
    }
}

/// Set the diagnostic and return `STATUS_BAD_FUNCTION_TABLE` in one step.
#[inline]
fn bail(reject: *mut UnwindReject, reason: u32, addr: u64, opcode_index: u32) -> i32 {
    set_reject(reject, reason, addr, opcode_index);
    STATUS_BAD_FUNCTION_TABLE
}

/// Read one integer register from the `&context->Rax` array.
#[inline]
fn get_int(int_regs: *mut u64, reg: usize) -> u64 {
    // SAFETY: `reg` is in 0..=16; the array holds Rax..R15,Rip.
    unsafe { *int_regs.add(reg) }
}

/// Write one integer register in the `&context->Rax` array.
#[inline]
fn set_int(int_regs: *mut u64, reg: usize, val: u64) {
    // SAFETY: `reg` is in 0..=16; the array holds Rax..R15,Rip.
    unsafe { *int_regs.add(reg) = val };
}

/// `~0u32`: sentinel the caller passes for `prolog_offset` when not in a prolog.
/// (Used by the tests; the live path just compares the caller-supplied value.)
#[cfg(test)]
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
/// * `reject` -> out: on a non-success return, why/where the frame was rejected
///   (offending address, reason, opcode index), or NULL if the caller doesn't
///   want the diagnostic.
///
/// Returns `STATUS_SUCCESS`, or `STATUS_BAD_FUNCTION_TABLE` on an out-of-bounds
/// stack read or a malformed opcode stream.
///
/// # Safety
/// `int_regs` (>=17 u64), `xmm_regs` (>=16*16 bytes), `unwind_info` (a valid
/// UNWIND_INFO), `frame_ret` and `mach_frame` must be valid; `ctx_int`,
/// `ctx_float`, `limit_low`, `limit_high`, `reject` are valid or NULL. Wine's
/// ntdll guarantees this at the single call site.
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
    reject: *mut UnwindReject,
) -> i32 {
    let limits = read_limits(limit_low, limit_high);

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
            return bail(reject, UNWIND_REJECT_MALFORMED, 0, i as u32);
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
                    return bail(reject, UNWIND_REJECT_OOB_READ, rsp, i as u32);
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
                    return bail(reject, UNWIND_REJECT_OOB_READ, off, i as u32);
                }
                // SAFETY: `off` validated against the limits.
                let val = unsafe { (off as *const u64).read_unaligned() };
                set_int_reg(int_regs, ctx_int, info as usize, off as *mut u64, val);
            }
            UWOP_SAVE_NONVOL_FAR => {
                // movq %reg,nn(%rsp)
                let off = frame + dword_at(i + 1) as u64;
                if !stack_in_bounds(off, limits) {
                    return bail(reject, UNWIND_REJECT_OOB_READ, off, i as u32);
                }
                // SAFETY: `off` validated against the limits.
                let val = unsafe { (off as *const u64).read_unaligned() };
                set_int_reg(int_regs, ctx_int, info as usize, off as *mut u64, val);
            }
            UWOP_SAVE_XMM128 => {
                // movaps %xmmreg,n(%rsp)
                let off = frame + word_at(i + 1) as u64 * 16;
                if !stack_in_bounds(off, limits) {
                    return bail(reject, UNWIND_REJECT_OOB_READ, off, i as u32);
                }
                set_float_reg(xmm_regs, ctx_float, info as usize, off);
            }
            UWOP_SAVE_XMM128_FAR => {
                // movaps %xmmreg,nn(%rsp)
                let off = frame + dword_at(i + 1) as u64;
                if !stack_in_bounds(off, limits) {
                    return bail(reject, UNWIND_REJECT_OOB_READ, off, i as u32);
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
                        return bail(reject, UNWIND_REJECT_OOB_READ, rsp, i as u32);
                    }
                    // machine frame: { Rip, Cs, EFlags, Rsp, Ss } at [rsp..].
                    // SAFETY: both reads validated against the limits.
                    let rip = unsafe { (rsp as *const u64).read_unaligned() };
                    let new_rsp = unsafe { ((rsp + 24) as *const u64).read_unaligned() };
                    set_int(int_regs, REG_RIP, rip);
                    set_rsp(new_rsp);
                    mach = 1;
                }
            }
            UWOP_EPILOG => {
                // v2 epilog markers carry no unwind action; older versions fall
                // through to the unknown-code path (as in Wine).
                if version != 2 {
                    return bail(reject, UNWIND_REJECT_UNKNOWN_OPCODE, 0, i as u32);
                }
            }
            _ => return bail(reject, UNWIND_REJECT_UNKNOWN_OPCODE, 0, i as u32), // unknown opcode
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

/// Pop a return address off the stack with bounds-checking: `Rip = *Rsp; Rsp += 8`.
/// Both the leaf-function path and the final "pop return address" in
/// `RtlVirtualUnwind2` do exactly this; routing them here replaces the interim C
/// `UNWIND_BAD_STACK` macro so the read honors the stack limits in one place.
///
/// Returns `STATUS_SUCCESS`, or `STATUS_BAD_FUNCTION_TABLE` (filling `reject`)
/// when `Rsp` lies outside `[limit_low, limit_high)`.
///
/// # Safety
/// `int_regs` (>=17 u64) must be valid; `limit_low`/`limit_high`/`reject` are
/// valid or NULL.
#[unsafe(no_mangle)]
pub extern "C" fn rs_rtl_unwind_pop_return(
    int_regs: *mut u64,
    limit_low: *const u64,
    limit_high: *const u64,
    reject: *mut UnwindReject,
) -> i32 {
    let limits = read_limits(limit_low, limit_high);
    let rsp = get_int(int_regs, REG_RSP);
    if !stack_in_bounds(rsp, limits) {
        return bail(reject, UNWIND_REJECT_OOB_READ, rsp, u32::MAX);
    }
    // SAFETY: `rsp` validated against the limits (or unchecked by contract).
    let rip = unsafe { (rsp as *const u64).read_unaligned() };
    set_int(int_regs, REG_RIP, rip);
    set_int(int_regs, REG_RSP, rsp + 8);
    STATUS_SUCCESS
}

/// Execute a function epilog -- the Rust port of `interpret_epilog`
/// (dlls/ntdll/unwind.c). The caller must have validated that `pc` points inside
/// an epilog (`is_inside_epilog`, still in C). The instruction-byte reads at `pc`
/// touch the module's own code exactly as the C did; the *stack* reads
/// (`pop`/`ret`) are now bounds-checked against `[limit_low, limit_high)`, so a
/// corrupt frame aborts cleanly instead of faulting inside the unwinder.
///
/// Returns `STATUS_SUCCESS`, or `STATUS_BAD_FUNCTION_TABLE` (filling `reject`)
/// on an out-of-bounds stack read.
///
/// # Safety
/// `int_regs` (>=17 u64) valid; `pc` points at a validated epilog instruction
/// stream; `ctx_int`, `limit_low`, `limit_high`, `reject` are valid or NULL.
#[unsafe(no_mangle)]
pub extern "C" fn rs_rtl_interpret_epilog(
    int_regs: *mut u64,
    ctx_int: *mut *mut u64,
    pc: *const u8,
    limit_low: *const u64,
    limit_high: *const u64,
    reject: *mut UnwindReject,
) -> i32 {
    let limits = read_limits(limit_low, limit_high);
    let mut p = pc;

    // Little-endian code reads relative to the current instruction pointer.
    // SAFETY: `p` walks a validated epilog within mapped code -- the same reads
    // the C interpret_epilog did (code, never the stack).
    let code_u8 = |p: *const u8, n: usize| -> u8 { unsafe { *p.add(n) } };
    let code_i8 = |p: *const u8, n: usize| -> i64 { (unsafe { *p.add(n) } as i8) as i64 };
    let code_i32 = |p: *const u8, n: usize| -> i64 {
        let b = unsafe { core::slice::from_raw_parts(p.add(n), 4) };
        i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64
    };
    let code_u16 = |p: *const u8, n: usize| -> u64 {
        let b = unsafe { core::slice::from_raw_parts(p.add(n), 2) };
        u16::from_le_bytes([b[0], b[1]]) as u64
    };

    // Read the return address off the (bounds-checked) stack into Rip and pop it.
    let pop_rip = |reject: *mut UnwindReject| -> Option<()> {
        let rsp = get_int(int_regs, REG_RSP);
        if !stack_in_bounds(rsp, limits) {
            set_reject(reject, UNWIND_REJECT_OOB_READ, rsp, u32::MAX);
            return None;
        }
        // SAFETY: `rsp` validated against the limits.
        let rip = unsafe { (rsp as *const u64).read_unaligned() };
        set_int(int_regs, REG_RIP, rip);
        Some(())
    };

    loop {
        let mut rex = 0u8;
        let mut b = code_u8(p, 0);
        if (b & 0xf0) == 0x40 {
            rex = b & 0x0f; // rex prefix
            p = unsafe { p.add(1) };
            b = code_u8(p, 0);
        }

        match b {
            0x58..=0x5f => {
                // pop %reg: restore it from the (checked) stack, then advance Rsp
                // from its current value (matches C, including pop %rsp order).
                let reg = (b - 0x58) as usize + (rex as usize & 1) * 8;
                let rsp = get_int(int_regs, REG_RSP);
                if !stack_in_bounds(rsp, limits) {
                    return bail(reject, UNWIND_REJECT_OOB_READ, rsp, u32::MAX);
                }
                // SAFETY: `rsp` validated against the limits.
                let val = unsafe { (rsp as *const u64).read_unaligned() };
                set_int_reg(int_regs, ctx_int, reg, rsp as *mut u64, val);
                set_int(int_regs, REG_RSP, get_int(int_regs, REG_RSP) + 8);
                p = unsafe { p.add(1) };
            }
            0x81 => {
                // add $nnnn,%rsp
                let n = code_i32(p, 2);
                set_int(int_regs, REG_RSP, get_int(int_regs, REG_RSP).wrapping_add(n as u64));
                p = unsafe { p.add(6) };
            }
            0x83 => {
                // add $n,%rsp
                let n = code_i8(p, 2);
                set_int(int_regs, REG_RSP, get_int(int_regs, REG_RSP).wrapping_add(n as u64));
                p = unsafe { p.add(3) };
            }
            0x8d => {
                // lea n(reg),%rsp  /  lea nnnn(reg),%rsp
                let modrm = code_u8(p, 1);
                let reg = (modrm & 7) as usize + (rex as usize & 1) * 8;
                if (modrm >> 6) == 1 {
                    let disp = code_i8(p, 2);
                    set_int(int_regs, REG_RSP, get_int(int_regs, reg).wrapping_add(disp as u64));
                    p = unsafe { p.add(3) };
                } else {
                    let disp = code_i32(p, 2);
                    set_int(int_regs, REG_RSP, get_int(int_regs, reg).wrapping_add(disp as u64));
                    p = unsafe { p.add(6) };
                }
            }
            0xc2 => {
                // ret $nn
                if pop_rip(reject).is_none() {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                let rsp = get_int(int_regs, REG_RSP);
                set_int(int_regs, REG_RSP, rsp + 8 + code_u16(p, 1));
                return STATUS_SUCCESS;
            }
            0xe9 | 0xeb | 0xc3 | 0xf3 | 0xff => {
                // jmp nnnn / jmp n / ret / rep;ret / jmp: pop the return address.
                if pop_rip(reject).is_none() {
                    return STATUS_BAD_FUNCTION_TABLE;
                }
                let rsp = get_int(int_regs, REG_RSP);
                set_int(int_regs, REG_RSP, rsp + 8);
                return STATUS_SUCCESS;
            }
            _ => return STATUS_SUCCESS, // matches C default: stop interpreting
        }
    }
}

/// Validate an x86_64 SEH `SCOPE_TABLE` before `__C_specific_handler` dispatches
/// it. The table is `dispatch->HandlerData`: a `u32 Count` followed by `Count`
/// 16-byte `SCOPE_RECORD`s `{ BeginAddress, EndAddress, HandlerAddress,
/// JumpTarget }` (all module RVAs). The handler calls `base + HandlerAddress`
/// and unwinds to `base + JumpTarget`, so a corrupt/foreign table (e.g. a
/// VMProtect module) could make it call or jump to a wild address. This checks
/// the whole table lies in `[base, base+image_size)` and every RVA is `<=
/// image_size` -- one call, then the C loop can trust it.
///
/// Returns `STATUS_SUCCESS`, or `STATUS_BAD_FUNCTION_TABLE` (filling `reject`
/// with the offending absolute address + record index) so the caller skips the
/// handler instead of calling into garbage.
///
/// # Safety
/// `table`/`table_addr` describe the same readable SCOPE_TABLE; `reject` is
/// valid or NULL. The 4-byte `Count` read is bounds-checked against the image
/// before any record is touched.
#[unsafe(no_mangle)]
pub extern "C" fn rs_seh_validate_scope_table(
    table: *const u8,
    table_addr: u64,
    base: u64,
    image_size: u64,
    reject: *mut UnwindReject,
) -> i32 {
    // The scope table normally lives in the module's .xdata. If HandlerData
    // points outside the image -- a stack-built table (ntdll's own exception
    // tests do this) or unusual dynamic dispatch -- we can't bound it against
    // the image, so SKIP validation (best-effort, like the unwinder with NULL
    // limits) rather than decline a legitimate handler. The real threat (a
    // corrupt in-module VMProtect .xdata) is still fully checked below.
    if table_addr < base || table_addr.wrapping_add(4) > base.wrapping_add(image_size) {
        return STATUS_SUCCESS;
    }
    // SAFETY: the 4-byte Count is within the (mapped) image per the check above.
    let count = unsafe { (table as *const u32).read_unaligned() };

    // The whole table (Count + Count*16) must fit in the image, else `count` is
    // garbage and walking the records would read out of bounds.
    let off = table_addr - base;
    let table_bytes = 4u64 + (count as u64) * 16;
    if off + table_bytes > image_size {
        return bail(reject, UNWIND_REJECT_BAD_SCOPE, table_addr, count);
    }

    // Every record's RVAs must point inside the image.
    // SAFETY: the record array is within the image per the size check above.
    let records = unsafe { table.add(4) as *const u32 };
    for i in 0..count as usize {
        let fields = [
            unsafe { records.add(i * 4).read_unaligned() },     // BeginAddress
            unsafe { records.add(i * 4 + 1).read_unaligned() }, // EndAddress
            unsafe { records.add(i * 4 + 2).read_unaligned() }, // HandlerAddress (called)
            unsafe { records.add(i * 4 + 3).read_unaligned() }, // JumpTarget (unwound to)
        ];
        for &rva in &fields {
            if rva as u64 > image_size {
                return bail(reject, UNWIND_REJECT_BAD_SCOPE, base + rva as u64, i as u32);
            }
        }
    }
    STATUS_SUCCESS
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
            core::ptr::null_mut(),
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

    #[test]
    fn reject_diagnostic_is_filled_on_oob_read() {
        // The opcode interpreter must report WHY/WHERE it bailed so the C caller
        // can log it (the info missing when triaging AFOP/WDL).
        let mut regs = [0u64; 17];
        regs[REG_RSP] = 0xdb4e_ec4b_fe20_e508; // garbage RSP (AFOP)
        let mut xmm = [0u8; 16 * 16];
        let mut frame_ret = 0u64;
        let mut mach = 0i32;
        let mut reject = UnwindReject { addr: 0, reason: UNWIND_REJECT_NONE, opcode_index: 0 };
        let info = unwind_info(&[0x00, 0x50]); // pushq %rbp
        let (lo, hi) = (0x7fff_fe11_0000u64, 0x7fff_fe21_0000u64);
        let status = rs_rtl_unwind_run_codes(
            regs.as_mut_ptr(),
            xmm.as_mut_ptr(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            info.as_ptr(),
            0,
            NOT_IN_PROLOG,
            &lo,
            &hi,
            &mut frame_ret,
            &mut mach,
            &mut reject,
        );
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
        assert_eq!(reject.reason, UNWIND_REJECT_OOB_READ);
        assert_eq!(reject.addr, 0xdb4e_ec4b_fe20_e508); // the exact offending address
        assert_eq!(reject.opcode_index, 0);
    }

    #[test]
    fn pop_return_reads_rip_and_advances() {
        let stack = [0x1234_5678_9abc_def0u64];
        let base = stack.as_ptr() as u64;
        let mut regs = [0u64; 17];
        regs[REG_RSP] = base;
        let status = rs_rtl_unwind_pop_return(regs.as_mut_ptr(), &base, &(base + 8), core::ptr::null_mut());
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(regs[REG_RIP], 0x1234_5678_9abc_def0); // Rip popped
        assert_eq!(regs[REG_RSP], base + 8); // Rsp advanced
    }

    #[test]
    fn pop_return_oob_aborts_with_reject() {
        let mut regs = [0u64; 17];
        regs[REG_RSP] = 0xdead_beef_0000_0000; // outside the window
        let mut reject = UnwindReject { addr: 0, reason: UNWIND_REJECT_NONE, opcode_index: 0 };
        let (lo, hi) = (0x7fff_0000_0000u64, 0x7fff_0001_0000u64);
        let status = rs_rtl_unwind_pop_return(regs.as_mut_ptr(), &lo, &hi, &mut reject);
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
        assert_eq!(reject.reason, UNWIND_REJECT_OOB_READ);
        assert_eq!(regs[REG_RIP], 0); // never read
    }

    #[test]
    fn interpret_epilog_add_pop_ret() {
        // Epilog: `add $0x28,%rsp; pop %rbp; ret`. rbp restored from base+40,
        // Rip from base+48, Rsp ends at base+56.
        let mut stack = [0u64; 8];
        stack[5] = 0xAAAA_AAAA_AAAA_AAAA; // base+40 -> saved rbp
        stack[6] = 0xBBBB_BBBB_BBBB_BBBB; // base+48 -> return address
        let base = stack.as_ptr() as u64;
        let mut regs = [0u64; 17];
        regs[REG_RSP] = base;
        let code: [u8; 6] = [0x48, 0x83, 0xc4, 0x28, 0x5d, 0xc3];
        let status = rs_rtl_interpret_epilog(
            regs.as_mut_ptr(),
            core::ptr::null_mut(),
            code.as_ptr(),
            &base,
            &(base + 64),
            core::ptr::null_mut(),
        );
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(regs[5], 0xAAAA_AAAA_AAAA_AAAA); // rbp restored
        assert_eq!(regs[REG_RIP], 0xBBBB_BBBB_BBBB_BBBB); // Rip = return address
        assert_eq!(regs[REG_RSP], base + 56);
    }

    #[test]
    fn interpret_epilog_oob_pop_aborts() {
        // A `pop %rbp` whose RSP is outside the limits must abort, not fault.
        let mut regs = [0u64; 17];
        regs[REG_RSP] = 0x4141_4141_0000_0000; // outside the window
        let mut reject = UnwindReject { addr: 0, reason: UNWIND_REJECT_NONE, opcode_index: 0 };
        let code: [u8; 2] = [0x5d, 0xc3]; // pop %rbp; ret
        let (lo, hi) = (0x7fff_0000_0000u64, 0x7fff_0001_0000u64);
        let status = rs_rtl_interpret_epilog(
            regs.as_mut_ptr(),
            core::ptr::null_mut(),
            code.as_ptr(),
            &lo,
            &hi,
            &mut reject,
        );
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
        assert_eq!(reject.reason, UNWIND_REJECT_OOB_READ);
        assert_eq!(regs[5], 0); // rbp untouched
    }

    /// Build a SCOPE_TABLE buffer: u32 Count, then 16-byte records.
    fn scope_table(records: &[[u32; 4]]) -> alloc::vec::Vec<u8> {
        let mut v = alloc::vec::Vec::new();
        v.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for r in records {
            for f in r {
                v.extend_from_slice(&f.to_le_bytes());
            }
        }
        v
    }

    fn validate(buf: &[u8], table_addr: u64, base: u64, image_size: u64) -> (i32, UnwindReject) {
        let mut reject = UnwindReject { addr: 0, reason: UNWIND_REJECT_NONE, opcode_index: 0 };
        let status =
            rs_seh_validate_scope_table(buf.as_ptr(), table_addr, base, image_size, &mut reject);
        (status, reject)
    }

    #[test]
    fn scope_table_valid_passes() {
        let base = 0x1_4000_0000u64;
        let img = 0x10000u64;
        // begin/end/handler/jump all in-image.
        let buf = scope_table(&[[0x1000, 0x1100, 0x1200, 0], [0x2000, 0x2100, 1, 0x2080]]);
        let (status, _) = validate(&buf, base + 0x8000, base, img);
        assert_eq!(status, STATUS_SUCCESS);
    }

    #[test]
    fn scope_table_wild_handler_rejected() {
        let base = 0x1_4000_0000u64;
        let img = 0x10000u64;
        // record 0 ok; record 1 has a HandlerAddress way past the image (VMProtect-style).
        let buf = scope_table(&[[0x1000, 0x1100, 0x1200, 0], [0x2000, 0x2100, 0xDEAD_BEEF, 0]]);
        let (status, reject) = validate(&buf, base + 0x8000, base, img);
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
        assert_eq!(reject.reason, UNWIND_REJECT_BAD_SCOPE);
        assert_eq!(reject.addr, base + 0xDEAD_BEEF); // the would-be call target
        assert_eq!(reject.opcode_index, 1); // offending record index
    }

    #[test]
    fn scope_table_garbage_count_rejected_before_walk() {
        let base = 0x1_4000_0000u64;
        let img = 0x10000u64;
        // a huge Count that can't fit in the image -> rejected before reading any record.
        let mut buf = alloc::vec::Vec::new();
        buf.extend_from_slice(&0x00FF_FFFFu32.to_le_bytes()); // absurd Count, no records follow
        let (status, reject) = validate(&buf, base + 0x8000, base, img);
        assert_eq!(status, STATUS_BAD_FUNCTION_TABLE);
        assert_eq!(reject.reason, UNWIND_REJECT_BAD_SCOPE);
    }

    #[test]
    fn scope_table_outside_image_is_skipped() {
        // A stack-built table (HandlerData outside the module image) must be
        // SKIPPED, not rejected -- declining it would break legitimate handlers
        // (ntdll's own exception tests build scope tables on the stack).
        let base = 0x1_4000_0000u64;
        let img = 0x10000u64;
        let buf = scope_table(&[[0x1000, 0x1100, 0x1200, 0]]);
        // table_addr far below the image base (a "stack" address).
        let (status, reject) = validate(&buf, 0x7fff_0000_0000u64, base, img);
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(reject.reason, UNWIND_REJECT_NONE);
    }
}
