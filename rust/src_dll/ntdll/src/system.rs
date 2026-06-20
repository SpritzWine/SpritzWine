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

/// Mirror of Wine's `SYSTEM_BATTERY_STATE` (see `include/winnt.h`). 32 bytes,
/// no padding (the four `u32`s are naturally aligned after the 8 leading bytes).
#[repr(C)]
struct SystemBatteryState {
    ac_on_line: u8,          // AcOnLine (BOOLEAN)
    battery_present: u8,     // BatteryPresent
    charging: u8,            // Charging
    discharging: u8,         // Discharging
    spare1: [u8; 3],         // Spare1[3]
    tag: u8,                 // Tag (BYTE)
    max_capacity: u32,       // MaxCapacity (ULONG)
    remaining_capacity: u32, // RemainingCapacity
    rate: u32,               // Rate
    estimated_time: u32,     // EstimatedTime
    default_alert1: u32,     // DefaultAlert1
    default_alert2: u32,     // DefaultAlert2
}

/// Handles the `SystemBatteryState` case of `NtPowerInformation`, the data
/// behind Win32's `GetSystemPowerStatus`.
///
/// Called from `dlls/ntdll/unix/system.c`. Ports Wine's Linux `fill_battery_state`
/// 1:1, reading `/sys/class/power_supply`. `output` points to a caller-provided
/// `SYSTEM_BATTERY_STATE` of `out_size` bytes.
///
/// # Safety
/// `output` must be either null or a valid, writable pointer to at least
/// `out_size` bytes. The caller (ntdll) guarantees this.
#[unsafe(no_mangle)]
pub extern "C" fn rs_nt_fill_battery_state(output: *mut c_void, out_size: u32) -> i32 {
    if output.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if (out_size as usize) < core::mem::size_of::<SystemBatteryState>() {
        return STATUS_BUFFER_TOO_SMALL;
    }

    let bs = read_battery_state();

    // SAFETY: validated non-null and size-checked above; ntdll guarantees the
    // buffer is writable for `out_size` bytes.
    unsafe {
        core::ptr::write(output as *mut SystemBatteryState, bs);
    }

    STATUS_SUCCESS
}

fn read_battery_state() -> SystemBatteryState {
    // Default to "AC online, no battery" — matches Wine's C when there is no
    // power_supply class (e.g. a desktop or a container).
    let mut bs = SystemBatteryState {
        ac_on_line: 1,
        battery_present: 0,
        charging: 0,
        discharging: 0,
        spare1: [0; 3],
        tag: 0,
        max_capacity: 0,
        remaining_capacity: 0,
        rate: 0,
        estimated_time: 0,
        default_alert1: 0,
        default_alert2: 0,
    };

    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
        return bs;
    };

    let mut found_ac = false;

    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();

        // Skip device-scoped supplies (peripheral batteries: mice, keyboards…).
        if sys_str(&path, "scope").as_deref() == Some("Device") {
            continue;
        }
        let Some(kind) = sys_str(&path, "type") else { continue };

        match kind.as_str() {
            "Mains" => {
                let Some(online) = sys_str(&path, "online") else { continue };
                // Wine reports only the first mains supply.
                if found_ac {
                    continue;
                }
                bs.ac_on_line = (online.parse::<i32>().unwrap_or(0) != 0) as u8;
                found_ac = true;
            }
            "Battery" => {
                let Some(status) = sys_str(&path, "status") else { continue };
                // Wine reports only the first battery.
                if bs.battery_present != 0 {
                    continue;
                }
                bs.charging = (status == "Charging") as u8;
                bs.discharging = (status == "Discharging") as u8;
                bs.battery_present = 1;

                let voltage = sys_int(&path, "voltage_now"); // microvolts
                bs.max_capacity = scale(sys_int(&path, "charge_full"), voltage);
                bs.remaining_capacity = scale(sys_int(&path, "charge_now"), voltage);
                bs.rate = scale(-sys_int(&path, "current_now"), voltage);

                // Rate is stored unsigned but read back as a signed LONG; a
                // negative rate means discharging, so we can estimate runtime.
                if bs.charging == 0 && (bs.rate as i32) < 0 {
                    let drain = (-(bs.rate as i32)) as u32;
                    bs.estimated_time = 3600u32.wrapping_mul(bs.remaining_capacity) / drain;
                } else {
                    bs.estimated_time = u32::MAX;
                }
            }
            _ => {}
        }
    }

    bs
}

/// Mirrors Wine's `get_sys_str`: the first line of `<dir>/<name>`, trimmed.
/// Returns `None` if the attribute is absent (the sysfs file doesn't exist).
fn sys_str(dir: &Path, name: &str) -> Option<String> {
    let content = fs::read_to_string(dir.join(name)).ok()?;
    Some(content.lines().next().unwrap_or("").trim().to_string())
}

/// Mirrors Wine's `get_sys_int`: the attribute parsed as an integer, 0 if
/// missing or unparseable (matching `atoi` on a failed read).
fn sys_int(dir: &Path, name: &str) -> i64 {
    sys_str(dir, name)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Mirrors Wine's `get_sys_int(...) * voltage / 1e9`: a 64-bit product scaled by
/// 1e9 in floating point, truncated toward zero, then stored into a 32-bit
/// `ULONG` (wrapping for negative results, as the C cast to `ULONG` does).
fn scale(value: i64, voltage: i64) -> u32 {
    ((value * voltage) as f64 / 1e9) as i64 as u32
}
