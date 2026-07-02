//! The Sony rumble state machine and output-report renderer.
//!
//! Wine's PID reports arrive pre-parsed from `hid.c`'s dispatcher; we keep a
//! per-effect snapshot (type, magnitudes, duration, gains), fold every playing
//! effect plus the simple-haptics levels into the two motors, and emit the
//! firmware's output report when the levels change. The report layouts and
//! flag semantics follow the Linux kernel's `drivers/hid/hid-playstation.c`.

/// PID usage-page constants (see `include/wine/hid.h`).
const ET_CONSTANT_FORCE: u16 = 0x26;
const ET_RAMP: u16 = 0x27;
const ET_SQUARE: u16 = 0x30;
const ET_SINE: u16 = 0x31;
const ET_TRIANGLE: u16 = 0x32;
const ET_SAWTOOTH_UP: u16 = 0x33;
const ET_SAWTOOTH_DOWN: u16 = 0x34;

const OP_EFFECT_START: u16 = 0x79;
const OP_EFFECT_START_SOLO: u16 = 0x7a;
const OP_EFFECT_STOP: u16 = 0x7b;

const DC_ENABLE_ACTUATORS: u16 = 0x97;
const DC_DISABLE_ACTUATORS: u16 = 0x98;
const DC_STOP_ALL_EFFECTS: u16 = 0x99;
const DC_DEVICE_RESET: u16 = 0x9a;
const DC_DEVICE_PAUSE: u16 = 0x9b;
const DC_DEVICE_CONTINUE: u16 = 0x9c;

/// An effect duration of 0xffff (PID "null") and a loop count of 0xff both
/// mean "play until stopped".
const DURATION_INFINITE: u16 = 0xffff;
const ITERATIONS_INFINITE: u8 = 0xff;

/// While a motor is running, refresh the output report so the firmware's
/// idle timeout never silences an infinite effect.
const KEEPALIVE_MS: u64 = 2000;

/// Largest report we emit (the Bluetooth reports).
pub const MAX_REPORT_SIZE: usize = 78;

/// CRC seed byte for Sony Bluetooth *output* reports (0xa2; input uses 0xa1).
const OUTPUT_CRC_SEED: u8 = 0xa2;

#[derive(Clone, Copy, PartialEq)]
pub enum Family {
    DualShock4,
    DualSense,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Bus {
    Usb,
    Bluetooth,
}

#[derive(Clone, Copy, Default)]
struct Effect {
    playing: bool,
    /// Absolute stop time in ms; 0 = play until stopped.
    stop_time: u64,
    effect_type: u16,
    duration: u16,
    start_delay: u16,
    gain_percent: u8,
    /// Type-independent magnitude, 0..0x7fff (the PID logical range).
    base_level: u16,
}

pub struct SonyRumble {
    family: Family,
    bus: Bus,
    /// DualSense firmware >= 2.21 wants the revised vibration flag.
    vibration_v2: bool,
    bt_seq: u8,

    gain_percent: u8,
    actuators_enabled: bool,
    paused: bool,

    haptics_left: u16,
    haptics_right: u16,
    haptics_deadline: u64,

    effects: Box<[Effect; 256]>,

    motor_left: u8,
    motor_right: u8,
    last_write_ms: u64,
}

impl SonyRumble {
    pub fn new(family: Family, bus: Bus, vibration_v2: bool) -> Self {
        Self {
            family,
            bus,
            vibration_v2,
            bt_seq: 0,
            gain_percent: 100,
            actuators_enabled: true,
            paused: false,
            haptics_left: 0,
            haptics_right: 0,
            haptics_deadline: 0,
            effects: Box::new([Effect::default(); 256]),
            motor_left: 0,
            motor_right: 0,
            last_write_ms: 0,
        }
    }

    pub fn haptics_start(&mut self, now_ms: u64, duration_ms: u64, rumble: u16, buzz: u16) {
        self.haptics_left = rumble;
        self.haptics_right = buzz;
        self.haptics_deadline = now_ms.saturating_add(duration_ms);
    }

    pub fn haptics_stop(&mut self) {
        self.haptics_left = 0;
        self.haptics_right = 0;
        self.haptics_deadline = 0;
    }

    /// Returns true when the control is fully supported (pause/continue are
    /// only tracked, matching the lnxev backend).
    pub fn device_control(&mut self, control: u16) -> bool {
        match control {
            DC_ENABLE_ACTUATORS => self.actuators_enabled = true,
            DC_DISABLE_ACTUATORS => self.actuators_enabled = false,
            DC_STOP_ALL_EFFECTS => self.stop_all(),
            DC_DEVICE_RESET => {
                self.stop_all();
                self.haptics_stop();
                self.gain_percent = 100;
                self.actuators_enabled = true;
                self.paused = false;
            }
            DC_DEVICE_PAUSE => {
                self.paused = true;
                return false;
            }
            DC_DEVICE_CONTINUE => {
                self.paused = false;
                return false;
            }
            _ => return false,
        }
        true
    }

