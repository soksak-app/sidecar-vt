#[repr(C)]
pub struct CMetrics {
    pub width: u32,
    pub height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub font_size: f64,
    pub font: *const CFrameFont,
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
}

#[repr(C)]
pub struct CFrameFont {
    _private: [u8; 0],
}

#[derive(Clone, Debug)]
pub struct GlyphRenderBounds {
    pub row: u32,
    pub col: u32,
    pub width: u32,
    pub font_run_count: u32,
    pub grapheme: String,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub ink: Option<[f64; 4]>,
    pub underline: Option<[f64; 4]>,
}

#[derive(Clone, Debug)]
pub struct FrameRenderMetadata {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub bytes_per_row: usize,
    pub cell_width: u32,
    pub cell_height: u32,
    pub rows: u32,
    pub cols: u32,
    pub grid_origin_x: u32,
    pub grid_origin_y: u32,
    pub grid_logical_row_origin: u32,
    pub font_family: String,
    pub font_size: f64,
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub baseline_from_cell_top: f64,
    pub underline_position: f64,
    pub underline_thickness: f64,
    pub cursor: serde_json::Value,
    pub glyphs: Vec<GlyphRenderBounds>,
}

impl FrameRenderMetadata {
    pub fn value(&self, frame: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "frame": frame,
            "pixel": {
                "width": self.pixel_width,
                "height": self.pixel_height,
                "bytesPerRow": self.bytes_per_row,
                "format": "bgra8",
                "alpha": "premultiplied",
                "colorSpace": "sRGB",
                "origin": "top-left"
            },
            "grid": {
                "rows": self.rows,
                "cols": self.cols,
                "origin": { "x": self.grid_origin_x, "y": self.grid_origin_y },
                "logicalRowOrigin": self.grid_logical_row_origin,
                "cellWidth": self.cell_width,
                "cellHeight": self.cell_height
            },
            "font": {
                "family": self.font_family,
                "size": self.font_size,
                "ascent": self.ascent,
                "descent": self.descent,
                "leading": self.leading,
                "baselineFromCellTop": self.baseline_from_cell_top
            },
            "underline": {
                "positionFromBaseline": self.underline_position,
                "positionCoordinate": "CoreText y-up",
                "thickness": self.underline_thickness,
                "minimumThickness": 1.0,
                "colorSource": "glyph.foreground"
            },
            "cursor": self.cursor,
            "glyphs": self.glyphs.iter().map(|glyph| {
                serde_json::json!({
                    "row": glyph.row,
                    "col": glyph.col,
                    "width": glyph.width,
                    "fontRuns": glyph.font_run_count,
                    "grapheme": glyph.grapheme,
                    "foreground": color_hex(glyph.foreground),
                    "background": color_hex(glyph.background),
                    "bounds": glyph.ink.map(|rect| serde_json::json!({
                        "x": rect[0], "y": rect[1], "width": rect[2], "height": rect[3]
                    })),
                    "underline": { "rect": glyph.underline.map(|rect| serde_json::json!({
                        "x": rect[0], "y": rect[1], "width": rect[2], "height": rect[3]
                    })) }
                })
            }).collect::<Vec<_>>()
        })
    }
}

/// The screen copy and cursor used by both the raster and the serialized screen event.
pub(crate) struct RenderLayout {
    pub screen: crate::protocol::Screen,
    pub logical_row_origin: u32,
}

/// Builds the viewport and preedit overlay without changing the engine screen.
pub(crate) fn render_layout(screen: &crate::protocol::Screen) -> Result<RenderLayout, String> {
    let mut render_screen = screen.clone();
    let logical_row_origin = if let Some(preedit) = render_screen.cursor.preedit.clone() {
        apply_preedit(&mut render_screen, &preedit)?
    } else {
        0
    };
    Ok(RenderLayout {
        screen: render_screen,
        logical_row_origin,
    })
}

fn color_hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[cfg(feature = "fault-injection")]
pub mod fault {
    use std::cell::Cell;

    thread_local! {
        static FAIL_NEXT_FRAME_DRAW: Cell<bool> = const { Cell::new(false) };
    }

    /// Fails the next frame draw on the calling thread after layout validation.
    pub fn fail_next_draw() {
        FAIL_NEXT_FRAME_DRAW.with(|failure| failure.set(true));
    }

