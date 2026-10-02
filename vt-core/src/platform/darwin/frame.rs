#[repr(C)]
pub struct CMetrics {
    pub width: u32,
    pub height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub font_size: f64,
    pub font: *const CFrameFont,
}

#[repr(C)]
pub struct CFrameFont {
    _private: [u8; 0],
}

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[repr(C)]
pub struct CCell {
    pub col: u32,
    pub row: u32,
    pub width: u32,
    pub ch: *const u8,
    pub ch_len: u32,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub has_fg: u8,
    pub has_bg: u8,
    pub inverse: u8,
    pub underline: u8,
}

#[repr(C)]
pub struct CScreen {
    pub width: u32,
    pub height: u32,
    pub cursor_col: u32,
    pub cursor_row: u32,
    pub cells: *mut CCell,
    pub cell_count: u32,
    pub cursor_visible: u8,
    pub cursor_blink_visible: u8,
    pub cursor_shape: u8,
    pub default_foreground: [u8; 3],
    pub default_background: [u8; 3],
    pub default_cursor: [u8; 3],
    pub cursor_width: u32,
}

#[repr(C)]
pub struct CInlineImageRaster {
    pub data: *const u8,
    pub data_len: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub preserve_aspect_ratio: u8,
    pub visible: u8,
}

// 불투명 frame type
#[repr(C)]
pub struct CFrame {
    _private: [u8; 0],
}

// IOSurface는 thread-safe이므로 Frame을 thread 사이에 보내도 안전하다
unsafe impl Send for CFrame {}
unsafe impl Sync for CFrame {}

extern "C" {
    fn frame_new(width_px: u32, height_px: u32) -> *mut CFrame;
    fn frame_id(frame: *mut CFrame) -> u32;
    fn frame_nonce(frame: *mut CFrame, buf: *mut u8);
    fn frame_draw_with_inline_images(
        frame: *mut CFrame,
        screen: *mut CScreen,
        metrics: *mut CMetrics,
        images: *mut CInlineImageRaster,
        image_count: u32,
    ) -> i32;
    fn frame_drop(frame: *mut CFrame);
    fn frame_metrics(font: *const CFrameFont, font_size: f64, scale: f64) -> CMetrics;
    fn frame_font_system_monospace() -> *mut CFrameFont;
    fn frame_font_named(family: *const std::ffi::c_char) -> *mut CFrameFont;
    fn frame_font_drop(font: *mut CFrameFont);
    fn frame_font_family(font: *const CFrameFont) -> *mut std::ffi::c_char;
    fn free(pointer: *mut std::ffi::c_void);
    fn frame_pixel(frame: *mut CFrame, x: u32, y: u32, out: *mut u8) -> i32;
}

pub struct Frame {
    ptr: *mut CFrame,
}

pub use crate::protocol::{CursorBlinkPolicy, UnfocusedCursor};

pub fn cursor_blink_visible(
    policy: CursorBlinkPolicy,
    program_blinking: bool,
    focused: bool,
    elapsed_ms: u64,
    interval_ms: u64,
    idle_timeout_ms: u64,
) -> bool {
    if !focused || matches!(policy, CursorBlinkPolicy::Never) {
        return true;
    }
    if idle_timeout_ms != 0 && elapsed_ms >= idle_timeout_ms {
        return true;
    }
    let enabled = match policy {
        CursorBlinkPolicy::Never => false,
        CursorBlinkPolicy::Always | CursorBlinkPolicy::On => true,
        CursorBlinkPolicy::Off => program_blinking,
    };
    if !enabled {
        return true;
    }
    let interval = interval_ms.max(1);
    (elapsed_ms / interval).is_multiple_of(2)
}

