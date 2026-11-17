// A draw releases the objects that it creates before it returns: the thread that draws in the sidecar has no autorelease
// pool of its own, so an object that a draw leaves to an outer pool stays allocated for the life of the thread.
use soksak_sidecar_vt_core::TerminalTheme;
use soksak_sidecar_vt_core::platform::darwin::frame::{
    CursorRender, Frame, UnfocusedCursor, metrics,
};
use soksak_sidecar_vt_core::protocol::{Cell, Cursor, CursorShape, Screen};

#[repr(C)]
#[derive(Default)]
struct MallocStatistics {
    blocks_in_use: u32,
    size_in_use: usize,
    max_size_in_use: usize,
    size_allocated: usize,
}

unsafe extern "C" {
    fn malloc_zone_statistics(zone: *mut std::ffi::c_void, stats: *mut MallocStatistics);
}

fn blocks_in_use() -> u32 {
    let mut stats = MallocStatistics::default();
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
    stats.blocks_in_use
}

// A screen whose text holds its number, so every row differs from the row of the screen before it. The draws cycle through
// SCREENS screens, so a cache of the text that the system keeps is full after the first cycles and the draws that the test
// counts add no text to it.
const SCREENS: usize = 10;

fn screen(draw: usize) -> Screen {
    let cols = 20;
    let rows = 40;
    let text = format!("{draw:020}");
    let lines = (0..rows)
        .map(|_| {
            text.chars()
                .map(|ch| Cell {
                    ch: Some(ch.to_string().into()),
                    ..Cell::default()
                })
                .collect()
        })
        .collect();
    Screen {
        cols,
        rows,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: None,
        },
        scrollback: Default::default(),
        background: "#1e1e1e".to_string(),
        lines,
    }
}

#[test]
fn repeated_strip_draws_keep_no_allocation() {
    // A new thread has no autorelease pool, as the drawing thread of the sidecar.
    let (before, after, draws) = std::thread::spawn(|| {
        let metrics = metrics(13.0, 2.0);
        let width = metrics.cell_width as u32 * 20;
        let height = metrics.cell_height as u32 * 40;
        let theme = TerminalTheme::dark();
        let cursor = CursorRender {
            visible: true,
            focused: true,
            blink_visible: true,
            shape: CursorShape::Block,
            unfocused: UnfocusedCursor::Hollow,
        };
        let frame = Frame::new_strip(width, height).expect("strip frame");
        let screens: Vec<Screen> = (0..SCREENS).map(screen).collect();
        for screen in screens.iter().cycle().take(2 * SCREENS) {
            frame
                .draw_with_theme(screen, &metrics, cursor, &theme)
                .expect("draw");
        }
        let before = blocks_in_use();
        let draws = 200;
        for screen in screens.iter().cycle().take(draws) {
            frame
                .draw_with_theme(screen, &metrics, cursor, &theme)
                .expect("draw");
        }
        (before, blocks_in_use(), draws)
    })
    .join()
    .expect("drawing thread");
    let kept = after as i64 - before as i64;
    println!("{kept} allocated blocks remain after {draws} strip draws of 40 rows");
    // A draw that leaves one object to an outer pool keeps one block per draw.
    assert!(
        kept < draws as i64,
        "{kept} allocated blocks remain after {draws} strip draws of 40 rows"
    );
}
