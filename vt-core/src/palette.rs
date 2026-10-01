/// VT 기본 팔레트와 특수 색상 슬롯을 정의한다.
///
/// 엔진은 이 값을 화면 셀과 OSC 색상 응답에 사용하고, 네이티브 renderer는
/// 같은 호출에서 전달받은 foreground/background/cursor 값을 사용한다.
pub const DEFAULT_FOREGROUND_RGB: [u8; 3] = [0xd0, 0xd0, 0xd0];
pub const DEFAULT_BACKGROUND_RGB: [u8; 3] = [0x1e, 0x1e, 0x1e];
pub const DEFAULT_CURSOR_RGB: [u8; 3] = DEFAULT_FOREGROUND_RGB;
pub const DEFAULT_FOREGROUND_HEX: &str = "#d0d0d0";
pub const DEFAULT_BACKGROUND_HEX: &str = "#1e1e1e";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalTheme {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub cursor: [u8; 3],
    /// 선택한 칸의 배경. 프로그램이 OSC 17 강조 배경을 정하지 않았을 때 쓴다.
    pub selection: [u8; 3],
    pub palette: [[u8; 3]; 256],
    /// 인덱스 ANSI 팔레트를 고른 외관.
    pub mode: ThemeMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl ThemeMode {
    pub const fn name(self) -> &'static str {
        match self {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }
}

const ANSI: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x00, 0x00],
    [0x00, 0xcd, 0x00],
    [0xcd, 0xcd, 0x00],
    [0x00, 0x00, 0xee],
    [0xcd, 0x00, 0xcd],
    [0x00, 0xcd, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x7f, 0x7f, 0x7f],
    [0xff, 0x00, 0x00],
    [0x00, 0xff, 0x00],
    [0xff, 0xff, 0x00],
    [0x5c, 0x5c, 0xff],
    [0xff, 0x00, 0xff],
    [0x00, 0xff, 0xff],
    [0xff, 0xff, 0xff],
];

const LIGHT_ANSI: [[u8; 3]; 16] = [
    [0x3b, 0x3b, 0x3b],
    [0xa4, 0x00, 0x00],
    [0x00, 0x6a, 0x00],
    [0x8f, 0x5b, 0x00],
    [0x00, 0x3d, 0xa5],
    [0x7a, 0x00, 0x7a],
    [0x00, 0x66, 0x66],
    [0xd0, 0xd0, 0xd0],
    [0x70, 0x70, 0x70],
    [0xcc, 0x00, 0x00],
    [0x00, 0x80, 0x00],
    [0x9a, 0x6b, 0x00],
    [0x00, 0x4e, 0xcc],
    [0x8f, 0x00, 0x8f],
    [0x00, 0x80, 0x80],
    [0x20, 0x20, 0x20],
];

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

const fn build_default_palette() -> [[u8; 3]; 256] {
    let mut palette = [[0; 3]; 256];
    let mut index = 0;
    while index < 16 {
        palette[index] = ANSI[index];
        index += 1;
    }

    let mut cube = 0;
    while cube < 216 {
        let red = cube / 36;
        let green = (cube / 6) % 6;
        let blue = cube % 6;
        palette[16 + cube] = [CUBE[red], CUBE[green], CUBE[blue]];
        cube += 1;
    }

    let mut gray = 0;
    while gray < 24 {
        let value = 8 + gray * 10;
        palette[232 + gray] = [value as u8, value as u8, value as u8];
        gray += 1;
    }
    palette
}

const fn build_palette(ansi: [[u8; 3]; 16]) -> [[u8; 3]; 256] {
    let mut palette = [[0; 3]; 256];
    let mut index = 0;
    while index < 16 {
        palette[index] = ansi[index];
        index += 1;
    }
    let mut cube = 0;
    while cube < 216 {
        let red = cube / 36;
        let green = (cube / 6) % 6;
        let blue = cube % 6;
        palette[16 + cube] = [CUBE[red], CUBE[green], CUBE[blue]];
        cube += 1;
    }
    let mut gray = 0;
    while gray < 24 {
        let value = 8 + gray * 10;
        palette[232 + gray] = [value as u8, value as u8, value as u8];
        gray += 1;
    }
    palette
}

pub const DEFAULT_PALETTE: [[u8; 3]; 256] = build_default_palette();
pub const LIGHT_PALETTE: [[u8; 3]; 256] = build_palette(LIGHT_ANSI);

impl TerminalTheme {
    pub const fn dark() -> Self {
        Self {
            foreground: DEFAULT_FOREGROUND_RGB,
            background: DEFAULT_BACKGROUND_RGB,
            cursor: DEFAULT_CURSOR_RGB,
            selection: [0x44, 0x47, 0x5a],
            palette: DEFAULT_PALETTE,
            mode: ThemeMode::Dark,
        }
    }

    pub const fn light() -> Self {
        Self {
            foreground: [0x24, 0x24, 0x24],
            background: [0xf7, 0xf7, 0xf5],
            cursor: [0x24, 0x24, 0x24],
            selection: [0xc8, 0xcc, 0xd8],
            palette: LIGHT_PALETTE,
            mode: ThemeMode::Light,
        }
    }

    /// 페이지의 theme 요청에서 외관을 만든다. 모드와 네 색(#rrggbb)이 모두 있어야 한다.
    pub fn from_request(
        mode: Option<&str>,
        background: Option<&str>,
        foreground: Option<&str>,
        cursor: Option<&str>,
        selection: Option<&str>,
    ) -> Result<Self, String> {
        let base = match mode {
            Some("dark") => Self::dark(),
            Some("light") => Self::light(),
            _ => return Err("theme.mode must be dark or light".to_string()),
        };
        let color = |name: &str, value: Option<&str>| {
            value
                .and_then(parse_hex)
                .ok_or_else(|| format!("theme.{name} must be a #rrggbb color"))
        };
        Ok(Self {
            background: color("background", background)?,
            foreground: color("foreground", foreground)?,
            cursor: color("cursor", cursor)?,
            selection: color("selection", selection)?,
            ..base
        })
    }

    pub const fn color(&self, index: usize) -> Option<[u8; 3]> {
        if index < 256 {
            return Some(self.palette[index]);
        }
        match index {
            256 => Some(self.foreground),
            257 => Some(self.background),
            258 => Some(self.cursor),
            259..=266 => Some(dim(self.palette[index - 259])),
            267 => Some(self.foreground),
            268 => Some(dim(self.foreground)),
            _ => None,
        }
    }
}

/// `#rrggbb` 를 읽는다. 다른 형식이면 None.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    // 기본값: 위에서 16진수 여섯 자리를 확인했으므로 두 자리 변환은 실패하지 않는다.
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

const fn dim(color: [u8; 3]) -> [u8; 3] {
    [color[0] / 2, color[1] / 2, color[2] / 2]
}

/// 0..255 기본 팔레트와 256..268 named 슬롯을 해석한다.
pub const fn default_terminal_color(index: usize) -> Option<[u8; 3]> {
    if index < 256 {
        return Some(DEFAULT_PALETTE[index]);
    }
    match index {
        256 => Some(DEFAULT_FOREGROUND_RGB),
        257 => Some(DEFAULT_BACKGROUND_RGB),
        258 => Some(DEFAULT_CURSOR_RGB),
        259..=266 => Some(dim(DEFAULT_PALETTE[index - 259])),
        267 => Some(DEFAULT_FOREGROUND_RGB),
        268 => Some(dim(DEFAULT_FOREGROUND_RGB)),
        _ => None,
    }
}