pub fn effective_cursor_shape(
    program_shape: crate::protocol::CursorShape,
    focused: bool,
    unfocused: UnfocusedCursor,
) -> crate::protocol::CursorShape {
    if focused {
        return program_shape;
    }
    match unfocused {
        UnfocusedCursor::Hollow => crate::protocol::CursorShape::HollowBlock,
        UnfocusedCursor::Solid => crate::protocol::CursorShape::Block,
        UnfocusedCursor::Underline => crate::protocol::CursorShape::Underline,
        UnfocusedCursor::Beam => crate::protocol::CursorShape::Beam,
        UnfocusedCursor::Unchanged => program_shape,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorRender {
    pub visible: bool,
    pub focused: bool,
    pub blink_visible: bool,
    pub shape: crate::protocol::CursorShape,
    pub unfocused: UnfocusedCursor,
}

impl CursorRender {
    pub(crate) fn from_protocol(cursor: &crate::protocol::Cursor) -> Self {
        Self {
            visible: cursor.visible,
            focused: cursor.focused,
            // 엔진의 blinking 값은 활성화 상태일 뿐이다. phase는 서비스 scheduler가 전달한다.
            blink_visible: cursor.blink_visible,
            shape: cursor.shape,
            // 서비스가 포커스 없는 커서 정책을 화면의 커서 모양에 이미 적용했으므로 다시 바꾸지 않는다.
            unfocused: UnfocusedCursor::Unchanged,
        }
    }
}

impl Default for CursorRender {
    fn default() -> Self {
        Self {
            visible: true,
            focused: false,
            blink_visible: true,
            shape: crate::protocol::CursorShape::Block,
            unfocused: UnfocusedCursor::Hollow,
        }
    }
}

// CFrame(IOSurface)이 thread-safe이므로 Frame은 Send/Sync이다
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    pub fn new(width_px: u32, height_px: u32) -> Option<Frame> {
        unsafe {
            let ptr = frame_new(width_px, height_px);
            if ptr.is_null() {
                None
            } else {
                Some(Frame { ptr })
            }
        }
    }

    pub fn id(&self) -> u32 {
        unsafe { frame_id(self.ptr) }
    }

    pub fn nonce(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        unsafe {
            frame_nonce(self.ptr, buf.as_mut_ptr());
        }
        buf
    }

    pub fn draw(&self, screen: &crate::protocol::Screen, metrics: &Metrics) -> Result<(), String> {
        self.draw_with_theme(
            screen,
            metrics,
            CursorRender::from_protocol(&screen.cursor),
            &crate::palette::TerminalTheme::dark(),
        )
    }

    pub fn draw_with_cursor(
        &self,
        screen: &crate::protocol::Screen,
        metrics: &Metrics,
        cursor: CursorRender,
    ) -> Result<(), String> {
        self.draw_with_theme(
            screen,
            metrics,
            cursor,
            &crate::palette::TerminalTheme::dark(),
        )
    }

    pub fn draw_with_theme(
        &self,
        screen: &crate::protocol::Screen,
        metrics: &Metrics,
        cursor: CursorRender,
        theme: &crate::palette::TerminalTheme,
    ) -> Result<(), String> {
        self.draw_with_theme_and_inline_images(screen, metrics, cursor, theme, &[])
    }

    pub fn draw_with_theme_and_inline_images(
        &self,
        screen: &crate::protocol::Screen,
        metrics: &Metrics,
        cursor: CursorRender,
        theme: &crate::palette::TerminalTheme,
        images: &[crate::protocol::InlineImagePlacement],
    ) -> Result<(), String> {
        let mut render_screen = screen.clone();
        if let Some(preedit) = render_screen.cursor.preedit.clone() {
            apply_preedit(&mut render_screen, &preedit)?;
        }
        // 기본 배경은 화면의 현재 기본 배경(OSC 11 포함)이다. 칸 밖 여백도 이 색으로 채운다.
        let (default_background, valid) = parse_hex_color(&Some(render_screen.background.clone()));
        if !valid {
            return Err(format!(
                "invalid screen background {:?}",
                render_screen.background
            ));
        }
        // render_screen과 CCell이 참조하는 문자열은 이 호출이 끝날 때까지 Rust가 소유한다.
        // 네이티브 함수는 이 빌린 포인터를 저장하지 않는다.
        // Rust Screen을 C Screen으로 변환한다.
        let mut cells = Vec::new();
        for row in &render_screen.lines {
            for cell in row {
                let (fg, has_fg) = parse_hex_color(&cell.fg);
                let (bg, has_bg) = parse_hex_color(&cell.bg);

                cells.push(CCell {
                    col: 0, // 셀별 위치를 아래에서 설정한다.
                    row: 0, // 셀별 위치를 아래에서 설정한다.
                    width: cell.width as u32,
                    ch: cell
                        .ch
                        .as_ref()
                        .map_or(std::ptr::null(), |text| text.as_ptr()),
                    ch_len: cell.ch.as_ref().map_or(0, |text| text.len() as u32),
                    fg,
                    bg,
                    has_fg: if has_fg { 1 } else { 0 },
                    has_bg: if has_bg { 1 } else { 0 },
                    inverse: if cell.inverse { 1 } else { 0 },
                    // 링크 셀은 밑줄로 보인다.
                    underline: if cell.underline || cell.link.is_some() {
                        1
                    } else {
                        0
                    },
                });
            }
        }

        // 모든 셀의 행과 열을 설정한다.
        let mut cell_idx = 0;
        for row_idx in 0..render_screen.lines.len() {
            for col_idx in 0..render_screen.lines[row_idx].len() {
                if cell_idx < cells.len() {
                    cells[cell_idx].row = row_idx as u32;
                    cells[cell_idx].col = col_idx as u32;
                    cell_idx += 1;
                }
            }
        }

        // 커서는 그 위치 글자의 폭만큼, 화면 오른쪽 끝을 넘지 않게 덮는다. 줄에 셀이 없는 위치는 빈 셀 한 칸이다.
        let remaining = u32::from(render_screen.cols)
            .saturating_sub(u32::from(render_screen.cursor.col))
            .max(1);
        let cursor_width = render_screen
            .lines
            .get(render_screen.cursor.row as usize)
            .and_then(|line| line.get(render_screen.cursor.col as usize))
            .map_or(1, |cell| u32::from(cell.width.max(1)))
            .min(remaining);
        let effective_shape =
            effective_cursor_shape(cursor.shape, cursor.focused, cursor.unfocused);
        let mut c_screen = CScreen {
            width: render_screen.cols as u32,
            height: render_screen.rows as u32,
            cursor_col: render_screen.cursor.col as u32,
            cursor_row: render_screen.cursor.row as u32,
            cells: cells.as_mut_ptr(),
            cell_count: cells.len() as u32,
            cursor_visible: cursor.visible as u8,
            cursor_blink_visible: cursor.blink_visible as u8,
            cursor_shape: match effective_shape {
                crate::protocol::CursorShape::Block => 0,
                crate::protocol::CursorShape::Underline => 1,
                crate::protocol::CursorShape::Beam => 2,
                crate::protocol::CursorShape::HollowBlock => 3,
                crate::protocol::CursorShape::Hidden => 4,
            },
            default_foreground: theme.foreground,
            default_background,
            default_cursor: theme.cursor,
            cursor_width,
        };

        let mut c_metrics = CMetrics {
            width: 0,
            height: 0,
            cell_width: metrics.cell_width as u32,
            cell_height: metrics.cell_height as u32,
            font_size: metrics.font_size as f64,
            font: metrics.font.ptr,
        };

        let mut c_images = images
            .iter()
            .map(|image| CInlineImageRaster {
                data: image.data.as_ptr(),
                data_len: image.data.len() as u32,
                x: image.x,
                y: image.y,
                width: image.width,
                height: image.height,
                preserve_aspect_ratio: image.preserve_aspect_ratio as u8,
                visible: image.visible as u8,
            })
            .collect::<Vec<_>>();
        let result = unsafe {
            frame_draw_with_inline_images(
                self.ptr,
                &mut c_screen,
                &mut c_metrics,
                c_images.as_mut_ptr(),
                c_images.len() as u32,
            )
        };

        if result == 0 {
            Ok(())
        } else {
            Err("Frame draw failed".to_string())
        }
    }

    pub fn read_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let mut bgra = [0u8; 4];
        unsafe {
            if frame_pixel(self.ptr, x, y, bgra.as_mut_ptr()) == 0 {
                Some(bgra)
            } else {
                None
            }
        }
    }
}

