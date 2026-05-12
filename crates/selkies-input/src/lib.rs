//! Input event handling: mouse, keyboard, gamepad, clipboard, resize.
//!
//! Ported from: webrtc_input.py (772 lines), gamepad.py (395 lines)
//!
//! This module provides:
//! - Constants and enums for input events
//! - Gamepad axis/button remapping (browser W3C → Xbox 360 uinput)
//! - Gamepad fast-path packet format (4-byte DGRAM to C bridge)
//! - Resolution validation and rounding
//! - InputInjector trait (X11 implementation deferred to when x11rb is added)
//!
// TODO(port): X11 mouse/keyboard injection (needs x11rb crate)
// TODO(port): gamepad.py SelkiesGamepad (uinput device creation)
// TODO(port): clipboard read/write (xclip subprocess or x11rb selections)

/// Mouse action constants matching browser-side enum.
pub mod mouse {
    pub const POSITION: u8 = 10;
    pub const MOVE: u8 = 11;
    pub const SCROLL_UP: u8 = 20;
    pub const SCROLL_DOWN: u8 = 21;
    pub const BUTTON_PRESS: u8 = 30;
    pub const BUTTON_RELEASE: u8 = 31;
    pub const BUTTON: u8 = 40;
    pub const BUTTON_LEFT: u8 = 41;
    pub const BUTTON_MIDDLE: u8 = 42;
    pub const BUTTON_RIGHT: u8 = 43;
}

/// Uinput event type/code pairs for mouse buttons and axes.
pub mod uinput {
    pub const BTN_LEFT: (u16, u16) = (0x01, 0x110);
    pub const BTN_MIDDLE: (u16, u16) = (0x01, 0x112);
    pub const BTN_RIGHT: (u16, u16) = (0x01, 0x111);
    pub const REL_X: (u16, u16) = (0x02, 0x00);
    pub const REL_Y: (u16, u16) = (0x02, 0x01);
    pub const REL_WHEEL: (u16, u16) = (0x02, 0x08);
}

/// Gamepad axis remapping: browser W3C Standard Gamepad → Xbox 360 uinput.
///
/// Browser axes: 0=LX, 1=LY, 2=RX, 3=RY
/// C bridge:     0=ABS_X, 1=ABS_Y, 2=ABS_Z, 3=ABS_RX, 4=ABS_RY, 5=ABS_RZ
pub fn remap_gamepad_axis(browser_axis: u8) -> u8 {
    match browser_axis {
        0 => 0, // LX → ABS_X
        1 => 1, // LY → ABS_Y
        2 => 3, // RX → ABS_RX
        3 => 4, // RY → ABS_RY
        n => n, // pass-through (triggers, etc.)
    }
}

/// Convert browser axis float (-1.0..1.0) to i16 (-32768..32767).
pub fn axis_float_to_i16(value: f64) -> i16 {
    (value * 32767.0).clamp(-32768.0, 32767.0) as i16
}

/// Gamepad fast-path packet format for the C bridge.
///
/// 4-byte little-endian DGRAM: [type: u8, id: u8, value: i16]
/// Type 1 = button, Type 2 = axis
pub fn pack_gamepad_button(btn_num: u8, btn_val: i16) -> [u8; 4] {
    let val_bytes = btn_val.to_le_bytes();
    [1, btn_num, val_bytes[0], val_bytes[1]]
}

pub fn pack_gamepad_axis(axis_num: u8, axis_val: i16) -> [u8; 4] {
    let val_bytes = axis_val.to_le_bytes();
    [2, axis_num, val_bytes[0], val_bytes[1]]
}

/// Validate and normalize a resolution string "WxH".
///
/// Ensures both dimensions are divisible by 2 (required by video encoders).
/// Returns None if the format is invalid.
pub fn validate_resolution(res: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = res.split('x').collect();
    if parts.len() != 2 {
        return None;
    }
    let w: u32 = parts[0].parse().ok()?;
    let h: u32 = parts[1].parse().ok()?;
    // Round up to nearest even number
    Some((w + w % 2, h + h % 2))
}

/// Format a validated resolution back to "WxH" string.
pub fn format_resolution(w: u32, h: u32) -> String {
    format!("{w}x{h}")
}

/// Validate a scaling ratio string.
pub fn validate_scale(scale_str: &str) -> Option<f64> {
    let val: f64 = scale_str.parse().ok()?;
    if val > 0.0 { Some(val) } else { None }
}

/// Trait for input injection backends.
///
/// Implementations will provide X11 (via x11rb), uinput, or Wayland injection.
pub trait InputInjector: Send + Sync {
    /// Send a key press or release.
    fn send_keypress(&self, keysym: u32, down: bool);
    /// Reset all held keys.
    fn reset_keyboard(&self);
    /// Send a mouse event (absolute or relative position, buttons, scroll).
    fn send_mouse(&self, x: i32, y: i32, button_mask: u32, scroll: i32, relative: bool);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remap_gamepad_axis() {
        assert_eq!(remap_gamepad_axis(0), 0); // LX → ABS_X
        assert_eq!(remap_gamepad_axis(1), 1); // LY → ABS_Y
        assert_eq!(remap_gamepad_axis(2), 3); // RX → ABS_RX
        assert_eq!(remap_gamepad_axis(3), 4); // RY → ABS_RY
        assert_eq!(remap_gamepad_axis(5), 5); // pass-through
    }

    #[test]
    fn test_axis_float_to_i16() {
        assert_eq!(axis_float_to_i16(1.0), 32767);
        assert_eq!(axis_float_to_i16(-1.0), -32767);
        assert_eq!(axis_float_to_i16(0.0), 0);
        assert_eq!(axis_float_to_i16(0.5), 16383);
        // Clamp beyond range
        assert_eq!(axis_float_to_i16(2.0), 32767);
        assert_eq!(axis_float_to_i16(-2.0), -32768);
    }

    #[test]
    fn test_pack_gamepad_button() {
        let pkt = pack_gamepad_button(3, 1);
        assert_eq!(pkt[0], 1); // type = button
        assert_eq!(pkt[1], 3); // btn_num
        assert_eq!(i16::from_le_bytes([pkt[2], pkt[3]]), 1); // value
    }

    #[test]
    fn test_pack_gamepad_axis() {
        let pkt = pack_gamepad_axis(2, -16384);
        assert_eq!(pkt[0], 2); // type = axis
        assert_eq!(pkt[1], 2); // axis_num
        assert_eq!(i16::from_le_bytes([pkt[2], pkt[3]]), -16384);
    }

    #[test]
    fn test_validate_resolution() {
        assert_eq!(validate_resolution("1920x1080"), Some((1920, 1080)));
        assert_eq!(validate_resolution("1921x1081"), Some((1922, 1082))); // round up to even
        assert_eq!(validate_resolution("invalid"), None);
        assert_eq!(validate_resolution("1920"), None);
        assert_eq!(validate_resolution("axb"), None);
    }

    #[test]
    fn test_validate_scale() {
        assert_eq!(validate_scale("1.5"), Some(1.5));
        assert_eq!(validate_scale("1"), Some(1.0));
        assert_eq!(validate_scale("0"), None); // zero not valid
        assert_eq!(validate_scale("abc"), None);
    }

    #[test]
    fn test_format_resolution() {
        assert_eq!(format_resolution(1920, 1080), "1920x1080");
    }
}
