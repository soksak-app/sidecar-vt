pub mod darwin;
pub mod platform;
pub mod pty;

pub use platform::service;
pub use platform::ImageState;
pub use platform::{
    default_font, metrics, metrics_for, resolve_font_list, FontSelection, TerminalFont,
};

// Frame과 metrics는 platform별이며 platform.rs가 조건부로 export한다
// protocol module은 platform 차이를 처리하는 ImageState를 사용한다
