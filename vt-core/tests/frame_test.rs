use soksak_sidecar_vt_core::platform::darwin::frame::{
    cursor_blink_visible, effective_cursor_shape, metrics, CursorBlinkPolicy, CursorRender, Frame,
    UnfocusedCursor,
};
use soksak_sidecar_vt_core::protocol::{
    Cell, Cursor, CursorShape, InlineImagePlacement, JsonRange, Preedit, Screen,
};
use soksak_sidecar_vt_core::TerminalTheme;

fn screen(cols: u16, rows: u16) -> Screen {
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
            focused: false,
            preedit: None,
        },
        scrollback: Default::default(),
        background: "#1e1e1e".to_string(),
        lines: (0..rows)
            .map(|_| (0..cols).map(|_| Cell::default()).collect())
            .collect(),
    }
}

#[test]
fn inline_image_raster_is_composited_without_erasing_terminal_background() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32 * 4;
    let height = metrics.cell_height as u32;
    let png = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let placement = InlineImagePlacement {
        name: "red.png".into(),
        data: png.to_vec(),
        x: metrics.cell_width as u32,
        y: 0,
        width: metrics.cell_width as u32,
        height: metrics.cell_height as u32,
        preserve_aspect_ratio: true,
        anchor_row: 0,
        anchor_scroll: 0,
        visible: true,
    };
    let frame = Frame::new(width, height).expect("frame");
    frame
        .draw_with_theme_and_inline_images(
            &screen(4, 1),
            &metrics,
            CursorRender {
                visible: false,
                ..CursorRender::default()
            },
            &TerminalTheme::dark(),
            &[placement],
        )
        .expect("inline image composite");
    let image_pixel = frame
        .read_pixel(
            metrics.cell_width as u32 + metrics.cell_width as u32 / 2,
            height / 2,
        )
        .expect("image pixel");
    assert!(
        image_pixel[2] > image_pixel[0],
        "expected red image pixel: {image_pixel:?}"
    );
    let background = frame
        .read_pixel(metrics.cell_width as u32 / 2, height / 2)
        .expect("background");
    assert!(background[0] < 80 && background[1] < 80 && background[2] < 80);
}

fn bright(frame: &Frame, width: u32, height: u32) -> usize {
    (0..width)
        .flat_map(|x| (0..height).map(move |y| (x, y)))
        .filter_map(|(x, y)| frame.read_pixel(x, y))
        .filter(|pixel| pixel[0] > 100 || pixel[1] > 100 || pixel[2] > 100)
        .count()
}

fn bright_region(frame: &Frame, x0: u32, x1: u32, y0: u32, y1: u32) -> usize {
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter_map(|(x, y)| frame.read_pixel(x, y))
        .filter(|pixel| pixel[0] > 100 || pixel[1] > 100 || pixel[2] > 100)
        .count()
}

fn pixels(frame: &Frame, width: u32, height: u32) -> Vec<[u8; 4]> {
    (0..width)
        .flat_map(|x| (0..height).map(move |y| (x, y)))
        .map(|(x, y)| frame.read_pixel(x, y).expect("pixel in frame"))
        .collect()
}

#[test]
fn cursor_styles_and_row_coordinates_are_pixel_distinct() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(1, 3);
    state.cursor.row = 2;
    state.cursor.focused = true;
    let frame = Frame::new(width, height * 3).expect("frame");
    frame.draw(&state, &metrics).expect("cursor");
    assert!(bright(&frame, width, height * 3) > 0);
    let row_zero = frame.read_pixel(width / 2, height / 2).expect("row zero");
    let row_two = frame
        .read_pixel(width / 2, height * 2 + height / 2)
        .expect("row two");
    assert!(row_zero[0] < 100 && row_zero[1] < 100 && row_zero[2] < 100);
    assert!(row_two[0] > 100 || row_two[1] > 100 || row_two[2] > 100);

    let mut text_state = screen(1, 1);
    text_state.cursor.focused = true;
    text_state.lines[0][0].ch = Some("X".to_string());
    let text_frame = Frame::new(width, height).expect("text frame");
    text_frame
        .draw(&text_state, &metrics)
        .expect("inverted cursor");
    assert!(
        bright(&text_frame, width, height) < (width * height) as usize,
        "focused block erased the glyph instead of inverting it"
    );

    let hidden = Frame::new(width, height).expect("hidden frame");
    hidden
        .draw_with_cursor(
            &screen(1, 1),
            &metrics,
            CursorRender {
                visible: false,
                ..CursorRender::default()
            },
        )
        .expect("hidden cursor");
    assert_eq!(bright(&hidden, width, height), 0);
}

