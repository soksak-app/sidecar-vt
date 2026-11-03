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
    lines[0][1].bg = Some(soksak_sidecar_vt_core::Rgb([0xcc, 0x33, 0x11]));
    lines[0][2].bg = Some(soksak_sidecar_vt_core::Rgb([0xcc, 0x33, 0x11]));
    lines[1][3].fg = Some(soksak_sidecar_vt_core::Rgb([0x11, 0xaa, 0x44]));
    lines[1][3].inverse = true;
    lines[2][0].bg = Some(soksak_sidecar_vt_core::Rgb([0x22, 0x55, 0xee]));
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
fn the_strip_frame_fills_backgrounds_and_the_cursor_with_the_pixels_of_the_coregraphics_frame() {
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
        let coregraphics = Frame::new(width, height).expect("coregraphics frame");
        coregraphics
            .draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("coregraphics draw");
        let strip = Frame::new_strip(width, height).expect("strip frame");
        strip
            .draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("strip draw");
        let expected = pixels(&coregraphics, width, height);
        let found = pixels(&strip, width, height);
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
            "{shape:?}: the strip frame differs from the coregraphics frame at (x, y, coregraphics, strip) {differing:?}"
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
            screen.lines[row][col].ch = (!text.is_empty()).then(|| (*text).into());
        }
    }
    screen.lines[1][0].width = 2;
    screen.lines[1][1].width = 0;
    screen.lines[1][2].bg = Some(soksak_sidecar_vt_core::Rgb([0x33, 0x55, 0x77]));
    screen.lines[0][3].underline = true;
    screen.lines[0][5].link = Some("https://example.com".to_string());
    screen.lines[2][4].fg = Some(soksak_sidecar_vt_core::Rgb([0xff, 0xcc, 0x00]));
    screen.lines[2][5].inverse = true;
    screen
}

fn assert_same_pixels(coregraphics: &Frame, strip: &Frame, width: u32, height: u32, what: &str) {
    let expected = pixels(coregraphics, width, height);
    let found = pixels(strip, width, height);
    let differing: Vec<(u32, u32, [u8; 4], [u8; 4])> = expected
        .iter()
        .zip(&found)
        .enumerate()
        .filter(|(_, (left, right))| left != right)
        .map(|(index, (left, right))| (index as u32 % width, index as u32 / width, *left, *right))
        .collect();
    assert!(
        differing.is_empty(),
        "{what}: {} pixels differ, first (x, y, coregraphics, strip) {:?}",
        differing.len(),
        &differing[..differing.len().min(8)]
    );
}

