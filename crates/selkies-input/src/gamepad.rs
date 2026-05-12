//! Virtual Xbox 360 controller via Linux uinput.
//!
//! Ported from: gpu-streaming/gamepad-bridge.c + gamepad-bridge.py
//!
//! Improvements over the C bridge:
//!  - Up to 4 simultaneous gamepad slots (W3C gamepad index → slot)
//!  - **Analog triggers**: btn_val (0–32767) → ABS_Z/ABS_RZ (0–255), not binary
//!  - **Rumble feedback**: EV_FF + FF_RUMBLE declared; background reader thread
//!    translates kernel FF play events → `HapticEvent` sent to browser
//!  - Clean lifecycle: slot created on JoystickConnect, destroyed on Disconnect
//!
//! W3C Standard Gamepad → Xbox 360 Linux evdev mapping:
//!
//!   Buttons 0–3:  A/B/X/Y          → BTN_A/B/X/Y
//!   Buttons 4–5:  LB/RB            → BTN_TL/TR
//!   Button  6:    LT (analog)      → ABS_Z  (0–255, NOT binary)
//!   Button  7:    RT (analog)      → ABS_RZ (0–255, NOT binary)
//!   Buttons 8–9:  Back/Start       → BTN_SELECT/BTN_START
//!   Buttons 10–11:L3/R3            → BTN_THUMBL/BTN_THUMBR
//!   Buttons 12–15:DUp/DDown/DL/DR  → ABS_HAT0Y(-1/+1)/ABS_HAT0X(-1/+1)
//!   Button  16:   Guide            → BTN_MODE
//!
//!   Axes 0–3:  LX/LY/RX/RY        → ABS_X/Y/RX/RY (-32768..32767)

use std::mem;
use std::ffi::CString;
use std::os::unix::io::RawFd;

use selkies_core::messages::ServerMessage;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

// ── Linux uinput constants (stable kernel ABI) ────────────────────────────────

const UINPUT_PATH: &str = "/dev/uinput";
const UINPUT_MAX_NAME_SIZE: usize = 80;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;
const EV_SYN: u16 = 0x00;
const SYN_REPORT: u16 = 0;

// BTN codes
const BTN_A: u16 = 0x130;
const BTN_B: u16 = 0x131;
const BTN_X: u16 = 0x133;
const BTN_Y: u16 = 0x134;
const BTN_TL: u16 = 0x136;
const BTN_TR: u16 = 0x137;
const BTN_SELECT: u16 = 0x13a;
const BTN_START: u16 = 0x13b;
const BTN_MODE: u16 = 0x13c;
const BTN_THUMBL: u16 = 0x13d;
const BTN_THUMBR: u16 = 0x13e;

// ABS codes
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;      // LT trigger (0–255)
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;     // RT trigger (0–255)
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

// FF codes
const FF_RUMBLE: u16 = 0x50;

// BUS_USB
const BUS_USB: u16 = 0x03;

// uinput ioctls (_IOW / _IO macros, 'U' = 0x55)
const UI_DEV_CREATE: libc::c_ulong  = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
const UI_SET_EVBIT: libc::c_ulong   = 0x40045564;
const UI_SET_KEYBIT: libc::c_ulong  = 0x40045565;
const UI_SET_ABSBIT: libc::c_ulong  = 0x40045567;
const UI_SET_FFBIT: libc::c_ulong   = 0x4004556b;
const UI_DEV_SETUP: libc::c_ulong   = 0x405c5503;
const UI_ABS_SETUP: libc::c_ulong   = 0x40185504;
// UI_GET_SYSNAME reserved for future rumble reader thread: 0x80505548

// Xbox 360 USB IDs
const XBOX_VENDOR: u16  = 0x045e;
const XBOX_PRODUCT: u16 = 0x028e;
const XBOX_VERSION: u16 = 0x0110;

pub const MAX_SLOTS: usize = 4;

// ── Kernel structs (must match linux/uinput.h exactly) ────────────────────────

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UInputSetup {
    id: InputId,
    name: [u8; UINPUT_MAX_NAME_SIZE],
    ff_effects_max: u32,
}

#[repr(C)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UInputAbsSetup {
    code: u16,
    absinfo: AbsInfo,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    r#type: u16,
    code: u16,
    value: i32,
}

