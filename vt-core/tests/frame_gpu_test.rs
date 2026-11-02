use soksak_sidecar_vt_core::TerminalTheme;
use soksak_sidecar_vt_core::platform::darwin::frame::{
    CursorRender, Frame, UnfocusedCursor, metrics,
};
use soksak_sidecar_vt_core::protocol::{
    Cell, Cursor, CursorShape, InlineImagePlacement, Preedit, Screen,
};

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

// A screen with text in the forms that the strips draw: narrow and wide characters, a wide character next to a filled
// cell, inverse and colored text, an underline, a link and a stack of combining marks.
fn text_screen() -> Screen {
    let mut screen = filled_screen(CursorShape::Block);
    let texts = [
        ["a", "W", "g", "@", "", "~"],
        ["한", "", "x", "e\u{301}\u{316}", "Q", "y"],
        ["0", "", "", "|", "_", "j"],
    ];
    for (row, line) in texts.iter().enumerate() {
        for (col, text) in line.iter().enumerate() {
            screen.lines[row][col].ch = (!text.is_empty()).then(|| text.to_string());
        }
    }
    screen.lines[1][0].width = 2;
    screen.lines[1][1].width = 0;
    screen.lines[1][2].bg = Some("#335577".to_string());
    screen.lines[0][3].underline = true;
    screen.lines[0][5].link = Some("https://example.com".to_string());
    screen.lines[2][4].fg = Some("#ffcc00".to_string());
    screen.lines[2][5].inverse = true;
    screen
}

fn assert_same_pixels(cpu: &Frame, gpu: &Frame, width: u32, height: u32, what: &str) {
    let expected = pixels(cpu, width, height);
    let found = pixels(gpu, width, height);
    let differing: Vec<(u32, u32, [u8; 4], [u8; 4])> = expected
        .iter()
        .zip(&found)
        .enumerate()
        .filter(|(_, (left, right))| left != right)
        .map(|(index, (left, right))| (index as u32 % width, index as u32 / width, *left, *right))
        .collect();
    assert!(
        differing.is_empty(),
        "{what}: {} pixels differ, first (x, y, cpu, gpu) {:?}",
        differing.len(),
        &differing[..differing.len().min(8)]
    );
}

#[test]
fn the_gpu_frame_draws_text_with_the_pixels_of_the_cpu_frame_across_draws() {
    let metrics = metrics(13.0, 2.0);
    let width = metrics.cell_width as u32 * 6 + 3;
    let height = metrics.cell_height as u32 * 3 + 5;
    let theme = TerminalTheme::dark();
    let gpu = Frame::new_gpu(width, height).expect("gpu frame");
    let mut screen = text_screen();
    // The first draw draws every strip; the second changes one row and moves the block cursor over a glyph; the third
    // adds a preedit, which the layout places in the cells of the cursor row.
    for step in 0..3 {
        if step == 1 {
            screen.lines[2][3].ch = Some("#".to_string());
            screen.cursor.col = 4;
            screen.cursor.row = 1;
        }
        if step == 2 {
            screen.cursor.preedit = Some(Preedit {
                text: "한".to_string(),
                marked_range: None,
                selected_range: None,
                replacement_range: None,
                attributed: false,
            });
        }
        let cursor = cursor_render(CursorShape::Block);
        let cpu = Frame::new(width, height).expect("cpu frame");
        cpu.draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("cpu draw");
        gpu.draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("gpu draw");
        assert_same_pixels(&cpu, &gpu, width, height, &format!("draw {step}"));
    }
}

#[test]
fn the_gpu_frame_refuses_an_inline_image_that_it_does_not_draw() {
    let metrics = metrics(13.0, 1.0);
    let screen = filled_screen(CursorShape::Block);
    let png = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let image = InlineImagePlacement {
        name: "red.png".into(),
        data: png.to_vec(),
        x: 0,
        y: 0,
        width: metrics.cell_width as u32,
        height: metrics.cell_height as u32,
        preserve_aspect_ratio: true,
        anchor_row: 0,
        anchor_scroll: 0,
        visible: true,
    };
    let frame = Frame::new_gpu(
        metrics.cell_width as u32 * 6,
        metrics.cell_height as u32 * 3,
    )
    .expect("gpu frame");
    let error = frame
        .draw_with_theme_and_inline_images(
            &screen,
            &metrics,
            cursor_render(CursorShape::Block),
            &TerminalTheme::dark(),
            &[image],
        )
        .expect_err("an inline image is refused");
    assert_eq!(error, "the GPU frame draws no inline image");
}

#[cfg(feature = "diagnostics")]
#[test]
fn the_gpu_frame_draws_only_the_strips_of_changed_rows() {
    let metrics = metrics(13.0, 1.0);
    let frame = Frame::new_gpu(
        metrics.cell_width as u32 * 6,
        metrics.cell_height as u32 * 3,
    )
    .expect("gpu frame");
    let theme = TerminalTheme::dark();
    let mut screen = text_screen();
    let cursor = cursor_render(CursorShape::Block);
    frame
        .draw_with_theme(&screen, &metrics, cursor, &theme)
        .expect("first draw");
    assert_eq!(frame.strips_drawn(), 3, "the first draw draws every row");
    frame
        .draw_with_theme(&screen, &metrics, cursor, &theme)
        .expect("same draw");
    assert_eq!(
        frame.strips_drawn(),
        0,
        "an unchanged screen draws no strip"
    );
    screen.lines[2][3].ch = Some("#".to_string());
    screen.cursor.row = 2;
    frame
        .draw_with_theme(&screen, &metrics, cursor, &theme)
        .expect("changed draw");
    assert_eq!(
        frame.strips_drawn(),
        1,
        "a change of one row and of the cursor draws one strip"
    );
}
