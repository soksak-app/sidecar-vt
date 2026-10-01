#[path = "../src/engine.rs"]
mod engine;

use engine::{AlacrittyEngine, OscOutcome, CSI_SELECTOR_INVENTORY, OSC_SELECTOR_INVENTORY};
use soksak_sidecar_vt_core::{
    default_terminal_color, inline_image::Dimension, inline_image::InlineImageCommand, CursorShape,
    Engine, EngineEvent, ShellMarker, TerminalTheme, DEFAULT_PALETTE,
};

fn text(screen: &soksak_sidecar_vt_core::Screen) -> String {
    screen
        .lines
        .iter()
        .flat_map(|line| line.iter().filter_map(|cell| cell.ch.as_deref()))
        .collect()
}

#[test]
fn vt_events_are_retained_and_exposed_in_order() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]2;title\x07\x07\x1b[6n\x1b[c");

    let events = engine.drain_events();
    assert!(matches!(&events[0], EngineEvent::Title(title) if title == "title"));
    assert!(matches!(&events[1], EngineEvent::Bell));
    let replies: Vec<&[u8]> = events
        .iter()
        .filter_map(|event| {
            if let EngineEvent::PtyWrite(bytes) = event {
                Some(bytes.as_slice())
            } else {
                None
            }
        })
        .collect();
    assert!(replies.contains(&b"\x1b[1;1R".as_slice()));
    assert!(replies.contains(&b"\x1b[?6c".as_slice()));
    assert!(engine.drain_events().is_empty());
}

#[test]
fn bel_and_st_terminated_effects_and_queries_preserve_response_order() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(10, 4);
    engine.feed(
        b"\x1b]2;st-title\x1b\\\x1b]4;1;rgb:0000/ffff/ffff\x07\x1b[38;5;1mA\x1b]4;1;?\x1b\\\x1b[6n",
    );

    let events = engine.drain_events();
    assert!(matches!(events.first(), Some(EngineEvent::Title(title)) if title == "st-title"));
    let replies: Vec<&[u8]> = events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::PtyWrite(bytes) => Some(bytes.as_slice()),
            _ => None,
        })
        .collect();
    let color_reply = replies
        .iter()
        .position(|reply| reply.windows(18).any(|part| part == b"rgb:0000/ffff/ffff"))
        .expect("OSC 4 query must return the changed indexed color");
    let cursor_reply = replies
        .iter()
        .position(|reply| *reply == b"\x1b[1;2R")
        .expect("CSI 6n must return the cursor position");
    assert!(
        color_reply < cursor_reply,
        "responses must retain input order"
    );
    assert_eq!(engine.screen().lines[0][0].ch.as_deref(), Some("A"));
    assert_eq!(engine.screen().lines[0][0].fg.as_deref(), Some("#00ffff"));
}

#[test]
fn csi_device_status_reports_are_observable() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[5n\x1b[6n\x1b[c\x1b[>c");
    let replies: Vec<Vec<u8>> = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::PtyWrite(bytes) => Some(bytes),
            _ => None,
        })
        .collect();
    assert_eq!(replies[0], b"\x1b[0n");
    assert_eq!(replies[1], b"\x1b[1;1R");
    assert_eq!(replies[2], b"\x1b[?6c");
    assert!(replies[3].starts_with(b"\x1b[>0;"));
    assert!(replies[3].ends_with(b";1c"));
}

#[test]
fn osc1337_inline_image_is_typed_and_survives_input_chunk_boundaries() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"before\x1b]1337;File=name=ZmlsZS5wbmc=;size=5;inline=1;width=2px:aGVsbG8=");
    assert!(
        engine.drain_events().is_empty(),
        "incomplete OSC must remain pending"
    );

    engine.feed(b"\x07after");
    let events = engine.drain_events();
    assert!(matches!(
        events.as_slice(),
        [EngineEvent::InlineImage { command: InlineImageCommand::Display {
            name,
            data,
            width: Dimension::Pixels(2),
            ..
        }, .. }] if name == "file.png" && data == b"hello"
    ));
    let screen = engine.screen();
    let text = screen
        .lines
        .iter()
        .flat_map(|line| line.iter().filter_map(|cell| cell.ch.as_deref()))
        .collect::<String>();
    assert!(
        text.contains("before"),
        "text before image was lost: {text:?}"
    );
    assert!(
        text.contains("after"),
        "text after image was lost: {text:?}"
    );
}

#[test]
fn scroll_generation_advances_when_output_scrolls_the_primary_grid() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(8, 2);
    let before = engine.scroll_generation();
    engine.feed(b"one\ntwo\nthree");
    assert!(engine.scroll_generation() > before);
}

#[test]
fn malformed_osc1337_is_an_explicit_engine_error() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]1337;File=inline=1:%%%\x07");
    let events = engine.drain_events();
    assert!(matches!(
        events.as_slice(),
        [EngineEvent::Error(reason)] if reason.contains("invalid base64")
    ));
}

#[test]
fn osc_selector_inventory_records_unsupported_operations() {
    let unsupported: Vec<_> = OSC_SELECTOR_INVENTORY
        .iter()
        .filter(|entry| entry.outcome == OscOutcome::Unsupported)
        .collect();
    assert!(!unsupported.is_empty());
    for entry in unsupported {
        assert!(!entry.selector.is_empty());
        assert_eq!(
            entry.test,
            "osc_selector_inventory_records_unsupported_operations"
        );
    }
}

#[test]
fn unsupported_osc_selector_is_an_explicit_error_after_fragmented_bel() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]13;ignored");
    assert!(engine.drain_events().is_empty());
    engine.feed(b"\x07");
    assert!(matches!(
        engine.drain_events().as_slice(),
        [EngineEvent::Error(reason)] if reason == "unsupported OSC selector 13"
    ));
}

#[test]
fn unsupported_osc_selector_is_an_explicit_error_after_st() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]I;ignored\x1b\\");
    assert!(matches!(
        engine.drain_events().as_slice(),
        [EngineEvent::Error(reason)] if reason == "unsupported OSC selector I"
    ));
}

#[test]
fn every_unsupported_osc_inventory_selector_emits_an_explicit_error() {
    for selector in [
        "1", "3", "13", "18", "21", "46", "51", "60", "62", "I", "l", "L",
    ] {
        let mut engine = AlacrittyEngine::new();
        engine.feed(format!("\x1b]{selector};ignored\x07").as_bytes());
        assert!(
            matches!(
                engine.drain_events().as_slice(),
                [EngineEvent::Error(reason)] if reason == &format!("unsupported OSC selector {selector}")
            ),
            "selector {selector} did not produce an explicit rejection"
        );
    }
}

#[test]
fn x11_and_tektronix_osc_selectors_are_explicitly_rejected() {
    // 17 과 19(강조 색)는 구현했다. 나머지는 포인터 색과 Tektronix 색이다.
    for selector in ["13", "14", "15", "16", "18"] {
        let mut engine = AlacrittyEngine::new();
        engine.feed(format!("\x1b]{selector};ignored\x07").as_bytes());
        assert!(
            matches!(
                engine.drain_events().as_slice(),
                [EngineEvent::Error(reason)] if reason == &format!("unsupported OSC selector {selector}")
            ),
            "physical selector {selector} did not produce an explicit rejection"
        );
    }
}

#[test]
fn implemented_and_vendor_osc_selectors_do_not_emit_unsupported_errors() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]2;title\x07\x1b]7;file:///tmp\x07");
    let events = engine.drain_events();
    assert!(events
        .iter()
        .any(|event| event == &EngineEvent::Title("title".to_string())));
    assert!(!events
        .iter()
        .any(|event| matches!(event, EngineEvent::Error(_))));
}

#[test]
fn vendor_osc_contracts_are_separate() {
    let vendor: Vec<_> = OSC_SELECTOR_INVENTORY
        .iter()
        .filter(|entry| entry.outcome == OscOutcome::Vendor)
        .collect();
    assert_eq!(
        vendor
            .iter()
            .map(|entry| entry.selector)
            .collect::<Vec<_>>(),
        ["7,8,9,133", "1337"]
    );
    assert!(vendor.iter().all(|entry| !entry.test.is_empty()));
}

