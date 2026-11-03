use soksak_sidecar_vt_core::TerminalTheme;
use soksak_sidecar_vt_core::platform::darwin::frame::{
    CursorBlinkPolicy, CursorRender, Frame, UnfocusedCursor, cursor_blink_visible,
    effective_cursor_shape, metrics,
};
use soksak_sidecar_vt_core::protocol::{
    Cell, Cursor, CursorShape, InlineImagePlacement, JsonRange, Preedit, Screen,
};
#[cfg(feature = "diagnostics")]
use unicode_segmentation::UnicodeSegmentation;

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
fn unrequested_render_metadata_is_not_collected() {
    let metrics = metrics(13.0, 1.0);
    let frame = Frame::new(metrics.cell_width as u32, metrics.cell_height as u32).expect("frame");
    let metadata = frame
        .draw_with_render_metadata(
            &screen(1, 1),
            &metrics,
            CursorRender {
                visible: false,
                ..CursorRender::default()
            },
            &TerminalTheme::dark(),
            &[],
            false,
        )
        .expect("draw without metadata");
    assert!(
        metadata.is_none(),
        "default draw returned renderer metadata: {metadata:?}"
    );
}

#[cfg(feature = "diagnostics")]
#[test]
fn opt_in_render_metadata_describes_the_drawn_glyph_underline_and_cursor() {
    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32 * 3 + 5;
    let height = metrics.cell_height as u32 * 2 + 7;
    let mut state = screen(3, 2);
    state.lines[1][1].ch = Some("X".into());
    state.lines[1][1].underline = true;
    state.cursor.col = 2;
    state.cursor.row = 0;
    state.cursor.focused = true;
    let frame = Frame::new(width, height).expect("frame");
    let metadata = frame
        .draw_with_render_metadata(
            &state,
            &metrics,
            CursorRender {
                visible: true,
                focused: true,
                blink_visible: true,
                shape: CursorShape::Block,
                unfocused: UnfocusedCursor::Unchanged,
            },
            &TerminalTheme::dark(),
            &[],
            true,
        )
        .expect("draw with metadata")
        .expect("opt-in metadata");
    let value = metadata.value(serde_json::json!({ "generation": 3, "raster": 7, "sequence": 12 }));
    assert_eq!(value["pixel"]["width"], width);
    assert_eq!(value["pixel"]["height"], height);
    assert!(value["pixel"]["bytesPerRow"].as_u64().unwrap() >= u64::from(width * 4));
    assert_eq!(value["pixel"]["origin"], "top-left");
    assert_eq!(
        value["grid"]["origin"],
        serde_json::json!({ "x": 0, "y": 0 })
    );
    assert_eq!(value["grid"]["rows"], 2);
    assert_eq!(value["grid"]["cols"], 3);
    let glyph = value["glyphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|glyph| glyph["row"] == 1 && glyph["col"] == 1)
        .expect("drawn glyph bounds");
    let bounds = &glyph["bounds"];
    assert!(
        bounds.is_object(),
        "drawn grapheme has no ink bounds: {glyph}"
    );
    assert!(bounds["x"].as_f64().unwrap() >= 0.0, "{bounds}");
    assert!(bounds["y"].as_f64().unwrap() >= 0.0, "{bounds}");
    assert!(
        bounds["x"].as_f64().unwrap() + bounds["width"].as_f64().unwrap()
            <= metrics.cell_width as f64,
        "cell-local ink bounds exceed cell width: {bounds}"
    );
    assert!(
        bounds["y"].as_f64().unwrap() + bounds["height"].as_f64().unwrap()
            <= metrics.cell_height as f64,
        "cell-local ink bounds exceed cell height: {bounds}"
    );
    assert!(bounds["width"].as_f64().unwrap() > 0.0, "{bounds}");
    assert!(bounds["height"].as_f64().unwrap() > 0.0, "{bounds}");
    assert!(glyph["underline"]["rect"]["height"].as_f64().unwrap() >= 1.0);
    let cursor = &value["cursor"];
    assert_eq!(cursor["shape"], "block");
    assert_eq!(cursor["blend"], "difference");
    assert_eq!(
        cursor["rect"]["x"].as_f64(),
        Some(f64::from(metrics.cell_width as u32) * 2.0)
    );
    assert_eq!(cursor["rect"]["y"].as_f64(), Some(0.0));
    assert_eq!(
        cursor["rect"]["height"].as_f64(),
        Some(metrics.cell_height as f64)
    );

    let x0 = metrics.cell_width as u32 + bounds["x"].as_f64().unwrap().floor() as u32;
    let y0 = metrics.cell_height as u32 + bounds["y"].as_f64().unwrap().floor() as u32;
    let x1 = metrics.cell_width as u32
        + (bounds["x"].as_f64().unwrap() + bounds["width"].as_f64().unwrap()).ceil() as u32;
    let y1 = metrics.cell_height as u32
        + (bounds["y"].as_f64().unwrap() + bounds["height"].as_f64().unwrap()).ceil() as u32;
    let bg = frame.read_pixel(0, 0).expect("background pixel");
    let contains_ink =
        (x0..x1).any(|x| (y0..y1).any(|y| frame.read_pixel(x, y).is_some_and(|pixel| pixel != bg)));
    assert!(
        contains_ink,
        "the reported ink bounds contain no drawn pixel: {bounds}"
    );
}

#[cfg(feature = "diagnostics")]
#[test]
fn preedit_metadata_reports_effective_overlay_and_inverse_colors() {
    let metrics = metrics(13.0, 1.0);
    let frame =
        Frame::new(metrics.cell_width as u32 * 3, metrics.cell_height as u32).expect("frame");
    let mut state = screen(3, 1);
    state.lines[0][0] = Cell {
        ch: Some("!".into()),
        fg: Some(soksak_sidecar_vt_core::Rgb([0xff, 0x00, 0x00])),
        bg: Some(soksak_sidecar_vt_core::Rgb([0x00, 0x00, 0xff])),
        inverse: true,
        ..Default::default()
    };
    state.lines[0][2] = Cell {
        ch: Some("Z".into()),
        fg: Some(soksak_sidecar_vt_core::Rgb([0xff, 0x00, 0x00])),
        bg: Some(soksak_sidecar_vt_core::Rgb([0x00, 0x00, 0xff])),
        inverse: true,
        ..Default::default()
    };
    state.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        selected_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        replacement_range: None,
        attributed: false,
    });
    let metadata = frame
        .draw_with_render_metadata(
            &state,
            &metrics,
            CursorRender {
                visible: true,
                focused: true,
                blink_visible: true,
                shape: CursorShape::Beam,
                unfocused: UnfocusedCursor::Unchanged,
            },
            &TerminalTheme::dark(),
            &[],
            true,
        )
        .expect("draw frame")
        .expect("diagnostic metadata");
    let identity = serde_json::json!({ "generation": 1, "raster": 1, "sequence": 1 });
    let render = metadata.value(identity.clone());
    let overlay = render["glyphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|glyph| glyph["row"] == 0 && glyph["col"] == 0)
        .expect("preedit glyph");
    assert_eq!(overlay["grapheme"], "한");
    assert_eq!(overlay["foreground"], "#d0d0d0");
    assert_eq!(overlay["background"], "#808080");
    assert_eq!(render["underline"]["colorSource"], "glyph.foreground");
    assert_eq!(render["cursor"]["blend"], "normal");
    assert_eq!(render["cursor"]["color"], "#d0d0d0");
    let difference_cursor = frame
        .draw_with_render_metadata(
            &state,
            &metrics,
            CursorRender {
                visible: true,
                focused: true,
                blink_visible: true,
                shape: CursorShape::Block,
                unfocused: UnfocusedCursor::Unchanged,
            },
            &TerminalTheme::dark(),
            &[],
            true,
        )
        .expect("draw difference cursor")
        .expect("diagnostic metadata")
        .value(identity.clone());
    assert_eq!(difference_cursor["cursor"]["blend"], "difference");
    assert!(difference_cursor["cursor"]["color"].is_null());
    let inverse = render["glyphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|glyph| glyph["row"] == 0 && glyph["col"] == 2)
        .expect("inverse glyph");
    assert_eq!(inverse["foreground"], "#0000ff");
    assert_eq!(inverse["background"], "#ff0000");

    let message = soksak_sidecar_vt_core::protocol::screen_message(
        "boot",
        1,
        soksak_sidecar_vt_core::screen_stream::ScreenStream::default().order(),
        identity,
        render,
        &state,
        true,
    )
    .map(|(message, _)| message)
    .expect("screen message");
    assert_eq!(message["cells"][0]["grapheme"], "!");
    assert_eq!(message["cells"][0]["fg"], "#ff0000");
    assert_eq!(message["cells"][0]["bg"], "#0000ff");
    assert_eq!(
        message["cells"][0]["attributes"].as_u64().unwrap() & 0x20,
        0x20
    );
    assert_eq!(state.lines[0][0].ch.as_deref(), Some("!"));
}

#[cfg(feature = "diagnostics")]
#[test]
fn fallback_multirun_metadata_bounds_match_the_pixels_in_the_drawn_frame() {
    use soksak_sidecar_vt_core::platform::darwin::frame::{metrics_for, resolve_font_list};
    let font = resolve_font_list("Menlo").expect("Menlo").font;
    let metrics = metrics_for(&font, 13.0, 1.0).expect("Menlo metrics");
    let width = metrics.cell_width as u32 + 5;
    let height = metrics.cell_height as u32 + 7;
    let graphemes = [
        "a\u{301}",
        "a\u{20dd}",
        "a\u{20e3}",
        "A\u{0654}",
        "A\u{093c}",
        "漢\u{301}",
        "\u{2764}\u{fe0f}",
        "\u{2600}\u{fe0f}",
        "\u{1f469}\u{200d}\u{1f4bb}",
        "\u{1f9d1}\u{200d}\u{1f680}",
        "\u{0915}\u{093c}",
    ];
    let mut measured_multirun = None;
    for grapheme in graphemes {
        assert_eq!(grapheme.graphemes(true).count(), 1, "{grapheme:?}");
        let frame = Frame::new(width, height).expect("frame");
        let mut state = screen(1, 1);
        state.lines[0][0].ch = Some(grapheme.into());
        state.lines[0][0].fg = Some(soksak_sidecar_vt_core::Rgb([0xff, 0x00, 0x00]));
        let metadata = frame
            .draw_with_render_metadata(
                &state,
                &metrics,
                CursorRender {
                    visible: false,
                    ..CursorRender::default()
                },
                &TerminalTheme::dark(),
                &[],
                true,
            )
            .expect("draw fallback line")
            .expect("diagnostic metadata");
        let value = metadata.value(serde_json::json!({
            "generation": 9,
            "raster": 4,
            "sequence": 2
        }));
        let glyph = value["glyphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|glyph| glyph["row"] == 0 && glyph["col"] == 0)
            .expect("fallback grapheme metadata");
        if glyph["fontRuns"].as_u64().unwrap_or(0) <= 1 {
            continue;
        }
        measured_multirun.get_or_insert(grapheme);
        assert_eq!(glyph["grapheme"], grapheme);
        assert_eq!(glyph["foreground"], "#ff0000");
        let bounds = &glyph["bounds"];
        assert!(
            bounds.is_object(),
            "fallback line has no ink bounds: {glyph}"
        );
        let x0 = bounds["x"].as_f64().unwrap().floor() as u32;
        let y0 = bounds["y"].as_f64().unwrap().floor() as u32;
        let x1 = (bounds["x"].as_f64().unwrap() + bounds["width"].as_f64().unwrap()).ceil() as u32;
        let y1 = (bounds["y"].as_f64().unwrap() + bounds["height"].as_f64().unwrap()).ceil() as u32;
        let background = frame
            .read_pixel(
                metrics.cell_width as u32 - 1,
                metrics.cell_height as u32 - 1,
            )
            .expect("untouched cell corner");
        let mut total_ink = 0;
        let mut ink_in_bounds = 0;
        for y in 0..metrics.cell_height as u32 {
            for x in 0..metrics.cell_width as u32 {
                if frame.read_pixel(x, y).expect("pixel") != background {
                    total_ink += 1;
                    if x >= x0 && x < x1 && y >= y0 && y < y1 {
                        ink_in_bounds += 1;
                    }
                }
            }
        }
        assert!(total_ink > 0, "fallback line drew no ink: {grapheme:?}");
        assert_eq!(
            ink_in_bounds, total_ink,
            "reported bounds omit line pixels: {bounds}"
        );
    }
    assert_eq!(
        measured_multirun,
        Some("A\u{093c}"),
        "the named Menlo fallback fixture must split into multiple CoreText runs"
    );
}

fn differing_pixels(left: &Frame, right: &Frame, x0: u32, x1: u32, height: u32) -> usize {
    (x0..x1)
        .flat_map(|x| (0..height).map(move |y| (x, y)))
        .filter(|(x, y)| left.read_pixel(*x, *y) != right.read_pixel(*x, *y))
        .count()
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
    text_state.lines[0][0].ch = Some("X".into());
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
    state.lines[0][0].ch = Some("👩‍💻".into());
    state.lines[0][0].width = 2;
    state.lines[0][2].ch = Some("tail".into());
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
    state.lines[0][2].ch = Some("T".into());
    state.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        marked_range: None,
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
        r#"{"text":"한글","markedRange":null,"selectedRange":{"location":2,"length":0},"replacementRange":{"location":400,"length":30},"attributed":true}"#,
    )
    .expect("native JSON range contract");
    assert_eq!(decoded.selected_range.unwrap().location, 2);
    assert_eq!(decoded.replacement_range.unwrap().length, 30);
    assert_eq!(decoded.marked_range, None);

    let metrics = metrics(13.0, 1.0);
    let mut state = screen(8, 1);
    state.cursor.preedit = Some(Preedit {
        text: "한😀".to_string(),
        marked_range: None,
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
fn preedit_underlines_only_the_marked_utf16_graphemes() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut unmarked = screen(8, 1);
    unmarked.cursor.preedit = Some(
        serde_json::from_str(
            r#"{"text":"한글","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}"#,
        )
        .expect("null marked range"),
    );
    let mut partially_marked = screen(8, 1);
    partially_marked.cursor.preedit = Some(
        serde_json::from_str(
            r#"{"text":"한글","markedRange":{"location":1,"length":1},"selectedRange":null,"replacementRange":null,"attributed":false}"#,
        )
        .expect("second Hangul grapheme marked"),
    );

    let unmarked_frame = Frame::new(cell * 8, height).expect("unmarked frame");
    unmarked_frame
        .draw(&unmarked, &metrics)
        .expect("unmarked preedit");
    let marked_frame = Frame::new(cell * 8, height).expect("partially marked frame");
    marked_frame
        .draw(&partially_marked, &metrics)
        .expect("partially marked preedit");

    assert_eq!(
        differing_pixels(&unmarked_frame, &marked_frame, 0, cell * 2, height),
        0,
        "the first, unmarked grapheme changed"
    );
    assert!(
        differing_pixels(&unmarked_frame, &marked_frame, cell * 2, cell * 4, height) > 0,
        "the second, marked grapheme has no underline difference"
    );

    let mut empty_range = screen(8, 1);
    empty_range.cursor.preedit = Some(
        serde_json::from_str(
            r#"{"text":"한글","markedRange":{"location":1,"length":0},"selectedRange":null,"replacementRange":null,"attributed":false}"#,
        )
        .expect("empty marked range"),
    );
    let empty_frame = Frame::new(cell * 8, height).expect("empty-range frame");
    empty_frame
        .draw(&empty_range, &metrics)
        .expect("empty marked range");
    assert_eq!(
        differing_pixels(&unmarked_frame, &empty_frame, 0, cell * 8, height),
        0,
        "an empty marked range must draw no underline"
    );
}

#[test]
fn selected_range_keeps_its_background_when_marked_range_is_null() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let hidden = CursorRender {
        visible: false,
        ..CursorRender::default()
    };
    let mut unselected = screen(8, 1);
    unselected.cursor.preedit = Some(
        serde_json::from_str(
            r#"{"text":"한글","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}"#,
        )
        .expect("unselected preedit"),
    );
    let mut selected = screen(8, 1);
    selected.cursor.preedit = Some(
        serde_json::from_str(
            r#"{"text":"한글","markedRange":null,"selectedRange":{"location":1,"length":1},"replacementRange":null,"attributed":false}"#,
        )
        .expect("selected preedit"),
    );
    let unselected_frame = Frame::new(cell * 8, height).expect("unselected frame");
    unselected_frame
        .draw_with_cursor(&unselected, &metrics, hidden)
        .expect("unselected preedit render");
    let selected_frame = Frame::new(cell * 8, height).expect("selected frame");
    selected_frame
        .draw_with_cursor(&selected, &metrics, hidden)
        .expect("selected preedit render");
    assert_eq!(
        differing_pixels(&unselected_frame, &selected_frame, 0, cell * 2, height),
        0,
        "selectedRange changed the unselected grapheme"
    );
    assert!(
        differing_pixels(
            &unselected_frame,
            &selected_frame,
            cell * 2,
            cell * 4,
            height
        ) > 0,
        "selectedRange did not change the selected grapheme background"
    );
}

#[test]
fn preedit_marked_range_requires_a_valid_utf16_grapheme_range() {
    let missing = serde_json::from_str::<Preedit>(
        r#"{"text":"한","selectedRange":null,"replacementRange":null,"attributed":false}"#,
    )
    .expect_err("missing markedRange must be rejected");
    assert!(missing.to_string().contains("markedRange"), "{missing}");

    let malformed = serde_json::from_str::<Preedit>(
        r#"{"text":"한","markedRange":{"location":-1,"length":1},"selectedRange":null,"replacementRange":null,"attributed":false}"#,
    )
    .expect_err("negative markedRange must be rejected");
    assert!(malformed.to_string().contains("markedRange"), "{malformed}");

    let metrics = metrics(13.0, 1.0);
    let width = metrics.cell_width as u32 * 8;
    let height = metrics.cell_height as u32;
    for (text, range, expected) in [
        (
            "한",
            r#"{"location":18446744073709551615,"length":1}"#,
            "markedRange JSON range overflows UTF-16 location",
        ),
        (
            "한",
            r#"{"location":1,"length":1}"#,
            "markedRange JSON range [1, 1] exceeds preedit UTF-16 length 1",
        ),
        (
            "é",
            r#"{"location":0,"length":1}"#,
            "markedRange endpoint splits a preedit grapheme",
        ),
    ] {
        let encoded_text = serde_json::to_string(text).expect("text JSON");
        let input = format!(
            r#"{{"text":{encoded_text},"markedRange":{range},"selectedRange":null,"replacementRange":null,"attributed":false}}"#
        );
        let preedit: Preedit = serde_json::from_str(&input).expect("markedRange JSON shape");
        let mut state = screen(8, 1);
        state.cursor.preedit = Some(preedit);
        let frame = Frame::new(width, height).expect("frame");
        let error = frame
            .draw(&state, &metrics)
            .expect_err("invalid markedRange");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn a_wide_preedit_grapheme_wraps_before_the_last_column() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let mut state = screen(6, 2);
    state.cursor.col = 5;
    state.cursor.focused = true;
    state.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        selected_range: Some(JsonRange {
            location: 1,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 6, metrics.cell_height as u32 * 2).expect("frame");

    frame
        .draw(&state, &metrics)
        .expect("valid preedit at the last column");
    assert!(
        bright_region(
            &frame,
            0,
            cell * 2,
            metrics.cell_height as u32,
            metrics.cell_height as u32 * 2,
        ) > 0,
        "wrapped grapheme was not drawn on the next row"
    );
    assert_eq!((state.cursor.row, state.cursor.col), (0, 5));
}

#[test]
fn wrapped_preedit_replaces_only_overlay_cells_and_clear_restores_the_base_frame() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let row_height = metrics.cell_height as u32;
    let mut base = screen(4, 3);
    base.cursor.col = 3;
    base.cursor.visible = false;
    base.lines[1][0].ch = Some("P".into());
    base.lines[1][3].ch = Some("Z".into());
    let hidden = CursorRender {
        visible: false,
        ..CursorRender::default()
    };
    let base_frame = Frame::new(cell * 4, row_height * 3).expect("base frame");
    base_frame
        .draw_with_cursor(&base, &metrics, hidden)
        .expect("base screen");

    let mut overlay = base.clone();
    overlay.cursor.preedit = Some(Preedit {
        text: "AB".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 2,
        }),
        selected_range: Some(JsonRange {
            location: 2,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let overlay_frame = Frame::new(cell * 4, row_height * 3).expect("overlay frame");
    overlay_frame
        .draw_with_cursor(&overlay, &metrics, hidden)
        .expect("wrapped overlay");
    assert!(bright_region(&overlay_frame, cell * 3, cell * 4, 0, row_height) > 0);
    assert!(bright_region(&overlay_frame, 0, cell, row_height, row_height * 2) > 0);
    assert!(
        bright_region(
            &overlay_frame,
            cell * 3,
            cell * 4,
            row_height,
            row_height * 2,
        ) > 0,
        "the uncovered committed cell was not preserved"
    );
    assert_eq!(
        overlay.lines, base.lines,
        "rendering changed committed cells"
    );

    overlay.cursor.preedit = None;
    let cleared_frame = Frame::new(cell * 4, row_height * 3).expect("cleared frame");
    cleared_frame
        .draw_with_cursor(&overlay, &metrics, hidden)
        .expect("cleared screen");
    assert_eq!(
        pixels(&cleared_frame, cell * 4, row_height * 3),
        pixels(&base_frame, cell * 4, row_height * 3)
    );
}

#[test]
fn overlay_intersection_clears_the_whole_committed_wide_cell_span() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(4, 1);
    state.cursor.col = 2;
    state.cursor.visible = false;
    state.lines[0][1] = Cell {
        ch: Some("漢".into()),
        width: 2,
        ..Default::default()
    };
    state.lines[0][2] = Cell {
        width: 0,
        ..Default::default()
    };
    state.cursor.preedit = Some(Preedit {
        text: "A".to_string(),
        marked_range: None,
        selected_range: None,
        replacement_range: None,
        attributed: false,
    });
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
        .expect("overlay over a continuation cell");
    assert_eq!(bright_in_cell(&frame, cell, height, 0), 0.0);
    assert!(bright_in_cell(&frame, cell, height, 2) > 0.0);
    assert_eq!(state.lines[0][1].ch.as_deref(), Some("漢"));
    assert_eq!(state.lines[0][2].width, 0);
}

#[test]
fn a_wide_preedit_grapheme_clips_on_a_one_column_raster() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(1, 1);
    state.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        selected_range: Some(JsonRange {
            location: 0,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell, height).expect("one-cell frame");
    frame.draw(&state, &metrics).expect("clipped wide grapheme");
    assert!(bright_region(&frame, 0, cell, 0, height) > 0);
}

#[test]
fn a_full_single_row_preedit_keeps_its_glyphs_visible_at_the_end_boundary() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(4, 1);
    state.cursor.focused = true;
    state.cursor.preedit = Some(Preedit {
        text: "ABCD".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 4,
        }),
        selected_range: Some(JsonRange {
            location: 4,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let original_cursor = (state.cursor.row, state.cursor.col);
    let original_preedit = state.cursor.preedit.clone();
    let frame = Frame::new(cell * 4, height).expect("one-row frame");

    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                visible: true,
                focused: true,
                blink_visible: true,
                shape: CursorShape::Block,
                unfocused: UnfocusedCursor::Unchanged,
            },
        )
        .expect("full-row preedit with end caret");

    assert!(bright_region(&frame, 0, cell * 4, 0, height) > 0);
    assert!(
        bright_in_cell(&frame, cell, height, 3) > 0.5,
        "wrap-pending caret was not painted on the last occupied cell"
    );
    assert_eq!((state.cursor.row, state.cursor.col), original_cursor);
    assert_eq!(state.cursor.preedit, original_preedit);
}

#[test]
fn a_clipped_wide_grapheme_keeps_its_glyph_visible_at_a_one_cell_end_boundary() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(1, 1);
    state.cursor.focused = true;
    state.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        selected_range: Some(JsonRange {
            location: 1,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let original_cursor = (state.cursor.row, state.cursor.col);
    let original_preedit = state.cursor.preedit.clone();
    let frame = Frame::new(cell, height).expect("one-cell frame");

    frame
        .draw_with_cursor(
            &state,
            &metrics,
            CursorRender {
                visible: false,
                unfocused: UnfocusedCursor::Unchanged,
                ..CursorRender::default()
            },
        )
        .expect("clipped wide preedit with end caret");

    assert!(bright_region(&frame, 0, cell, 0, height) > 0);
    assert_eq!((state.cursor.row, state.cursor.col), original_cursor);
    assert_eq!(state.cursor.preedit, original_preedit);
}

#[test]
fn a_combining_grapheme_stays_with_its_base_before_a_wide_grapheme_wraps() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let row_height = metrics.cell_height as u32;
    let mut state = screen(4, 2);
    state.cursor.col = 3;
    state.cursor.visible = false;
    state.cursor.preedit = Some(Preedit {
        text: "e\u{301}한".to_string(),
        marked_range: Some(JsonRange {
            location: 2,
            length: 1,
        }),
        selected_range: Some(JsonRange {
            location: 2,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 4, row_height * 2).expect("frame");
    frame
        .draw(&state, &metrics)
        .expect("combined grapheme wrap");
    assert!(bright_region(&frame, cell * 3, cell * 4, 0, row_height) > 0);
    assert!(bright_region(&frame, 0, cell * 2, row_height, row_height * 2) > 0);
    assert_eq!(state.cursor.col, 3);
}

#[test]
fn cropped_preedit_keeps_full_state_and_clear_restores_committed_rows() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let row_height = metrics.cell_height as u32;
    let mut base = screen(4, 2);
    base.cursor.row = 0;
    base.cursor.col = 3;
    base.cursor.visible = false;
    base.lines[0][0].ch = Some("X".into());
    base.lines[1][3].ch = Some("Z".into());
    let hidden = CursorRender {
        visible: false,
        ..CursorRender::default()
    };
    let base_frame = Frame::new(cell * 4, row_height * 2).expect("base frame");
    base_frame
        .draw_with_cursor(&base, &metrics, hidden)
        .expect("base screen");

    let mut visible = base.clone();
    visible.cursor.preedit = Some(Preedit {
        text: "ABCDEFGH".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 8,
        }),
        selected_range: Some(JsonRange {
            location: 8,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let cropped_frame = Frame::new(cell * 4, row_height * 2).expect("cropped frame");
    cropped_frame
        .draw_with_cursor(&visible, &metrics, hidden)
        .expect("cropped preedit");
    assert!(bright_region(&cropped_frame, 0, cell * 3, 0, row_height) > 0);
    assert_eq!((visible.cursor.row, visible.cursor.col), (0, 3));
    assert_eq!(visible.lines, base.lines);
    assert_eq!(visible.cursor.preedit.as_ref().unwrap().text, "ABCDEFGH");

    visible.cursor.preedit = None;
    let restored_frame = Frame::new(cell * 4, row_height * 2).expect("restored frame");
    restored_frame
        .draw_with_cursor(&visible, &metrics, hidden)
        .expect("restored base");
    assert_eq!(
        pixels(&restored_frame, cell * 4, row_height * 2),
        pixels(&base_frame, cell * 4, row_height * 2)
    );
}

#[cfg(feature = "diagnostics")]
#[test]
fn cropped_preedit_metadata_reports_the_visible_rows_and_in_grid_glyph_span() {
    let metrics = metrics(13.0, 1.0);
    let cell = metrics.cell_width as u32;
    let row_height = metrics.cell_height as u32;
    let mut state = screen(4, 2);
    state.cursor.row = 0;
    state.cursor.col = 3;
    state.cursor.focused = true;
    state.cursor.preedit = Some(Preedit {
        text: "ABCDEFGH".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 8,
        }),
        selected_range: Some(JsonRange {
            location: 8,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let frame = Frame::new(cell * 4, row_height * 2).expect("frame");
    let metadata = frame
        .draw_with_render_metadata(
            &state,
            &metrics,
            CursorRender {
                visible: true,
                focused: true,
                blink_visible: true,
                shape: CursorShape::Block,
                unfocused: UnfocusedCursor::Unchanged,
            },
            &TerminalTheme::dark(),
            &[],
            true,
        )
        .expect("cropped preedit draw")
        .expect("diagnostic metadata");
    let retained = state
        .cursor
        .preedit
        .as_ref()
        .expect("preedit remains in input state");
    assert_eq!(retained.text, "ABCDEFGH");
    assert_eq!(
        retained.marked_range,
        Some(JsonRange {
            location: 0,
            length: 8
        })
    );
    assert_eq!(
        retained.selected_range,
        Some(JsonRange {
            location: 8,
            length: 0
        })
    );
    let value = metadata.value(serde_json::json!({ "generation": 1, "raster": 1, "sequence": 1 }));
    assert_eq!(value["grid"]["logicalRowOrigin"], 1);
    let glyphs = value["glyphs"].as_array().unwrap();
    assert!(!glyphs.iter().any(|glyph| glyph["grapheme"] == "A"));
    assert!(
        glyphs
            .iter()
            .any(|glyph| { glyph["grapheme"] == "B" && glyph["row"] == 0 && glyph["col"] == 0 })
    );
    assert!(
        glyphs
            .iter()
            .any(|glyph| { glyph["grapheme"] == "B" && glyph["underline"]["rect"].is_object() })
    );
    let cursor = &value["cursor"]["rect"];
    assert_eq!(cursor["y"].as_f64(), Some(row_height as f64));
    assert_eq!(cursor["x"].as_f64(), Some((cell * 3) as f64));

    let mut narrow = screen(1, 1);
    narrow.cursor.focused = true;
    narrow.cursor.preedit = Some(Preedit {
        text: "한".to_string(),
        marked_range: Some(JsonRange {
            location: 0,
            length: 1,
        }),
        selected_range: Some(JsonRange {
            location: 0,
            length: 0,
        }),
        replacement_range: None,
        attributed: false,
    });
    let narrow_frame = Frame::new(cell, row_height).expect("narrow frame");
    let narrow_metadata = narrow_frame
        .draw_with_render_metadata(
            &narrow,
            &metrics,
            CursorRender {
                visible: true,
                focused: true,
                blink_visible: true,
                shape: CursorShape::Block,
                unfocused: UnfocusedCursor::Unchanged,
            },
            &TerminalTheme::dark(),
            &[],
            true,
        )
        .expect("one-column clipped draw")
        .expect("one-column metadata");
    let narrow_value =
        narrow_metadata.value(serde_json::json!({ "generation": 1, "raster": 1, "sequence": 2 }));
    let wide_glyph = narrow_value["glyphs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|glyph| glyph["grapheme"] == "한")
        .expect("clipped glyph metadata");
    assert_eq!(wide_glyph["width"], 1);
    assert_eq!(narrow_value["grid"]["logicalRowOrigin"], 0);
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
        marked_range: None,
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

    // solid fills the whole cell even without focus. hollow leaves the center empty.
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
    state.lines[0][0].ch = Some("A".into());
    state.lines[0][1].ch = Some("한".into());
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
    state.lines[0][0].ch = Some("X".into());
    state.cursor.focused = true;
    let frame = Frame::new(width, height).expect("theme frame");

    // The engine reports the default background of the theme as the default background of the
    // screen.
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

    // A wide character being composed: the cursor covers both cells.
    let mut wide_preedit = screen(4, 1);
    wide_preedit.cursor.focused = true;
    wide_preedit.cursor.preedit = Some(Preedit {
        text: "글".to_string(),
        marked_range: None,
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

    // The cursor on a committed wide character also covers both cells.
    let mut wide_text = screen(4, 1);
    wide_text.cursor.focused = true;
    wide_text.lines[0][0].ch = Some("한".into());
    wide_text.lines[0][0].width = 2;
    wide_text.lines[0][1].width = 0;
    let frame = Frame::new(cell * 4, height).expect("frame");
    frame.draw(&wide_text, &metrics).expect("wide text");
    let second = bright_in_cell(&frame, cell, height, 1);
    assert!(
        second > 0.5,
        "the block cursor must cover the second cell of a wide character (bright {second:.2})"
    );

    // A narrow character being composed keeps a one-cell cursor.
    let mut narrow = screen(4, 1);
    narrow.cursor.focused = true;
    narrow.cursor.preedit = Some(Preedit {
        text: "a".to_string(),
        marked_range: None,
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

    // The ink of the second cell of a committed wide character is the reference.
    let mut committed = screen(4, 1);
    committed.lines[0][0].ch = Some("글".into());
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
        marked_range: None,
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

/// The service already applies the unfocused cursor policy to the screen. Drawing draws that shape
/// as it is.
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

/// If the width of a wide character is narrower than two cells (for example, the fallback Korean
/// font under Menlo), it is centered horizontally within the two cells.
#[test]
fn a_wide_glyph_narrower_than_two_cells_is_centred_in_them() {
    use soksak_sidecar_vt_core::platform::darwin::frame::{metrics_for, resolve_font_list};
    let font = resolve_font_list("Menlo").expect("Menlo").font;
    let metrics = metrics_for(&font, 13.0, 1.0).expect("Menlo metrics");
    let cell = metrics.cell_width as u32;
    let height = metrics.cell_height as u32;
    let mut state = screen(4, 1);
    state.lines[0][0].ch = Some("한".into());
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
    // If the region is not a multiple of the cell size, margin outside the cells remains on the
    // right and bottom of the grid. The default background that the program changed with OSC 11
    // must apply to both the cells and the margin. A remaining margin filled with the theme
    // background appears as a band in the card color.
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