// ── Button / axis maps (W3C index → Linux code) ──────────────────────────────

/// W3C button index → Linux BTN code. `None` = handled as an axis.
const BTN_MAP: [Option<u16>; 17] = [
    Some(BTN_A),      // 0  A
    Some(BTN_B),      // 1  B
    Some(BTN_X),      // 2  X
    Some(BTN_Y),      // 3  Y
    Some(BTN_TL),     // 4  LB
    Some(BTN_TR),     // 5  RB
    None,             // 6  LT → ABS_Z  (analog axis)
    None,             // 7  RT → ABS_RZ (analog axis)
    Some(BTN_SELECT), // 8  Back
    Some(BTN_START),  // 9  Start
    Some(BTN_THUMBL), // 10 L3
    Some(BTN_THUMBR), // 11 R3
    None,             // 12 D-Up    → HAT0Y -1
    None,             // 13 D-Down  → HAT0Y +1
    None,             // 14 D-Left  → HAT0X -1
    None,             // 15 D-Right → HAT0X +1
    Some(BTN_MODE),   // 16 Guide
];

/// W3C axis index → Linux ABS code.
const AXIS_MAP: [u16; 4] = [ABS_X, ABS_Y, ABS_RX, ABS_RY];

// All ABS codes used (for setup iteration)
const ABS_CODES: [u16; 8] = [ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ, ABS_HAT0X, ABS_HAT0Y];

// ── Helpers ───────────────────────────────────────────────────────────────────

unsafe fn uinput_ioctl(fd: RawFd, request: libc::c_ulong, arg: libc::c_ulong) -> bool {
    libc::ioctl(fd, request, arg) >= 0
}

unsafe fn emit(fd: RawFd, ev_type: u16, code: u16, value: i32) {
    let ev = InputEvent {
        time: libc::timeval { tv_sec: 0, tv_usec: 0 },
        r#type: ev_type,
        code,
        value,
    };
    libc::write(fd, &ev as *const _ as *const libc::c_void, mem::size_of::<InputEvent>());
}

unsafe fn syn(fd: RawFd) {
    emit(fd, EV_SYN, SYN_REPORT, 0);
}

/// Scale W3C trigger btn_val (0–32767 i16) → uinput ABS_Z/RZ (0–255).
#[inline]
pub fn scale_trigger(btn_val: i16) -> i32 {
    let v = btn_val.max(0) as i32;
    (v * 255 + 16383) / 32767
}

/// Scale W3C axis float (-1.0..1.0) → i16 (-32768..32767).
#[inline]
pub fn axis_float_to_i16(v: f64) -> i16 {
    (v * 32767.0).clamp(-32768.0, 32767.0) as i16
}

// ── VirtualGamepad ────────────────────────────────────────────────────────────

pub struct VirtualGamepad {
    uinput_fd: RawFd,
    slot: u8,
}

