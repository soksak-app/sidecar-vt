pub mod darwin;
pub mod pty;

#[cfg(target_os = "macos")]
pub use crate::platform::darwin::service;

#[cfg(target_os = "macos")]
pub use crate::platform::darwin::frame::{
    FontSelection, Frame, Metrics, TerminalFont, default_font, metrics, metrics_for,
    resolve_font_list,
};

#[cfg(target_os = "macos")]
pub struct ImageState {
    pub name: String,
    pub generation: u64,
    pub raster: u64,
    pub frame: Frame,
    pub metrics: Metrics,
    pub sequence: u32,
    pub render: Option<serde_json::Value>,
    pub pending_draw: bool,
    pub dirty: bool,
    /// Whether the host draws the region (core docs/spec/native-surfaces.md#image-regions). While
    /// it does not, the surface draws no raster and sends no screen, and changes stay dirty.
    pub drawing: bool,
    pub width_px: u32,
    pub height_px: u32,
    pub scale: f32,
    pub theme: crate::palette::TerminalTheme,
    pub inline_images: Vec<crate::protocol::InlineImagePlacement>,
    /// The page events sent with the next presentation. The state of an inline image changes
    /// together with the raster that drew it.
    pub presentation_events: Vec<String>,
    /// The time the pending transfer was sent. This is the instrument of the consumed wait time
    /// (V5-104).
    pub sent_at: Option<std::time::Instant>,
    /// The store of the connection that carries a transfer image awaiting a reply. If the surface
    /// releases this state before the reply, it hands the transfer image to that store.
    pub carrier: Option<std::sync::Arc<crate::protocol::TransferStore>>,
    /// The session whose screen this raster draws, which the event of a frame names.
    pub session: Option<String>,
}

/// The raster that the host sets with configure: name, generation, raster number, pixel size, and
/// scale.
#[derive(Debug, Clone)]
pub struct ImageConfiguration {
    pub name: String,
    pub generation: u64,
    pub raster: u64,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

#[cfg(target_os = "macos")]
impl ImageState {
    pub fn new(
        configuration: &ImageConfiguration,
        font: &std::sync::Arc<TerminalFont>,
        font_size: f32,
    ) -> Result<ImageState, String> {
        let ImageConfiguration {
            name,
            generation,
            raster,
            width: width_px,
            height: height_px,
            scale,
        } = configuration.clone();
        let frame = Frame::new(width_px, height_px)
            .ok_or_else(|| format!("IOSurface creation failed for {width_px}x{height_px}"))?;
        let device_metrics = metrics_for(font, font_size, scale)?;
        Ok(ImageState {
            name,
            generation,
            raster,
            frame,
            metrics: device_metrics,
            sequence: 0,
            render: None,
            pending_draw: false,
            dirty: false,
            drawing: true,
            width_px,
            height_px,
            scale,
            theme: crate::palette::TerminalTheme::dark(),
            inline_images: Vec::new(),
            presentation_events: Vec::new(),
            sent_at: None,
            carrier: None,
            session: None,
        })
    }

    pub fn selection_cell(&self, x: f64, y: f64) -> Result<(u16, u16), String> {
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
            return Err("selection coordinates must be finite and non-negative".to_string());
        }
        let cell_width = f64::from(self.metrics.cell_width) / f64::from(self.scale);
        let cell_height = f64::from(self.metrics.cell_height) / f64::from(self.scale);
        if cell_width <= 0.0 || cell_height <= 0.0 {
            return Err("selection cell metrics are unavailable".to_string());
        }
        let width = f64::from(self.width_px) / f64::from(self.scale);
        let height = f64::from(self.height_px) / f64::from(self.scale);
        // The host aligns the edges of the region to device pixels, so the region can be smaller
        // than the page view by less than one device pixel. Points in that band are in the last
        // column and row, like the margin of the region.
        let band = 1.0 / f64::from(self.scale);
        if x >= width + band || y >= height + band {
            return Err(format!(
                "selection coordinates are outside the terminal region: {x},{y}"
            ));
        }
        let cols = (self.width_px as f32 / self.metrics.cell_width) as u16;
        let rows = (self.height_px as f32 / self.metrics.cell_height) as u16;
        if cols == 0 || rows == 0 {
            return Err("the terminal region holds no whole cell".to_string());
        }
        // The region has a margin smaller than one cell after the last complete column and row. A
        // point in the margin belongs to the nearest cell.
        let col = ((x / cell_width).floor() as u16).min(cols - 1);
        let row = ((y / cell_height).floor() as u16).min(rows - 1);
        Ok((col, row))
    }

    /// The cell boundary and row of a selection point. The point moves to the nearest cell boundary
    /// in its row, so the pointer must pass the horizontal middle of a cell for that cell to be
    /// selected. Boundaries run from 0 to the column count, and the margin after the last column is
    /// the right boundary of the last column.
    pub fn selection_edge(&self, x: f64, y: f64) -> Result<(u16, u16), String> {
        let (_, row) = self.selection_cell(x, y)?;
        let cell_width = f64::from(self.metrics.cell_width) / f64::from(self.scale);
        let cols = (self.width_px as f32 / self.metrics.cell_width) as u16;
        let edge = ((x / cell_width + 0.5).floor() as u16).min(cols);
        Ok((edge, row))
    }
}

#[cfg(not(target_os = "macos"))]
pub struct ImageState;

#[cfg(not(target_os = "macos"))]
impl ImageState {
    pub fn new(
        _name: String,
        _generation: u64,
        _raster: u64,
        _width_px: u32,
        _height_px: u32,
        _scale: f32,
    ) -> Result<ImageState, String> {
        Err("terminal images are not implemented on this operating system".to_string())
    }

    pub fn selection_cell(&self, _x: f64, _y: f64) -> Result<(u16, u16), String> {
        Err("terminal selection is not implemented on this operating system".to_string())
    }

    pub fn selection_edge(&self, _x: f64, _y: f64) -> Result<(u16, u16), String> {
        Err("terminal selection is not implemented on this operating system".to_string())
    }
}