fn utf16_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn valid_json_range(
    range: Option<crate::protocol::JsonRange>,
    text_length: Option<usize>,
    name: &str,
) -> Result<(), String> {
    if let Some(crate::protocol::JsonRange { location, length }) = range {
        let end = location
            .checked_add(length)
            .ok_or_else(|| format!("{name} JSON range overflows UTF-16 location"))?;
        if let Some(limit) = text_length {
            if end > limit {
                return Err(format!("{name} JSON range [{location}, {length}] exceeds preedit UTF-16 length {limit}"));
            }
        }
    }
    Ok(())
}

fn selected_contains(range: Option<crate::protocol::JsonRange>, start: usize, end: usize) -> bool {
    range.is_some_and(|crate::protocol::JsonRange { location, length }| {
        let range_end = location.saturating_add(length);
        start < range_end && end > location
    })
}

fn apply_preedit(
    screen: &mut crate::protocol::Screen,
    preedit: &crate::protocol::Preedit,
) -> Result<(), String> {
    let length = utf16_length(&preedit.text);
    valid_json_range(preedit.selected_range, Some(length), "selectedRange")?;
    // replacementRange는 호스트 문서의 marked range인 [location,length]다.
    // 새 preedit 문자열의 범위가 아니므로 preedit UTF-16 길이로 제한하지 않는다.
    valid_json_range(preedit.replacement_range, None, "replacementRange")?;
    let row = screen.cursor.row as usize;
    while screen.lines.len() <= row {
        screen.lines.push(Vec::new());
    }
    let display_col = screen.cursor.col as usize;
    let line = &mut screen.lines[row];
    let mut marked = Vec::new();
    let mut utf16_col = 0;
    let mut selected_col = None;
    for grapheme in preedit.text.graphemes(true) {
        if grapheme.contains('\n') || grapheme.contains('\r') {
            return Err("preedit must be a single-line renderable grapheme sequence".to_string());
        }
        let utf16_end = utf16_col + grapheme.encode_utf16().count();
        if preedit
            .selected_range
            .is_some_and(|range| range.location == utf16_col)
        {
            selected_col = Some(display_col + marked.len());
        }
        if preedit
            .selected_range
            .is_some_and(|range| range.location > utf16_col && range.location < utf16_end)
        {
            return Err("selectedRange location splits a preedit grapheme".to_string());
        }
        let width = UnicodeWidthStr::width(grapheme);
        if width == 0 {
            utf16_col = utf16_end;
            continue;
        }
        let width = u8::try_from(width)
            .map_err(|_| "preedit grapheme display width exceeds Cell width".to_string())?;
        let mut cell = crate::protocol::Cell {
            ch: Some(grapheme.to_string()),
            width,
            underline: true,
            ..Default::default()
        };
        if selected_contains(preedit.selected_range, utf16_col, utf16_end) {
            cell.bg = Some("#808080".to_string());
        }
        marked.push(cell);
        // 엔진이 확정된 넓은 글자에 두는 spacer 와 같이 폭 0 인 이어짐 셀을 둔다.
        // 폭이 있는 셀은 배경을 그려 앞 글자의 오른쪽 절반을 덮는다.
        for _ in 1..width {
            marked.push(crate::protocol::Cell {
                width: 0,
                ..crate::protocol::Cell::default()
            });
        }
        utf16_col = utf16_end;
    }
    if preedit
        .selected_range
        .is_some_and(|range| range.location == utf16_col)
    {
        selected_col = Some(display_col + marked.len());
    }
    if display_col + marked.len() > screen.cols as usize {
        return Err("preedit extends beyond terminal columns".to_string());
    }
    while line.len() < display_col + marked.len() {
        line.push(crate::protocol::Cell::default());
    }
    let tail = line.split_off(display_col + marked.len());
    line.truncate(display_col);
    line.extend(marked);
    line.extend(tail);
    if let Some(col) = selected_col {
        screen.cursor.col = col.min(screen.cols.saturating_sub(1) as usize) as u16;
    }
    Ok(())
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            frame_drop(self.ptr);
        }
    }
}