#[test]
fn vendor_osc_effects_are_typed_and_survive_bel_st_and_fragmentation() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]7;file:///tmp/project\x1b\\\x1b]8;id=docs;https://example.test\x07");
    engine.feed(b"\x1b]9;build complete");
    engine.feed(b"\x07\x1b]133;A\x1b\\\x1b]133;D;0\x07");

    assert_eq!(
        engine.drain_events(),
        vec![
            EngineEvent::Directory {
                uri: "file:///tmp/project".to_string(),
                path: Some("/tmp/project".to_string()),
            },
            EngineEvent::Hyperlink {
                id: "docs".to_string(),
                uri: Some("https://example.test".to_string()),
            },
            EngineEvent::Notification {
                message: "build complete".to_string(),
            },
            EngineEvent::ShellState {
                marker: ShellMarker::PromptStart,
                params: Vec::new(),
            },
            EngineEvent::ShellState {
                marker: ShellMarker::CommandFinished,
                params: vec!["0".to_string()],
            },
        ]
    );
}

#[test]
fn malformed_vendor_osc_is_rejected_without_silent_drop() {
    for (sequence, expected) in [
        (b"\x1b]7;\x07".as_slice(), "OSC 7 directory URI is empty"),
        (
            b"\x1b]8;missing-separator\x07".as_slice(),
            "OSC 8 hyperlink payload must contain params and URI",
        ),
        (b"\x1b]9;\x07".as_slice(), "OSC 9 notification is empty"),
        (
            b"\x1b]133;Z\x07".as_slice(),
            "OSC 133 shell marker is unsupported: Z",
        ),
    ] {
        let mut engine = AlacrittyEngine::new();
        engine.feed(sequence);
        assert_eq!(
            engine.drain_events(),
            vec![EngineEvent::Error(expected.to_string())],
            "vendor sequence must have an explicit rejection"
        );
    }
}

#[test]
fn csi_inventory_links_only_executed_behavior_cases() {
    assert!(!CSI_SELECTOR_INVENTORY.is_empty());
    for entry in CSI_SELECTOR_INVENTORY {
        assert!(!entry.selector.is_empty());
        assert!(!entry.test.is_empty());
    }
}

#[test]
fn unsupported_csi_window_report_is_an_explicit_error() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[1");
    engine.feed(b"8t");
    let errors: Vec<_> = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::Error(reason) => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(errors, ["unsupported CSI window report 18t"]);
}

#[test]
fn unsupported_csi_rectangle_protected_and_palette_reports_are_explicit_errors() {
    for (prefix, suffix, expected) in [
        (
            b"\x1b[1$".as_slice(),
            b"x".as_slice(),
            "unsupported CSI rectangle report 1$x",
        ),
        (
            b"\x1b[1\"".as_slice(),
            b"q".as_slice(),
            "unsupported CSI protected-cell report 1\"q",
        ),
        (
            b"\x1b[#".as_slice(),
            b"p".as_slice(),
            "unsupported CSI palette report #p",
        ),
    ] {
        let mut engine = AlacrittyEngine::new();
        engine.feed(prefix);
        engine.feed(suffix);
        let errors: Vec<_> = engine
            .drain_events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::Error(reason) => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(errors, [expected]);
    }
}

#[test]
fn csi_cursor_movement_and_save_restore_are_observable() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"abc\x1b[s\x1b[2D\x1b[2B\x1b[3C\x1b[u");
    assert_eq!(
        engine.cursor().col,
        3,
        "CSI s/u must restore the saved column"
    );
    assert_eq!(engine.cursor().row, 0, "CSI s/u must restore the saved row");

    engine.feed(b"\x1b[2;5H\x1b[2A\x1b[3G");
    assert_eq!(
        engine.cursor().col,
        2,
        "CSI G must select the requested column"
    );
    assert_eq!(
        engine.cursor().row,
        0,
        "CSI A must move up by the requested count"
    );

    engine.feed(b"\x1b[2B\x1b[2D");
    assert_eq!(
        engine.cursor().col,
        0,
        "CSI D must move left by the requested count"
    );
    assert_eq!(
        engine.cursor().row,
        2,
        "CSI B must move down by the requested count"
    );
}

#[test]
fn csi_cursor_next_and_previous_line_are_observable() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"abc\x1b[2E");
    assert_eq!(engine.cursor().row, 2);
    assert_eq!(engine.cursor().col, 0);
    engine.feed(b"x\x1b[1F");
    assert_eq!(engine.cursor().row, 1);
    assert_eq!(engine.cursor().col, 0);
}

#[test]
fn osc50_cursor_shape_changes_program_cursor() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]50;CursorShape=2\x07");
    assert_eq!(engine.cursor().shape, CursorShape::Underline);
}

#[test]
fn osc104_resets_indexed_colors() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]4;1;rgb:0000/ffff/ffff\x07\x1b[38;5;1mX");
    assert_eq!(engine.screen().lines[0][0].fg.as_deref(), Some("#00ffff"));
    engine.feed(b"\x1b]104;1\x07\x1b[39mY\x1b[38;5;1mZ");
    let expected = default_terminal_color(1).expect("default color 1");
    let expected = format!("#{:02x}{:02x}{:02x}", expected[0], expected[1], expected[2]);
    assert_eq!(
        engine.screen().lines[0][2].fg.as_deref(),
        Some(expected.as_str())
    );
}

#[test]
fn osc_title_supports_bel_st_and_fragmentation() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]2;st");
    assert!(engine.drain_events().is_empty());
    engine.feed(b"\x1b\\\x1b]0;bel\x07");
    assert_eq!(
        engine.drain_events(),
        vec![
            EngineEvent::Title("st".to_string()),
            EngineEvent::Title("bel".to_string()),
        ]
    );
}

#[test]
fn osc104_without_parameters_resets_all_indexed_colors() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]4;1;rgb:0000/ffff/ffff\x07\x1b]4;2;rgb:ffff/0000/ffff\x07\x1b]104\x07\x1b[38;5;1mA\x1b[38;5;2mB");
    let expected_one = default_terminal_color(1).expect("default color 1");
    let expected_two = default_terminal_color(2).expect("default color 2");
    let expected_one = format!(
        "#{:02x}{:02x}{:02x}",
        expected_one[0], expected_one[1], expected_one[2]
    );
    let expected_two = format!(
        "#{:02x}{:02x}{:02x}",
        expected_two[0], expected_two[1], expected_two[2]
    );
    let screen = engine.screen();
    assert_eq!(
        screen.lines[0][0].fg.as_deref(),
        Some(expected_one.as_str())
    );
    assert_eq!(
        screen.lines[0][1].fg.as_deref(),
        Some(expected_two.as_str())
    );
}

#[test]
fn osc_dynamic_color_resets_restore_defaults() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]10;rgb:0000/ffff/ffff\x07\x1b]11;rgb:ffff/0000/ffff\x07\x1b]12;rgb:ffff/ffff/0000\x07");
    engine.drain_events();
    engine.feed(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");
    let replies = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::PtyWrite(bytes) => Some(bytes),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 3);
    assert!(replies
        .iter()
        .any(|reply| reply.windows(18).any(|part| part == b"rgb:d0d0/d0d0/d0d0")));
    assert!(replies
        .iter()
        .any(|reply| reply.windows(18).any(|part| part == b"rgb:1e1e/1e1e/1e1e")));
}

#[test]
fn osc_default_color_queries_match_renderer_defaults() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");
    let replies: Vec<Vec<u8>> = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| {
            if let EngineEvent::PtyWrite(bytes) = event {
                Some(bytes)
            } else {
                None
            }
        })
        .collect();
    assert!(replies
        .iter()
        .any(|reply| reply.windows(18).any(|part| part == b"rgb:d0d0/d0d0/d0d0")));
    assert!(replies
        .iter()
        .any(|reply| reply.windows(18).any(|part| part == b"rgb:1e1e/1e1e/1e1e")));
    assert_eq!(replies.len(), 3);
}