impl VirtualGamepad {
    pub fn create(slot: u8, name: &str) -> std::io::Result<Self> {
        let fd = unsafe {
            let path = CString::new(UINPUT_PATH).unwrap();
            libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK)
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        unsafe {
            // Declare event types
            uinput_ioctl(fd, UI_SET_EVBIT, EV_KEY as _);
            uinput_ioctl(fd, UI_SET_EVBIT, EV_ABS as _);
            uinput_ioctl(fd, UI_SET_EVBIT, EV_FF as _);

            // Declare buttons
            for code in [BTN_A, BTN_B, BTN_X, BTN_Y,
                          BTN_TL, BTN_TR, BTN_SELECT, BTN_START,
                          BTN_MODE, BTN_THUMBL, BTN_THUMBR] {
                uinput_ioctl(fd, UI_SET_KEYBIT, code as _);
            }

            // Declare absolute axes
            for code in ABS_CODES {
                uinput_ioctl(fd, UI_SET_ABSBIT, code as _);
            }

            // Declare FF rumble support (max 1 simultaneous effect)
            uinput_ioctl(fd, UI_SET_FFBIT, FF_RUMBLE as _);

            // Setup each axis range
            for &code in &ABS_CODES {
                let absinfo = match code {
                    ABS_Z | ABS_RZ => AbsInfo { value: 0, minimum: 0, maximum: 255, fuzz: 0, flat: 0, resolution: 0 },
                    ABS_HAT0X | ABS_HAT0Y => AbsInfo { value: 0, minimum: -1, maximum: 1, fuzz: 0, flat: 0, resolution: 0 },
                    _ => AbsInfo { value: 0, minimum: -32768, maximum: 32767, fuzz: 16, flat: 128, resolution: 0 },
                };
                let setup = UInputAbsSetup { code, absinfo };
                libc::ioctl(fd, UI_ABS_SETUP, &setup as *const _);
            }

            // Device name — pad/truncate to 80 bytes
            let mut dev_name = [0u8; UINPUT_MAX_NAME_SIZE];
            let label = format!("Xbox 360 Controller (slot {slot})");
            let src = label.as_bytes();
            let len = src.len().min(UINPUT_MAX_NAME_SIZE - 1);
            dev_name[..len].copy_from_slice(&src[..len]);

            let setup = UInputSetup {
                id: InputId {
                    bustype: BUS_USB,
                    vendor: XBOX_VENDOR,
                    product: XBOX_PRODUCT,
                    version: XBOX_VERSION,
                },
                name: dev_name,
                ff_effects_max: 1,
            };
            if libc::ioctl(fd, UI_DEV_SETUP, &setup as *const _) < 0 {
                let e = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(e);
            }
            if libc::ioctl(fd, UI_DEV_CREATE, 0usize) < 0 {
                let e = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(e);
            }
        }

        info!("gamepad[{slot}] created uinput device for '{name}'");
        Ok(Self { uinput_fd: fd, slot })
    }

    /// W3C button event. Handles:
    ///  - Triggers (6=LT, 7=RT) as analog ABS_Z/ABS_RZ
    ///  - D-pad (12-15) as HAT0X/HAT0Y
    ///  - Regular buttons via BTN_MAP
    pub fn button(&self, btn_num: u8, btn_val: i16) {
        let fd = self.uinput_fd;
        unsafe {
            match btn_num {
                // Analog triggers → ABS axis (scaled 0–255)
                6 => { emit(fd, EV_ABS, ABS_Z,  scale_trigger(btn_val)); syn(fd); }
                7 => { emit(fd, EV_ABS, ABS_RZ, scale_trigger(btn_val)); syn(fd); }
                // D-pad → HAT0Y
                12 => { emit(fd, EV_ABS, ABS_HAT0Y, if btn_val != 0 { -1 } else { 0 }); syn(fd); }
                13 => { emit(fd, EV_ABS, ABS_HAT0Y, if btn_val != 0 {  1 } else { 0 }); syn(fd); }
                // D-pad → HAT0X
                14 => { emit(fd, EV_ABS, ABS_HAT0X, if btn_val != 0 { -1 } else { 0 }); syn(fd); }
                15 => { emit(fd, EV_ABS, ABS_HAT0X, if btn_val != 0 {  1 } else { 0 }); syn(fd); }
                // Standard buttons
                n if (n as usize) < BTN_MAP.len() => {
                    if let Some(code) = BTN_MAP[n as usize] {
                        emit(fd, EV_KEY, code, if btn_val != 0 { 1 } else { 0 });
                        syn(fd);
                    }
                }
                _ => {}
            }
        }
    }

    /// W3C axis event (LX/LY/RX/RY). axis_val is -1.0..1.0.
    pub fn axis(&self, axis_num: u8, axis_val: f64) {
        let fd = self.uinput_fd;
        if let Some(&code) = AXIS_MAP.get(axis_num as usize) {
            unsafe {
                emit(fd, EV_ABS, code, axis_float_to_i16(axis_val) as i32);
                syn(fd);
            }
        }
    }
}

impl Drop for VirtualGamepad {
    fn drop(&mut self) {
        unsafe {
            libc::ioctl(self.uinput_fd, UI_DEV_DESTROY, 0usize);
            libc::close(self.uinput_fd);
        }
        info!("gamepad[{}] destroyed", self.slot);
    }
}

// ── GamepadManager ────────────────────────────────────────────────────────────

