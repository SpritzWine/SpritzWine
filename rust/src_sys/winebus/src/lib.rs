//! Sony gamepad (DualShock 4 / DualSense) rumble core for winebus.sys.
//!
//! Wine routes Sony pads through the hidraw passthrough backend (for the
//! native input protocol — PS glyphs, gyro, touchpad), but that backend has no
//! force-feedback support: the pads only rumble through vendor output reports.
//! The C side (`dlls/winebus.sys/bus_udev.c`) splices Wine's synthetic
//! haptics + PID reports into the passthrough descriptor and forwards the
//! parsed requests here; this crate owns everything device-independent:
//!
//! * [`descriptor`] — sanity scan of the untrusted device descriptor (must be
//!   well-formed and keep our synthetic report-ID range free),
//! * [`sony`] — the rumble state machine: PID effects + simple haptics are
//!   mixed into the two motor levels, with per-effect stop deadlines and a
//!   periodic keepalive, and rendered into the exact output report the
//!   firmware wants (USB `0x02`/`0x05`, Bluetooth `0x31`/`0x11` + CRC32),
//!   layouts per the kernel's `hid-playstation.c`.
//!
//! Threading contract: a `SonyRumble` handle is only ever touched under
//! winebus's `udev_cs` (vtbl callbacks and the rumble thread both hold it), so
//! the raw-pointer handle is dereferenced without further locking. All time is
//! passed in by the caller (monotonic milliseconds), keeping the logic pure
//! and host-testable.

use std::ffi::c_void;

mod descriptor;
mod sony;

use sony::{Bus, Family, SonyRumble};

/// Scans an untrusted HID report descriptor: every item must lie inside the
/// buffer, the descriptor must end by closing its application collection, and
/// no declared report ID may fall inside our synthetic range
/// `(base, base_max)` (exclusive bounds). Returns 1 when it is safe to splice
/// the synthetic reports in.
///
/// # Safety
/// `data` must point to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_check_descriptor(data: *const u8, len: u32, base: u8, base_max: u8) -> i32 {
    if data.is_null() {
        return 0;
    }
    let desc = unsafe { std::slice::from_raw_parts(data, len as usize) };
    descriptor::check_descriptor(desc, base, base_max) as i32
}

/// Allocates the rumble state. `family`/`bus` use the C-side constants
/// (family: 1 = DualShock 4, 2 = DualSense; bus: `BUS_TYPE_USB` = 1,
/// `BUS_TYPE_BLUETOOTH` = 2). Returns NULL on invalid arguments.
#[unsafe(no_mangle)]
pub extern "C" fn rs_sony_create(family: u32, bus: u32, vibration_v2: i32) -> *mut c_void {
    let family = match family {
        1 => Family::DualShock4,
        2 => Family::DualSense,
        _ => return std::ptr::null_mut(),
    };
    let bus = match bus {
        1 => Bus::Usb,
        2 => Bus::Bluetooth,
        _ => return std::ptr::null_mut(),
    };
    Box::into_raw(Box::new(SonyRumble::new(family, bus, vibration_v2 != 0))) as *mut c_void
}

/// # Safety
/// `state` must be a pointer from [`rs_sony_create`]; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_destroy(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state as *mut SonyRumble) });
    }
}

unsafe fn state_mut<'a>(state: *mut c_void) -> &'a mut SonyRumble {
    unsafe { &mut *(state as *mut SonyRumble) }
}

/// Simple-haptics start (`HID_USAGE_HAPTICS_INTENSITY` output report):
/// `rumble` drives the strong/left motor, `buzz` the weak/right one, until
/// `now_ms + duration_ms`.
///
/// # Safety
/// `state` must be a live [`rs_sony_create`] pointer, called under `udev_cs`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_haptics_start(state: *mut c_void, now_ms: u64, duration_ms: u32, rumble: u16, buzz: u16) {
    unsafe { state_mut(state) }.haptics_start(now_ms, duration_ms as u64, rumble, buzz);
}

/// # Safety
/// See [`rs_sony_haptics_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_haptics_stop(state: *mut c_void) {
    unsafe { state_mut(state) }.haptics_stop();
}

/// PID device control (`PID_USAGE_DC_*` usage). Returns 1 when the control is
/// supported, 0 when it is only tracked (pause/continue, like lnxev).
///
/// # Safety
/// See [`rs_sony_haptics_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_device_control(state: *mut c_void, control: u16) -> i32 {
    unsafe { state_mut(state) }.device_control(control) as i32
}

/// PID device gain (0..100).
///
/// # Safety
/// See [`rs_sony_haptics_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_set_gain(state: *mut c_void, percent: u8) {
    unsafe { state_mut(state) }.set_gain(percent);
}

/// Snapshot of the parameters of effect `index` (from the PID set-effect /
/// set-periodic / set-constant-force / set-ramp-force reports). The C side
/// extracts the scalars we need from `struct effect_params`, so no struct
/// layout is shared across the FFI.
///
/// # Safety
/// See [`rs_sony_haptics_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_effect_update(
    state: *mut c_void,
    index: u8,
    effect_type: u16,
    duration_ms: u16,
    start_delay_ms: u16,
    gain_percent: u8,
    periodic_magnitude: u16,
    constant_magnitude: i16,
    ramp_start: i16,
    ramp_end: i16,
) {
    unsafe { state_mut(state) }.effect_update(
        index,
        effect_type,
        duration_ms,
        start_delay_ms,
        gain_percent,
        periodic_magnitude,
        constant_magnitude,
        ramp_start,
        ramp_end,
    );
}

/// PID effect operation (`PID_USAGE_OP_EFFECT_*` usage) on effect `index`.
///
/// # Safety
/// See [`rs_sony_haptics_start`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_effect_control(state: *mut c_void, now_ms: u64, index: u8, control: u16, iterations: u8) {
    unsafe { state_mut(state) }.effect_control(now_ms, index, control, iterations);
}

/// One pass of the rumble thread: expires effects, mixes the motor levels and,
/// when the device needs a write (level change or keepalive refresh), renders
/// the output report into `buf` (capacity `cap` >= 78) and returns 1 with
/// `*out_len` set. `*out_next_ms` receives the next absolute deadline the
/// thread must wake at, or `u64::MAX` when there is none.
///
/// # Safety
/// See [`rs_sony_haptics_start`]; `buf` must point to `cap` writable bytes and
/// `out_len`/`out_next_ms` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rs_sony_tick(
    state: *mut c_void,
    now_ms: u64,
    buf: *mut u8,
    cap: u32,
    out_len: *mut u32,
    out_next_ms: *mut u64,
) -> i32 {
    let report = unsafe { std::slice::from_raw_parts_mut(buf, cap as usize) };
    let (len, next) = unsafe { state_mut(state) }.tick(now_ms, report);
    unsafe {
        *out_len = len as u32;
        *out_next_ms = next;
    }
    (len != 0) as i32
}
