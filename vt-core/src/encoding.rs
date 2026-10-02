//! 키·마우스·조합 입력을 PTY 에 쓸 바이트로 인코딩하는 순수 함수들.
//!
//! 터미널 엔진마다 다시 구현할 필요가 없는 공통 계층. 페이지에서 받은 논리적 입력을
//! 표준 VT 시퀀스로 변환한다.

use crate::Modes;

/// 입력 장치가 보낼 수 있는 키의 열거형.
/// 각 키는 표준 VT 시퀀스로 인코딩되어야 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    // 커서 키
    Up,
    Down,
    Left,
    Right,
    // 영역 키
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    // 기능 키
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
    // 특수 키
    Enter,
    Tab,
    Backspace,
    Escape,
    /// 숫자 키패드의 글자 키: `0`–`9`, `.`, `+`, `-`, `*`, `/`, `=`.
    Keypad(char),
    /// 숫자 키패드의 Enter.
    KeypadEnter,
}

/// 마우스 보고의 버튼. None 은 버튼을 누르지 않고 움직인 포인터다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    None,
    WheelUp,
    WheelDown,
}

/// 마우스 보고의 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// 마우스 보고 하나. 칸은 0부터 센다.
#[derive(Debug, Clone, Copy)]
pub struct MouseReport {
    pub button: MouseButton,
    pub action: MouseAction,
    pub col: u16,
    pub row: u16,
    pub alt: bool,
    pub ctrl: bool,
}

/// 키 인코딩 오류.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// 미지원 키 또는 입력.
    Unsupported,
    /// 기타 오류.
    Other(String),
}

/// 수식자 플래그 (shift, alt, ctrl의 조합).
/// 각 비트: shift=1, alt=2, ctrl=4
pub type Modifiers = u8;