#[test]
fn complex_utf8_and_cjk_are_not_truncated_or_overlapped() {
    let metrics = metrics(13.0, 1.0);
    let mut state = screen(8, 1);
    state.lines[0][0].ch = Some("👩‍💻".to_string());
    state.lines[0][0].width = 2;
    state.lines[0][2].ch = Some("tail".to_string());
    let frame =
        Frame::new(metrics.cell_width as u32 * 8, metrics.cell_height as u32).expect("frame");
    frame.draw(&state, &metrics).expect("complex grapheme");
    assert!(
        bright(
            &frame,
            metrics.cell_width as u32 * 8,
            metrics.cell_height as u32
        ) > 0
    );

    state.lines[0][0].ch = None;
    state.lines[0][0].width = 1;
    state.lines[0][2].ch = Some("T".to_string());
    state.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        selected_range: None,
        replacement_range: None,
        attributed: false,
    });
    let marked = Frame::new(metrics.cell_width as u32 * 8, metrics.cell_height as u32)
        .expect("marked frame");
    marked
        .draw(&state, &metrics)
        .expect("marked text with tail");
    let tail_bright = bright_region(
        &marked,
        metrics.cell_width as u32 * 2,
        metrics.cell_width as u32 * 3,
        0,
        metrics.cell_height as u32,
    );
    assert!(
        tail_bright > 0,
        "existing text was overwritten by preedit: bright={tail_bright}"
    );
}

#[test]
fn preedit_uses_json_location_length_and_document_replacement_range() {
    let decoded: Preedit = serde_json::from_str(
        r#"{"text":"한글","selectedRange":{"location":2,"length":0},"replacementRange":{"location":400,"length":30},"attributed":true}"#,
    )
    .expect("native JSON range contract");
    assert_eq!(decoded.selected_range.unwrap().location, 2);
    assert_eq!(decoded.replacement_range.unwrap().length, 30);

    let metrics = metrics(13.0, 1.0);
    let mut state = screen(8, 1);
    state.cursor.preedit = Some(Preedit {
        text: "한😀".to_string(),
        selected_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        replacement_range: Some(JsonRange {
            location: 400,
            length: 30,
        }),
        attributed: true,
    });
    let frame =
        Frame::new(metrics.cell_width as u32 * 8, metrics.cell_height as u32).expect("frame");
    frame
        .draw(&state, &metrics)
        .expect("document replacement range is not preedit-bounded");

    state.cursor.preedit.as_mut().unwrap().selected_range = Some(JsonRange {
        location: 0,
        length: 99,
    });
    assert!(frame.draw(&state, &metrics).is_err());
}

