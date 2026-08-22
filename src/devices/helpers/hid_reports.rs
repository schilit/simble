// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standard USB HID Report Descriptors, Keycodes, and ASCII converters.

/// Standard 8-byte USB Keyboard Report Descriptor.
pub const KEYBOARD_REPORT_MAP: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0xE0, //   Usage Minimum (224)
    0x29, 0xE7, //   Usage Maximum (231)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute) - Modifier byte
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Constant) - Reserved byte
    0x95, 0x05, //   Report Count (5)
    0x75, 0x01, //   Report Size (1)
    0x05, 0x08, //   Usage Page (LEDs)
    0x19, 0x01, //   Usage Minimum (1)
    0x29, 0x05, //   Usage Maximum (5)
    0x91, 0x02, //   Output (Data, Variable, Absolute) - LED Report
    0x95, 0x01, //   Report Count (1)
    0x75, 0x03, //   Report Size (3)
    0x91, 0x01, //   Output (Constant) - LED padding
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x81, 0x00, //   Input (Data, Array) - Key arrays (up to 6 keys)
    0xC0, // End Collection
];

/// Standard 4-byte USB Mouse Report Descriptor (Buttons, X, Y, Wheel).
pub const MOUSE_REPORT_MAP: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x01, //   Usage (Pointer)
    0xA1, 0x00, //   Collection (Physical)
    0x05, 0x09, //     Usage Page (Buttons)
    0x19, 0x01, //     Usage Minimum (1)
    0x29, 0x03, //     Usage Maximum (3)
    0x15, 0x00, //     Logical Minimum (0)
    0x25, 0x01, //     Logical Maximum (1)
    0x75, 0x01, //     Report Size (1)
    0x95, 0x03, //     Report Count (3)
    0x81, 0x02, //     Input (Data, Variable, Absolute) - 3 Button bits
    0x75, 0x05, //     Report Size (5)
    0x95, 0x01, //     Report Count (1)
    0x81, 0x01, //     Input (Constant) - 5-bit padding
    0x05, 0x01, //     Usage Page (Generic Desktop)
    0x09, 0x30, //     Usage (X)
    0x09, 0x31, //     Usage (Y)
    0x09, 0x38, //     Usage (Wheel)
    0x15, 0x81, //     Logical Minimum (-127)
    0x25, 0x7F, //     Logical Maximum (127)
    0x75, 0x08, //     Report Size (8)
    0x95, 0x03, //     Report Count (3)
    0x81, 0x06, //     Input (Data, Variable, Relative) - X, Y, Wheel
    0xC0, //   End Collection
    0xC0, // End Collection
];

/// HID Modifier Keys bitmask.
pub mod modifier {
    /// No modifier keys held.
    pub const NONE: u8 = 0x00;
    /// Left Control.
    pub const LCTRL: u8 = 0x01;
    /// Left Shift.
    pub const LSHIFT: u8 = 0x02;
    /// Left Alt.
    pub const LALT: u8 = 0x04;
    /// Left GUI/Meta (Windows/Command key).
    pub const LMETA: u8 = 0x08;
    /// Right Control.
    pub const RCTRL: u8 = 0x10;
    /// Right Shift.
    pub const RSHIFT: u8 = 0x20;
    /// Right Alt.
    pub const RALT: u8 = 0x40;
    /// Right GUI/Meta (Windows/Command key).
    pub const RMETA: u8 = 0x80;
}

/// Standard USB HID Mouse Buttons bitmask.
pub mod mouse_button {
    /// No buttons pressed.
    pub const NONE: u8 = 0x00;
    /// Left button.
    pub const LEFT: u8 = 0x01;
    /// Right button.
    pub const RIGHT: u8 = 0x02;
    /// Middle button.
    pub const MIDDLE: u8 = 0x04;
}

