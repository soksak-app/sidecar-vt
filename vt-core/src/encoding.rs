//! Pure functions that encode keyboard, mouse, and composition input into bytes written to the PTY.
//!
//! A common layer that terminal engines do not need to reimplement. It converts the logical input
//! received from the page into standard VT sequences.

use crate::Modes;

/// Enum of the keys that an input device can send. Each key must be encoded as a standard VT
/// sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    // Cursor keys
    Up,
    Down,
    Left,
    Right,
    // Navigation keys
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    // Function keys
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    // Special keys
    Enter,
    Tab,
    Backspace,
    Escape,
    /// Character key of the numeric keypad: `0`–`9`, `.`, `+`, `-`, `*`, `/`, `=`.
    Keypad(char),
    /// Enter key of the numeric keypad.
    KeypadEnter,
}

/// Button of a mouse report. None is a pointer that moved without a pressed button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    None,
    WheelUp,
    WheelDown,
}

/// Action of a mouse report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// One mouse report. Cell columns and rows are counted from 0.
#[derive(Debug, Clone, Copy)]
pub struct MouseReport {
    pub button: MouseButton,
    pub action: MouseAction,
    pub col: u16,
    pub row: u16,
    pub alt: bool,
    pub ctrl: bool,
}

/// Key encoding error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// Unsupported key or input.
    Unsupported,
    /// Other error.
    Other(String),
}

/// Modifier flags (a combination of shift, alt, and ctrl). Each bit: shift=1, alt=2, ctrl=4
pub type Modifiers = u8;