#[test]
fn preedit_cursor_follows_selected_utf16_location() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(8, 1);
    state.cursor.focused = true;
    state.cursor.preedit = Some(Preedit {
        text: "글은".to_string(),
        selected_range: Some(JsonRange {
            location: 2,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 8, height).expect("frame");
    frame.draw(&state, &metrics).expect("preedit at end");
    assert!(bright_in_cell(&frame, cell, height, 4) > 0.5);
    assert!(bright_in_cell(&frame, cell, height, 0) < 0.5);

    state.cursor.preedit.as_mut().unwrap().selected_range = Some(JsonRange {
        location: 1,
        length: 0,
    });
    frame
        .draw(&state, &metrics)
        .expect("preedit between syllables");
    assert!(bright_in_cell(&frame, cell, height, 2) > 0.5);

    state.cursor.preedit.as_mut().unwrap().text = "😀글".to_string();
    state.cursor.preedit.as_mut().unwrap().selected_range = Some(JsonRange {
        location: 1,
        length: 0,
    });
    assert!(frame.draw(&state, &metrics).is_err());
}

#[test]
fn cursor_metrics_are_stable_across_style_changes() {
    let before = metrics(13.0, 1.0);
    let frame = Frame::new(before.cell_width as u32, before.cell_height as u32).expect("frame");
    let state = screen(1, 1);
    for shape in [
        CursorShape::Block,
        CursorShape::Underline,
        CursorShape::Beam,
        CursorShape::HollowBlock,
    ] {
        frame
            .draw_with_cursor(
                &state,
                &before,
                CursorRender {
                    focused: true,
                    shape,
                    ..CursorRender::default()
                },
            )
            .expect("style");
    }
    let after = metrics(13.0, 1.0);
    assert_eq!(before.cell_width, after.cell_width);
    assert_eq!(before.cell_height, after.cell_height);
}

#[test]
fn unfocused_cursor_policy_selects_the_requested_shape_without_changing_program_shape() {
    assert_eq!(
        effective_cursor_shape(CursorShape::Block, false, UnfocusedCursor::Hollow),
        CursorShape::HollowBlock
    );
    assert_eq!(
        effective_cursor_shape(CursorShape::Block, false, UnfocusedCursor::Solid),
        CursorShape::Block
    );
    assert_eq!(
        effective_cursor_shape(CursorShape::Block, false, UnfocusedCursor::Underline),
        CursorShape::Underline
    );
    assert_eq!(
        effective_cursor_shape(CursorShape::Block, false, UnfocusedCursor::Beam),
        CursorShape::Beam
    );
    assert_eq!(
        effective_cursor_shape(CursorShape::Underline, false, UnfocusedCursor::Unchanged),
        CursorShape::Underline
    );
    assert_eq!(
        effective_cursor_shape(CursorShape::Beam, true, UnfocusedCursor::Hollow),
        CursorShape::Beam
    );
}

#[test]
fn cursor_blink_policy_observes_program_visibility_and_idle_timeout() {
    assert!(cursor_blink_visible(
        CursorBlinkPolicy::Never,
        true,
        true,
        751,
        750,
        0
    ));
    assert!(cursor_blink_visible(
        CursorBlinkPolicy::Off,
        false,
        true,
        751,
        750,
        0
    ));
    assert!(cursor_blink_visible(
        CursorBlinkPolicy::Off,
        true,
        true,
        0,
        750,
        0
    ));
    assert!(!cursor_blink_visible(
        CursorBlinkPolicy::Off,
        true,
        true,
        750,
        750,
        0
    ));
    assert!(!cursor_blink_visible(
        CursorBlinkPolicy::On,
        false,
        true,
        750,
        750,
        0
    ));
    assert!(cursor_blink_visible(
        CursorBlinkPolicy::Always,
        false,
        true,
        0,
        750,
        5000
    ));
    assert!(cursor_blink_visible(
        CursorBlinkPolicy::Always,
        false,
        true,
        5000,
        750,
        5000
    ));
    assert!(cursor_blink_visible(
        CursorBlinkPolicy::Always,
        false,
        false,
        750,
        750,
        0
    ));
}

#[test]
fn unfocused_cursor_policy_changes_only_cursor_pixels() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(1, 1);
    state.cursor.focused = false;
    let frame = Frame::new(width, height).expect("unfocused cursor frame");

    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                unfocused: UnfocusedCursor::Underline,
                ..CursorRender::default()
            },
        )
        .expect("unfocused underline");
    let underline_middle = frame.read_pixel(width / 2, height / 2).expect("middle");
    let underline_bottom = frame.read_pixel(width / 2, height - 1).expect("bottom");
    assert!(underline_middle[0] < 100 && underline_middle[1] < 100 && underline_middle[2] < 100);
    assert!(underline_bottom[0] > 100 || underline_bottom[1] > 100 || underline_bottom[2] > 100);

    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                unfocused: UnfocusedCursor::Beam,
                ..CursorRender::default()
            },
        )
        .expect("unfocused beam");
    let beam_left = frame.read_pixel(0, height / 2).expect("left");
    let beam_middle = frame
        .read_pixel(width / 2, height / 2)
        .expect("middle beam");
    assert!(beam_left[0] > 100 || beam_left[1] > 100 || beam_left[2] > 100);
    assert!(beam_middle[0] < 100 && beam_middle[1] < 100 && beam_middle[2] < 100);

    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                unfocused: UnfocusedCursor::Unchanged,
                shape: CursorShape::Underline,
                ..CursorRender::default()
            },
        )
        .expect("unchanged program shape");
    let unchanged_bottom = frame
        .read_pixel(width / 2, height - 1)
        .expect("unchanged bottom");
    assert!(unchanged_bottom[0] > 100 || unchanged_bottom[1] > 100 || unchanged_bottom[2] > 100);

    // solid 는 포커스가 없어도 칸 전체를 채운다. hollow 는 가운데를 비운다.
    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                unfocused: UnfocusedCursor::Solid,
                ..CursorRender::default()
            },
        )
        .expect("unfocused solid");
    let solid_middle = frame
        .read_pixel(width / 2, height / 2)
        .expect("solid middle");
    assert!(
        solid_middle[0] > 100 || solid_middle[1] > 100 || solid_middle[2] > 100,
        "an unfocused solid cursor left the cell middle empty: {solid_middle:?}"
    );
    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                unfocused: UnfocusedCursor::Hollow,
                ..CursorRender::default()
            },
        )
        .expect("unfocused hollow");
    let hollow_middle = frame
        .read_pixel(width / 2, height / 2)
        .expect("hollow middle");
    assert!(hollow_middle[0] < 100 && hollow_middle[1] < 100 && hollow_middle[2] < 100);
}