/// Standard USB HID Keycodes.
pub mod keycode {
    /// No key.
    pub const KEY_NONE: u8 = 0x00;
    /// The 'A' key.
    pub const KEY_A: u8 = 0x04;
    /// The 'B' key.
    pub const KEY_B: u8 = 0x05;
    /// The 'C' key.
    pub const KEY_C: u8 = 0x06;
    /// The 'D' key.
    pub const KEY_D: u8 = 0x07;
    /// The 'E' key.
    pub const KEY_E: u8 = 0x08;
    /// The 'F' key.
    pub const KEY_F: u8 = 0x09;
    /// The 'G' key.
    pub const KEY_G: u8 = 0x0A;
    /// The 'H' key.
    pub const KEY_H: u8 = 0x0B;
    /// The 'I' key.
    pub const KEY_I: u8 = 0x0C;
    /// The 'J' key.
    pub const KEY_J: u8 = 0x0D;
    /// The 'K' key.
    pub const KEY_K: u8 = 0x0E;
    /// The 'L' key.
    pub const KEY_L: u8 = 0x0F;
    /// The 'M' key.
    pub const KEY_M: u8 = 0x10;
    /// The 'N' key.
    pub const KEY_N: u8 = 0x11;
    /// The 'O' key.
    pub const KEY_O: u8 = 0x12;
    /// The 'P' key.
    pub const KEY_P: u8 = 0x13;
    /// The 'Q' key.
    pub const KEY_Q: u8 = 0x14;
    /// The 'R' key.
    pub const KEY_R: u8 = 0x15;
    /// The 'S' key.
    pub const KEY_S: u8 = 0x16;
    /// The 'T' key.
    pub const KEY_T: u8 = 0x17;
    /// The 'U' key.
    pub const KEY_U: u8 = 0x18;
    /// The 'V' key.
    pub const KEY_V: u8 = 0x19;
    /// The 'W' key.
    pub const KEY_W: u8 = 0x1A;
    /// The 'X' key.
    pub const KEY_X: u8 = 0x1B;
    /// The 'Y' key.
    pub const KEY_Y: u8 = 0x1C;
    /// The 'Z' key.
    pub const KEY_Z: u8 = 0x1D;

    /// The '1' key.
    pub const KEY_1: u8 = 0x1E;
    /// The '2' key.
    pub const KEY_2: u8 = 0x1F;
    /// The '3' key.
    pub const KEY_3: u8 = 0x20;
    /// The '4' key.
    pub const KEY_4: u8 = 0x21;
    /// The '5' key.
    pub const KEY_5: u8 = 0x22;
    /// The '6' key.
    pub const KEY_6: u8 = 0x23;
    /// The '7' key.
    pub const KEY_7: u8 = 0x24;
    /// The '8' key.
    pub const KEY_8: u8 = 0x25;
    /// The '9' key.
    pub const KEY_9: u8 = 0x26;
    /// The '0' key.
    pub const KEY_0: u8 = 0x27;

    /// Enter/Return key.
    pub const KEY_ENTER: u8 = 0x28;
    /// Escape key.
    pub const KEY_ESCAPE: u8 = 0x29;
    /// Backspace key.
    pub const KEY_BACKSPACE: u8 = 0x2A;
    /// Tab key.
    pub const KEY_TAB: u8 = 0x2B;
    /// Spacebar.
    pub const KEY_SPACE: u8 = 0x2C;

    /// `-` and `_`.
    pub const KEY_MINUS: u8 = 0x2D;
    /// `=` and `+`.
    pub const KEY_EQUAL: u8 = 0x2E;
    /// `[` and `{`.
    pub const KEY_LEFT_BRACKET: u8 = 0x2F;
    /// `]` and `}`.
    pub const KEY_RIGHT_BRACKET: u8 = 0x30;
    /// `\` and `|`.
    pub const KEY_BACKSLASH: u8 = 0x31;
    /// Non-US `#` and `~`.
    pub const KEY_NON_US_HASH: u8 = 0x32;
    /// `;` and `:`.
    pub const KEY_SEMICOLON: u8 = 0x33;
    /// `'` and `"`.
    pub const KEY_APOSTROPHE: u8 = 0x34;
    /// `` ` `` and `~`.
    pub const KEY_GRAVE: u8 = 0x35;
    /// `,` and `<`.
    pub const KEY_COMMA: u8 = 0x36;
    /// `.` and `>`.
    pub const KEY_PERIOD: u8 = 0x37;
    /// `/` and `?`.
    pub const KEY_SLASH: u8 = 0x38;
    /// Caps Lock.
    pub const KEY_CAPS_LOCK: u8 = 0x39;