#[test]
fn every_default_indexed_color_query_returns_the_default_palette() {
    let mut engine = AlacrittyEngine::new();
    for index in 0..256 {
        engine.feed(format!("\x1b]4;{index};?\x07").as_bytes());
        let events = engine.drain_events();
        let expected = default_terminal_color(index).expect("defined default color slot");
        let expected_reply = format!(
            "rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
            expected[0], expected[0], expected[1], expected[1], expected[2], expected[2]
        );
        assert!(events.iter().any(|event| {
            matches!(event, EngineEvent::PtyWrite(bytes)
                if bytes.windows(expected_reply.len()).any(|part| part == expected_reply.as_bytes()))
        }), "missing default response for indexed color {index}");
        assert!(!events
            .iter()
            .any(|event| matches!(event, EngineEvent::Error(_))));
    }

    engine.feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");
    let events = engine.drain_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, EngineEvent::PtyWrite(_)))
            .count(),
        3
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, EngineEvent::Error(_))));
}

#[test]
fn every_named_color_slot_has_a_default_value() {
    for index in 256..269 {
        assert!(
            default_terminal_color(index).is_some(),
            "missing named color {index}"
        );
    }
}

#[test]
fn event_queue_is_available_through_engine_trait() {
    let mut engine: Box<dyn Engine> = Box::new(AlacrittyEngine::new());
    engine.feed(b"\x07");
    assert!(matches!(
        engine.drain_events().as_slice(),
        [EngineEvent::Bell]
    ));
}

#[test]
fn simple_text_is_exported() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"hi");
    let screen = engine.screen();
    assert_eq!(screen.lines.len() as u16, screen.rows);
    assert_eq!(screen.lines[0].len(), 2);
    assert_eq!(screen.lines[0][0].ch.as_deref(), Some("h"));
    assert_eq!(screen.lines[0][1].ch.as_deref(), Some("i"));
}

#[test]
fn korean_text_keeps_wide_cell_width() {
    let mut engine = AlacrittyEngine::new();
    engine.feed("한글".as_bytes());
    let screen = engine.screen();
    assert_eq!(screen.lines.len() as u16, screen.rows);
    assert_eq!(screen.lines[0][0].width, 2);
}

#[test]
fn sgr_color_does_not_drop_the_character() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[31mA");
    let screen = engine.screen();
    assert_eq!(screen.lines.len() as u16, screen.rows);
    assert_eq!(screen.lines[0][0].ch.as_deref(), Some("A"));
}

#[test]
fn alternate_screen_mode_is_exported() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[?1049h");
    assert!(engine.modes().alt_screen);
}

#[test]
fn resize_updates_screen_dimensions() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(40, 10);
    let screen = engine.screen();
    assert_eq!(screen.cols, 40);
    assert_eq!(screen.rows, 10);
    assert_eq!(screen.lines.len(), 10);
}

#[test]
fn reset_clears_the_screen() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"hello");
    engine.reset();
    let screen = engine.screen();
    assert_eq!(screen.lines.len() as u16, screen.rows);
    assert!(screen.lines.iter().all(Vec::is_empty));
}

#[test]
fn empty_lines_in_the_middle_are_retained() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"a\r\n\r\nb");
    let screen = engine.screen();
    assert_eq!(screen.lines[0][0].ch.as_deref(), Some("a"));
    assert!(screen.lines[1].is_empty());
    assert_eq!(screen.lines[2][0].ch.as_deref(), Some("b"));
    assert_eq!(screen.cursor.row, 2);
}

#[test]
fn clipboard_query_uses_a_token_and_resolves_to_pty_bytes() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]52;c;?\x07");
    let events = engine.drain_events();
    let request_id = events
        .iter()
        .find_map(|event| {
            if let EngineEvent::ClipboardQuery { request_id, .. } = event {
                Some(*request_id)
            } else {
                None
            }
        })
        .expect("clipboard query must expose a request token");
    engine
        .resolve_clipboard(request_id, "secret")
        .expect("known clipboard token");
    assert!(engine.drain_events().iter().any(|event| {
        matches!(event, EngineEvent::PtyWrite(bytes) if bytes.windows(8).any(|window| window == b"c2VjcmV0"))
    }));
    assert!(engine.resolve_clipboard(request_id, "again").is_err());
}

#[test]
fn clipboard_rejection_clears_a_pending_query_token() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]52;c;?\x07");
    let request_id = engine
        .drain_events()
        .into_iter()
        .find_map(|event| match event {
            EngineEvent::ClipboardQuery { request_id, .. } => Some(request_id),
            _ => None,
        })
        .expect("clipboard query must expose a request token");
    engine
        .reject_clipboard(request_id, "denied")
        .expect("known clipboard token");
    assert!(engine.resolve_clipboard(request_id, "secret").is_err());
    assert!(engine.reject_clipboard(request_id, "again").is_err());
}

#[test]
fn clipboard_query_survives_a_fragmented_st_terminator() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]52;c;");
    assert!(engine.drain_events().is_empty());
    engine.feed(b"?\x1b");
    assert!(engine.drain_events().is_empty());
    engine.feed(b"\\");
    assert!(engine.drain_events().iter().any(|event| matches!(
        event,
        EngineEvent::ClipboardQuery {
            selection: soksak_sidecar_vt_core::ClipboardSelection::Clipboard,
            ..
        }
    )));
}

#[test]
fn native_selection_updates_raster_cells_and_returns_text_once() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"hello");
    engine.selection_start(0, 0).expect("selection start");
    engine.selection_update(5, 0).expect("selection update");
    let selected = engine.screen();
    assert_eq!(
        selected.lines[0][2].bg.as_deref(),
        Some("#44475a"),
        "a selected cell is drawn on the selection color"
    );
    assert_eq!(
        engine.selection_end().expect("selection copy").as_deref(),
        Some("hello")
    );
}

#[test]
fn the_current_selection_text_is_readable_until_the_selection_is_cleared() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"hello");
    assert_eq!(engine.selection_text(), None, "no selection has no text");
    engine.selection_start(0, 0).expect("selection start");
    engine.selection_update(5, 0).expect("selection update");
    engine.selection_end().expect("selection copy");
    assert_eq!(engine.selection_text().as_deref(), Some("hello"));
    // 움직이지 않은 클릭은 빈 선택으로 이전 선택을 지운다.
    engine.selection_start(8, 0).expect("click press");
    assert_eq!(engine.selection_end().expect("click release"), None);
    assert_eq!(
        engine.selection_text(),
        None,
        "a cleared selection has no text"
    );
}

#[test]
fn a_program_mouse_click_can_clear_a_previous_selection_without_copying() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"hello");
    engine.selection_start(0, 0).expect("selection start");
    engine.selection_update(5, 0).expect("selection update");
    engine.selection_end().expect("selection copy");
    assert_eq!(engine.selection_text().as_deref(), Some("hello"));
    let selected_pixel = engine.screen().lines[0][2].bg.clone();
    assert!(engine.selection_clear(), "the selected pixels changed");
    assert_eq!(engine.selection_text(), None);
    assert_ne!(engine.screen().lines[0][2].bg, selected_pixel);
    assert!(
        !engine.selection_clear(),
        "an empty selection changes no pixels"
    );
}