    pub fn set_gain(&mut self, percent: u8) {
        self.gain_percent = percent.min(100);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn effect_update(
        &mut self,
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
        let effect = &mut self.effects[usize::from(index)];
        effect.effect_type = effect_type;
        effect.duration = duration_ms;
        effect.start_delay = start_delay_ms;
        effect.gain_percent = gain_percent.min(100);
        effect.base_level = match effect_type {
            ET_SINE | ET_SQUARE | ET_TRIANGLE | ET_SAWTOOTH_UP | ET_SAWTOOTH_DOWN => periodic_magnitude,
            ET_CONSTANT_FORCE => constant_magnitude.unsigned_abs(),
            ET_RAMP => ramp_start.unsigned_abs().max(ramp_end.unsigned_abs()),
            _ => 0, // conditions etc. have no motor mapping
        };
    }

    pub fn effect_control(&mut self, now_ms: u64, index: u8, control: u16, iterations: u8) {
        match control {
            OP_EFFECT_START | OP_EFFECT_START_SOLO => {
                if control == OP_EFFECT_START_SOLO {
                    self.stop_all();
                }
                let effect = &mut self.effects[usize::from(index)];
                effect.playing = true;
                effect.stop_time = if effect.duration == DURATION_INFINITE || iterations == ITERATIONS_INFINITE {
                    0
                } else {
                    let total = u64::from(effect.duration) * u64::from(iterations.max(1));
                    now_ms.saturating_add(u64::from(effect.start_delay)).saturating_add(total)
                };
            }
            OP_EFFECT_STOP => self.effects[usize::from(index)].playing = false,
            _ => {}
        }
    }

    fn stop_all(&mut self) {
        for effect in self.effects.iter_mut() {
            effect.playing = false;
        }
    }

    /// Expires deadlines, mixes the motor levels and decides whether a report
    /// must go out. Returns the report length rendered into `buf` (0 = no
    /// write needed) and the next absolute wake-up time (`u64::MAX` = none).
    pub fn tick(&mut self, now_ms: u64, buf: &mut [u8]) -> (usize, u64) {
        let mut left: u32 = 0;
        let mut right: u32 = 0;
        let mut next: u64 = u64::MAX;

        if self.haptics_left != 0 || self.haptics_right != 0 {
            if now_ms >= self.haptics_deadline {
                self.haptics_stop();
            } else {
                left = left.max(u32::from(self.haptics_left));
                right = right.max(u32::from(self.haptics_right));
                next = next.min(self.haptics_deadline);
            }
        }

        if self.actuators_enabled && !self.paused {
            let device_gain = u32::from(self.gain_percent);
            for effect in self.effects.iter_mut().filter(|e| e.playing) {
                if effect.stop_time != 0 && now_ms >= effect.stop_time {
                    effect.playing = false;
                    continue;
                }
                // 0..0x7fff magnitude, effect gain, device gain -> 0..0xffff
                let mut level = u32::from(effect.base_level) * u32::from(effect.gain_percent) / 100 * device_gain / 100;
                level = (level * 2).min(0xffff);
                left = left.max(level);
                right = right.max(level);
                if effect.stop_time != 0 {
                    next = next.min(effect.stop_time);
                }
            }
        }

        let motor_left = (left >> 8) as u8;
        let motor_right = (right >> 8) as u8;

        let keepalive_due = (motor_left != 0 || motor_right != 0)
            && now_ms.saturating_sub(self.last_write_ms) >= KEEPALIVE_MS;
        let mut len = 0;
        if motor_left != self.motor_left || motor_right != self.motor_right || keepalive_due {
            self.motor_left = motor_left;
            self.motor_right = motor_right;
            self.last_write_ms = now_ms;
            len = self.build_report(buf);
        }

        if motor_left != 0 || motor_right != 0 {
            next = next.min(self.last_write_ms + KEEPALIVE_MS);
        }

        (len, next)
    }

    /// Renders the motor levels into the device's output report. Layouts per
    /// the kernel's hid-playstation.c (`dualsense_output_report_*`,
    /// `dualshock4_output_report_*`).
    fn build_report(&mut self, buf: &mut [u8]) -> usize {
        if buf.len() < MAX_REPORT_SIZE {
            return 0;
        }
        buf[..MAX_REPORT_SIZE].fill(0);

        let bt = self.bus == Bus::Bluetooth;
        let len;
        let common;
        match self.family {
            Family::DualSense => {
                if bt {
                    buf[0] = 0x31; // DS_OUTPUT_REPORT_BT
                    buf[1] = self.bt_seq << 4;
                    self.bt_seq = (self.bt_seq + 1) & 0x0f;
                    buf[2] = 0x10; // DS_OUTPUT_TAG, must be set
                    common = 3;
                    len = 78;
                } else {
                    buf[0] = 0x02; // DS_OUTPUT_REPORT_USB
                    common = 1;
                    len = 63;
                }
                // valid_flag0: select classic rumble haptics...
                buf[common] = 0x02; // HAPTICS_SELECT
                if self.vibration_v2 {
                    buf[common + 38] |= 0x04; // valid_flag2: COMPATIBLE_VIBRATION2
                } else {
                    buf[common] |= 0x01; // COMPATIBLE_VIBRATION
                }
                buf[common + 2] = self.motor_right;
                buf[common + 3] = self.motor_left;
            }
            Family::DualShock4 => {
                if bt {
                    buf[0] = 0x11; // DS4_OUTPUT_REPORT_BT
                    buf[1] = 0xc0; // hw_control: HID | CRC32
                    common = 3;
                    len = 78;
                } else {
                    buf[0] = 0x05; // DS4_OUTPUT_REPORT_USB
                    common = 1;
                    len = 32;
                }
                buf[common] = 0x01; // valid_flag0: MOTOR
                buf[common + 3] = self.motor_right;
                buf[common + 4] = self.motor_left;
            }
        }

        if bt {
            let crc = crc32_seeded(&buf[..len - 4]);
            buf[len - 4..len].copy_from_slice(&crc.to_le_bytes());
        }

        len
    }
}

/// IEEE CRC-32 (reflected, poly 0xedb88320) over the output seed byte followed
/// by `data` — the signature Sony firmware checks on Bluetooth reports.
fn crc32_seeded(data: &[u8]) -> u32 {
    let mut crc = crc32_update(0xffff_ffff, &[OUTPUT_CRC_SEED]);
    crc = crc32_update(crc, data);
    !crc
}

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick_report(state: &mut SonyRumble, now: u64) -> (Vec<u8>, u64) {
        let mut buf = [0u8; MAX_REPORT_SIZE];
        let (len, next) = state.tick(now, &mut buf);
        (buf[..len].to_vec(), next)
    }