    /// Right Arrow.
    pub const KEY_RIGHT_ARROW: u8 = 0x4F;
    /// Left Arrow.
    pub const KEY_LEFT_ARROW: u8 = 0x50;
    /// Down Arrow.
    pub const KEY_DOWN_ARROW: u8 = 0x51;
    /// Up Arrow.
    pub const KEY_UP_ARROW: u8 = 0x52;

    /// The largest usage ID that is not a real key.
    ///
    /// Usages 0x01–0x03 are the status codes ErrorRollOver, POSTFail and
    /// ErrorUndefined (Usage Tables 1.12, Section 10). A host that treats them
    /// as keystrokes types garbage the moment a keyboard's matrix ghosts, so
    /// every decode path here filters them out.
    pub const LAST_RESERVED: u8 = 0x03;
}

/// The US layout's punctuation keys, as (usage, unshifted, shifted).
///
/// The pairing is the layout's, not HID's: a usage ID names a *key position*
/// ("Keyboard - and _"), and only a keyboard layout says which character that
/// position produces. This table is the US English one that
/// [`KEYBOARD_REPORT_MAP`] implies.
const US_PUNCTUATION: &[(u8, char, char)] = &[
    (keycode::KEY_MINUS, '-', '_'),
    (keycode::KEY_EQUAL, '=', '+'),
    (keycode::KEY_LEFT_BRACKET, '[', '{'),
    (keycode::KEY_RIGHT_BRACKET, ']', '}'),
    (keycode::KEY_BACKSLASH, '\\', '|'),
    (keycode::KEY_SEMICOLON, ';', ':'),
    (keycode::KEY_APOSTROPHE, '\'', '"'),
    (keycode::KEY_GRAVE, '`', '~'),
    (keycode::KEY_COMMA, ',', '<'),
    (keycode::KEY_PERIOD, '.', '>'),
    (keycode::KEY_SLASH, '/', '?'),
];

/// The shifted characters of the number row, `1` through `9` then `0`.
const US_SHIFTED_DIGITS: [char; 10] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')'];

/// True if either Shift is held in a report's modifier byte.
pub fn shift_held(modifiers: u8) -> bool {
    modifiers & (modifier::LSHIFT | modifier::RSHIFT) != 0
}

/// Converts an ASCII character into an (HID Modifier, HID Keycode) pair.
pub fn ascii_to_hid(c: char) -> Option<(u8, u8)> {
    match c {
        'a'..='z' => {
            let offset = c as u8 - b'a';
            Some((modifier::NONE, keycode::KEY_A + offset))
        }
        'A'..='Z' => {
            let offset = c as u8 - b'A';
            Some((modifier::LSHIFT, keycode::KEY_A + offset))
        }
        '1'..='9' => {
            let offset = c as u8 - b'1';
            Some((modifier::NONE, keycode::KEY_1 + offset))
        }
        '0' => Some((modifier::NONE, keycode::KEY_0)),
        ' ' => Some((modifier::NONE, keycode::KEY_SPACE)),
        '\n' | '\r' => Some((modifier::NONE, keycode::KEY_ENTER)),
        '\t' => Some((modifier::NONE, keycode::KEY_TAB)),
        _ => US_PUNCTUATION
            .iter()
            .find_map(|&(usage, plain, shifted)| {
                if c == plain {
                    Some((modifier::NONE, usage))
                } else if c == shifted {
                    Some((modifier::LSHIFT, usage))
                } else {
                    None
                }
            })
            .or_else(|| {
                let index = US_SHIFTED_DIGITS.iter().position(|&s| s == c)? as u8;
                // The row reads 1..9 then 0, so index 9 is the zero key.
                let usage = if index == 9 {
                    keycode::KEY_0
                } else {
                    keycode::KEY_1 + index
                };
                Some((modifier::LSHIFT, usage))
            }),
    }
}

