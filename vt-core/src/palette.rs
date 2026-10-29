/// Defines the default VT palette and the special color slots.
///
/// The engine uses these values for screen cells and OSC color replies, and the native renderer
/// uses the foreground/background/cursor values passed in the same call.
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
    /// Background of the selected cells. Used when the program has not set the OSC 17 highlight
    /// background.
    pub selection: [u8; 3],
    pub palette: [[u8; 3]; 256],
    /// Appearance that selects the indexed ANSI palette.
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

    /// Builds the appearance from the page theme request. The mode and all four colors (#rrggbb)
    /// must be present.
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

/// Reads `#rrggbb`. Returns None for any other format.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    // default: the six hexadecimal digits were checked above, so the two-digit conversion cannot
    // fail.
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

const fn dim(color: [u8; 3]) -> [u8; 3] {
    [color[0] / 2, color[1] / 2, color[2] / 2]
}

/// Resolves the default palette at 0..255 and the named slots at 256..268.
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