fn line_text(screen: &soksak_sidecar_vt_core::Screen, row: usize) -> String {
    screen.lines[row]
        .iter()
        .map(|cell| cell.ch.as_deref().unwrap_or(" "))
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn the_viewport_scrolls_through_the_scrollback_and_returns_to_the_newest_output() {
    let mut engine = AlacrittyEngine::new();
    for index in 0..60 {
        engine.feed(format!("LINE{index:02}\r\n").as_bytes());
    }
    let newest = engine.screen();
    assert_eq!(
        line_text(&newest, 0),
        "LINE37",
        "the newest screen starts at line 37"
    );
    assert_eq!(engine.scrollback(), (0, 37));

    engine.scroll_viewport(10);
    let scrolled = engine.screen();
    assert_eq!(
        line_text(&scrolled, 0),
        "LINE27",
        "ten lines toward older output"
    );
    assert_eq!(engine.scrollback(), (10, 37));
    assert!(
        !scrolled.cursor.visible,
        "the cursor row is below the viewport"
    );

    engine.scroll_viewport(100);
    assert_eq!(
        engine.scrollback(),
        (37, 37),
        "the viewport stops at the oldest retained line"
    );
    assert_eq!(line_text(&engine.screen(), 0), "LINE00");

    // 스크롤한 뷰포트의 선택은 보이는 글자를 복사한다.
    engine.selection_start(0, 1).expect("selection start");
    engine.selection_update(6, 1).expect("selection update");
    assert_eq!(
        engine.selection_end().expect("selection copy").as_deref(),
        Some("LINE01")
    );

    engine.scroll_viewport(-5);
    assert_eq!(engine.scrollback(), (32, 37));
    engine.scroll_to_newest();
    assert_eq!(engine.scrollback(), (0, 37));
    assert_eq!(line_text(&engine.screen(), 0), "LINE37");
    assert!(engine.screen().cursor.visible);
}

/// 선택 배경(기본 테마의 선택 색)으로 그린 칸의 글자.
fn inverted_text(screen: &soksak_sidecar_vt_core::Screen, row: usize) -> String {
    let selection = format!(
        "#{:02x}{:02x}{:02x}",
        TerminalTheme::dark().selection[0],
        TerminalTheme::dark().selection[1],
        TerminalTheme::dark().selection[2]
    );
    screen.lines[row]
        .iter()
        .filter(|cell| cell.bg.as_deref() == Some(selection.as_str()))
        .map(|cell| cell.ch.as_deref().unwrap_or(" "))
        .collect()
}

#[test]
fn the_rendered_selection_and_the_copied_text_cover_the_same_cells_in_both_directions() {
    for (from, to, expected) in [
        (1u16, 6u16, "EFTED"),
        (6, 1, "EFTED"),
        (0, 8, "LEFTEDGE"),
        (8, 0, "LEFTEDGE"),
        (3, 3, ""),
    ] {
        let mut engine = AlacrittyEngine::new();
        engine.feed(b"LEFTEDGE");
        engine.selection_start(from, 0).expect("selection start");
        engine.selection_update(to, 0).expect("selection update");
        let rendered = inverted_text(&engine.screen(), 0);
        let copied = engine
            .selection_end()
            .expect("selection end")
            .unwrap_or_default();
        assert_eq!(
            (rendered.as_str(), copied.as_str()),
            (expected, expected),
            "a drag from edge {from} to edge {to} rendered {rendered:?} and copied {copied:?}"
        );
    }
}

#[test]
fn the_screen_reports_the_current_default_background() {
    let mut engine = AlacrittyEngine::new();
    assert_eq!(engine.screen().background, "#1e1e1e");
    engine.feed(b"\x1b]11;rgb:10/20/30\x07");
    assert_eq!(
        engine.screen().background,
        "#102030",
        "OSC 11 changes the default background"
    );
}

#[test]
fn blank_selection_release_is_not_an_error() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"hi");
    engine.selection_start(5, 0).expect("selection start");
    engine.selection_update(9, 0).expect("selection update");
    let ended = engine.selection_end();
    assert!(
        ended.is_ok(),
        "a selection over blank cells must end without an error: {ended:?}"
    );
    assert_eq!(
        ended.unwrap(),
        None,
        "a selection over blank cells has no text"
    );
    let screen = engine.screen();
    assert!(
        screen.lines.iter().flatten().all(|cell| !cell.inverse),
        "a selection without text is cleared"
    );
}

#[test]
fn text_area_callback_is_not_discarded() {
    let mut engine = AlacrittyEngine::new();
    engine.set_cell_metrics(8, 16).expect("renderer metrics");
    engine.feed(b"\x1b[14t");
    assert!(engine.drain_events().iter().any(|event| {
        matches!(event, EngineEvent::PtyWrite(bytes) if bytes == b"\x1b[4;384;640t")
    }));
}

#[test]
fn text_area_query_without_renderer_metrics_is_explicitly_rejected() {
    let mut engine: Box<dyn Engine> = Box::new(AlacrittyEngine::new());
    engine.feed(b"\x1b[14t");
    assert!(
        matches!(engine.drain_events().as_slice(), [EngineEvent::Error(message)] if message.contains("renderer metrics"))
    );
}

#[test]
fn indexed_colors_and_combining_characters_survive_export() {
    let mut engine = AlacrittyEngine::new();
    engine.feed("\x1b]4;196;rgb:ffff/0000/0000\x07\x1b[38;5;196mX e\u{301}".as_bytes());

    let screen = engine.screen();
    let first = &screen.lines[0];
    let x = first
        .iter()
        .find(|cell| cell.ch.as_deref() == Some("X"))
        .expect("X cell");
    assert_eq!(x.fg.as_deref(), Some("#ff0000"));
    assert!(first
        .iter()
        .any(|cell| cell.ch.as_deref() == Some("e\u{301}")));
}

#[test]
fn dynamic_color_replies_and_screen_colors_use_the_same_palette() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]10;rgb:1122/3344/5566\x07\x1b]11;rgb:7788/99aa/bbcc\x07\x1b[39mA");
    let screen = engine.screen();
    assert_eq!(screen.lines[0][0].fg.as_deref(), Some("#113355"));
    assert_eq!(screen.lines[0][0].bg.as_deref(), Some("#7799bb"));

    engine.feed(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07");
    let replies: Vec<Vec<u8>> = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| {
            if let EngineEvent::PtyWrite(bytes) = event {
                Some(bytes)
            } else {
                None
            }
        })
        .collect();
    assert!(replies
        .iter()
        .any(|reply| reply.windows(18).any(|part| part == b"rgb:1111/3333/5555")));
    assert!(replies
        .iter()
        .any(|reply| reply.windows(18).any(|part| part == b"rgb:7777/9999/bbbb")));
}

#[test]
fn default_palette_styled_cells_use_the_same_rgb_as_queries() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[38;5;196m\x1b[48;5;21mX");
    let cell = &engine.screen().lines[0][0];
    assert_eq!(cell.fg.as_deref(), Some("#ff0000"));
    let expected_bg = DEFAULT_PALETTE[21];
    let expected_bg_text = format!(
        "#{:02x}{:02x}{:02x}",
        expected_bg[0], expected_bg[1], expected_bg[2]
    );
    assert_eq!(cell.bg.as_deref(), Some(expected_bg_text.as_str()));
}

#[test]
fn application_theme_changes_default_and_ansi_raster_colors_without_changing_text() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"A\x1b[38;5;1mR");
    let dark = engine.screen();
    let dark_text = text(&dark);
    let dark_default = dark.lines[0][0].fg.clone();
    let dark_ansi = dark.lines[0][1].fg.clone();

    engine.set_theme(TerminalTheme::light());
    let light = engine.screen();
    assert_eq!(text(&light), dark_text);
    assert_ne!(light.lines[0][0].fg, dark_default);
    assert_ne!(light.lines[0][1].fg, dark_ansi);
    assert_eq!(engine.cursor().col, 2);
}

#[test]
fn display_points_are_used_as_cell_indices() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\r\x1b[3CZ");
    let screen = engine.screen();
    assert_eq!(screen.lines[0][3].ch.as_deref(), Some("Z"));
    assert_eq!(screen.lines[0].len(), 4);
}