/// Converts an (HID usage ID, modifier byte) pair back into the character a US
/// keyboard would produce — the inverse of [`ascii_to_hid`], and the mapping a
/// HID *host* needs.
///
/// Returns `None` for keys that produce no character (Escape, Backspace, the
/// arrows, the modifiers themselves). Caps Lock is deliberately not applied:
/// its state lives in the host's LED output report, not in any input report,
/// so a decoder that guessed at it would be wrong half the time.
pub fn hid_to_ascii(usage: u8, modifiers: u8) -> Option<char> {
    let shifted = shift_held(modifiers);
    match usage {
        keycode::KEY_A..=keycode::KEY_Z => {
            let offset = usage - keycode::KEY_A;
            Some(if shifted {
                (b'A' + offset) as char
            } else {
                (b'a' + offset) as char
            })
        }
        keycode::KEY_1..=keycode::KEY_9 => {
            let offset = usage - keycode::KEY_1;
            Some(if shifted {
                US_SHIFTED_DIGITS[offset as usize]
            } else {
                (b'1' + offset) as char
            })
        }
        keycode::KEY_0 => Some(if shifted { ')' } else { '0' }),
        keycode::KEY_SPACE => Some(' '),
        keycode::KEY_ENTER => Some('\n'),
        keycode::KEY_TAB => Some('\t'),
        _ => US_PUNCTUATION
            .iter()
            .find(|&&(key, _, _)| key == usage)
            .map(|&(_, plain, shift)| if shifted { shift } else { plain }),
    }
}

/// A short human name for a usage ID, for showing a key that produces no
/// character (Usage Tables 1.12, Section 10).
pub fn usage_label(usage: u8) -> Option<&'static str> {
    Some(match usage {
        keycode::KEY_ENTER => "Enter",
        keycode::KEY_ESCAPE => "Esc",
        keycode::KEY_BACKSPACE => "Backspace",
        keycode::KEY_TAB => "Tab",
        keycode::KEY_SPACE => "Space",
        keycode::KEY_CAPS_LOCK => "Caps Lock",
        keycode::KEY_RIGHT_ARROW => "Right",
        keycode::KEY_LEFT_ARROW => "Left",
        keycode::KEY_DOWN_ARROW => "Down",
        keycode::KEY_UP_ARROW => "Up",
        0xE0 => "Left Ctrl",
        0xE1 => "Left Shift",
        0xE2 => "Left Alt",
        0xE3 => "Left Meta",
        0xE4 => "Right Ctrl",
        0xE5 => "Right Shift",
        0xE6 => "Right Alt",
        0xE7 => "Right Meta",
        _ => return None,
    })
}

/// Length of a boot-protocol keyboard input report.
pub const KEYBOARD_REPORT_LEN: usize = 8;

/// A decoded boot-protocol keyboard input report: modifier bitmap, one
/// reserved byte, then up to six concurrently-held key usages.
///
/// This is the layout the Boot Keyboard Input Report (0x2A22) mandates and the
/// one [`KEYBOARD_REPORT_MAP`] declares, so a HOGP keyboard in either protocol
/// mode sends it. The report carries **the keys currently held**, not an
/// event: a host derives presses and releases by differencing consecutive
/// reports, which is what [`Self::newly_pressed`] and [`Self::released`] do.
///
/// The bytes arrive with no Report ID prefix. HOGP puts the report's ID in the
/// Report Reference descriptor (0x2908) on the characteristic rather than in
/// the value, and these report maps declare no Report ID item at all, so the
/// notification payload is the bare report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyboardReport {
    /// Which modifier keys are held (see [`modifier`]).
    pub modifiers: u8,
    /// The usage IDs of the keys held, zero-padded.
    pub keys: [u8; 6],
}