    pub(super) fn take_next_draw_failure() -> bool {
        FAIL_NEXT_FRAME_DRAW.with(|failure| failure.replace(false))
    }
}

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
#[derive(Clone, Copy, Default)]
pub struct CRenderCell {
    pub row: u32,
    pub col: u32,
    pub width: u32,
    pub font_run_count: u32,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub has_ink: u8,
    pub has_underline: u8,
    pub ink_x: f64,
    pub ink_y: f64,
    pub ink_width: f64,
    pub ink_height: f64,
    pub underline_x: f64,
    pub underline_y: f64,
    pub underline_width: f64,
    pub underline_height: f64,
}

#[repr(C)]
pub struct CRenderOutput {
    pub cells: *mut CRenderCell,
    pub cell_capacity: u32,
    pub cell_count: u32,
    pub bytes_per_row: usize,
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub underline_position: f64,
    pub underline_thickness: f64,
    pub cursor_visible: u8,
    pub cursor_blend_difference: u8,
    pub cursor_color: [u8; 3],
    pub cursor_x: f64,
    pub cursor_y: f64,
    pub cursor_width: f64,
    pub cursor_height: f64,
    pub cursor_border: f64,
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

// Opaque frame type
#[repr(C)]
pub struct CFrame {
    _private: [u8; 0],
}

// IOSurface is thread-safe, so sending a Frame between threads is safe
unsafe impl Send for CFrame {}
unsafe impl Sync for CFrame {}

unsafe extern "C" {
    fn frame_new(width_px: u32, height_px: u32) -> *mut CFrame;
    fn frame_new_gpu(width_px: u32, height_px: u32) -> *mut CFrame;
    fn frame_gpu_error() -> *const std::ffi::c_char;
    fn frame_id(frame: *mut CFrame) -> u32;
    fn frame_nonce(frame: *mut CFrame, buf: *mut u8);
    fn frame_draw_with_inline_images(
        frame: *mut CFrame,
        screen: *mut CScreen,
        metrics: *mut CMetrics,
        images: *mut CInlineImageRaster,
        image_count: u32,
        render_output: *mut CRenderOutput,
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
    width: u32,
    height: u32,
}

fn core_graphics_rect_to_top_left(frame_height: f64, rect: [f64; 4]) -> [f64; 4] {
    [
        rect[0],
        frame_height - (rect[1] + rect[3]),
        rect[2],
        rect[3],
    ]
}

#[cfg(test)]
mod render_coordinate_tests {
    use super::core_graphics_rect_to_top_left;

    #[test]
    fn transforms_an_asymmetric_fractional_rectangle_to_top_left_pixels() {
        assert_eq!(
            core_graphics_rect_to_top_left(71.0, [5.25, 13.5, 7.75, 2.25]),
            [5.25, 55.25, 7.75, 2.25]
        );
    }
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
            // The engine's blinking value only states that blinking is enabled. The service
            // scheduler supplies the phase.
            blink_visible: cursor.blink_visible,
            shape: cursor.shape,
            // The service has already applied the unfocused cursor policy to the cursor shape of
            // the screen, so it is not changed again.
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

// CFrame (IOSurface) is thread-safe, so Frame is Send/Sync
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    pub fn new(width_px: u32, height_px: u32) -> Option<Frame> {
        unsafe {
            let ptr = frame_new(width_px, height_px);
            if ptr.is_null() {
                None
            } else {
                Some(Frame {
                    ptr,
                    width: width_px,
                    height: height_px,
                })
            }
        }
    }

    /// A frame whose draws run through a Metal render pass into its IOSurface.
    pub fn new_gpu(width_px: u32, height_px: u32) -> Result<Frame, String> {
        let ptr = unsafe { frame_new_gpu(width_px, height_px) };
        if ptr.is_null() {
            let cause = unsafe { std::ffi::CStr::from_ptr(frame_gpu_error()) };
            return Err(format!(
                "the GPU frame is not created: {}",
                cause.to_string_lossy()
            ));
        }
        Ok(Frame {
            ptr,
            width: width_px,
            height: height_px,
        })
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
        self.draw_with_render_metadata(screen, metrics, cursor, theme, images, false)
            .map(|_| ())
    }

    pub fn draw_with_render_metadata(
        &self,
        screen: &crate::protocol::Screen,
        metrics: &Metrics,
        cursor: CursorRender,
        theme: &crate::palette::TerminalTheme,
        images: &[crate::protocol::InlineImagePlacement],
        collect_metadata: bool,
    ) -> Result<Option<FrameRenderMetadata>, String> {
        let layout = render_layout(screen)?;
        self.draw_render_layout(&layout, metrics, cursor, theme, images, collect_metadata)
    }

    pub(crate) fn draw_render_layout(
        &self,
        layout: &RenderLayout,
        metrics: &Metrics,
        cursor: CursorRender,
        theme: &crate::palette::TerminalTheme,
        images: &[crate::protocol::InlineImagePlacement],
        collect_metadata: bool,
    ) -> Result<Option<FrameRenderMetadata>, String> {
        let render_screen = &layout.screen;
        #[cfg(feature = "fault-injection")]
        if fault::take_next_draw_failure() {
            return Err("frame draw failed by fault-injection".to_string());
        }
        // The default background is the current default background of the screen (including OSC
        // 11). The margin outside the cells is also filled with this color.
        let (default_background, valid) = parse_hex_color(&Some(render_screen.background.clone()));
        if !valid {
            return Err(format!(
                "invalid screen background {:?}",
                render_screen.background
            ));
        }
        // Rust owns the strings that render_screen and CCell refer to until this call ends. The
        // native function does not store these borrowed pointers. Converts the Rust Screen to the C
        // Screen.
        let mut cells = Vec::new();
        for row in &render_screen.lines {
            for cell in row {
                let (fg, has_fg) = parse_hex_color(&cell.fg);
                let (bg, has_bg) = parse_hex_color(&cell.bg);

                cells.push(CCell {
                    col: 0, // Set per cell below.
                    row: 0, // Set per cell below.
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
                    // A link cell is shown with an underline.
                    underline: if cell.underline || cell.link.is_some() {
                        1
                    } else {
                        0
                    },
                });
            }
        }

        // Sets the row and column of every cell.
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

        // The cursor covers the width of the character at its position, without passing the right
        // end of the screen. A position with no cell in its line is one empty cell.
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
            ascent: metrics.ascent,
            descent: metrics.descent,
            leading: metrics.leading,
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
        let mut render_cells = if collect_metadata {
            vec![CRenderCell::default(); cells.len()]
        } else {
            Vec::new()
        };
        let mut render_output = CRenderOutput {
            cells: render_cells.as_mut_ptr(),
            cell_capacity: render_cells.len() as u32,
            cell_count: 0,
            bytes_per_row: 0,
            ascent: 0.0,
            descent: 0.0,
            leading: 0.0,
            underline_position: 0.0,
            underline_thickness: 0.0,
            cursor_visible: 0,
            cursor_blend_difference: 0,
            cursor_color: [0; 3],
            cursor_x: 0.0,
            cursor_y: 0.0,
            cursor_width: 0.0,
            cursor_height: 0.0,
            cursor_border: 0.0,
        };
        let render_output_ptr = if collect_metadata {
            &mut render_output
        } else {
            std::ptr::null_mut()
        };
        let result = unsafe {
            frame_draw_with_inline_images(
                self.ptr,
                &mut c_screen,
                &mut c_metrics,
                c_images.as_mut_ptr(),
                c_images.len() as u32,
                render_output_ptr,
            )
        };

        match result {
            0 => {}
            -2 => return Err("the GPU frame draws no text, underline or inline image".to_string()),
            -3 => return Err("the GPU frame collects no render metadata".to_string()),
            -4 => {
                let cause = unsafe { std::ffi::CStr::from_ptr(frame_gpu_error()) };
                return Err(cause.to_string_lossy().into_owned());
            }
            _ => return Err("Frame draw failed".to_string()),
        }
        if !collect_metadata {
            return Ok(None);
        }
        if render_output.bytes_per_row == 0 || render_output.cell_count as usize != cells.len() {
            return Err("Frame render metadata is incomplete after draw".to_string());
        }
        let mut glyphs = Vec::new();
        let mut cell_index = 0;
        for (row, cells_in_row) in render_screen.lines.iter().enumerate() {
            for (col, cell) in cells_in_row.iter().enumerate() {
                let rendered = render_cells
                    .get(cell_index)
                    .ok_or_else(|| format!("render metadata has no drawn cell at {row},{col}"))?;
                cell_index += 1;
                let Some(grapheme) = cell.ch.as_ref().filter(|text| !text.is_empty()) else {
                    continue;
                };
                let cell_rect = |x: f64, y: f64, width: f64, height: f64| {
                    let [frame_x, frame_y, width, height] = core_graphics_rect_to_top_left(
                        f64::from(self.height),
                        [x, y, width, height],
                    );
                    [
                        frame_x - col as f64 * f64::from(metrics.cell_width as u32),
                        frame_y - row as f64 * f64::from(metrics.cell_height as u32),
                        width,
                        height,
                    ]
                };
                glyphs.push(GlyphRenderBounds {
                    row: row as u32,
                    col: col as u32,
                    width: u32::from(cell.width)
                        .min(u32::from(render_screen.cols).saturating_sub(col as u32)),
                    font_run_count: rendered.font_run_count,
                    grapheme: grapheme.clone(),
                    foreground: rendered.foreground,
                    background: rendered.background,
                    ink: (rendered.has_ink != 0).then(|| {
                        cell_rect(
                            rendered.ink_x,
                            rendered.ink_y,
                            rendered.ink_width,
                            rendered.ink_height,
                        )
                    }),
                    underline: (rendered.has_underline != 0).then(|| {
                        cell_rect(
                            rendered.underline_x,
                            rendered.underline_y,
                            rendered.underline_width,
                            rendered.underline_height,
                        )
                    }),
                });
            }
        }
        let family = metrics.font.family()?;
        let cursor_rect = (render_output.cursor_visible != 0).then(|| {
            let [x, y, width, height] = core_graphics_rect_to_top_left(
                f64::from(self.height),
                [
                    render_output.cursor_x,
                    render_output.cursor_y,
                    render_output.cursor_width,
                    render_output.cursor_height,
                ],
            );
            serde_json::json!({ "x": x, "y": y, "width": width, "height": height })
        });
        Ok(Some(FrameRenderMetadata {
            pixel_width: self.width,
            pixel_height: self.height,
            bytes_per_row: render_output.bytes_per_row,
            cell_width: metrics.cell_width as u32,
            cell_height: metrics.cell_height as u32,
            rows: u32::from(render_screen.rows),
            cols: u32::from(render_screen.cols),
            grid_origin_x: 0,
            grid_origin_y: 0,
            grid_logical_row_origin: layout.logical_row_origin,
            font_family: family,
            font_size: f64::from(metrics.font_size),
            ascent: render_output.ascent,
            descent: render_output.descent,
            leading: render_output.leading,
            baseline_from_cell_top: f64::from(metrics.cell_height) - render_output.descent,
            underline_position: render_output.underline_position,
            underline_thickness: render_output.underline_thickness.max(1.0),
            cursor: serde_json::json!({
                "rect": cursor_rect,
                "shape": match effective_shape {
                    crate::protocol::CursorShape::Block => "block",
                    crate::protocol::CursorShape::Underline => "underline",
                    crate::protocol::CursorShape::Beam => "beam",
                    crate::protocol::CursorShape::HollowBlock => "hollowBlock",
                    crate::protocol::CursorShape::Hidden => "hidden",
                },
                "visible": cursor.visible,
                "blinkVisible": cursor.blink_visible,
                "focused": cursor.focused,
                "blend": if render_output.cursor_blend_difference != 0 { "difference" } else { "normal" },
                "color": (render_output.cursor_visible != 0 && render_output.cursor_blend_difference == 0)
                    .then(|| color_hex(render_output.cursor_color)),
                "border": render_output.cursor_border
            }),
            glyphs,
        }))
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
        if let Some(limit) = text_length
            && end > limit
        {
            return Err(format!(
                "{name} JSON range [{location}, {length}] exceeds preedit UTF-16 length {limit}"
            ));
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
) -> Result<u32, String> {
    let length = utf16_length(&preedit.text);
    preedit.validate_marked_range()?;
    valid_json_range(preedit.selected_range, Some(length), "selectedRange")?;
    // replacementRange is the marked range [location,length] of the host document. It is not the
    // range of the new preedit string, so it is not limited to the preedit UTF-16 length.
    valid_json_range(preedit.replacement_range, None, "replacementRange")?;
    let cols = usize::from(screen.cols);
    let rows = usize::from(screen.rows);
    if cols == 0 || rows == 0 {
        return Err("preedit cannot be rendered on an empty terminal grid".to_string());
    }
    let engine_cursor = (
        usize::from(screen.cursor.row),
        usize::from(screen.cursor.col),
    );
    let mut logical_row = engine_cursor.0;
    let mut logical_col = engine_cursor.1;
    let mut placements = Vec::new();
    let mut last_occupied = None;
    let mut utf16_col = 0;
    let mut selected_cursor = None;
    for grapheme in preedit.text.graphemes(true) {
        if grapheme.contains('\n') || grapheme.contains('\r') {
            return Err("preedit must be a single-line renderable grapheme sequence".to_string());
        }
        let utf16_end = utf16_col + grapheme.encode_utf16().count();
        let width = UnicodeWidthStr::width(grapheme);
        if width > 0 && logical_col > 0 && logical_col + width > cols {
            logical_row += 1;
            logical_col = 0;
        }
        if preedit
            .selected_range
            .is_some_and(|range| range.location == utf16_col)
        {
            selected_cursor = Some((logical_row, logical_col));
        }
        if preedit
            .selected_range
            .is_some_and(|range| range.location > utf16_col && range.location < utf16_end)
        {
            return Err("selectedRange location splits a preedit grapheme".to_string());
        }
        if width == 0 {
            utf16_col = utf16_end;
            continue;
        }
        let cell_width = u8::try_from(width)
            .map_err(|_| "preedit grapheme display width exceeds Cell width".to_string())?;
        let mut cell = crate::protocol::Cell {
            ch: Some(grapheme.to_string()),
            width: cell_width,
            underline: selected_contains(preedit.marked_range, utf16_col, utf16_end),
            ..Default::default()
        };
        if selected_contains(preedit.selected_range, utf16_col, utf16_end) {
            cell.bg = Some("#808080".to_string());
        }
        placements.push((logical_row, logical_col, cell));
        for offset in 1..width {
            if logical_col + offset < cols {
                placements.push((
                    logical_row,
                    logical_col + offset,
                    crate::protocol::Cell {
                        width: 0,
                        ..crate::protocol::Cell::default()
                    },
                ));
            }
        }
        last_occupied = Some((logical_row, (logical_col + width - 1).min(cols - 1)));
        if width > cols {
            logical_row += 1;
            logical_col = 0;
        } else {
            logical_col += width;
            if logical_col == cols {
                logical_row += 1;
                logical_col = 0;
            }
        }
        utf16_col = utf16_end;
    }
    if preedit
        .selected_range
        .is_some_and(|range| range.location == utf16_col)
    {
        selected_cursor = Some((logical_row, logical_col));
    }
    // default: without a selected preedit range, the engine cursor remains the caret.
    let logical_caret = selected_cursor.unwrap_or(engine_cursor);
    let end_boundary = preedit
        .selected_range
        .is_some_and(|range| range.location == utf16_col);
    let wrap_pending = end_boundary
        && selected_cursor.is_some()
        && logical_caret.1 == 0
        && last_occupied.is_some_and(|(row, _)| row + 1 == logical_caret.0);
    let (caret_row, caret_col) = if wrap_pending {
        last_occupied.expect("wrap-pending caret has an occupied grapheme")
    } else {
        logical_caret
    };
    let logical_row_origin = selected_cursor
        .map(|_| caret_row.saturating_sub(rows.saturating_sub(1)))
        // default: without a selected preedit caret, the viewport keeps its original first row.
        .unwrap_or(0);
    let max_logical_row = placements
        .iter()
        .map(|(row, _, _)| *row)
        .fold(caret_row, usize::max);
    let mut logical_lines = screen.lines.clone();
    if logical_lines.len() < max_logical_row + 1 {
        logical_lines.resize_with(max_logical_row + 1, Vec::new);
    }
    for (row, line) in logical_lines.iter_mut().enumerate() {
        if line.len() > cols {
            return Err(format!(
                "screen row {row} contains {} cells beyond terminal columns {cols}",
                line.len()
            ));
        }
        line.resize(cols, crate::protocol::Cell::default());
    }

    // Clear any committed wide-cell span touched by the overlay before writing preedit cells.
    let base_lines = logical_lines.clone();
    let mut clear_spans = Vec::new();
    for (row, col, _) in &placements {
        let line = &base_lines[*row];
        let cell = &line[*col];
        let span = if cell.width == 0 {
            (0..*col).rev().find_map(|start| {
                let width = usize::from(line[start].width);
                (width > 1 && start + width > *col).then_some((start, start + width))
            })
        } else if cell.width > 1 {
            Some((*col, *col + usize::from(cell.width)))
        } else {
            None
        };
        if let Some((start, end)) = span
            && !clear_spans.contains(&(*row, start, end))
        {
            clear_spans.push((*row, start, end));
        }
    }
    for (row, start, end) in clear_spans {
        for cell in logical_lines[row].iter_mut().take(end).skip(start) {
            *cell = crate::protocol::Cell::default();
        }
    }
    for (row, col, cell) in placements {
        logical_lines[row][col] = cell;
    }

    let mut visible_lines = Vec::with_capacity(rows);
    for row in logical_row_origin..logical_row_origin + rows {
        visible_lines.push(
            logical_lines
                .get(row)
                .cloned()
                // default: rows beyond the logical content are blank terminal cells.
                .unwrap_or_else(|| vec![crate::protocol::Cell::default(); cols]),
        );
    }
    screen.lines = visible_lines;
    screen.cursor.row = u16::try_from(caret_row - logical_row_origin)
        .map_err(|_| "preedit caret row exceeds terminal grid".to_string())?;
    screen.cursor.col = u16::try_from(caret_col)
        .map_err(|_| "preedit caret column exceeds terminal grid".to_string())?;
    u32::try_from(logical_row_origin)
        .map_err(|_| "preedit viewport row origin exceeds render metadata".to_string())
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            frame_drop(self.ptr);
        }
    }
}

/// Terminal font. A CoreText font description made from an installed font name or from the system
/// fixed-pitch font.
pub struct TerminalFont {
    ptr: *mut CFrameFont,
}

// A CoreText font description does not change after it is made, so it can be shared between
// threads.
unsafe impl Send for TerminalFont {}
unsafe impl Sync for TerminalFont {}

impl TerminalFont {
    /// The user's system fixed-pitch font.
    pub fn system_monospace() -> Result<Self, String> {
        let ptr = unsafe { frame_font_system_monospace() };
        if ptr.is_null() {
            return Err("the system fixed-pitch font is unavailable".to_string());
        }
        Ok(Self { ptr })
    }

    /// The installed font whose family name matches exactly. None if there is no such font.
    pub fn installed(family: &str) -> Option<Self> {
        // silent: a family name with a NUL byte names no installed font, and None says so.
        let name = std::ffi::CString::new(family).ok()?;
        let ptr = unsafe { frame_font_named(name.as_ptr()) };
        (!ptr.is_null()).then_some(Self { ptr })
    }

    /// The family name of the font.
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

/// The font selected from a family priority list joined with `;`.
pub struct FontSelection {
    pub font: std::sync::Arc<TerminalFont>,
    /// A family that was skipped because it is not installed.
    pub skipped: Vec<String>,
    /// Whether no family in the list is installed, so the system fixed-pitch font was used.
    pub system: bool,
}

/// Selects the first installed family in the family list. If none is installed, the system
/// fixed-pitch font is used. An error is returned when the list has no family.
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

/// Reads the system fixed-pitch font. The service calls this at startup, and it does not start when
/// the font cannot be read.
pub fn load_default_font() -> Result<(), String> {
    if DEFAULT_FONT.get().is_none() {
        let font = std::sync::Arc::new(TerminalFont::system_monospace()?);
        DEFAULT_FONT.get_or_init(|| font);
    }
    Ok(())
}

/// The font before any font request. It is the system fixed-pitch font, which the service reads
/// first at startup with load_default_font.
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
    pub ascent: f64,
    pub descent: f64,
    pub leading: f64,
    pub font: std::sync::Arc<TerminalFont>,
}

/// The metrics of the default font.
pub fn metrics(font_size: f32, scale: f32) -> Metrics {
    metrics_for(&default_font(), font_size, scale)
        .expect("the system fixed-pitch font has cell metrics")
}

/// The metrics of a font. A font whose cell width or height cannot be measured is an error.
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
        ascent: c_metrics.ascent,
        descent: c_metrics.descent,
        leading: c_metrics.leading,
        font: font.clone(),
    })
}

fn parse_hex_color(color_opt: &Option<String>) -> ([u8; 3], bool) {
    if let Some(color_str) = color_opt
        && color_str.starts_with('#')
        && color_str.len() == 7
        && let Ok(val) = u32::from_str_radix(&color_str[1..], 16)
    {
        let r = ((val >> 16) & 0xFF) as u8;
        let g = ((val >> 8) & 0xFF) as u8;
        let b = (val & 0xFF) as u8;
        return ([r, g, b], true);
    }
    ([0, 0, 0], false)
}