#[test]
fn cursor_visibility_and_application_shape_are_exported() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[?25l");
    assert!(!engine.cursor().visible);
    engine.feed(b"\x1b[?25h\x1b[4 q");
    let cursor = engine.cursor();
    assert!(cursor.visible);
    assert_eq!(cursor.shape, CursorShape::Underline);
    assert!(!cursor.blinking);
    engine.feed(b"\x1b[?12h");
    assert!(engine.cursor().blinking);
    engine.feed(b"\x1b[?12l");
    assert!(engine
        .drain_events()
        .iter()
        .any(|event| matches!(event, EngineEvent::CursorBlinkingChange)));
    assert!(!engine.cursor().blinking);
}

#[test]
fn decscusr_cursor_style_ids_are_observable() {
    let mut engine = AlacrittyEngine::new();
    let expected = [
        (1, CursorShape::Block, true),
        (2, CursorShape::Block, false),
        (3, CursorShape::Underline, true),
        (4, CursorShape::Underline, false),
        (5, CursorShape::Beam, true),
        (6, CursorShape::Beam, false),
    ];

    for (id, shape, blinking) in expected {
        engine.feed(b"\x1b[");
        engine.feed(id.to_string().as_bytes());
        engine.feed(b" q");
        let cursor = engine.cursor();
        assert_eq!(cursor.shape, shape, "DECSCUSR {id} shape");
        assert_eq!(cursor.blinking, blinking, "DECSCUSR {id} blink");
    }
}

#[test]
fn decscusr_initial_cursor_resources_are_observable() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[5 q");
    assert_eq!(engine.cursor().shape, CursorShape::Beam);
    assert!(engine.cursor().blinking);

    engine.feed(b"\x1b[");
    engine.feed(b"7");
    engine.feed(b" q");
    assert_eq!(engine.cursor().shape, CursorShape::Block);
    assert!(!engine.cursor().blinking);

    engine.feed(b"\x1b[3 q");
    assert_eq!(engine.cursor().shape, CursorShape::Underline);
    engine.feed(b"\x1b[0 q");
    assert_eq!(engine.cursor().shape, CursorShape::Block);
    assert!(!engine.cursor().blinking);
}

#[test]
fn csi_fragmentation_and_malformed_input_preserve_engine_state() {
    let mut complete = AlacrittyEngine::new();
    complete.feed(b"\x1b[2;3H\x1b[2CX");

    let mut fragmented = AlacrittyEngine::new();
    for chunk in [
        b"\x1b".as_slice(),
        b"[2".as_slice(),
        b";3".as_slice(),
        b"H".as_slice(),
        b"\x1b[".as_slice(),
        b"2C".as_slice(),
        b"X".as_slice(),
    ] {
        fragmented.feed(chunk);
    }
    assert_eq!(text(&fragmented.screen()), text(&complete.screen()));
    assert_eq!(fragmented.cursor().row, complete.cursor().row);
    assert_eq!(fragmented.cursor().col, complete.cursor().col);

    let mut cancelled = AlacrittyEngine::new();
    cancelled.feed(b"\x1b[12;");
    cancelled.feed(b"\x18X");
    let mut clean = {
        let mut engine = AlacrittyEngine::new();
        engine.feed(b"X");
        engine
    };
    assert_eq!(text(&cancelled.screen()), text(&clean.screen()));
    assert_eq!(cancelled.cursor().row, clean.cursor().row);
    assert_eq!(cancelled.cursor().col, clean.cursor().col);
}

#[test]
fn primary_screen_reflows_without_losing_text_when_width_changes() {
    let mut engine = AlacrittyEngine::new();
    let value = "AAAA-BBBB-CCCC-DDDD-EEEE-FFFF-GGGG-HHHH";
    engine.resize(20, 10);
    engine.feed(value.as_bytes());
    let narrow = engine.screen();
    assert_eq!(text(&narrow), value);
    assert!(narrow.lines.iter().filter(|line| !line.is_empty()).count() > 1);

    engine.resize(80, 10);
    let wide = engine.screen();
    assert_eq!(text(&wide), value);
    assert_eq!(wide.lines.iter().filter(|line| !line.is_empty()).count(), 1);
}

#[test]
fn soft_wraps_rejoin_but_explicit_newlines_remain_after_resize() {
    let mut engine = AlacrittyEngine::new();
    let first = "AAAA-BBBB-CCCC-DDDD";
    let second = "hard-line";
    engine.resize(6, 8);
    engine.feed(format!("{first}\r\n{second}").as_bytes());
    let narrow = engine.screen();
    assert_eq!(text(&narrow), format!("{first}{second}"));
    assert!(narrow.lines.iter().filter(|line| !line.is_empty()).count() >= 4);

    engine.resize(40, 8);
    let wide = engine.screen();
    assert_eq!(text(&wide), format!("{first}{second}"));
    assert_eq!(
        wide.lines.iter().filter(|line| !line.is_empty()).count(),
        2,
        "the explicit newline must remain after soft wraps rejoin"
    );
}

#[test]
fn wide_cells_keep_their_width_and_text_through_reflow() {
    let mut engine = AlacrittyEngine::new();
    let value = "한글한글-終";
    engine.resize(5, 8);
    engine.feed(value.as_bytes());
    assert_eq!(text(&engine.screen()), value);
    assert!(engine.screen().lines[0].iter().any(|cell| cell.width == 2));

    engine.resize(20, 8);
    let wide = engine.screen();
    assert_eq!(text(&wide), value);
    assert!(wide.lines[0].iter().any(|cell| cell.width == 2));
}

#[test]
fn cell_metrics_are_fixed_renderer_values_across_grid_resize() {
    let mut engine = AlacrittyEngine::new();
    engine
        .set_cell_metrics(9, 17)
        .expect("positive renderer metrics");
    engine.feed(b"\x1b[14t");
    let before = engine.drain_events();
    engine.resize(40, 10);
    engine.feed(b"\x1b[14t");
    let after = engine.drain_events();
    let reply = |events: &[EngineEvent]| {
        events.iter().find_map(|event| match event {
            EngineEvent::PtyWrite(bytes) => Some(bytes.clone()),
            _ => None,
        })
    };
    assert_eq!(
        reply(&before).as_deref(),
        Some(b"\x1b[4;408;720t".as_slice())
    );
    assert_eq!(
        reply(&after).as_deref(),
        Some(b"\x1b[4;170;360t".as_slice())
    );
}

#[test]
fn scrollback_keeps_recent_visible_lines_after_overflow_and_resize() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 4);
    engine.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    let narrow = engine.screen();
    assert!(text(&narrow).contains("six"));
    assert!(!text(&narrow).contains("one"));

    engine.resize(40, 4);
    let wide = engine.screen();
    assert!(text(&wide).contains("six"));
    assert!(!text(&wide).contains("one"));
}

#[test]
fn alternate_screen_is_separate_from_primary_scrollback() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 4);
    engine.feed(b"primary\r\ntext");
    let primary = text(&engine.screen());
    engine.feed(b"\x1b[?1049h");
    engine.feed(b"alternate");
    assert!(engine.modes().alt_screen);
    assert!(text(&engine.screen()).contains("alternate"));
    assert!(!text(&engine.screen()).contains("primary"));
    engine.feed(b"\x1b[?1049l");
    assert!(!engine.modes().alt_screen);
    assert!(text(&engine.screen()).contains(&primary));
    assert!(!text(&engine.screen()).contains("alternate"));
}

#[test]
fn unsupported_csi_alternate_modes_are_explicit_errors() {
    for selector in ["?47", "?1047", "?1048"] {
        let mut engine = AlacrittyEngine::new();
        engine.feed(format!("\x1b[{selector}h").as_bytes());
        assert!(
            matches!(
                engine.drain_events().as_slice(),
                [EngineEvent::Error(reason)] if reason == &format!("unsupported CSI alternate screen mode {selector}h")
            ),
            "selector {selector}h did not produce an explicit rejection"
        );
    }
}