impl KeyboardReport {
    /// Decodes one input report, or `None` if it is not 8 bytes long.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != KEYBOARD_REPORT_LEN {
            return None;
        }
        let mut keys = [0u8; 6];
        keys.copy_from_slice(&bytes[2..8]);
        Some(Self {
            modifiers: bytes[0],
            keys,
        })
    }

    /// Re-encodes the report, byte 1 being the reserved (always zero) octet.
    pub fn to_bytes(self) -> [u8; KEYBOARD_REPORT_LEN] {
        let mut bytes = [0u8; KEYBOARD_REPORT_LEN];
        bytes[0] = self.modifiers;
        bytes[2..8].copy_from_slice(&self.keys);
        bytes
    }

    /// True when the keyboard is reporting more keys held than it can
    /// distinguish: every slot carries ErrorRollOver (0x01). The held keys are
    /// unknowable, so a host must ignore the report rather than diff it.
    pub fn is_rollover_error(&self) -> bool {
        self.keys.iter().all(|&k| k == 0x01)
    }

    /// True if `usage` is one of the keys this report says is held.
    pub fn holds(&self, usage: u8) -> bool {
        self.keys.contains(&usage)
    }

    /// The keys held now that were not held in `previous`.
    ///
    /// Without this set difference a host repeats a character for every report
    /// that arrives while a key stays down — a keyboard sends the same report
    /// again on every unrelated change, such as a second key going down.
    pub fn newly_pressed(&self, previous: &Self) -> Vec<u8> {
        self.keys
            .iter()
            .copied()
            .filter(|&k| k > keycode::LAST_RESERVED && !previous.holds(k))
            .collect()
    }

    /// The keys held in `previous` that this report no longer lists.
    pub fn released(&self, previous: &Self) -> Vec<u8> {
        previous
            .keys
            .iter()
            .copied()
            .filter(|&k| k > keycode::LAST_RESERVED && !self.holds(k))
            .collect()
    }
}

/// A decoded mouse input report: a button bitmap and relative motion.
///
/// `dx`, `dy` and `wheel` are **signed relative** displacements, which is why
/// [`MOUSE_REPORT_MAP`] declares Logical Minimum -127: read as unsigned, a
/// leftward move of one unit becomes a jump of 255 to the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MouseReport {
    /// Button bitmap (see [`mouse_button`]).
    pub buttons: u8,
    /// Horizontal displacement since the last report, right positive.
    pub dx: i8,
    /// Vertical displacement since the last report, down positive.
    pub dy: i8,
    /// Wheel detents since the last report, away-from-user positive.
    pub wheel: i8,
}

impl MouseReport {
    /// Decodes a `[buttons, dx, dy]` or `[buttons, dx, dy, wheel]` report.
    ///
    /// Both lengths are accepted because both descriptors are common: the
    /// wheel is an optional axis, and a host that rejected the three-byte form
    /// would ignore every wheel-less mouse.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 3 {
            return None;
        }
        Some(Self {
            buttons: bytes[0],
            dx: bytes[1] as i8,
            dy: bytes[2] as i8,
            wheel: bytes.get(3).map_or(0, |&w| w as i8),
        })
    }

    /// Re-encodes the report in the four-byte (wheeled) form.
    pub fn to_bytes(self) -> [u8; 4] {
        [
            self.buttons,
            self.dx as u8,
            self.dy as u8,
            self.wheel as u8,
        ]
    }

    /// Buttons down now that were up in `previous_buttons`.
    pub fn newly_pressed(&self, previous_buttons: u8) -> u8 {
        self.buttons & !previous_buttons
    }

    /// Buttons up now that were down in `previous_buttons`.
    pub fn released(&self, previous_buttons: u8) -> u8 {
        previous_buttons & !self.buttons
    }
}