    fn start_sine(state: &mut SonyRumble, now: u64, index: u8, magnitude: u16, duration: u16) {
        state.effect_update(index, ET_SINE, duration, 0, 100, magnitude, 0, 0, 0);
        state.effect_control(now, index, OP_EFFECT_START, 1);
    }

    #[test]
    fn crc32_check_vector() {
        // standard CRC-32 check value, without the Sony seed
        assert_eq!(!crc32_update(0xffff_ffff, b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn dualsense_usb_report_layout() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        start_sine(&mut state, 0, 1, 0x7fff, DURATION_INFINITE);
        let (report, next) = tick_report(&mut state, 0);
        assert_eq!(report.len(), 63);
        assert_eq!(report[0], 0x02);
        assert_eq!(report[1], 0x03); // HAPTICS_SELECT | COMPATIBLE_VIBRATION
        assert_eq!(report[1 + 2], 0xff); // motor_right
        assert_eq!(report[1 + 3], 0xff); // motor_left
        assert_eq!(next, KEEPALIVE_MS);
    }

    #[test]
    fn dualsense_usb_vibration_v2_flags() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, true);
        start_sine(&mut state, 0, 1, 0x4000, DURATION_INFINITE);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report[1], 0x02); // HAPTICS_SELECT only
        assert_eq!(report[1 + 38], 0x04); // valid_flag2: COMPATIBLE_VIBRATION2
    }

