pub mod darwin;
pub mod pty;

#[cfg(target_os = "macos")]
pub use crate::platform::darwin::service;

#[cfg(target_os = "macos")]
pub use crate::platform::darwin::frame::{
    default_font, metrics, metrics_for, resolve_font_list, FontSelection, Frame, Metrics,
    TerminalFont,
};

#[cfg(target_os = "macos")]
pub struct ImageState {
    pub name: String,
    pub generation: u64,
    pub raster: u64,
    pub frame: Frame,
    pub metrics: Metrics,
    pub sequence: u32,
    pub pending_draw: bool,
    pub dirty: bool,
    pub width_px: u32,
    pub height_px: u32,
    pub scale: f32,
    pub theme: crate::palette::TerminalTheme,
    pub inline_images: Vec<crate::protocol::InlineImagePlacement>,
    /// 다음 표시와 함께 보낼 페이지 이벤트. 인라인 그림의 상태는 그 그림을 그린 래스터와 함께 바뀐다.
    pub presentation_events: Vec<String>,
    /// 대기 중인 전송을 보낸 시각. consumed 대기 시간의 계기다(V5-104).
    pub sent_at: Option<std::time::Instant>,
}

/// 호스트가 configure 로 정한 래스터: 이름, 세대, 래스터 번호, 픽셀 크기, 배율.
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
            pending_draw: false,
            dirty: false,
            width_px,
            height_px,
            scale,
            theme: crate::palette::TerminalTheme::dark(),
            inline_images: Vec::new(),
            presentation_events: Vec::new(),
            sent_at: None,
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
        if x >= width || y >= height {
            return Err(format!(
                "selection coordinates are outside the terminal region: {x},{y}"
            ));
        }
        let cols = (self.width_px as f32 / self.metrics.cell_width) as u16;
        let rows = (self.height_px as f32 / self.metrics.cell_height) as u16;
        if cols == 0 || rows == 0 {
            return Err("the terminal region holds no whole cell".to_string());
        }
        // 영역은 마지막 완전한 열과 행 뒤에 한 칸보다 작은 여백을 가진다. 여백의 점은 가장 가까운 칸이다.
        let col = ((x / cell_width).floor() as u16).min(cols - 1);
        let row = ((y / cell_height).floor() as u16).min(rows - 1);
        Ok((col, row))
    }

    /// 선택 점의 칸 경계와 행. 점은 그 행의 가장 가까운 칸 경계로 가므로, 포인터가 칸의 가로 가운데를 지나야
    /// 그 칸이 선택에 든다. 경계는 0부터 열 수까지이고, 마지막 열 뒤의 여백은 마지막 열의 오른쪽 경계다.
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