/// The (Usage Page, Usage) of a report descriptor's first Application
/// Collection — how a host learns that a device is a keyboard rather than a
/// mouse, before a single report arrives.
///
/// Walks HID short items (Device Class Definition for HID 1.11, Section 6.2.2):
/// each item's prefix byte carries a 2-bit size (0, 1, 2 or 4 data bytes), a
/// 2-bit type and a 4-bit tag. Returns `None` for a descriptor with no
/// Application Collection, and skips long items (prefix 0xFE), which no
/// keyboard or mouse descriptor uses.
pub fn top_level_usage(report_map: &[u8]) -> Option<(u16, u16)> {
    const LONG_ITEM: u8 = 0xFE;
    let mut usage_page: u16 = 0;
    let mut usage: Option<u16> = None;
    let mut i = 0;
    while i < report_map.len() {
        let prefix = report_map[i];
        i += 1;
        if prefix == LONG_ITEM {
            // Long item: [0xFE, data size, tag, data...].
            let size = *report_map.get(i)? as usize;
            i += size + 2;
            continue;
        }
        let size = match prefix & 0x03 {
            3 => 4,
            n => n as usize,
        };
        let data = report_map.get(i..i + size)?;
        i += size;
        let value = data
            .iter()
            .enumerate()
            .fold(0u32, |acc, (n, &b)| acc | (b as u32) << (8 * n));
        match prefix & 0xFC {
            // Global, tag 0: Usage Page.
            0x04 => usage_page = value as u16,
            // Local, tag 0: Usage. Only the first one, which names the
            // collection, matters here.
            0x08 => usage = usage.or(Some(value as u16)),
            // Main, tag 10: Collection. Data 0x01 is Application.
            0xA0 if value == 0x01 => return Some((usage_page, usage?)),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The usage IDs asserted here are read off the USB HID Usage Tables 1.12
    /// Keyboard/Keypad Page (0x07), not off `ascii_to_hid`. A round trip
    /// through this module's own encoder would agree with any consistent
    /// mistake; a real host would not.
    #[test]
    fn test_usage_ids_match_the_published_keyboard_page() {
        assert_eq!(ascii_to_hid('a'), Some((modifier::NONE, 0x04)));
        assert_eq!(ascii_to_hid('z'), Some((modifier::NONE, 0x1D)));
        assert_eq!(ascii_to_hid('1'), Some((modifier::NONE, 0x1E)));
        assert_eq!(ascii_to_hid('9'), Some((modifier::NONE, 0x26)));
        // Zero sits *after* nine, not before one.
        assert_eq!(ascii_to_hid('0'), Some((modifier::NONE, 0x27)));
        assert_eq!(ascii_to_hid('\n'), Some((modifier::NONE, 0x28)));
        assert_eq!(ascii_to_hid(' '), Some((modifier::NONE, 0x2C)));
        assert_eq!(ascii_to_hid('-'), Some((modifier::NONE, 0x2D)));
        assert_eq!(ascii_to_hid('\\'), Some((modifier::NONE, 0x31)));
        // 0x32 is the non-US "# and ~" key; ';' is 0x33, one later.
        assert_eq!(ascii_to_hid(';'), Some((modifier::NONE, 0x33)));
        assert_eq!(ascii_to_hid('`'), Some((modifier::NONE, 0x35)));
        assert_eq!(ascii_to_hid('/'), Some((modifier::NONE, 0x38)));
        assert_eq!(hid_to_ascii(0x04, modifier::LSHIFT), Some('A'));
        assert_eq!(hid_to_ascii(0x1F, modifier::LSHIFT), Some('@'));
        assert_eq!(hid_to_ascii(0x27, modifier::LSHIFT), Some(')'));
        assert_eq!(hid_to_ascii(0x37, modifier::RSHIFT), Some('>'));
    }

    #[test]
    fn test_every_printable_ascii_character_survives_a_round_trip() {
        for c in ' '..='~' {
            let (modifiers, usage) = ascii_to_hid(c).unwrap_or_else(|| panic!("no usage for {c:?}"));
            assert_eq!(hid_to_ascii(usage, modifiers), Some(c), "round trip {c:?}");
        }
    }

    #[test]
    fn test_keys_that_produce_no_character_decode_to_none() {
        assert_eq!(hid_to_ascii(keycode::KEY_ESCAPE, modifier::NONE), None);
        assert_eq!(hid_to_ascii(keycode::KEY_BACKSPACE, modifier::NONE), None);
        assert_eq!(hid_to_ascii(keycode::KEY_UP_ARROW, modifier::NONE), None);
        assert_eq!(usage_label(keycode::KEY_BACKSPACE), Some("Backspace"));
        assert_eq!(usage_label(keycode::KEY_A), None);
    }

    #[test]
    fn test_a_held_key_is_pressed_once_however_many_reports_repeat_it() {
        // Hold 'a', then press 'b' while 'a' is still down. The second report
        // still lists 'a', and a host that decoded the report rather than the
        // change would type "aab".
        let idle = KeyboardReport::default();
        let a_down = KeyboardReport::parse(&[0, 0, 0x04, 0, 0, 0, 0, 0]).unwrap();
        let ab_down = KeyboardReport::parse(&[0, 0, 0x04, 0x05, 0, 0, 0, 0]).unwrap();

        assert_eq!(a_down.newly_pressed(&idle), vec![0x04]);
        assert_eq!(ab_down.newly_pressed(&a_down), vec![0x05]);
        assert!(ab_down.released(&a_down).is_empty());
        assert_eq!(idle.released(&ab_down), vec![0x04, 0x05]);
    }

    #[test]
    fn test_the_rollover_status_code_is_not_a_keystroke() {
        let ghosted = KeyboardReport::parse(&[0, 0, 1, 1, 1, 1, 1, 1]).unwrap();
        assert!(ghosted.is_rollover_error());
        assert!(
            ghosted.newly_pressed(&KeyboardReport::default()).is_empty(),
            "0x01 is ErrorRollOver, not a key"
        );
    }

    #[test]
    fn test_a_keyboard_report_must_be_eight_bytes() {
        assert!(KeyboardReport::parse(&[0, 0, 0x04]).is_none());
        assert!(KeyboardReport::parse(&[0; 9]).is_none());
        let report = KeyboardReport::parse(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(report.modifiers, modifier::LSHIFT);
        assert_eq!(report.to_bytes(), [0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn test_mouse_motion_is_signed() {
        // 0xFB is -5, not 251: the descriptor's Logical Minimum is -127.
        let report = MouseReport::parse(&[0x01, 0xFB, 0x05, 0xFF]).unwrap();
        assert_eq!(report.dx, -5);
        assert_eq!(report.dy, 5);
        assert_eq!(report.wheel, -1);
        assert_eq!(report.buttons, mouse_button::LEFT);
        // A wheel-less mouse sends three bytes and still decodes.
        assert_eq!(MouseReport::parse(&[0x00, 0x01, 0x02]).unwrap().wheel, 0);
        assert!(MouseReport::parse(&[0x00, 0x01]).is_none());
    }

    #[test]
    fn test_mouse_buttons_are_edges_not_levels() {
        let held = MouseReport::parse(&[mouse_button::LEFT, 0, 0]).unwrap();
        assert_eq!(held.newly_pressed(mouse_button::NONE), mouse_button::LEFT);
        assert_eq!(
            held.newly_pressed(mouse_button::LEFT),
            mouse_button::NONE,
            "still held is not pressed again"
        );
        assert_eq!(held.released(mouse_button::RIGHT), mouse_button::RIGHT);
    }

    /// The descriptors this crate ships are the oracle here: a walker that
    /// mis-sizes an item drifts and reports the wrong usage.
    #[test]
    fn test_the_report_map_says_which_device_this_is() {
        // Generic Desktop (0x01), Keyboard (0x06) / Mouse (0x02).
        assert_eq!(top_level_usage(KEYBOARD_REPORT_MAP), Some((0x01, 0x06)));
        assert_eq!(top_level_usage(MOUSE_REPORT_MAP), Some((0x01, 0x02)));
        assert_eq!(top_level_usage(&[]), None);
        // A truncated descriptor must not panic or read past the end.
        assert_eq!(top_level_usage(&KEYBOARD_REPORT_MAP[..3]), None);
    }
}