/// 터미널 글꼴. 설치된 글꼴 이름이나 시스템 고정폭 글꼴로 만든 CoreText 글꼴 설명이다.
pub struct TerminalFont {
    ptr: *mut CFrameFont,
}

// CoreText 글꼴 설명은 만든 뒤 바뀌지 않으므로 스레드 사이에서 공유할 수 있다.
unsafe impl Send for TerminalFont {}
unsafe impl Sync for TerminalFont {}

impl TerminalFont {
    /// 사용자의 시스템 고정폭 글꼴.
    pub fn system_monospace() -> Result<Self, String> {
        let ptr = unsafe { frame_font_system_monospace() };
        if ptr.is_null() {
            return Err("the system fixed-pitch font is unavailable".to_string());
        }
        Ok(Self { ptr })
    }

    /// 설치된 글꼴 가운데 family 이름이 정확히 같은 글꼴. 없으면 None 이다.
    pub fn installed(family: &str) -> Option<Self> {
        let name = std::ffi::CString::new(family).ok()?;
        let ptr = unsafe { frame_font_named(name.as_ptr()) };
        (!ptr.is_null()).then_some(Self { ptr })
    }

    /// 글꼴의 family 이름.
    pub fn family(&self) -> Result<String, String> {
        let pointer = unsafe { frame_font_family(self.ptr) };
        if pointer.is_null() {
            return Err("the terminal font has no family name".to_string());
        }
        let family = unsafe { std::ffi::CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        unsafe { free(pointer as *mut std::ffi::c_void) };
        Ok(family)
    }
}

impl std::fmt::Debug for TerminalFont {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.family() {
            Ok(family) => write!(formatter, "TerminalFont({family})"),
            Err(reason) => write!(formatter, "TerminalFont(<{reason}>)"),
        }
    }
}