    #[test]
    fn dualsense_bt_report_golden() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Bluetooth, false);
        // right motor 0x40, left 0x80: use haptics for asymmetric levels
        state.haptics_start(0, 1000, 0x8000 | 0xff, 0x4000 | 0xff);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report.len(), 78);
        assert_eq!(&report[..3], &[0x31, 0x00, 0x10]);
        assert_eq!(report[3], 0x03);
        assert_eq!(report[3 + 2], 0x40);
        assert_eq!(report[3 + 3], 0x80);
        // golden CRC computed with zlib over 0xa2 + report[..74]
        assert_eq!(&report[74..], &0x2479_73c8u32.to_le_bytes());
    }

    #[test]
    fn dualsense_bt_sequence_increments_and_wraps() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Bluetooth, false);
        for i in 0..18u64 {
            // alternate levels so every tick writes
            let level = if i % 2 == 0 { 0x7fff } else { 0x4000 };
            start_sine(&mut state, i, 1, level, DURATION_INFINITE);
            let (report, _) = tick_report(&mut state, i);
            assert_eq!(report[1] >> 4, (i % 16) as u8, "seq at write {i}");
        }
    }

    #[test]
    fn dualshock4_reports() {
        let mut state = SonyRumble::new(Family::DualShock4, Bus::Usb, false);
        state.haptics_start(0, 1000, 0x2200 | 0xff, 0x1100 | 0xff);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report.len(), 32);
        assert_eq!(report[0], 0x05);
        assert_eq!(report[1], 0x01); // MOTOR
        assert_eq!(report[1 + 3], 0x11); // motor_right
        assert_eq!(report[1 + 4], 0x22); // motor_left

        let mut state = SonyRumble::new(Family::DualShock4, Bus::Bluetooth, false);
        state.haptics_start(0, 1000, 0x2200 | 0xff, 0x1100 | 0xff);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report.len(), 78);
        assert_eq!(&report[..3], &[0x11, 0xc0, 0x00]);
        assert_eq!(report[3 + 3], 0x11);
        assert_eq!(report[3 + 4], 0x22);
        assert_eq!(&report[74..], &0x08b9_e9e0u32.to_le_bytes());
    }

    #[test]
    fn effect_expires_at_deadline_and_writes_stop() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        start_sine(&mut state, 1000, 1, 0x7fff, 500);
        let (report, next) = tick_report(&mut state, 1000);
        assert!(!report.is_empty());
        assert_eq!(next, 1500);
        // still playing right before the deadline: no state change, no write
        let (report, _) = tick_report(&mut state, 1499);
        assert!(report.is_empty());
        // at the deadline the effect expires and the motors are silenced
        let (report, next) = tick_report(&mut state, 1500);
        assert_eq!(report.len(), 63);
        assert_eq!(report[1 + 2], 0);
        assert_eq!(report[1 + 3], 0);
        assert_eq!(next, u64::MAX);
    }

    #[test]
    fn keepalive_refreshes_running_motors() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        start_sine(&mut state, 0, 1, 0x7fff, DURATION_INFINITE);
        let (report, next) = tick_report(&mut state, 0);
        assert!(!report.is_empty());
        assert_eq!(next, KEEPALIVE_MS);
        let (report, _) = tick_report(&mut state, KEEPALIVE_MS - 1);
        assert!(report.is_empty(), "no write before the keepalive");
        let (report, next) = tick_report(&mut state, KEEPALIVE_MS);
        assert!(!report.is_empty(), "keepalive write");
        assert_eq!(next, 2 * KEEPALIVE_MS);
    }

    #[test]
    fn gains_scale_the_level() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        state.set_gain(50);
        state.effect_update(1, ET_SINE, DURATION_INFINITE, 0, 50, 0x7fff, 0, 0, 0);
        state.effect_control(0, 1, OP_EFFECT_START, 1);
        let (report, _) = tick_report(&mut state, 0);
        // 0x7fff * 50% * 50% * 2 = 0x3fff -> motor 0x3f
        assert_eq!(report[1 + 3], 0x3f);
    }

    #[test]
    fn stop_all_and_disable_actuators_silence() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        start_sine(&mut state, 0, 1, 0x7fff, DURATION_INFINITE);
        let (report, _) = tick_report(&mut state, 0);
        assert!(!report.is_empty());

        assert!(state.device_control(DC_STOP_ALL_EFFECTS));
        let (report, next) = tick_report(&mut state, 1);
        assert_eq!(report[1 + 3], 0);
        assert_eq!(next, u64::MAX);

        start_sine(&mut state, 2, 1, 0x7fff, DURATION_INFINITE);
        let (report, _) = tick_report(&mut state, 2);
        assert!(!report.is_empty());
        assert!(state.device_control(DC_DISABLE_ACTUATORS));
        let (report, _) = tick_report(&mut state, 3);
        assert_eq!(report[1 + 3], 0);
    }

    #[test]
    fn constant_and_ramp_levels() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        state.effect_update(1, ET_CONSTANT_FORCE, DURATION_INFINITE, 0, 100, 0, -0x4000, 0, 0);
        state.effect_control(0, 1, OP_EFFECT_START, 1);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report[1 + 3], 0x80); // |−0x4000| * 2 -> 0x8000

        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        state.effect_update(1, ET_RAMP, DURATION_INFINITE, 0, 100, 0, 0, 0x1000, -0x2000);
        state.effect_control(0, 1, OP_EFFECT_START, 1);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report[1 + 3], 0x40); // max(|0x1000|, |−0x2000|) * 2 -> 0x4000
    }

    #[test]
    fn haptics_and_effects_merge_with_max() {
        let mut state = SonyRumble::new(Family::DualSense, Bus::Usb, false);
        start_sine(&mut state, 0, 1, 0x2000, DURATION_INFINITE); // -> 0x4000
        state.haptics_start(0, 10_000, 0x8000, 0x1000);
        let (report, _) = tick_report(&mut state, 0);
        assert_eq!(report[1 + 3], 0x80); // left: haptics rumble wins
        assert_eq!(report[1 + 2], 0x40); // right: effect wins over buzz
    }
}