#[test]
fn csi_scroll_moves_the_visible_grid_and_respects_a_scroll_region() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(10, 4);
    engine.feed(b"A\r\nB\r\nC\r\nD");

    engine.feed(b"\x1b[2S");
    let after_full_scroll = engine.screen();
    assert_eq!(after_full_scroll.lines[0][0].ch.as_deref(), Some("C"));
    assert_eq!(after_full_scroll.lines[1][0].ch.as_deref(), Some("D"));

    engine.reset();
    engine.resize(10, 4);
    engine.feed(b"A\r\nB\r\nC\r\nD");
    engine.feed(b"\x1b[2;3r\x1b[1S");
    let after_region_scroll = engine.screen();
    assert_eq!(after_region_scroll.lines[0][0].ch.as_deref(), Some("A"));
    assert_eq!(after_region_scroll.lines[1][0].ch.as_deref(), Some("C"));
    assert!(after_region_scroll.lines[2].is_empty());
    assert_eq!(after_region_scroll.lines[3][0].ch.as_deref(), Some("D"));
}

#[test]
fn csi_erase_display_and_line_change_only_the_requested_cells() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(8, 3);
    engine.feed(b"top\r\nmid\r\nbottom");
    engine.feed(b"\x1b[2;2H\x1b[0J");
    let after_display_erase = engine.screen();
    assert_eq!(after_display_erase.lines[0][0].ch.as_deref(), Some("t"));
    assert_eq!(after_display_erase.lines[1][0].ch.as_deref(), Some("m"));
    assert!(after_display_erase.lines[1]
        .get(1)
        .is_none_or(|cell| cell.ch.is_none()));
    assert!(after_display_erase.lines[2].is_empty());

    engine.reset();
    engine.resize(8, 3);
    engine.feed(b"abcdef");
    engine.feed(b"\x1b[1;4H\x1b[0K");
    let after_line_erase = engine.screen();
    assert_eq!(after_line_erase.lines[0][0].ch.as_deref(), Some("a"));
    assert_eq!(after_line_erase.lines[0][2].ch.as_deref(), Some("c"));
    assert!(after_line_erase.lines[0]
        .get(3)
        .is_none_or(|cell| cell.ch.is_none()));
    assert!(after_line_erase.lines[0]
        .get(4)
        .is_none_or(|cell| cell.ch.is_none()));
}

#[test]
fn csi_insert_delete_characters_and_lines_preserve_requested_cells() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(8, 4);
    engine.feed(b"abcd");
    engine.feed(b"\x1b[1;3H\x1b[2@");
    let after_insert = engine.screen();
    assert_eq!(after_insert.lines[0][0].ch.as_deref(), Some("a"));
    assert_eq!(after_insert.lines[0][1].ch.as_deref(), Some("b"));
    assert!(after_insert.lines[0]
        .get(2)
        .is_none_or(|cell| cell.ch.is_none()));
    assert!(after_insert.lines[0]
        .get(3)
        .is_none_or(|cell| cell.ch.is_none()));
    assert_eq!(after_insert.lines[0][4].ch.as_deref(), Some("c"));
    assert_eq!(after_insert.lines[0][5].ch.as_deref(), Some("d"));

    engine.reset();
    engine.resize(8, 4);
    engine.feed(b"abcd\x1b[1;3H\x1b[1P");
    let after_delete = engine.screen();
    assert_eq!(after_delete.lines[0][0].ch.as_deref(), Some("a"));
    assert_eq!(after_delete.lines[0][1].ch.as_deref(), Some("b"));
    assert_eq!(after_delete.lines[0][2].ch.as_deref(), Some("d"));
    assert!(after_delete.lines[0]
        .get(3)
        .is_none_or(|cell| cell.ch.is_none()));

    engine.reset();
    engine.resize(8, 4);
    engine.feed(b"A\r\nB\r\nC");
    engine.feed(b"\x1b[2;1H\x1b[1L");
    let after_line_insert = engine.screen();
    assert!(after_line_insert.lines[1].is_empty());
    assert_eq!(after_line_insert.lines[2][0].ch.as_deref(), Some("B"));
    engine.feed(b"\x1b[2;1H\x1b[1M");
    let after_line_delete = engine.screen();
    assert_eq!(after_line_delete.lines[1][0].ch.as_deref(), Some("B"));
    assert_eq!(after_line_delete.lines[2][0].ch.as_deref(), Some("C"));
}

#[test]
fn csi_tabulation_forward_and_backward_use_tab_stops() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 2);
    engine.feed(b"\x1b[1I");
    assert_eq!(
        engine.cursor().col,
        8,
        "CSI I must move to the next tab stop"
    );
    engine.feed(b"\x1b[1Z");
    assert_eq!(
        engine.cursor().col,
        0,
        "CSI Z must move to the previous tab stop"
    );
}

#[test]
fn csi_repeat_repeats_the_last_printed_character() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(8, 2);
    engine.feed(b"A\x1b[2b");
    let screen = engine.screen();
    assert_eq!(screen.lines[0][0].ch.as_deref(), Some("A"));
    assert_eq!(screen.lines[0][1].ch.as_deref(), Some("A"));
    assert_eq!(screen.lines[0][2].ch.as_deref(), Some("A"));
}

#[test]
fn csi_private_modes_export_keyboard_paste_and_mouse_state() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b[?1h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h");
    let enabled = engine.modes();
    assert!(enabled.app_cursor);
    // 마우스 모드는 하나만 켜진다. 마지막으로 켠 ?1003 이 남는다.
    assert!(enabled.mouse_motion && !enabled.mouse_click && !enabled.mouse_drag);
    assert!(enabled.bracketed_paste);

    engine.feed(b"\x1b[?1004h\x1b[?1007h");
    let extended = engine.modes();
    assert!(extended.focus_in_out);
    assert!(extended.alternate_scroll);

    engine.feed(b"\x1b[?1005h");
    assert!(engine.modes().utf8_mouse);
    assert!(!engine.modes().sgr_mouse);
    engine.feed(b"\x1b[?1006h");
    assert!(engine.modes().sgr_mouse);
    assert!(!engine.modes().utf8_mouse);

    engine.feed(b"\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\x1b[?1007l\x1b[?2004l");
    let disabled = engine.modes();
    assert!(!disabled.app_cursor);
    assert!(!disabled.mouse_report());
    engine.feed(b"\x1b=");
    assert!(
        engine.modes().app_keypad,
        "ESC = selects the application keypad"
    );
    engine.feed(b"\x1b>");
    assert!(
        !engine.modes().app_keypad,
        "ESC > selects the numeric keypad"
    );
    engine.feed(b"\x1b[?1000h");
    assert!(engine.modes().mouse_click && !engine.modes().mouse_drag);
    engine.feed(b"\x1b[?1002h");
    assert!(engine.modes().mouse_drag && !engine.modes().mouse_click);
    assert!(!disabled.bracketed_paste);
    assert!(!disabled.focus_in_out);
    assert!(!disabled.utf8_mouse);
    assert!(!disabled.sgr_mouse);
    assert!(!disabled.alternate_scroll);
}

#[test]
fn csi_application_keypad_mode_uses_the_private_equals_prefix() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b=");
    assert!(engine.modes().app_keypad);
    engine.feed(b"\x1b>");
    assert!(!engine.modes().app_keypad);
}

fn row_text(screen: &soksak_sidecar_vt_core::Screen, row: usize) -> String {
    screen.lines[row]
        .iter()
        .filter_map(|cell| cell.ch.as_deref())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn wrapped_prompt(mark: &[u8]) -> AlacrittyEngine {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"o1\r\no2\r\no3\r\n");
    engine.feed(mark);
    // 프롬프트 "P>" 와 입력 30자는 폭 20에서 3, 4행을 차지한다. 폭 10에서는 네 행이다.
    engine.feed(b"P>abcdefghijklmnopqrstuvwxyz0123");
    engine.drain_events();
    engine
}

const PROMPT_LINE: &str = "o1o2o3P>abcdefghijklmnopqrstuvwxyz0123";