/// Encodes a key into bytes written to the PTY.
///
/// # Encoding rules
/// - Cursor keys (`Up`/`Down`/`Right`/`Left`): normally `ESC [ A|B|C|D`, and
///   `ESC O A|B|C|D` in app cursor mode.
/// - `Home`/`End`: `ESC [ H` / `ESC [ F` (`ESC O H` / `ESC O F` in app cursor mode).
/// - `Insert`/`Delete`/`PageUp`/`PageDown`: `ESC [ 2~` / `ESC [ 3~` / `ESC [ 5~` / `ESC [ 6~`.
/// - `F1..F12`: VT convention (F1–F4 are `ESC O P|Q|R|S`, F5 is `ESC [ 15~`, and so on).
/// - When a modifier (shift=1, alt=2, ctrl=4) is present, the form is `ESC [ 1 ; <n> A`
///   (cursor keys) or `ESC [ 2 ; <n> ~` (tilde-family keys).
/// - `Enter` → `\r`, `Tab` → `\t`, `Backspace` → `\x7f`, `Escape` → `\x1b`.
/// - Character-based keys (`A`–`Z`, `0`–`9`, and so on) are not passed to this function.
///   Encode characters directly with `encode_text()`.
///
/// # Unsupported
/// - **Application keypad mode**: when `modes.app_keypad` is on, keypad keys without a
///   modifier become `ESC O` sequences. Number keys are sent unchanged. Planned for future
///   extension.
///
/// # Errors An unknown key or an input that cannot be matched returns `Err(Unsupported)`. Errors
/// are not silently discarded; the caller handles them.
pub fn encode_key(key: Key, modifiers: Modifiers, modes: &Modes) -> Result<Vec<u8>, EncodeError> {
    match key {
        // Cursor keys
        Key::Up => {
            if modifiers == 0 {
                if modes.app_cursor {
                    Ok(b"\x1bOA".to_vec())
                } else {
                    Ok(b"\x1b[A".to_vec())
                }
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}A", n).into_bytes())
            }
        }
        Key::Down => {
            if modifiers == 0 {
                if modes.app_cursor {
                    Ok(b"\x1bOB".to_vec())
                } else {
                    Ok(b"\x1b[B".to_vec())
                }
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}B", n).into_bytes())
            }
        }
        Key::Right => {
            if modifiers == 0 {
                if modes.app_cursor {
                    Ok(b"\x1bOC".to_vec())
                } else {
                    Ok(b"\x1b[C".to_vec())
                }
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}C", n).into_bytes())
            }
        }
        Key::Left => {
            if modifiers == 0 {
                if modes.app_cursor {
                    Ok(b"\x1bOD".to_vec())
                } else {
                    Ok(b"\x1b[D".to_vec())
                }
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}D", n).into_bytes())
            }
        }

        // Home/End keys
        Key::Home => {
            if modifiers == 0 {
                if modes.app_cursor {
                    Ok(b"\x1bOH".to_vec())
                } else {
                    Ok(b"\x1b[H".to_vec())
                }
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}H", n).into_bytes())
            }
        }
        Key::End => {
            if modifiers == 0 {
                if modes.app_cursor {
                    Ok(b"\x1bOF".to_vec())
                } else {
                    Ok(b"\x1b[F".to_vec())
                }
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}F", n).into_bytes())
            }
        }

        // Tilde-family keys (Insert, Delete, PageUp, PageDown)
        Key::Insert => {
            if modifiers == 0 {
                Ok(b"\x1b[2~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[2;{}~", n).into_bytes())
            }
        }
        Key::Delete => {
            if modifiers == 0 {
                Ok(b"\x1b[3~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[3;{}~", n).into_bytes())
            }
        }
        Key::PageUp => {
            if modifiers == 0 {
                Ok(b"\x1b[5~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[5;{}~", n).into_bytes())
            }
        }
        Key::PageDown => {
            if modifiers == 0 {
                Ok(b"\x1b[6~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[6;{}~", n).into_bytes())
            }
        }

        // Function keys (F1–F12) F1–F4 use SS3 without a modifier; with a modifier, the modifier
        // parameter is carried in CSI.
        Key::F1 | Key::F2 | Key::F3 | Key::F4 => {
            let final_byte = match key {
                Key::F1 => b'P',
                Key::F2 => b'Q',
                Key::F3 => b'R',
                _ => b'S',
            };
            if modifiers == 0 {
                Ok(vec![0x1b, b'O', final_byte])
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[1;{}{}", n, final_byte as char).into_bytes())
            }
        }
        Key::F5 => {
            if modifiers == 0 {
                Ok(b"\x1b[15~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[15;{}~", n).into_bytes())
            }
        }
        Key::F6 => {
            if modifiers == 0 {
                Ok(b"\x1b[17~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[17;{}~", n).into_bytes())
            }
        }
        Key::F7 => {
            if modifiers == 0 {
                Ok(b"\x1b[18~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[18;{}~", n).into_bytes())
            }
        }
        Key::F8 => {
            if modifiers == 0 {
                Ok(b"\x1b[19~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[19;{}~", n).into_bytes())
            }
        }
        Key::F9 => {
            if modifiers == 0 {
                Ok(b"\x1b[20~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[20;{}~", n).into_bytes())
            }
        }
        Key::F10 => {
            if modifiers == 0 {
                Ok(b"\x1b[21~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[21;{}~", n).into_bytes())
            }
        }
        Key::F11 => {
            if modifiers == 0 {
                Ok(b"\x1b[23~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[23;{}~", n).into_bytes())
            }
        }
        Key::F12 => {
            if modifiers == 0 {
                Ok(b"\x1b[24~".to_vec())
            } else {
                let n = compute_modifier_param(modifiers);
                Ok(format!("\x1b[24;{}~", n).into_bytes())
            }
        }

        // Special keys
        Key::Enter => Ok(b"\r".to_vec()),
        // Shift+Tab is BackTab (CSI Z). Full-screen programs bind mode switching to this sequence.
        Key::Tab if modifiers & 1 != 0 => Ok(b"\x1b[Z".to_vec()),
        Key::Tab => Ok(b"\t".to_vec()),
        // Alt+Backspace is delete previous word (ESC DEL). readline binds it to backward-kill-word.
        Key::Backspace if modifiers & 2 != 0 => Ok(b"\x1b\x7f".to_vec()),
        Key::Backspace => Ok(b"\x7f".to_vec()),
        Key::Escape => Ok(b"\x1b".to_vec()),
        // In application keypad mode (ESC =), keypad keys without a modifier are SS3 sequences.
        // Otherwise they send their characters.
        Key::Keypad(ch) => {
            let application = match ch {
                '0'..='9' => (b'p' + (ch as u8 - b'0')) as char,
                '.' => 'n',
                '+' => 'k',
                '-' => 'm',
                '*' => 'j',
                '/' => 'o',
                '=' => 'X',
                _ => return Err(EncodeError::Unsupported),
            };
            if modes.app_keypad && modifiers == 0 {
                Ok(format!("\x1bO{application}").into_bytes())
            } else {
                Ok(ch.to_string().into_bytes())
            }
        }
        Key::KeypadEnter => {
            if modes.app_keypad && modifiers == 0 {
                Ok(b"\x1bOM".to_vec())
            } else {
                Ok(b"\r".to_vec())
            }
        }
    }
}