#[test]
fn engine_blink_enablement_does_not_hide_visible_phase() {
    let metrics = metrics(13.0, 1.0);
    let mut state = screen(1, 1);
    state.cursor.focused = true;
    state.cursor.blinking = true;
    let frame = Frame::new(metrics.cell_width as u32, metrics.cell_height as u32).expect("frame");

    frame.draw(&state, &metrics).expect("visible blink phase");
    let first = bright(
        &frame,
        metrics.cell_width as u32,
        metrics.cell_height as u32,
    );
    frame
        .draw(&state, &metrics)
        .expect("same visible blink phase");
    let second = bright(
        &frame,
        metrics.cell_width as u32,
        metrics.cell_height as u32,
    );
    assert!(
        first > 0,
        "enabled blinking cursor disappeared in the visible phase"
    );
    assert_eq!(
        first, second,
        "frame renderer introduced its own blink timing"
    );

    let hidden_phase = Frame::new(metrics.cell_width as u32, metrics.cell_height as u32)
        .expect("hidden phase frame");
    hidden_phase
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                blink_visible: false,
                ..CursorRender::default()
            },
        )
        .expect("scheduler-owned hidden phase");
    assert_eq!(
        bright(
            &hidden_phase,
            metrics.cell_width as u32,
            metrics.cell_height as u32
        ),
        0
    );
}

#[test]
fn cell_raster_is_stable_when_cursor_phase_changes_outside_the_cell() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32 * 4;
    let height = metrics.cell_height as u32 * 2;
    let mut state = screen(4, 2);
    state.lines[0][0].ch = Some("A".to_string());
    state.lines[0][1].ch = Some("한".to_string());
    state.lines[0][1].width = 2;
    state.cursor.row = 1;
    state.cursor.focused = true;
    let frame = Frame::new(width, height).expect("frame");
    frame.draw(&state, &metrics).expect("first raster");
    let first = pixels(&frame, width, metrics.cell_height as u32);
    frame.draw(&state, &metrics).expect("second raster");
    let second = pixels(&frame, width, metrics.cell_height as u32);
    assert_eq!(
        first, second,
        "cell raster changed between identical frames"
    );

    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                blink_visible: false,
                ..CursorRender::default()
            },
        )
        .expect("scheduler hidden phase");
    let hidden_cursor = pixels(&frame, width, metrics.cell_height as u32);
    assert_eq!(
        first, hidden_cursor,
        "cursor phase changed unrelated cell pixels"
    );
}