#[test]
fn the_strip_frame_draws_text_with_the_pixels_of_the_coregraphics_frame_across_draws() {
    let metrics = metrics(13.0, 2.0);
    let width = metrics.cell_width as u32 * 6 + 3;
    let height = metrics.cell_height as u32 * 3 + 5;
    let theme = TerminalTheme::dark();
    let strip = Frame::new_strip(width, height).expect("strip frame");
    let mut screen = text_screen();
    // The first draw draws every strip; the second changes one row and moves the block cursor over a glyph; the third
    // adds a preedit, which the layout places in the cells of the cursor row.
    for step in 0..3 {
        if step == 1 {
            screen.lines[2][3].ch = Some("#".into());
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
        let coregraphics = Frame::new(width, height).expect("coregraphics frame");
        coregraphics
            .draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("coregraphics draw");
        strip
            .draw_with_theme(&screen, &metrics, cursor, &theme)
            .expect("strip draw");
        assert_same_pixels(
            &coregraphics,
            &strip,
            width,
            height,
            &format!("draw {step}"),
        );
    }
}

// A 3 x 2 image whose pixels are opaque, translucent and transparent, so a scaled draw blends edges and alpha.
const TRANSLUCENT_PNG: [u8; 85] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x9d, 0x74, 0x66,
    0x1a, 0x00, 0x00, 0x00, 0x1c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x08, 0x1b, 0x80, 0x94, 0x06, 0xc3, 0xff, 0xff, 0x0c, 0x27, 0x40, 0x04, 0x10, 0xfe, 0x07,
    0x00, 0x8e, 0xd5, 0x0c, 0x66, 0x95, 0x20, 0xfa, 0xdc, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn image(x: u32, y: u32, width: u32, height: u32) -> InlineImagePlacement {
    InlineImagePlacement {
        name: "translucent.png".into(),
        data: TRANSLUCENT_PNG.to_vec(),
        x,
        y,
        width,
        height,
        preserve_aspect_ratio: true,
        anchor_row: 0,
        anchor_scroll: 0,
        visible: true,
    }
}

#[test]
fn the_strip_frame_composites_inline_images_with_the_pixels_of_the_coregraphics_frame() {
    let metrics = metrics(13.0, 2.0);
    let cell_width = metrics.cell_width as u32;
    let cell_height = metrics.cell_height as u32;
    let width = cell_width * 6 + 3;
    let height = cell_height * 3 + 5;
    let theme = TerminalTheme::dark();
    let mut screen = text_screen();
    screen.cursor.col = 2;
    screen.cursor.row = 1;
    // The second image has only a width, so its height keeps the aspect ratio and ends between two pixel rows; a hidden
    // image is not drawn.
    let mut hidden = image(0, 0, cell_width * 6, cell_height * 3);
    hidden.visible = false;
    let images = [
        image(cell_width, 0, cell_width * 3, cell_height * 2),
        image(cell_width * 3 + 1, cell_height + 3, cell_width * 2 + 1, 0),
        hidden,
    ];
    let cursor = cursor_render(CursorShape::Block);
    let coregraphics = Frame::new(width, height).expect("coregraphics frame");
    coregraphics
        .draw_with_theme_and_inline_images(&screen, &metrics, cursor, &theme, &images)
        .expect("coregraphics draw");
    let strip = Frame::new_strip(width, height).expect("strip frame");
    strip
        .draw_with_theme_and_inline_images(&screen, &metrics, cursor, &theme, &images)
        .expect("strip draw");
    assert_same_pixels(&coregraphics, &strip, width, height, "inline images");
}

#[test]
fn the_strip_frame_refuses_an_inline_image_outside_the_frame_as_the_coregraphics_frame_does() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32 * 6;
    let height = metrics.cell_height as u32 * 3;
    let screen = filled_screen(CursorShape::Block);
    let images = [image(width - 2, 0, 4, 4)];
    let cursor = cursor_render(CursorShape::Block);
    let theme = TerminalTheme::dark();
    let coregraphics = Frame::new(width, height).expect("coregraphics frame");
    let strip = Frame::new_strip(width, height).expect("strip frame");
    let coregraphics_error = coregraphics
        .draw_with_theme_and_inline_images(&screen, &metrics, cursor, &theme, &images)
        .expect_err("coregraphics refuses");
    let strip_error = strip
        .draw_with_theme_and_inline_images(&screen, &metrics, cursor, &theme, &images)
        .expect_err("strip refuses");
    assert_eq!(strip_error, coregraphics_error);
}

#[cfg(feature = "diagnostics")]
#[test]
fn the_strip_frame_draws_only_the_strips_of_changed_rows() {
    let metrics = metrics(13.0, 1.0);
    let frame = Frame::new_strip(
        metrics.cell_width as u32 * 6,
        metrics.cell_height as u32 * 3,
    )
    .expect("strip frame");
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
    screen.lines[2][3].ch = Some("#".into());
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

// A frame draws into a surface that the host does not hold. Holding the drawn surface moves the next draw to another
// surface, and releasing it lets a later draw use it again; both frames keep this pool.
// The image surface of a session draws on the strip frame, which redraws only the rows that changed; the CoreGraphics frame
// draws every cell on every frame.
#[cfg(feature = "diagnostics")]
#[test]
fn an_image_surface_of_a_session_draws_on_the_strip_frame() {
    use soksak_sidecar_vt_core::platform::darwin::frame::default_font;
    use soksak_sidecar_vt_core::platform::{ImageConfiguration, ImageState};
    let configuration = ImageConfiguration {
        name: "view".to_string(),
        generation: 1,
        raster: 1,
        width: 120,
        height: 90,
        scale: 1.0,
    };
    let state = ImageState::new(&configuration, &default_font(), 13.0, false).expect("image state");
    let screen = filled_screen(CursorShape::Block);
    let theme = TerminalTheme::dark();
    state
        .frame
        .draw_with_theme(
            &screen,
            &state.metrics,
            cursor_render(CursorShape::Block),
            &theme,
        )
        .expect("first draw");
    assert_eq!(
        state.frame.strips_drawn(),
        u32::from(screen.rows),
        "the first draw of the image surface draws every row strip"
    );
}
