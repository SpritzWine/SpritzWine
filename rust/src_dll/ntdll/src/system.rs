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

// The FFI contract depends on this matching Wine's SYSTEM_BATTERY_STATE exactly.
const _: () = assert!(core::mem::size_of::<SystemBatteryState>() == 32);

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

/// One power supply's sysfs attributes, abstracted over the backing store so the
/// aggregation logic can be unit-tested without a real `/sys/class/power_supply`.
trait PowerSupply {
    /// A string attribute (the sysfs file's first line, trimmed), or `None` if
    /// the attribute is absent.
    fn attr(&self, name: &str) -> Option<String>;

    /// An integer attribute, 0 if missing or unparseable (matching Wine's `atoi`).
    fn attr_int(&self, name: &str) -> i64 {
        self.attr(name).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0)
    }
}

/// A power supply backed by a real `/sys/class/power_supply/<name>` directory.
struct SysfsSupply(std::path::PathBuf);

impl PowerSupply for SysfsSupply {
    fn attr(&self, name: &str) -> Option<String> {
        let content = fs::read_to_string(self.0.join(name)).ok()?;
        Some(content.lines().next().unwrap_or("").trim().to_string())
    }
}

fn read_battery_state() -> SystemBatteryState {
    let supplies: Vec<SysfsSupply> = match fs::read_dir("/sys/class/power_supply") {
        Ok(entries) => entries.filter_map(Result::ok).map(|e| SysfsSupply(e.path())).collect(),
        // No power-supply class at all (desktop / container): report AC online.
        Err(_) => Vec::new(),
    };
    aggregate_battery_state(&supplies)
}