#[test]
fn terminal_theme_changes_background_foreground_and_cursor_pixels_without_metrics_change() {
    let initial_metrics = metrics(13.0, 1.0);
    let cursor = CursorRender {
        visible: true,
        focused: true,
        blink_visible: true,
        shape: CursorShape::Block,
        unfocused: UnfocusedCursor::Hollow,
    };
    let width = initial_metrics.cell_width as u32;
    let height = initial_metrics.cell_height as u32;
    let mut state = screen(1, 1);
    state.lines[0][0].ch = Some("X".to_string());
    state.cursor.focused = true;
    let frame = Frame::new(width, height).expect("theme frame");

    // 엔진은 테마의 기본 배경을 화면의 기본 배경으로 보고한다.
    let hex = |rgb: [u8; 3]| format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    state.background = hex(TerminalTheme::dark().background);
    frame
        .draw_with_theme(&state, &initial_metrics, cursor, &TerminalTheme::dark())
        .expect("dark theme");
    let dark_background = frame.read_pixel(0, 0).expect("dark background");
    let dark_cursor = frame
        .read_pixel(width / 2, height / 2)
        .expect("dark cursor");

    state.background = hex(TerminalTheme::light().background);
    frame
        .draw_with_theme(&state, &initial_metrics, cursor, &TerminalTheme::light())
        .expect("light theme");
    let light_background = frame.read_pixel(0, 0).expect("light background");
    let light_cursor = frame
        .read_pixel(width / 2, height / 2)
        .expect("light cursor");

    assert_ne!(dark_background, light_background);
    assert_ne!(dark_cursor, light_cursor);
    let unchanged = metrics(13.0, 1.0);
    assert_eq!(initial_metrics.cell_width, unchanged.cell_width);
    assert_eq!(initial_metrics.cell_height, unchanged.cell_height);
    assert_eq!(initial_metrics.font_size, unchanged.font_size);
}

fn bright_in_cell(frame: &Frame, metrics_width: u32, height: u32, cell: u32) -> f64 {
    let start = metrics_width * cell;
    let bright = (start..start + metrics_width)
        .flat_map(|x| (0..height).map(move |y| (x, y)))
        .filter_map(|(x, y)| frame.read_pixel(x, y))
        .filter(|pixel| pixel[0] > 100 || pixel[1] > 100 || pixel[2] > 100)
        .count();
    bright as f64 / (metrics_width * height) as f64
}