/// 키를 PTY 에 쓸 바이트로 인코딩한다.
///
/// # 인코딩 규칙
/// - 커서 키 (`Up`/`Down`/`Right`/`Left`): 보통 `ESC [ A|B|C|D`, app cursor 모드면 `ESC O A|B|C|D`.
/// - `Home`/`End`: `ESC [ H` / `ESC [ F` (app cursor 면 `ESC O H` / `ESC O F`).
/// - `Insert`/`Delete`/`PageUp`/`PageDown`: `ESC [ 2~` / `ESC [ 3~` / `ESC [ 5~` / `ESC [ 6~`.
/// - `F1..F12`: VT 관례 (F1–F4 는 `ESC O P|Q|R|S`, F5 `ESC [ 15~`, 등).
/// - 수식자(shift=1, alt=2, ctrl=4)가 있으면 `ESC [ 1 ; <n> A` 꼴 (커서 키),
///   또는 `ESC [ 2 ; <n> ~` 꼴 (Tilde 계열).
/// - `Enter` → `\r`, `Tab` → `\t`, `Backspace` → `\x7f`, `Escape` → `\x1b`.
/// - 문자 기반 키 (`A`–`Z`, `0`–`9` 등)는 이 함수에 전달되지 않음.
///   문자는 `encode_text()`를 통해 직접 인코딩하기.
///
/// # 미지원
/// - **응용 키패드 모드**: `modes.app_keypad` 가 켜져 있으면 수정 키 없는 키패드 키가 `ESC O` 시퀀스가 됨.
///   숫자 키는 그대로 보냄. 향후 확장 예정.
///
/// # 에러
/// 모르는 키나 매칭 불가능한 입력은 `Err(Unsupported)` 를 반환함.
/// 오류를 조용히 삼키지 않으니 호출자가 적절히 처리하기.
pub fn encode_key(key: Key, modifiers: Modifiers, modes: &Modes) -> Result<Vec<u8>, EncodeError> {
    match key {
        // 커서 키
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

        // Home/End 키
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

        // Tilde 계열 키 (Insert, Delete, PageUp, PageDown)
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

        // 기능 키 (F1–F12)
        // F1–F4 는 수식자가 없으면 SS3, 있으면 CSI 에 수식자 인자를 싣는다.
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

        // 특수 키
        Key::Enter => Ok(b"\r".to_vec()),
        // Shift+Tab 은 BackTab(CSI Z) 이다. 전체화면 프로그램이 이 시퀀스로 모드 순환을 묶는다.
        Key::Tab if modifiers & 1 != 0 => Ok(b"\x1b[Z".to_vec()),
        Key::Tab => Ok(b"\t".to_vec()),
        // Alt+Backspace 는 앞 단어 지우기(ESC DEL)다. readline 이 backward-kill-word 로 묶는다.
        Key::Backspace if modifiers & 2 != 0 => Ok(b"\x1b\x7f".to_vec()),
        Key::Backspace => Ok(b"\x7f".to_vec()),
        Key::Escape => Ok(b"\x1b".to_vec()),
        // 응용 키패드 모드(ESC =)에서 수정 키 없는 키패드 키는 SS3 시퀀스다. 그 밖에는 키의 글자다.
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

/// Ctrl 기반 바이트 인코딩. `ctrl+a` → `0x01` 부터 `ctrl+z` → `0x1a` 까지.
/// 일반 문자가 아닌 특수 시퀀스도 지원: `ctrl+[` → `0x1b`, `ctrl+\` → `0x1c`, `ctrl+]` → `0x1d`, `ctrl+space` → `0x00`.
///
/// # 에러
/// `ch` 가 컨트롤 가능한 문자가 아니면 `Err(Unsupported)`.
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

/// 텍스트를 utf-8 바이트로 인코딩한다. 텍스트 입력 처리용.
pub fn encode_text(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Alt + 문자 조합. 문자 앞에 ESC 를 붙인다.
/// 예: `alt+a` → `ESC` + `a` 의 UTF-8 바이트.
pub fn encode_alt_char(ch: char) -> Vec<u8> {
    let mut result = vec![0x1b]; // ESC
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    result.extend_from_slice(encoded.as_bytes());
    result
}

/// 수식자 파라미터를 계산한다.
/// shift=1, alt=2, ctrl=4 의 비트가 주어질 때, (shift+alt+ctrl) + 1 을 반환.
/// 예: alt+ctrl → (2+4)+1=7, shift 만 → (1)+1=2, 수식자 없음 → (0)+1=1.
fn compute_modifier_param(modifiers: Modifiers) -> u8 {
    modifiers + 1
}

/// 마우스 보고 하나를 현재 인코딩으로 만든다. SGR(1006), UTF-8(1005), 기본 인코딩 순으로 모드를 따른다.
///
/// 버튼 값은 왼쪽 0, 버튼 없음 3, 휠 64/65 이고, 움직임은 32, alt 는 8, ctrl 은 16 을 더한다. SGR 은 뗌을 `m` 으로
/// 끝내고 버튼을 유지한다. 기본과 UTF-8 인코딩의 뗌은 버튼 3 이다. 기본 인코딩이 나타낼 수 없는 칸은 오류다.
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

/// 포인터 칸 (col, row) 의 휠 버튼 이벤트 하나. older 가 참이면 버튼 64(오래된 출력 쪽), 아니면 65 다.
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

/// 붙여넣기 텍스트를 인코딩한다.
///
/// # 처리
/// - `bracketed_paste` 모드가 켜져 있으면 `ESC [ 200 ~` 로 시작해 `ESC [ 201 ~` 로 끝남.
/// - 텍스트 내의 `ESC [ 201 ~` 시퀀스는 감싸기를 빠져나가지 못하도록 거부함.
/// - 텍스트의 UTF-8 바이트와 개행을 그대로 보존함.
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

/// 조합(IME) 상태를 추적하는 간단한 상태 머신.
/// 조합 중인 문자열을 보관하고, 확정 시점에만 바이트를 돌려준다.
///
/// # 사용법
/// ```
/// use soksak_sidecar_vt_core::encoding::CompositionState;
/// let mut composer = CompositionState::new();
/// // 조합 중에는 PTY 로 나가는 바이트가 없다.
/// assert_eq!(composer.add_char('한'), None);
/// assert!(composer.is_composing());
/// // 확정할 때 한 번만 나간다.
/// assert_eq!(composer.confirm(), Some("한".as_bytes().to_vec()));
/// assert!(!composer.is_composing());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CompositionState {
    buffer: String,
}

impl CompositionState {
    /// 새로운 조합 상태 생성.
    pub fn new() -> Self {
        Self::default()
    }

    /// 조합 중인 문자를 추가한다.
    /// **PTY 로 보내지 않음**. 호출자가 화면에 미리 그려야 함.
    /// `None` 을 반환 (바이트 없음).
    pub fn add_char(&mut self, ch: char) -> Option<Vec<u8>> {
        self.buffer.push(ch);
        None // 조합 중에는 바이트 반환 안 함
    }

    /// 조합을 취소한다.
    pub fn cancel(&mut self) {
        self.buffer.clear();
    }

    /// 조합을 확정한다.
    /// 버퍼의 텍스트를 UTF-8 바이트로 돌려주고 버퍼를 비운다.
    pub fn confirm(&mut self) -> Option<Vec<u8>> {
        if self.buffer.is_empty() {
            None
        } else {
            let bytes = self.buffer.as_bytes().to_vec();
            self.buffer.clear();
            Some(bytes)
        }
    }

    /// 현재 조합 중인 텍스트를 돌려줌 (읽기 전용).
    pub fn current(&self) -> &str {
        &self.buffer
    }

    /// 조합이 진행 중인지 확인.
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

        // 일반 모드
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

        // App cursor 모드
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
        // 수식자가 없으면 SS3.
        assert_eq!(encode_key(Key::F1, 0, &modes).unwrap(), b"\x1bOP".to_vec());
        assert_eq!(encode_key(Key::F4, 0, &modes).unwrap(), b"\x1bOS".to_vec());
        // shift(1) 이면 인자는 2, ctrl(4) 이면 5.
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

        // 일반 모드
        assert_eq!(
            encode_key(Key::Home, 0, &modes_normal).unwrap(),
            b"\x1b[H".to_vec()
        );
        assert_eq!(
            encode_key(Key::End, 0, &modes_normal).unwrap(),
            b"\x1b[F".to_vec()
        );

        // App cursor 모드
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

        // modifier 포함
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

        // Shift를 누른 F5 (modifier 1)
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
        // Shift+Tab 은 BackTab(CSI Z) 이다. 전체화면 프로그램이 이 시퀀스로 모드 전환을 묶는다.
        assert_eq!(encode_key(Key::Tab, 1, &modes).unwrap(), b"\x1b[Z".to_vec());
        // Alt+Backspace 는 앞 단어 지우기(ESC DEL)다.
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
        // Ctrl+A부터 Ctrl+Z까지
        assert_eq!(encode_ctrl_char('a').unwrap(), vec![0x01]);
        assert_eq!(encode_ctrl_char('u').unwrap(), vec![0x15]);
        assert_eq!(encode_ctrl_char('z').unwrap(), vec![0x1a]);
        assert_eq!(encode_ctrl_char('A').unwrap(), vec![0x01]);
        assert_eq!(encode_ctrl_char('Z').unwrap(), vec![0x1a]);

        // 특수 ctrl code
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

        // Alt+한 (한글)
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

        // bracketed paste mode 켜짐
        let result = encode_paste("hello", &modes_bracketed).unwrap();
        assert_eq!(result, b"\x1b[200~hello\x1b[201~".to_vec());

        // bracketed paste mode 꺼짐
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

        // 확정 전에는 byte가 없다
        assert_eq!(composer.add_char('あ'), None);
        assert!(composer.is_composing());

        // 확정하고 byte를 얻는다
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

        // 다음 조합은 새로 시작한다
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