/// Byte encoding for Ctrl. `ctrl+a` → `0x01` through `ctrl+z` → `0x1a`. Also supports special
/// sequences that are not plain letters: `ctrl+[` → `0x1b`, `ctrl+\` → `0x1c`, `ctrl+]` → `0x1d`,
/// `ctrl+space` → `0x00`.
///
/// # Errors Returns `Err(Unsupported)` when `ch` is not a controllable character.
pub fn encode_ctrl_char(ch: char) -> Result<Vec<u8>, EncodeError> {
    match ch {
        'a'..='z' => {
            let code = ch as u8 - b'a' + 1;
            Ok(vec![code])
        }
        'A'..='Z' => {
            let code = ch as u8 - b'A' + 1;
            Ok(vec![code])
        }
        '[' => Ok(vec![0x1b]),  // Ctrl+[
        '\\' => Ok(vec![0x1c]), // Ctrl+\
        ']' => Ok(vec![0x1d]),  // Ctrl+]
        ' ' => Ok(vec![0x00]),  // Ctrl+Space
        _ => Err(EncodeError::Unsupported),
    }
}

/// Encodes text as UTF-8 bytes. Used for text input.
pub fn encode_text(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Alt plus a character. Prepends ESC to the character. Example: `alt+a` → `ESC` followed by the
/// UTF-8 bytes of `a`.
pub fn encode_alt_char(ch: char) -> Vec<u8> {
    let mut result = vec![0x1b]; // ESC
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    result.extend_from_slice(encoded.as_bytes());
    result
}

/// Computes the modifier parameter. Given the bits shift=1, alt=2, ctrl=4, returns (shift+alt+ctrl)
/// + 1. Examples: alt+ctrl → (2+4)+1=7, shift only → (1)+1=2, no modifier → (0)+1=1.
fn compute_modifier_param(modifiers: Modifiers) -> u8 {
    modifiers + 1
}

/// Builds one mouse report in the current encoding. The mode is followed in this order: SGR (1006),
/// UTF-8 (1005), then the default encoding.
///
/// The button value is 0 for left, 3 for no button, and 64/65 for the wheel. Motion adds 32, alt
/// adds 8, and ctrl adds 16. SGR ends a release with `m` and keeps the button. In the default and
/// UTF-8 encodings, a release reports button 3. A cell that the default encoding cannot represent
/// is an error.
pub fn encode_mouse(report: &MouseReport, modes: &Modes) -> Result<Vec<u8>, String> {
    let mut code: u32 = match report.button {
        MouseButton::Left => 0,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    if report.action == MouseAction::Motion {
        code += 32;
    }
    if report.alt {
        code += 8;
    }
    if report.ctrl {
        code += 16;
    }
    let (x, y) = (u32::from(report.col) + 1, u32::from(report.row) + 1);
    if modes.sgr_mouse {
        let end = if report.action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return Ok(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    if report.action == MouseAction::Release {
        code = 3 + (code & (8 | 16));
    }
    if modes.utf8_mouse {
        let mut bytes = b"\x1b[M".to_vec();
        for value in [32 + code, 32 + x, 32 + y] {
            let ch = char::from_u32(value)
                .ok_or_else(|| format!("mouse coordinate {value} cannot be encoded"))?;
            let mut buffer = [0u8; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        }
        return Ok(bytes);
    }
    if x > 223 || y > 223 {
        return Err(format!(
            "mouse cell {},{} cannot be encoded without SGR or UTF-8 mouse mode",
            report.col, report.row
        ));
    }
    Ok(vec![
        0x1b,
        b'[',
        b'M',
        (32 + code) as u8,
        (32 + x) as u8,
        (32 + y) as u8,
    ])
}

/// One wheel-button event at pointer cell (col, row). When `older` is true, the button is 64
/// (toward older output); otherwise it is 65.
pub fn encode_wheel(modes: &Modes, older: bool, col: u16, row: u16) -> Result<Vec<u8>, String> {
    let button = if older {
        MouseButton::WheelUp
    } else {
        MouseButton::WheelDown
    };
    encode_mouse(
        &MouseReport {
            button,
            action: MouseAction::Press,
            col,
            row,
            alt: false,
            ctrl: false,
        },
        modes,
    )
}

/// Encodes paste text.
///
/// # Processing
/// - When `bracketed_paste` mode is on, the output starts with `ESC [ 200 ~` and ends with
///   `ESC [ 201 ~`.
/// - An `ESC [ 201 ~` sequence inside the text is rejected so that it cannot end the wrapping
///   early.
/// - The UTF-8 bytes and line breaks of the text are preserved unchanged.
pub fn encode_paste(text: &str, modes: &Modes) -> Result<Vec<u8>, String> {
    if modes.bracketed_paste && text.contains("\x1b[201~") {
        return Err("paste text contains the bracketed-paste terminator".to_string());
    }
    let mut result = Vec::new();

    if modes.bracketed_paste {
        result.extend_from_slice(b"\x1b[200~");
    }

    result.extend_from_slice(text.as_bytes());

    if modes.bracketed_paste {
        result.extend_from_slice(b"\x1b[201~");
    }

    Ok(result)
}

/// A simple state machine that tracks composition (IME) state. It holds the composing string and
/// returns bytes only at the confirmation point: `add_char` returns `None` while composing, and
/// `confirm` returns the composed bytes once and ends the composition.
///
/// The usage is covered by the unit test `test_composition_state_not_composing`.
#[derive(Debug, Clone, Default)]
pub struct CompositionState {
    buffer: String,
}

impl CompositionState {
    /// Creates a new composition state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a character to the composition.
    /// **Not sent to the PTY**. The caller must draw it on the screen in advance. Returns `None`
    /// (no bytes).
    pub fn add_char(&mut self, ch: char) -> Option<Vec<u8>> {
        self.buffer.push(ch);
        None // No bytes are returned while composing
    }

    /// Cancels the composition.
    pub fn cancel(&mut self) {
        self.buffer.clear();
    }

    /// Confirms the composition. Returns the buffered text as UTF-8 bytes and empties the buffer.
    pub fn confirm(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            None
        } else {
            let bytes = self.buffer.as_bytes().to_vec();
            self.buffer.clear();
            Some(bytes)
        }
    }

    /// Returns the text currently being composed (read-only).
    pub fn current(&self) -> &str {
        &self.buffer
    }

    /// Checks whether a composition is in progress.
    pub fn is_composing(&self) -> bool {
        !self.buffer.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypad_keys_send_ss3_sequences_only_in_application_keypad_mode() {
        let numeric = Modes::default();
        let application = Modes {
            app_keypad: true,
            ..Modes::default()
        };
        for (ch, final_byte) in [
            ('0', 'p'),
            ('5', 'u'),
            ('9', 'y'),
            ('.', 'n'),
            ('+', 'k'),
            ('-', 'm'),
            ('*', 'j'),
            ('/', 'o'),
            ('=', 'X'),
        ] {
            assert_eq!(
                encode_key(Key::Keypad(ch), 0, &application).unwrap(),
                format!("\x1bO{final_byte}").into_bytes()
            );
            assert_eq!(
                encode_key(Key::Keypad(ch), 0, &numeric).unwrap(),
                ch.to_string().into_bytes()
            );
            assert_eq!(
                encode_key(Key::Keypad(ch), 1, &application).unwrap(),
                ch.to_string().into_bytes(),
                "a modified keypad key sends its character"
            );
        }
        assert_eq!(
            encode_key(Key::KeypadEnter, 0, &application).unwrap(),
            b"\x1bOM"
        );
        assert_eq!(encode_key(Key::KeypadEnter, 0, &numeric).unwrap(), b"\r");
        assert!(encode_key(Key::Keypad('a'), 0, &application).is_err());
    }

    fn press(button: MouseButton, action: MouseAction, col: u16, row: u16) -> MouseReport {
        MouseReport {
            button,
            action,
            col,
            row,
            alt: false,
            ctrl: false,
        }
    }

    #[test]
    fn sgr_reports_keep_the_button_on_release_and_add_motion_and_modifier_bits() {
        let modes = Modes {
            mouse_click: true,
            sgr_mouse: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_mouse(&press(MouseButton::Left, MouseAction::Press, 9, 4), &modes).unwrap(),
            b"\x1b[<0;10;5M"
        );
        assert_eq!(
            encode_mouse(
                &press(MouseButton::Left, MouseAction::Release, 9, 4),
                &modes
            )
            .unwrap(),
            b"\x1b[<0;10;5m"
        );
        assert_eq!(
            encode_mouse(&press(MouseButton::Left, MouseAction::Motion, 9, 4), &modes).unwrap(),
            b"\x1b[<32;10;5M"
        );
        assert_eq!(
            encode_mouse(&press(MouseButton::None, MouseAction::Motion, 9, 4), &modes).unwrap(),
            b"\x1b[<35;10;5M"
        );
        let modified = MouseReport {
            alt: true,
            ctrl: true,
            ..press(MouseButton::Left, MouseAction::Press, 0, 0)
        };
        assert_eq!(encode_mouse(&modified, &modes).unwrap(), b"\x1b[<24;1;1M");
    }

    #[test]
    fn default_and_utf8_reports_release_as_button_three() {
        let plain = Modes {
            mouse_click: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_mouse(&press(MouseButton::Left, MouseAction::Press, 9, 4), &plain).unwrap(),
            b"\x1b[M *%"
        );
        assert_eq!(
            encode_mouse(
                &press(MouseButton::Left, MouseAction::Release, 9, 4),
                &plain
            )
            .unwrap(),
            b"\x1b[M#*%"
        );
        let modified = MouseReport {
            ctrl: true,
            ..press(MouseButton::Left, MouseAction::Release, 9, 4)
        };
        assert_eq!(encode_mouse(&modified, &plain).unwrap(), b"\x1b[M3*%");
        assert!(
            encode_mouse(
                &press(MouseButton::Left, MouseAction::Press, 223, 0),
                &plain
            )
            .is_err(),
            "the default encoding cannot represent column 224"
        );
        let utf8 = Modes {
            mouse_click: true,
            utf8_mouse: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_mouse(&press(MouseButton::Left, MouseAction::Press, 99, 4), &utf8).unwrap(),
            "\x1b[M \u{84}%".as_bytes()
        );
    }

    #[test]
    fn wheel_events_follow_the_mouse_encoding() {
        let sgr = Modes {
            mouse_click: true,
            sgr_mouse: true,
            ..Modes::default()
        };
        assert_eq!(encode_wheel(&sgr, true, 4, 6).unwrap(), b"\x1b[<64;5;7M");
        assert_eq!(encode_wheel(&sgr, false, 0, 0).unwrap(), b"\x1b[<65;1;1M");
        let plain = Modes {
            mouse_click: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_wheel(&plain, true, 0, 0).unwrap(),
            vec![0x1b, b'[', b'M', 96, 33, 33]
        );
        assert!(
            encode_wheel(&plain, true, 300, 0).is_err(),
            "the default encoding cannot address column 301"
        );
        let utf8 = Modes {
            mouse_click: true,
            utf8_mouse: true,
            ..Modes::default()
        };
        let encoded = encode_wheel(&utf8, false, 300, 0).unwrap();
        assert_eq!(&encoded[..4], &[0x1b, b'[', b'M', 97]);
        assert_eq!(std::str::from_utf8(&encoded[4..]).unwrap(), "\u{14d}!");
    }

    #[test]
    fn test_cursor_keys_app_cursor_mode() {
        let modes_normal = Modes {
            app_cursor: false,
            app_keypad: false,
            bracketed_paste: false,
            mouse_click: false,
            alt_screen: false,
            ..Default::default()
        };
        let modes_app = Modes {
            app_cursor: true,
            app_keypad: false,
            bracketed_paste: false,
            mouse_click: false,
            alt_screen: false,
            ..Default::default()
        };

        // Normal mode
        assert_eq!(
            encode_key(Key::Up, 0, &modes_normal).unwrap(),
            b"\x1b[A".to_vec()
        );
        assert_eq!(
            encode_key(Key::Down, 0, &modes_normal).unwrap(),
            b"\x1b[B".to_vec()
        );
        assert_eq!(
            encode_key(Key::Right, 0, &modes_normal).unwrap(),
            b"\x1b[C".to_vec()
        );
        assert_eq!(
            encode_key(Key::Left, 0, &modes_normal).unwrap(),
            b"\x1b[D".to_vec()
        );

        // App cursor mode
        assert_eq!(
            encode_key(Key::Up, 0, &modes_app).unwrap(),
            b"\x1bOA".to_vec()
        );
        assert_eq!(
            encode_key(Key::Down, 0, &modes_app).unwrap(),
            b"\x1bOB".to_vec()
        );
        assert_eq!(
            encode_key(Key::Right, 0, &modes_app).unwrap(),
            b"\x1bOC".to_vec()
        );
        assert_eq!(
            encode_key(Key::Left, 0, &modes_app).unwrap(),
            b"\x1bOD".to_vec()
        );
    }

    #[test]
    fn test_function_keys_with_modifiers() {
        let modes = Modes::default();
        // Without a modifier, SS3 is used.
        assert_eq!(encode_key(Key::F1, 0, &modes).unwrap(), b"\x1bOP".to_vec());
        assert_eq!(encode_key(Key::F4, 0, &modes).unwrap(), b"\x1bOS".to_vec());
        // With shift (1), the parameter is 2; with ctrl (4), it is 5.
        assert_eq!(
            encode_key(Key::F1, 1, &modes).unwrap(),
            b"\x1b[1;2P".to_vec()
        );
        assert_eq!(
            encode_key(Key::F3, 4, &modes).unwrap(),
            b"\x1b[1;5R".to_vec()
        );
    }

    #[test]
    fn test_cursor_keys_with_modifiers() {
        let modes = Modes::default();

        // Shift+Up (modifier 1)
        assert_eq!(
            encode_key(Key::Up, 1, &modes).unwrap(),
            b"\x1b[1;2A".to_vec()
        );

        // Ctrl+Left (modifier 4)
        assert_eq!(
            encode_key(Key::Left, 4, &modes).unwrap(),
            b"\x1b[1;5D".to_vec()
        );

        // Shift+Ctrl+Down (modifier 5)
        assert_eq!(
            encode_key(Key::Down, 5, &modes).unwrap(),
            b"\x1b[1;6B".to_vec()
        );
    }

    #[test]
    fn test_home_end_keys() {
        let modes_normal = Modes::default();
        let modes_app = Modes {
            app_cursor: true,
            ..Default::default()
        };

        // Normal mode
        assert_eq!(
            encode_key(Key::Home, 0, &modes_normal).unwrap(),
            b"\x1b[H".to_vec()
        );
        assert_eq!(
            encode_key(Key::End, 0, &modes_normal).unwrap(),
            b"\x1b[F".to_vec()
        );

        // App cursor mode
        assert_eq!(
            encode_key(Key::Home, 0, &modes_app).unwrap(),
            b"\x1bOH".to_vec()
        );
        assert_eq!(
            encode_key(Key::End, 0, &modes_app).unwrap(),
            b"\x1bOF".to_vec()
        );
    }

    #[test]
    fn test_tilde_keys() {
        let modes = Modes::default();

        // Insert/Delete/PageUp/PageDown
        assert_eq!(
            encode_key(Key::Insert, 0, &modes).unwrap(),
            b"\x1b[2~".to_vec()
        );
        assert_eq!(
            encode_key(Key::Delete, 0, &modes).unwrap(),
            b"\x1b[3~".to_vec()
        );
        assert_eq!(
            encode_key(Key::PageUp, 0, &modes).unwrap(),
            b"\x1b[5~".to_vec()
        );
        assert_eq!(
            encode_key(Key::PageDown, 0, &modes).unwrap(),
            b"\x1b[6~".to_vec()
        );

        // With a modifier
        assert_eq!(
            encode_key(Key::Delete, 2, &modes).unwrap(),
            b"\x1b[3;3~".to_vec() // alt (2+1=3)
        );
    }

    #[test]
    fn test_function_keys() {
        let modes = Modes::default();

        // F1-F4: ESC O letter
        assert_eq!(encode_key(Key::F1, 0, &modes).unwrap(), b"\x1bOP".to_vec());
        assert_eq!(encode_key(Key::F2, 0, &modes).unwrap(), b"\x1bOQ".to_vec());
        assert_eq!(encode_key(Key::F3, 0, &modes).unwrap(), b"\x1bOR".to_vec());
        assert_eq!(encode_key(Key::F4, 0, &modes).unwrap(), b"\x1bOS".to_vec());

        // F5-F12: ESC [ num ~
        assert_eq!(
            encode_key(Key::F5, 0, &modes).unwrap(),
            b"\x1b[15~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F6, 0, &modes).unwrap(),
            b"\x1b[17~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F7, 0, &modes).unwrap(),
            b"\x1b[18~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F8, 0, &modes).unwrap(),
            b"\x1b[19~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F9, 0, &modes).unwrap(),
            b"\x1b[20~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F10, 0, &modes).unwrap(),
            b"\x1b[21~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F11, 0, &modes).unwrap(),
            b"\x1b[23~".to_vec()
        );
        assert_eq!(
            encode_key(Key::F12, 0, &modes).unwrap(),
            b"\x1b[24~".to_vec()
        );

        // F5 with Shift (modifier 1)
        assert_eq!(
            encode_key(Key::F5, 1, &modes).unwrap(),
            b"\x1b[15;2~".to_vec()
        );
    }

    #[test]
    fn test_special_keys() {
        let modes = Modes::default();

        assert_eq!(encode_key(Key::Enter, 0, &modes).unwrap(), b"\r".to_vec());
        assert_eq!(encode_key(Key::Tab, 0, &modes).unwrap(), b"\t".to_vec());
        // Shift+Tab is BackTab (CSI Z). Full-screen programs bind mode switching to this sequence.
        assert_eq!(encode_key(Key::Tab, 1, &modes).unwrap(), b"\x1b[Z".to_vec());
        // Alt+Backspace is delete previous word (ESC DEL).
        assert_eq!(
            encode_key(Key::Backspace, 2, &modes).unwrap(),
            b"\x1b\x7f".to_vec()
        );
        assert_eq!(
            encode_key(Key::Backspace, 0, &modes).unwrap(),
            b"\x7f".to_vec()
        );
        assert_eq!(
            encode_key(Key::Escape, 0, &modes).unwrap(),
            b"\x1b".to_vec()
        );
    }

    #[test]
    fn test_encode_ctrl_char() {
        // Ctrl+A through Ctrl+Z
        assert_eq!(encode_ctrl_char('a').unwrap(), vec![0x01]);
        assert_eq!(encode_ctrl_char('u').unwrap(), vec![0x15]);
        assert_eq!(encode_ctrl_char('z').unwrap(), vec![0x1a]);
        assert_eq!(encode_ctrl_char('A').unwrap(), vec![0x01]);
        assert_eq!(encode_ctrl_char('Z').unwrap(), vec![0x1a]);

        // Special ctrl codes
        assert_eq!(encode_ctrl_char('[').unwrap(), vec![0x1b]);
        assert_eq!(encode_ctrl_char('\\').unwrap(), vec![0x1c]);
        assert_eq!(encode_ctrl_char(']').unwrap(), vec![0x1d]);
        assert_eq!(encode_ctrl_char(' ').unwrap(), vec![0x00]);

        // Unsupported
        assert_eq!(encode_ctrl_char('!').unwrap_err(), EncodeError::Unsupported);
    }

    #[test]
    fn test_encode_alt_char() {
        // Alt+A → ESC + 'a'
        let result = encode_alt_char('a');
        assert_eq!(result, vec![0x1b, b'a']);

        // Alt plus a Korean syllable
        let result = encode_alt_char('한');
        let mut expected = vec![0x1b];
        let korean_str = "한";
        expected.extend_from_slice(korean_str.as_bytes());
        assert_eq!(result, expected);
    }

    #[test]
    fn test_encode_paste_bracketed() {
        let modes_bracketed = Modes {
            bracketed_paste: true,
            ..Default::default()
        };
        let modes_normal = Modes::default();

        // bracketed paste mode on
        let result = encode_paste("hello", &modes_bracketed).unwrap();
        assert_eq!(result, b"\x1b[200~hello\x1b[201~".to_vec());

        // bracketed paste mode off
        let result = encode_paste("hello", &modes_normal).unwrap();
        assert_eq!(result, b"hello".to_vec());
    }

    #[test]
    fn test_encode_paste_rejects_end_marker_without_dropping_it() {
        let modes = Modes {
            bracketed_paste: true,
            ..Default::default()
        };

        let error = encode_paste("hello\x1b[201~world", &modes).unwrap_err();
        assert_eq!(error, "paste text contains the bracketed-paste terminator");
    }

    #[test]
    fn test_encode_paste_preserves_line_endings() {
        let modes = Modes {
            bracketed_paste: true,
            ..Default::default()
        };

        let result = encode_paste("hello\nworld", &modes).unwrap();
        assert_eq!(result, b"\x1b[200~hello\nworld\x1b[201~".to_vec());
        let result = encode_paste("hello\r\nworld", &modes).unwrap();
        assert_eq!(result, b"\x1b[200~hello\r\nworld\x1b[201~".to_vec());
        let result = encode_paste("hello\rworld", &modes).unwrap();
        assert_eq!(result, b"\x1b[200~hello\rworld\x1b[201~".to_vec());
    }

    #[test]
    fn test_composition_state_not_composing() {
        let mut composer = CompositionState::new();

        // No bytes exist before confirmation
        assert_eq!(composer.add_char('あ'), None);
        assert!(composer.is_composing());

        // Confirm and obtain the bytes
        let confirmed = composer.confirm();
        assert_eq!(confirmed, Some("あ".as_bytes().to_vec()));
        assert!(!composer.is_composing());
    }

    #[test]
    fn test_composition_state_cancel() {
        let mut composer = CompositionState::new();

        composer.add_char('あ');
        composer.add_char('い');
        assert!(composer.is_composing());

        composer.cancel();
        assert!(!composer.is_composing());
        assert_eq!(composer.current(), "");
    }

    #[test]
    fn test_composition_state_multiple_chars() {
        let mut composer = CompositionState::new();

        composer.add_char('a');
        composer.add_char('b');
        composer.add_char('c');

        let confirmed = composer.confirm();
        assert_eq!(confirmed, Some(b"abc".to_vec()));
        assert!(!composer.is_composing());

        // The next composition starts anew
        composer.add_char('x');
        let confirmed2 = composer.confirm();
        assert_eq!(confirmed2, Some(b"x".to_vec()));
    }

    #[test]
    fn test_composition_state_current() {
        let mut composer = CompositionState::new();

        composer.add_char('한');
        composer.add_char('글');
        assert_eq!(composer.current(), "한글");

        composer.confirm();
        assert_eq!(composer.current(), "");
    }

    #[test]
    fn test_composition_state_confirm_empty() {
        let mut composer = CompositionState::new();
        assert_eq!(composer.confirm(), None);
    }
}