#[test]
fn block_cursor_covers_the_full_width_of_the_character_under_it() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;

    // 조합 중인 넓은 글자: 커서는 두 칸을 모두 덮는다.
    let mut wide_preedit = screen(4, 1);
    wide_preedit.cursor.focused = true;
    wide_preedit.cursor.preedit = Some(Preedit {
        text: "글".to_string(),
        selected_range: None,
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame.draw(&wide_preedit, &metrics).expect("wide preedit");
    let second = bright_in_cell(&frame, cell, height, 1);
    assert!(
        second > 0.5,
        "the block cursor must cover the second cell of a wide preedit (bright {second:.2})"
    );
    let third = bright_in_cell(&frame, cell, height, 2);
    assert!(
        third < 0.1,
        "the cursor must not extend past the wide preedit (bright {third:.2})"
    );

    // 확정된 넓은 글자 위의 커서도 두 칸을 덮는다.
    let mut wide_text = screen(4, 1);
    wide_text.cursor.focused = true;
    wide_text.lines[0][0].ch = Some("한".to_string());
    wide_text.lines[0][0].width = 2;
    wide_text.lines[0][1].width = 0;
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame.draw(&wide_text, &metrics).expect("wide text");
    let second = bright_in_cell(&frame, cell, height, 1);
    assert!(
        second > 0.5,
        "the block cursor must cover the second cell of a wide character (bright {second:.2})"
    );

    // 좁은 조합 글자는 한 칸 커서를 유지한다.
    let mut narrow = screen(4, 1);
    narrow.cursor.focused = true;
    narrow.cursor.preedit = Some(Preedit {
        text: "a".to_string(),
        selected_range: None,
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame.draw(&narrow, &metrics).expect("narrow preedit");
    let second = bright_in_cell(&frame, cell, height, 1);
    assert!(
        second < 0.1,
        "a narrow preedit keeps a one-cell cursor (bright {second:.2})"
    );
}

fn ink_in_cell(frame: &Frame, cell_width: u32, height: u32, cell: u32) -> usize {
    let start = cell_width * cell;
    (start..start + cell_width)
        .flat_map(|x| (0..height).map(move |y| (x, y)))
        .filter_map(|(x, y)| frame.read_pixel(x, y))
        .filter(|pixel| pixel[0] > 100 || pixel[1] > 100 || pixel[2] > 100)
        .count()
}

#[test]
fn wide_preedit_glyph_is_not_clipped_by_its_continuation_cell() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let hidden = CursorRender {
        visible: false,
        ..CursorRender::default()
    };

    // 확정된 넓은 글자의 둘째 칸 잉크가 기준이다.
    let mut committed = screen(4, 1);
    committed.lines[0][0].ch = Some("글".to_string());
    committed.lines[0][0].width = 2;
    committed.lines[0][1].width = 0;
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame
        .draw_with_cursor(&committed, &metrics, hidden)
        .expect("committed");
    let reference = ink_in_cell(&frame, cell, height, 1);
    assert!(
        reference > 0,
        "the committed wide glyph must have ink in its second cell"
    );

    let mut preedit = screen(4, 1);
    preedit.cursor.preedit = Some(Preedit {
        text: "글".to_string(),
        selected_range: None,
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame
        .draw_with_cursor(&preedit, &metrics, hidden)
        .expect("preedit");
    let ink = ink_in_cell(&frame, cell, height, 1);
    assert!(
        ink >= reference,
        "the preedit glyph's second cell lost ink: {ink} of {reference}"
    );
}

#[test]
fn a_font_list_uses_the_first_installed_family_and_the_system_font_when_none_is_installed() {
    use soksak_sidecar_vt_core::platform::darwin::frame::{
        default_font, metrics_for, resolve_font_list,
    };
    let selection =
        resolve_font_list("No Such Terminal Font Family; Menlo ;Courier").expect("list");
    assert_eq!(selection.font.family().expect("family"), "Menlo");
    assert_eq!(
        selection.skipped,
        vec!["No Such Terminal Font Family".to_string()]
    );
    assert!(!selection.system);

    let courier = resolve_font_list("Courier;Menlo").expect("list");
    assert_eq!(
        courier.font.family().expect("family"),
        "Courier",
        "the list order decides the family"
    );
    let menlo = metrics_for(&selection.font, 13.0, 2.0).expect("Menlo metrics");
    let courier_metrics = metrics_for(&courier.font, 13.0, 2.0).expect("Courier metrics");
    assert_ne!(
        (menlo.cell_width, menlo.cell_height),
        (courier_metrics.cell_width, courier_metrics.cell_height),
        "different families produce different cell metrics"
    );

    let none =
        resolve_font_list("No Such Terminal Font Family;Another Missing Family").expect("list");
    assert!(
        none.system,
        "no installed family selects the system fixed-pitch font"
    );
    assert_eq!(none.skipped.len(), 2);
    assert_eq!(
        none.font.family().expect("family"),
        default_font().family().expect("family")
    );

    assert_eq!(
        resolve_font_list(" ; ").err().as_deref(),
        Some("font.family must name at least one family")
    );
}

/// 서비스는 포커스 없는 커서 정책을 화면에 이미 적용한다. 그리기는 그 모양을 그대로 그린다.
#[test]
fn drawing_an_unfocused_screen_keeps_its_cursor_shape() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let frame = Frame::new(width, height).expect("frame");
    let mut state = screen(1, 1);
    state.cursor.focused = false;
    state.cursor.shape = CursorShape::Block;
    frame.draw(&state, &metrics).expect("unfocused block");
    let middle = frame.read_pixel(width / 2, height / 2).expect("middle");
    assert!(
        middle[0] > 100 || middle[1] > 100 || middle[2] > 100,
        "an unfocused block was drawn hollow: {middle:?}"
    );
    state.cursor.shape = CursorShape::Underline;
    frame.draw(&state, &metrics).expect("unfocused underline");
    let top = frame.read_pixel(width / 2, 1).expect("top");
    let bottom = frame.read_pixel(width / 2, height - 1).expect("bottom");
    assert!(
        top[0] < 100 && top[1] < 100 && top[2] < 100,
        "an unfocused underline was drawn hollow: {top:?}"
    );
    assert!(bottom[0] > 100 || bottom[1] > 100 || bottom[2] > 100);
}

/// 넓은 글자의 폭이 두 칸보다 좁으면(예: Menlo 아래의 대체 한글 글꼴) 두 칸 안에서 가로 가운데에 둔다.
#[test]
fn a_wide_glyph_narrower_than_two_cells_is_centred_in_them() {
    use soksak_sidecar_vt_core::platform::darwin::frame::{metrics_for, resolve_font_list};
    let font = resolve_font_list("Menlo").expect("Menlo").font;
    let metrics = metrics_for(&font, 13.0, 1.0).expect("Menlo metrics");
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(4, 1);
    state.lines[0][0].ch = Some("한".to_string());
    state.lines[0][0].width = 2;
    state.lines[0][1].width = 0;
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                visible: false,
                ..CursorRender::default()
            },
        )
        .expect("wide glyph");
    let inked: Vec<u32> = (0..cell * 2)
        .filter(|&x| {
            (0..height).any(|y| {
                frame
                    .read_pixel(x, y)
                    .map(|pixel| pixel[0] > 100 || pixel[1] > 100 || pixel[2] > 100)
                    .unwrap_or(false)
            })
        })
        .collect();
    let (first, last) = (*inked.first().expect("ink"), *inked.last().expect("ink"));
    let left = first as f64;
    let right = (cell * 2 - 1 - last) as f64;
    assert!(
        (left - right).abs() <= 2.0,
        "the wide glyph is not centred in its two cells: {left} px left and {right} px right of its ink, cells {cell} px"
    );
}

#[test]
fn underlined_and_linked_cells_draw_a_line_below_the_text() {
    let metrics = metrics(14.0, 1.0);
    let (cell_width, cell_height) = (metrics.cell_width as u32, metrics.cell_height as u32);
    let mut screen = screen(3, 1);
    screen.cursor.visible = false;
    screen.lines[0][1].underline = true;
    screen.lines[0][2].link = Some("https://example.test".to_string());
    let frame = Frame::new(cell_width * 3, cell_height).expect("frame");
    frame.draw(&screen, &metrics).expect("draw");
    let plain = bright_region(&frame, 0, cell_width, 0, cell_height);
    let underlined = bright_region(&frame, cell_width, cell_width * 2, 0, cell_height);
    let linked = bright_region(&frame, cell_width * 2, cell_width * 3, 0, cell_height);
    assert_eq!(plain, 0, "a plain blank cell drew pixels");
    assert!(
        underlined >= cell_width as usize,
        "an underlined blank cell drew {underlined} pixels"
    );
    assert!(
        linked >= cell_width as usize,
        "a linked blank cell drew {linked} pixels"
    );
}

#[test]
fn the_padding_outside_the_cell_grid_uses_the_screen_default_background() {
    // 영역이 칸의 배수가 아니면 격자 오른쪽과 아래에 칸 밖 여백이 남는다. 프로그램이 OSC 11 로 바꾼 기본 배경은
    // 칸과 여백에 함께 적용되어야 한다. 테마 배경이 남은 여백은 카드 색 띠로 보인다.
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32 + 5;
    let height = metrics.cell_height as u32 + 5;
    let mut state = screen(1, 1);
    state.background = "#102030".to_string();
    let frame = Frame::new(width, height).expect("padding frame");
    let cursor = CursorRender {
        visible: false,
        focused: false,
        blink_visible: false,
        shape: CursorShape::Block,
        unfocused: UnfocusedCursor::Hollow,
    };
    frame
        .draw_with_theme(&state, &metrics, cursor, &TerminalTheme::dark())
        .expect("draw");
    let [b, g, r, _] = frame
        .read_pixel(width - 1, height - 1)
        .expect("padding pixel");
    assert_eq!(
        [r, g, b],
        [0x10, 0x20, 0x30],
        "the padding keeps the theme background instead of the screen's"
    );
    let [b, g, r, _] = frame.read_pixel(0, 0).expect("cell pixel");
    assert_eq!(
        [r, g, b],
        [0x10, 0x20, 0x30],
        "the empty cell keeps the theme background instead of the screen's"
    );
}
