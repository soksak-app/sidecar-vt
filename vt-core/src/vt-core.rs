// Common VT layer: protocol, session lifetime, screen types, and the Engine trait
pub mod arguments;
pub mod directory_uri;
pub mod encoding;
pub mod engine_trace;
pub mod events;
pub mod guard;
pub mod handoff;
pub mod inline_image;
pub mod locale;
pub mod messaging;
pub mod palette;
pub mod platform;
pub mod policy;
pub mod protocol;
pub mod pty;
pub mod screen_hash;
pub mod screen_stream;
pub mod service;
pub mod shell_integration;
pub mod shell_state;
pub mod stall;
pub mod thread;

pub use palette::{
    DEFAULT_BACKGROUND_HEX, DEFAULT_BACKGROUND_RGB, DEFAULT_CURSOR_RGB, DEFAULT_FOREGROUND_HEX,
    DEFAULT_FOREGROUND_RGB, DEFAULT_PALETTE, LIGHT_PALETTE, TerminalTheme, ThemeMode,
    default_terminal_color, parse_hex,
};
pub use protocol::{
    AlternateRows, Cell, ClipboardSelection, Cursor, CursorShape, DaemonEvent, Engine, EngineEvent,
    Foreground, InlineAnchor, LocalSessionPort, Modes, Preedit, Rgb, RowScreen, Screen, Scrollback,
    SessionPort, ShellMarker, ShellRequest, make_default_session_port_factory, serve,
};
