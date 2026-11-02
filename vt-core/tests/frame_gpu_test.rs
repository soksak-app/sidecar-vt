use soksak_sidecar_vt_core::TerminalTheme;
use soksak_sidecar_vt_core::platform::darwin::frame::{
    CursorRender, Frame, UnfocusedCursor, metrics,
};
use soksak_sidecar_vt_core::protocol::{Cell, Cursor, CursorShape, Screen};

// A screen of colored, inverse and default cells without text, so the frame draws only fills.
fn filled_screen(shape: CursorShape) -> Screen {
    let cols = 6;
    let rows = 3;
    let mut lines: Vec<Vec<Cell>> = (0..rows)
        .map(|_| (0..cols).map(|_| Cell::default()).collect())
        .collect();
    lines[0][1].bg = Some("#cc3311".to_string());
    lines[0][2].bg = Some("#cc3311".to_string());
    lines[1][3].fg = Some("#11aa44".to_string());
    lines[1][3].inverse = true;
    lines[2][0].bg = Some("#2255ee".to_string());
    lines[2][0].width = 2;
    lines[2][1].width = 0;
    lines[2][5].inverse = true;
    Screen {
        cols,
        rows,
        cursor: Cursor {
            col: 1,
            row: 0,
            shape,
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

fn cursor_render(shape: CursorShape) -> CursorRender {
    CursorRender {
        visible: true,
        focused: true,
        blink_visible: true,
        shape,
        unfocused: UnfocusedCursor::Hollow,
    }
}

fn pixels(frame: &Frame, width: u32, height: u32) -> Vec<[u8; 4]> {
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| frame.read_pixel(x, y).expect("pixel"))
        .collect()
}

#[test]
fn the_gpu_frame_fills_backgrounds_and_the_cursor_with_the_pixels_of_the_cpu_frame() {
    let metrics = metrics(13.0, 2.0);
    let width = metrics.cell_width as u32 * 6 + 3;
    let height = metrics.cell_height as u32 * 3 + 5;
    let theme = TerminalTheme::dark();
    for shape in [
        CursorShape::Block,
        CursorShape::Underline,
        CursorShape::Beam,
        CursorShape::HollowBlock,
        CursorShape::Hidden,
    ] {
        let screen = filled_screen(shape);
        let cursor = cursor_render(shape);
        let cpu = Frame::new(width, height).expect("cpu frame");
        cpu.draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("cpu draw");
        let gpu = Frame::new_gpu(width, height).expect("gpu frame");
        gpu.draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("gpu draw");
        let expected = pixels(&cpu, width, height);
        let found = pixels(&gpu, width, height);
        let differing: Vec<(u32, u32, [u8; 4], [u8; 4])> = expected
            .iter()
            .zip(&found)
            .enumerate()
            .filter(|(_, (left, right))| left != right)
            .map(|(index, (left, right))| {
                (index as u32 % width, index as u32 / width, *left, *right)
            })
            .take(8)
            .collect();
        assert!(
            differing.is_empty(),
            "{shape:?}: the gpu frame differs from the cpu frame at (x, y, cpu, gpu) {differing:?}"
        );
    }
}

#[test]
fn the_gpu_frame_refuses_a_screen_with_text_that_it_does_not_draw() {
    let metrics = metrics(13.0, 1.0);
    let mut screen = filled_screen(CursorShape::Block);
    screen.lines[0][0].ch = Some("a".to_string());
    let frame = Frame::new_gpu(
        metrics.cell_width as u32 * 6,
        metrics.cell_height as u32 * 3,
    )
    .expect("gpu frame");
    let error = frame
        .draw_with_theme(
            &screen,
            &metrics,
            cursor_render(CursorShape::Block),
            &TerminalTheme::dark(),
        )
        .expect_err("a screen with text is refused");
    assert_eq!(
        error,
        "the GPU frame draws no text, underline or inline image"
    );
}