impl Drop for TerminalFont {
    fn drop(&mut self) {
        unsafe { frame_font_drop(self.ptr) };
    }
}

/// `;` 로 이은 family 우선순위 목록에서 고른 글꼴.
pub struct FontSelection {
    pub font: std::sync::Arc<TerminalFont>,
    /// 설치되어 있지 않아 건너뛴 family.
    pub skipped: Vec<String>,
    /// 목록의 family 가 하나도 설치되어 있지 않아 시스템 고정폭 글꼴을 썼는지.
    pub system: bool,
}

/// family 목록에서 설치된 첫 family 를 고른다. 하나도 없으면 시스템 고정폭 글꼴이다.
/// 목록에 family 가 없으면 오류다.
pub fn resolve_font_list(list: &str) -> Result<FontSelection, String> {
    let families = list
        .split(';')
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .collect::<Vec<_>>();
    if families.is_empty() {
        return Err("font.family must name at least one family".to_string());
    }
    let mut skipped = Vec::new();
    for family in families {
        if let Some(font) = TerminalFont::installed(family) {
            return Ok(FontSelection {
                font: std::sync::Arc::new(font),
                skipped,
                system: false,
            });
        }
        skipped.push(family.to_string());
    }
    Ok(FontSelection {
        font: default_font(),
        skipped,
        system: true,
    })
}

static DEFAULT_FONT: std::sync::OnceLock<std::sync::Arc<TerminalFont>> = std::sync::OnceLock::new();

/// 시스템 고정폭 글꼴을 읽는다. 서비스는 시작할 때 호출하며, 읽지 못하면 시작하지 않는다.
pub fn load_default_font() -> Result<(), String> {
    if DEFAULT_FONT.get().is_none() {
        let font = std::sync::Arc::new(TerminalFont::system_monospace()?);
        DEFAULT_FONT.get_or_init(|| font);
    }
    Ok(())
}

/// font 요청 전의 글꼴. 시스템 고정폭 글꼴이다. 서비스는 시작할 때 load_default_font 로 먼저 읽는다.
pub fn default_font() -> std::sync::Arc<TerminalFont> {
    DEFAULT_FONT
        .get_or_init(|| {
            std::sync::Arc::new(
                TerminalFont::system_monospace().expect("the system fixed-pitch font must load"),
            )
        })
        .clone()
}

pub struct Metrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub font_size: f32,
    pub font: std::sync::Arc<TerminalFont>,
}

/// 기본 글꼴의 메트릭.
pub fn metrics(font_size: f32, scale: f32) -> Metrics {
    metrics_for(&default_font(), font_size, scale)
        .expect("the system fixed-pitch font has cell metrics")
}

/// font 의 메트릭. 셀 폭이나 높이를 잴 수 없는 글꼴은 오류다.
pub fn metrics_for(
    font: &std::sync::Arc<TerminalFont>,
    font_size: f32,
    scale: f32,
) -> Result<Metrics, String> {
    let c_metrics = unsafe { frame_metrics(font.ptr, font_size as f64, scale as f64) };
    if c_metrics.cell_width == 0 || c_metrics.cell_height == 0 {
        return Err("the terminal font has no measurable cell size".to_string());
    }
    Ok(Metrics {
        cell_width: c_metrics.cell_width as f32,
        cell_height: c_metrics.cell_height as f32,
        font_size: c_metrics.font_size as f32,
        font: font.clone(),
    })
}

fn parse_hex_color(color_opt: &Option<String>) -> ([u8; 3], bool) {
    if let Some(color_str) = color_opt {
        if color_str.starts_with('#') && color_str.len() == 7 {
            if let Ok(val) = u32::from_str_radix(&color_str[1..], 16) {
                let r = ((val >> 16) & 0xFF) as u8;
                let g = ((val >> 8) & 0xFF) as u8;
                let b = (val & 0xFF) as u8;
                return ([r, g, b], true);
            }
        }
    }
    ([0, 0, 0], false)
}
