// 공통 VT 계층: 프로토콜, 세션 수명, 화면 타입, Engine 트레이트
pub mod directory_uri;
pub mod encoding;
pub mod engine_trace;
pub mod inline_image;
pub mod locale;
pub mod palette;
pub mod performance;
pub mod platform;
pub mod protocol;
pub mod pty;
pub mod service;
pub mod shell_integration;

pub use palette::{
    default_terminal_color, parse_hex, TerminalTheme, ThemeMode, DEFAULT_BACKGROUND_HEX,
    DEFAULT_BACKGROUND_RGB, DEFAULT_CURSOR_RGB, DEFAULT_FOREGROUND_HEX, DEFAULT_FOREGROUND_RGB,
    DEFAULT_PALETTE, LIGHT_PALETTE,
};
pub use protocol::{
    make_default_session_port_factory, serve, Cell, ClipboardSelection, Cursor, CursorShape,
    DaemonEvent, Engine, EngineEvent, InlineAnchor, LocalSessionPort, Modes, Preedit, Screen,
    Scrollback, SessionPort, ShellMarker, ShellRequest,
};
