//! Mirrors `dlls/ntdll/unix/system.c`.

use core::ffi::c_void;

use crate::status::{STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_PARAMETER, STATUS_SUCCESS};

/// Mirror of Wine's `SYSTEM_POWER_INFORMATION` (see `include/winternl.h`).
#[repr(C)]
struct SystemPowerInformation {
    max_idleness_allowed: u32, // MaxIdlenessAllowed (ULONG)
    idleness: u32,             // Idleness (ULONG)
    time_remaining: u32,       // TimeRemaining (ULONG)
    cooling_mode: u8,          // CoolingMode (UCHAR)
}

use std::fs;
use std::path::Path;

/// Handles the `SystemPowerInformation` case of `NtPowerInformation`.
///
/// Called from `dlls/ntdll/unix/system.c`. `output` points to a caller-provided
/// `SYSTEM_POWER_INFORMATION` of `out_size` bytes.
///
/// # Safety
/// `output` must be either null or a valid, writable pointer to at least
/// `out_size` bytes. The caller (ntdll) guarantees this.
#[unsafe(no_mangle)]
pub extern "C" fn rs_nt_system_power_information(output: *mut c_void, out_size: u32) -> i32 {
    if output.is_null() || out_size == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    if out_size as usize != core::mem::size_of::<SystemPowerInformation>() {
        return STATUS_BUFFER_TOO_SMALL;
    }

    let time_remaining = get_battery_time_remaining();
    let idleness = get_idleness();

    // SAFETY: validated non-null and size-matched above; ntdll guarantees the
    // buffer is writable for `out_size` bytes.
    unsafe {
        let info = &mut *(output as *mut SystemPowerInformation);
        info.max_idleness_allowed = 100;
        info.idleness = idleness;
        info.time_remaining = time_remaining;
        info.cooling_mode = 0; // 0 = Active cooling (fans), 1 = Passive
    }

    STATUS_SUCCESS
}

fn get_battery_time_remaining() -> u32 {
    let power_supply_dir = Path::new("/sys/class/power_supply");
    let Ok(entries) = fs::read_dir(power_supply_dir) else {
        return u32::MAX; // AC power / no battery
    };

    let mut is_ac_online = false;
    let mut total_time_remaining = 0;
    let mut has_battery = false;

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        
        let Ok(type_str) = fs::read_to_string(path.join("type")) else { continue };
        let type_str = type_str.trim();

        if type_str == "Mains" {
            if let Ok(online) = fs::read_to_string(path.join("online")) {
                if online.trim() == "1" {
                    is_ac_online = true;
                }
            }
        } else if type_str == "Battery" {
            has_battery = true;
            // Energy is in µWh or charge in µAh
            let energy_now = fs::read_to_string(path.join("energy_now"))
                .or_else(|_| fs::read_to_string(path.join("charge_now")))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());

            // Power is in µW or current in µA
            let power_now = fs::read_to_string(path.join("power_now"))
                .or_else(|_| fs::read_to_string(path.join("current_now")))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());

            if let (Some(en), Some(pn)) = (energy_now, power_now) {
                if pn > 0 {
                    let seconds = (en * 3600) / pn;
                    total_time_remaining += seconds as u32;
                }
            }
        }
    }

    if is_ac_online || !has_battery {
        u32::MAX // Returns -1 (0xFFFFFFFF) as per Windows spec for AC power
    } else {
        total_time_remaining
    }
}

fn get_idleness() -> u32 {
    // A quick way to estimate idleness is from /proc/stat boot average
    if let Ok(stat) = fs::read_to_string("/proc/stat") {
        if let Some(line) = stat.lines().next() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 && parts[0] == "cpu" {
                let mut total: u64 = 0;
                let mut idle: u64 = 0;
                for (i, part) in parts.iter().skip(1).enumerate() {
                    if let Ok(val) = part.parse::<u64>() {
                        total += val;
                        // idle is 4th column (index 3) and iowait is 5th (index 4)
                        if i == 3 || i == 4 {
                            idle += val;
                        }
                    }
                }
                if total > 0 {
                    return ((idle * 100) / total) as u32;
                }
            }
        }
    }
    0
}