/// 기록과 화면의 모든 칸 글자를 위에서부터 이어 붙인다.
fn all_text(engine: &mut AlacrittyEngine) -> String {
    let (_, history) = engine.scrollback();
    engine.scroll_viewport(history as i32);
    let mut value = text(&engine.screen());
    let rows = engine.screen().lines.len();
    engine.scroll_to_newest();
    let screen = engine.screen();
    let hidden = rows.min(history);
    value.push_str(
        &screen.lines[rows - hidden..]
            .iter()
            .flat_map(|line| line.iter().filter_map(|cell| cell.ch.as_deref()))
            .collect::<String>(),
    );
    value
}

#[test]
fn a_resize_in_the_prompt_state_clears_the_cursor_logical_line_to_the_bottom() {
    let mut engine = wrapped_prompt(b"\x1b]133;A\x07");
    engine.resize(10, 6);
    let screen = engine.screen();
    assert_eq!(
        row_text(&screen, 0),
        "o3",
        "the output above the prompt must remain"
    );
    for row in 1..6 {
        assert_eq!(
            row_text(&screen, row),
            "",
            "row {row} of the prompt line must be cleared"
        );
    }
    let cursor = engine.cursor();
    assert_eq!(
        (cursor.row, cursor.col),
        (2, 0),
        "the cursor must lie one row below the first prompt row, as at the previous width"
    );
    assert!(engine
        .drain_events()
        .iter()
        .all(|event| !matches!(event, EngineEvent::Error(_))));
    assert_eq!(all_text(&mut engine), "o1o2o3");
}

#[test]
fn redraw_last_places_the_cursor_as_redraw_1_does() {
    // 폭 20에서 커서는 논리적 행의 첫 행(3행)보다 한 행 아래(4행)에 있었다. readline 도 한 행 올라가 다시 그린다.
    let mut engine = wrapped_prompt(b"\x1b]133;A;redraw=last;cl=line\x07");
    engine.resize(10, 6);
    let screen = engine.screen();
    assert_eq!(row_text(&screen, 0), "o3");
    for row in 1..6 {
        assert_eq!(
            row_text(&screen, row),
            "",
            "row {row} of the prompt line must be cleared"
        );
    }
    let cursor = engine.cursor();
    assert_eq!((cursor.row, cursor.col), (2, 0));
}

#[test]
fn redraw_last_scrolls_the_screen_when_the_offset_row_is_below_the_bottom() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(10, 5);
    // 폭 10에서 프롬프트 행은 네 행이고 커서는 그 첫 행보다 세 행 아래에 있다.
    engine.feed(b"o1\r\no2\r\n\x1b]133;A;redraw=last\x07P>abcdefghijklmnopqrstuvwxyz0123");
    engine.resize(40, 5);
    let cursor = engine.cursor();
    let screen = engine.screen();
    let rows: Vec<String> = (0..5).map(|row| row_text(&screen, row)).collect();
    let last_output = rows.iter().rposition(|row| !row.is_empty()).unwrap();
    assert_eq!(
        (usize::from(cursor.row), cursor.col),
        (last_output + 1 + 3, 0),
        "the cursor must lie three rows below the first prompt row: {rows:?}"
    );
    assert_eq!(
        all_text(&mut engine),
        "o1o2",
        "the output above the prompt must remain"
    );
}

#[test]
fn prompt_rows_that_the_reflow_moved_into_the_scrollback_are_cleared() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 3);
    engine.feed(b"o1\r\n\x1b]133;A;redraw=last\x07P>abcdefghijklmnopqrstuvwxyz0123");
    // 폭 10에서 프롬프트 행은 네 행이고, 커서 행을 유지하는 재배치가 앞의 행을 기록으로 옮긴다.
    engine.resize(10, 3);
    assert_eq!(all_text(&mut engine), "o1");
    let cursor = engine.cursor();
    assert_eq!(
        (cursor.row, cursor.col),
        (0, 0),
        "the cursor must move to the top row"
    );
}

#[test]
fn a_resize_keeps_the_screen_without_a_redrawing_prompt_state() {
    for (name, mark) in [
        ("no mark", b"".as_slice()),
        ("redraw=0", b"\x1b]133;A;redraw=0\x07".as_slice()),
        ("command start", b"\x1b]133;A\x07\x1b]133;C\x07".as_slice()),
        (
            "command finished",
            b"\x1b]133;A\x07\x1b]133;D;0\x07".as_slice(),
        ),
    ] {
        let mut engine = wrapped_prompt(mark);
        engine.resize(10, 6);
        assert_eq!(
            all_text(&mut engine),
            PROMPT_LINE,
            "{name}: the screen must only reflow"
        );
    }
}

#[test]
fn a_resize_to_the_current_dimensions_keeps_the_prompt() {
    let mut engine = wrapped_prompt(b"\x1b]133;A\x07");
    engine.resize(20, 6);
    assert_eq!(all_text(&mut engine), PROMPT_LINE);
    let cursor = engine.cursor();
    assert_eq!((cursor.row, cursor.col), (4, 12));
}

#[test]
fn a_resize_on_the_alternate_screen_keeps_its_cells() {
    let mut engine = wrapped_prompt(b"\x1b]133;A\x07");
    engine.feed(b"\x1b[?1049h\x1b[Hfull-screen-program");
    engine.resize(10, 6);
    assert_eq!(row_text(&engine.screen(), 0), "full-scree");
}

#[test]
fn a_prompt_start_moves_a_cursor_off_column_zero_to_the_next_line() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    // 표시가 입력 조각 사이에서 끊겨도 앞의 출력 뒤에 적용된다.
    engine.feed(b"partial\x1b]133;");
    engine.feed(b"A\x07P>");
    let screen = engine.screen();
    assert_eq!(row_text(&screen, 0), "partial");
    assert_eq!(row_text(&screen, 1), "P>");
    let cursor = engine.cursor();
    assert_eq!((cursor.row, cursor.col), (1, 2));

    engine.feed(b"\r\n\x1b]133;A\x07");
    let cursor = engine.cursor();
    assert_eq!(
        (cursor.row, cursor.col),
        (2, 0),
        "a prompt start at column 0 must not add a line"
    );
}

#[test]
fn an_unsupported_redraw_value_is_rejected_without_a_state_change() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"\x1b]133;A;redraw=2\x07");
    assert_eq!(
        engine.drain_events(),
        vec![EngineEvent::Error(
            "OSC 133 redraw value is unsupported: 2".to_string()
        )]
    );
    engine.feed(b"P>abcdefghijklmnopqrstuvwxyz0123");
    engine.resize(10, 6);
    assert_eq!(all_text(&mut engine), "P>abcdefghijklmnopqrstuvwxyz0123");
}

#[test]
fn osc7_reports_the_local_path_or_none_for_another_machine_and_rejects_other_uris() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]7;file://another-machine.invalid/tmp\x07\x1b]7;http://localhost/tmp\x07");
    assert_eq!(
        engine.drain_events(),
        vec![
            EngineEvent::Directory {
                uri: "file://another-machine.invalid/tmp".to_string(),
                path: None,
            },
            EngineEvent::Error(
                "OSC 7 directory URI is not a file URI: \"http://localhost/tmp\"".to_string()
            ),
        ]
    );
}

#[test]
fn osc8_linked_cells_carry_their_uri() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 2);
    engine.feed(b"\x1b]8;id=a;https://example.test/a\x07LINK\x1b]8;;\x07 no");
    let screen = engine.screen();
    let links: Vec<Option<&str>> = screen.lines[0]
        .iter()
        .take(7)
        .map(|cell| cell.link.as_deref())
        .collect();
    assert_eq!(
        links,
        vec![
            Some("https://example.test/a"),
            Some("https://example.test/a"),
            Some("https://example.test/a"),
            Some("https://example.test/a"),
            None,
            None,
            None
        ]
    );
}