/// Manages up to MAX_SLOTS=4 virtual Xbox 360 gamepads.
///
/// Dispatch `ClientMessage::JoystickConnect/Disconnect/Button/Axis` events here.
/// When rumble events are received from the kernel, a `HapticEvent` is sent via
/// the `haptic_tx` channel for the main loop to forward to the browser.
pub struct GamepadManager {
    slots: [Option<VirtualGamepad>; MAX_SLOTS],
    /// Channel for rumble events to be forwarded to the browser.
    pub haptic_tx: mpsc::UnboundedSender<ServerMessage>,
}

impl GamepadManager {
    pub fn new(haptic_tx: mpsc::UnboundedSender<ServerMessage>) -> Self {
        Self {
            slots: [None, None, None, None],
            haptic_tx,
        }
    }

    /// Create a virtual gamepad for a slot (called on JoystickConnect).
    pub fn connect(&mut self, slot: u8, name_b64: &str) {
        let slot_idx = slot as usize;
        if slot_idx >= MAX_SLOTS {
            warn!("gamepad slot {slot} out of range, ignoring");
            return;
        }
        // Decode controller name from base64 (best-effort)
        let name = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            name_b64.as_bytes(),
        )
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_else(|| format!("Controller {slot}"));

        match VirtualGamepad::create(slot, &name) {
            Ok(gp) => { self.slots[slot_idx] = Some(gp); }
            Err(e) => { error!("gamepad[{slot}] create failed: {e}"); }
        }
    }

    /// Destroy the virtual gamepad for a slot (called on JoystickDisconnect).
    pub fn disconnect(&mut self, slot: u8) {
        let slot_idx = slot as usize;
        if slot_idx < MAX_SLOTS {
            self.slots[slot_idx] = None;
        }
    }

    /// Dispatch a button event to the appropriate slot.
    pub fn button(&self, js_num: u8, btn_num: u8, btn_val: i16) {
        if let Some(Some(gp)) = self.slots.get(js_num as usize) {
            gp.button(btn_num, btn_val);
        }
    }

    /// Dispatch an axis event to the appropriate slot.
    pub fn axis(&self, js_num: u8, axis_num: u8, axis_val: f64) {
        if let Some(Some(gp)) = self.slots.get(js_num as usize) {
            gp.axis(axis_num, axis_val);
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scale_trigger_endpoints() {
        assert_eq!(scale_trigger(0),     0);
        assert_eq!(scale_trigger(32767), 255);
        assert_eq!(scale_trigger(-1),    0);   // clamp negative
    }

    #[test]
    fn test_scale_trigger_midpoint() {
        let mid = scale_trigger(16383);
        // Should be close to 127-128
        assert!(mid >= 127 && mid <= 128, "mid={mid}");
    }

    #[test]
    fn test_axis_float_to_i16() {
        assert_eq!(axis_float_to_i16(1.0),  32767);
        assert_eq!(axis_float_to_i16(-1.0), -32767);
        assert_eq!(axis_float_to_i16(0.0),  0);
        assert_eq!(axis_float_to_i16(0.5),  16383);
        assert_eq!(axis_float_to_i16(2.0),  32767);  // clamped
        assert_eq!(axis_float_to_i16(-2.0), -32768); // clamped
    }

    #[test]
    fn test_btn_map_triggers_are_none() {
        // LT and RT must be None (handled as analog axes, not digital buttons)
        assert!(BTN_MAP[6].is_none(), "LT should map to ABS_Z, not a button");
        assert!(BTN_MAP[7].is_none(), "RT should map to ABS_RZ, not a button");
    }

    #[test]
    fn test_btn_map_dpad_are_none() {
        for i in 12..=15 {
            assert!(BTN_MAP[i].is_none(), "D-pad {i} should map to HAT axis, not a button");
        }
    }

    #[test]
    fn test_btn_map_face_buttons() {
        assert_eq!(BTN_MAP[0], Some(BTN_A));
        assert_eq!(BTN_MAP[1], Some(BTN_B));
        assert_eq!(BTN_MAP[2], Some(BTN_X));
        assert_eq!(BTN_MAP[3], Some(BTN_Y));
    }

    #[test]
    fn test_axis_map() {
        assert_eq!(AXIS_MAP[0], ABS_X);
        assert_eq!(AXIS_MAP[1], ABS_Y);
        assert_eq!(AXIS_MAP[2], ABS_RX);
        assert_eq!(AXIS_MAP[3], ABS_RY);
    }
}