/// Builds the composite `SYSTEM_BATTERY_STATE` across *all* mains and batteries,
/// the way Windows reports them as a single logical battery. This is where the
/// Rust port improves on Wine's C, which only ever looked at the first of each
/// (it logged a FIXME and ignored the rest).
fn aggregate_battery_state<S: PowerSupply>(supplies: &[S]) -> SystemBatteryState {
    let mut bs = SystemBatteryState {
        ac_on_line: 0,
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

    let mut found_mains = false;
    let mut ac_online = false;
    let mut any_charging = false;
    let mut any_discharging = false;
    let mut total_max: i64 = 0;
    let mut total_remaining: i64 = 0;
    let mut total_rate: i64 = 0; // signed net rate: + charging, - discharging

    for s in supplies {
        // Skip device-scoped supplies (peripheral batteries: mice, keyboards…).
        if s.attr("scope").as_deref() == Some("Device") {
            continue;
        }
        match s.attr("type").as_deref() {
            Some("Mains") => {
                let Some(online) = s.attr("online") else { continue };
                found_mains = true;
                ac_online |= online.parse::<i32>().unwrap_or(0) != 0;
            }
            Some("Battery") => {
                let Some(status) = s.attr("status") else { continue };
                bs.battery_present = 1;

                let voltage = s.attr_int("voltage_now"); // microvolts
                total_max += scale(s.attr_int("charge_full"), voltage);
                total_remaining += scale(s.attr_int("charge_now"), voltage);

                // current_now is a magnitude on most drivers; take the direction
                // from `status` rather than its (driver-dependent) sign.
                let magnitude = scale(s.attr_int("current_now").abs(), voltage);
                match status.as_str() {
                    "Charging" => {
                        any_charging = true;
                        total_rate += magnitude;
                    }
                    "Discharging" => {
                        any_discharging = true;
                        total_rate -= magnitude;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // AC is online if any mains is online; default to online when there is no
    // mains supply at all (matches Wine's "assume AC" fallback).
    bs.ac_on_line = (!found_mains || ac_online) as u8;

    if bs.battery_present != 0 {
        bs.max_capacity = total_max as u32;
        bs.remaining_capacity = total_remaining as u32;
        // Negative (net discharging) wraps into the ULONG bit pattern, so apps
        // reading Rate as a signed LONG see a negative value.
        bs.rate = total_rate as u32;

        // Charge direction from the net rate, falling back to the reported
        // status when the net rate is unknown (0).
        if total_rate > 0 {
            bs.charging = 1;
        } else if total_rate < 0 {
            bs.discharging = 1;
        } else {
            bs.charging = (any_charging && !any_discharging) as u8;
            bs.discharging = (any_discharging && !any_charging) as u8;
        }

        bs.estimated_time = if total_rate < 0 {
            let drain = (-total_rate) as u32;
            3600u32.wrapping_mul(bs.remaining_capacity) / drain
        } else {
            u32::MAX
        };
    }

    bs
}

/// Wine's `get_sys_int(...) * voltage / 1e9`: a 64-bit product scaled by 1e9 in
/// floating point and truncated toward zero (microamp-hours × microvolts → the
/// milliwatt-hour-ish units Windows expects). Kept signed so net charge/discharge
/// rates can be summed across batteries.
fn scale(value: i64, voltage: i64) -> i64 {
    ((value * voltage) as f64 / 1e9) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_state_rejects_bad_buffers() {
        let mut buf = [0u8; 32];
        assert_eq!(
            rs_nt_fill_battery_state(core::ptr::null_mut(), 32),
            STATUS_INVALID_PARAMETER
        );
        assert_eq!(
            rs_nt_fill_battery_state(buf.as_mut_ptr() as *mut c_void, 16),
            STATUS_BUFFER_TOO_SMALL
        );
    }

    #[test]
    fn battery_state_fills_a_coherent_struct() {
        let mut bs = std::mem::MaybeUninit::<SystemBatteryState>::zeroed();
        let rc = rs_nt_fill_battery_state(
            bs.as_mut_ptr() as *mut c_void,
            core::mem::size_of::<SystemBatteryState>() as u32,
        );
        assert_eq!(rc, STATUS_SUCCESS);

        // Reading the host's real /sys/class/power_supply must not panic and must
        // leave a coherent result: with no battery, the battery-only fields stay 0.
        let bs = unsafe { bs.assume_init() };
        if bs.battery_present == 0 {
            assert_eq!(bs.charging, 0);
            assert_eq!(bs.discharging, 0);
            assert_eq!(bs.remaining_capacity, 0);
        }
    }

    #[test]
    fn scale_matches_wine_float_truncation() {
        // 5_000_000 µAh * 12_000_000 µV / 1e9 = 60_000 (truncated toward zero).
        assert_eq!(scale(5_000_000, 12_000_000), 60_000);
        assert_eq!(scale(-1_000_000, 12_000_000), -12_000);
        // Stored into the ULONG Rate field, a negative value reads back as a
        // negative LONG — what EstimatedTime relies on.
        assert!((scale(-1_000_000, 12_000_000) as u32 as i32) < 0);
    }

    /// A power supply with attributes supplied directly, so the aggregation can
    /// be exercised without a real `/sys/class/power_supply`.
    struct FakeSupply(std::collections::HashMap<&'static str, &'static str>);

    impl FakeSupply {
        fn new(attrs: &[(&'static str, &'static str)]) -> Self {
            FakeSupply(attrs.iter().copied().collect())
        }
    }

    impl PowerSupply for FakeSupply {
        fn attr(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|s| s.to_string())
        }
    }

    #[test]
    fn aggregates_multiple_batteries() {
        let supplies = vec![
            FakeSupply::new(&[("type", "Mains"), ("online", "1")]),
            // Discharging at 12_000 (mW-ish): 60_000 max / 30_000 remaining.
            FakeSupply::new(&[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("voltage_now", "12000000"),
                ("charge_full", "5000000"),
                ("charge_now", "2500000"),
                ("current_now", "1000000"),
            ]),
            // Charging at 6_000: 48_000 max / 12_000 remaining.
            FakeSupply::new(&[
                ("type", "Battery"),
                ("status", "Charging"),
                ("voltage_now", "12000000"),
                ("charge_full", "4000000"),
                ("charge_now", "1000000"),
                ("current_now", "500000"),
            ]),
        ];

        let bs = aggregate_battery_state(&supplies);
        assert_eq!(bs.ac_on_line, 1);
        assert_eq!(bs.battery_present, 1);
        assert_eq!(bs.max_capacity, 108_000); // 60_000 + 48_000
        assert_eq!(bs.remaining_capacity, 42_000); // 30_000 + 12_000
        assert_eq!(bs.rate as i32, -6_000); // -12_000 + 6_000 net discharge
        assert_eq!(bs.discharging, 1);
        assert_eq!(bs.charging, 0);
        assert_eq!(bs.estimated_time, 3600 * 42_000 / 6_000); // 25_200
    }

    #[test]
    fn ac_online_if_any_mains_online() {
        let mixed = vec![
            FakeSupply::new(&[("type", "Mains"), ("online", "0")]),
            FakeSupply::new(&[("type", "Mains"), ("online", "1")]),
        ];
        assert_eq!(aggregate_battery_state(&mixed).ac_on_line, 1);

        let all_off = vec![
            FakeSupply::new(&[("type", "Mains"), ("online", "0")]),
            FakeSupply::new(&[("type", "Mains"), ("online", "0")]),
        ];
        assert_eq!(aggregate_battery_state(&all_off).ac_on_line, 0);
    }

    #[test]
    fn no_supplies_reports_ac_and_no_battery() {
        let bs = aggregate_battery_state::<FakeSupply>(&[]);
        assert_eq!(bs.ac_on_line, 1);
        assert_eq!(bs.battery_present, 0);
        assert_eq!(bs.estimated_time, 0);
    }

    #[test]
    fn skips_device_scoped_batteries() {
        // A peripheral (e.g. a wireless mouse) must not count as the system battery.
        let supplies = vec![FakeSupply::new(&[
            ("type", "Battery"),
            ("scope", "Device"),
            ("status", "Discharging"),
            ("voltage_now", "4000000"),
            ("charge_full", "1000000"),
            ("charge_now", "500000"),
            ("current_now", "100000"),
        ])];
        assert_eq!(aggregate_battery_state(&supplies).battery_present, 0);
    }
}