#[test]
fn erase_display_clears_the_screen_without_moving_it_into_the_history() {
    // ED 2 는 화면을 지우고 기록으로 옮기지 않는다. clear 는 ED 3 뒤에 ED 2 를 보낸다.
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"one\r\ntwo\r\nsh$ clear\r\n");
    engine.feed(b"\x1b[3J\x1b[H\x1b[2J");
    assert_eq!(
        engine.scrollback(),
        (0, 0),
        "clear left lines in the history"
    );
    assert_eq!(text(&engine.screen()), "", "clear left text on the screen");

    // 커서는 움직이지 않고, 지운 칸은 현재 배경색을 가진다.
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"one\r\ntwo\x1b[41m\x1b[2J");
    assert_eq!(engine.scrollback(), (0, 0));
    assert_eq!(text(&engine.screen()), "");
    let cursor = engine.cursor();
    assert_eq!((cursor.row, cursor.col), (1, 3), "ED 2 moved the cursor");
    assert_eq!(
        engine.screen().lines[5][19].bg.as_deref(),
        engine.screen().lines[0][0].bg.as_deref()
    );
    assert_ne!(
        engine.screen().lines[0][0].bg.as_deref(),
        Some("#1e1e1e"),
        "erased cells lost the current background"
    );
}

#[test]
fn erase_above_clears_every_line_above_the_cursor_and_the_line_up_to_it() {
    // ED 1 은 화면 처음부터 커서까지(커서 칸 포함) 지운다. 커서가 둘째 줄에 있어도 첫 줄을 지워야 한다.
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 6);
    engine.feed(b"one\r\ntwo-three\x1b[2;4H\x1b[1J");
    let screen = engine.screen();
    assert_eq!(row_text(&screen, 0), "", "ED 1 left the first line");
    assert_eq!(
        row_text(&screen, 1).trim_start(),
        "three",
        "ED 1 did not erase up to the cursor"
    );
    let cursor = engine.cursor();
    assert_eq!((cursor.row, cursor.col), (1, 3));
}

fn replies(engine: &mut AlacrittyEngine) -> Vec<String> {
    engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::PtyWrite(bytes) => Some(String::from_utf8(bytes).unwrap()),
            _ => None,
        })
        .collect()
}

#[test]
fn osc_highlight_colors_are_set_queried_reset_and_draw_the_selection() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(10, 2);
    engine.feed(b"AB");
    let default_background = engine.screen().background;
    // 설정하지 않은 강조 배경은 테마의 선택 배경, 강조 글자는 기본 전경색이다.
    engine.set_theme(
        TerminalTheme::from_request(
            Some("dark"),
            Some("#101010"),
            Some("#e0e0e0"),
            Some("#e0e0e0"),
            Some("#334455"),
        )
        .unwrap(),
    );
    engine.feed(b"\x1b]17;?\x07\x1b]19;?\x1b\\");
    let unset = replies(&mut engine);
    assert_eq!(
        unset,
        [
            "\x1b]17;rgb:3333/4444/5555\x07",
            "\x1b]19;rgb:e0e0/e0e0/e0e0\x1b\\"
        ],
        "{unset:?}"
    );

    engine.feed(b"\x1b]17;rgb:12/34/56\x07\x1b]19;#abcdef\x07\x1b]17;?\x07\x1b]19;?\x07");
    assert_eq!(
        replies(&mut engine),
        [
            "\x1b]17;rgb:1212/3434/5656\x07",
            "\x1b]19;rgb:abab/cdcd/efef\x07"
        ]
    );
    engine.selection_start(0, 0).unwrap();
    engine.selection_update(1, 0).unwrap();
    let cell = engine.screen().lines[0][0].clone();
    assert_eq!(
        (cell.bg.as_deref(), cell.fg.as_deref(), cell.inverse),
        (Some("#123456"), Some("#abcdef"), false),
        "a selected cell uses the highlight colors instead of inverse"
    );

    engine.feed(b"\x1b]117\x07\x1b]119\x07\x1b]17;?\x07");
    assert_eq!(
        replies(&mut engine),
        unset[..1].to_vec(),
        "OSC 117 restores the default highlight background"
    );
    let cell = engine.screen().lines[0][0].clone();
    assert_eq!(
        (cell.bg.as_deref(), cell.fg.as_deref(), cell.inverse),
        (Some("#334455"), Some("#e0e0e0"), false),
        "without highlight colors a selected cell keeps its text color on the theme selection"
    );
    assert!(default_background.starts_with('#'));

    engine.feed(b"\x1b]17;not-a-color\x07");
    assert!(engine
        .drain_events()
        .iter()
        .any(|event| matches!(event, EngineEvent::Error(reason) if reason.contains("OSC 17"))));
}

#[test]
fn osc22_sets_the_pointer_shape_and_rejects_unknown_shapes() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]22;hand2\x07\x1b]22;text\x07\x1b]22;\x07\x1b]22;spaceship\x07");
    let events = engine.drain_events();
    let shapes: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::PointerShape(shape) => Some(shape.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(shapes, ["pointer", "text", "default"]);
    assert!(events
        .iter()
        .any(|event| matches!(event, EngineEvent::Error(reason) if reason.contains("spaceship"))));
}

#[test]
fn osc_special_colors_draw_attributed_text_when_enabled_and_answer_queries() {
    let mut engine = AlacrittyEngine::new();
    engine.resize(10, 2);
    engine.feed(b"\x1b]5;0;?\x07");
    let unset = replies(&mut engine);
    assert!(
        unset.len() == 1 && unset[0].starts_with("\x1b]5;0;rgb:"),
        "{unset:?}"
    );
    let default_bold = engine
        .screen()
        .lines
        .first()
        .and_then(|line| line.first())
        .and_then(|cell| cell.fg.clone());

    engine.feed(
        b"\x1b]5;0;rgb:11/22/33;4;#445566\x07\x1b[1mB\x1b[0m\x1b[3mI\x1b[0m\x1b[1;31mR\x1b[0m",
    );
    let before = engine.screen().lines[0].clone();
    assert_ne!(
        before[0].fg.as_deref(),
        Some("#112233"),
        "a special color is not used until OSC 6 enables it"
    );
    engine.feed(b"\x1b]6;0;1\x07\x1b]106;4;1\x07\x1b]5;0;?\x07");
    assert_eq!(replies(&mut engine), ["\x1b]5;0;rgb:1111/2222/3333\x07"]);
    let line = engine.screen().lines[0].clone();
    assert_eq!(
        line[0].fg.as_deref(),
        Some("#112233"),
        "bold text with the default foreground uses the bold color"
    );
    assert_eq!(
        line[1].fg.as_deref(),
        Some("#445566"),
        "italic text uses the italic color"
    );
    assert_ne!(
        line[2].fg.as_deref(),
        Some("#112233"),
        "an explicit foreground is kept"
    );

    engine.feed(b"\x1b]105;0\x07\x1b]5;0;?\x07");
    assert_eq!(replies(&mut engine), unset, "OSC 105 restores the default");
    engine.feed(b"\x1b]6;0;0\x07");
    assert_eq!(engine.screen().lines[0][0].fg, before[0].fg);
    let _ = default_bold;

    engine.feed(b"\x1b]5;2;#ffffff\x07");
    assert!(
        engine
            .drain_events()
            .iter()
            .any(|event| matches!(event, EngineEvent::Error(reason) if reason.contains("blink"))),
        "the blink color cannot apply and must be rejected"
    );
}

#[test]
fn an_inline_image_is_anchored_at_the_cursor_where_its_sequence_appears() {
    // 같은 출력 조각에서 그림 시퀀스 뒤에 온 글자가 커서를 옮겨도 그림은 시퀀스 자리의 커서에 놓인다.
    let mut engine = AlacrittyEngine::new();
    engine.resize(20, 10);
    engine.feed(
        b"\x1b[6;3H\x1b]1337;File=name=ZmlsZS5wbmc=;inline=1:aGVsbG8=\x07\r\nafter\r\nmore\r\n",
    );
    let anchors: Vec<_> = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::InlineImage { anchor, .. } => Some(anchor),
            _ => None,
        })
        .collect();
    assert_eq!(anchors.len(), 1, "{anchors:?}");
    assert_eq!((anchors[0].col, anchors[0].row), (2, 5));
}
