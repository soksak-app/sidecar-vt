use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell as GridCell, Flags};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, Handler, NamedColor, Processor, Rgb};
use alacritty_terminal::vte::{Params, Parser, Perform};
use soksak_sidecar_vt_core::directory_uri::local_path;
use soksak_sidecar_vt_core::{
    Cell, ClipboardSelection, Cursor, CursorShape as ProtocolCursorShape, Engine, EngineEvent,
    InlineAnchor, Modes, Screen, ShellMarker, TerminalTheme, default_terminal_color,
    inline_image::MAX_ENCODED_BYTES, inline_image::parse as parse_inline_image,
};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscOutcome {
    Implemented,
    Unsupported,
    Vendor,
}

fn osc_outcome(selector: &[u8]) -> OscOutcome {
    let Ok(selector) = std::str::from_utf8(selector) else {
        return OscOutcome::Unsupported;
    };
    let Ok(number) = selector.parse::<u16>() else {
        return OscOutcome::Unsupported;
    };
    match number {
        0
        | 2
        | 4
        | 5
        | 6
        | 10..=12
        | 17
        | 19
        | 22
        | 50
        | 52
        | 104
        | 105
        | 106
        | 110..=112
        | 117
        | 119 => OscOutcome::Implemented,
        7 | 8 | 9 | 133 | 1337 => OscOutcome::Vendor,
        _ => OscOutcome::Unsupported,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OscSelectorEvidence {
    pub selector: &'static str,
    pub outcome: OscOutcome,
    pub test: &'static str,
}

/// Selector-level scope used by the protocol audit. This inventory is not derived from parser
/// acceptance. Each row names an observable test or an explicit separate vendor contract.
pub const OSC_SELECTOR_INVENTORY: &[OscSelectorEvidence] = &[
    OscSelectorEvidence {
        selector: "0,2",
        outcome: OscOutcome::Implemented,
        test: "vt_events_are_retained_and_exposed_in_order",
    },
    OscSelectorEvidence {
        selector: "1,3",
        outcome: OscOutcome::Unsupported,
        test: "osc_selector_inventory_records_unsupported_operations",
    },
    OscSelectorEvidence {
        selector: "4",
        outcome: OscOutcome::Implemented,
        test: "indexed_colors_and_combining_characters_survive_export",
    },
    OscSelectorEvidence {
        selector: "5,6,105,106",
        outcome: OscOutcome::Implemented,
        test: "osc_special_colors_draw_attributed_text_when_enabled_and_answer_queries",
    },
    OscSelectorEvidence {
        selector: "10-12",
        outcome: OscOutcome::Implemented,
        test: "dynamic_color_replies_and_screen_colors_use_the_same_palette",
    },
    OscSelectorEvidence {
        selector: "13-16,18,21,46",
        outcome: OscOutcome::Unsupported,
        test: "osc_selector_inventory_records_unsupported_operations",
    },
    OscSelectorEvidence {
        selector: "17,19,117,119",
        outcome: OscOutcome::Implemented,
        test: "osc_highlight_colors_are_set_queried_reset_and_draw_the_selection",
    },
    OscSelectorEvidence {
        selector: "22",
        outcome: OscOutcome::Implemented,
        test: "osc22_sets_the_pointer_shape_and_rejects_unknown_shapes",
    },
    OscSelectorEvidence {
        selector: "50",
        outcome: OscOutcome::Implemented,
        test: "osc50_cursor_shape_changes_program_cursor",
    },
    OscSelectorEvidence {
        selector: "51",
        outcome: OscOutcome::Unsupported,
        test: "osc_selector_inventory_records_unsupported_operations",
    },
    OscSelectorEvidence {
        selector: "52",
        outcome: OscOutcome::Implemented,
        test: "clipboard_query_uses_a_token_and_resolves_to_pty_bytes",
    },
    OscSelectorEvidence {
        selector: "60-62",
        outcome: OscOutcome::Unsupported,
        test: "osc_selector_inventory_records_unsupported_operations",
    },
    OscSelectorEvidence {
        selector: "104",
        outcome: OscOutcome::Implemented,
        test: "osc104_resets_indexed_colors",
    },
    OscSelectorEvidence {
        selector: "110-112",
        outcome: OscOutcome::Implemented,
        test: "osc_dynamic_color_resets_restore_defaults",
    },
    OscSelectorEvidence {
        selector: "I,l,L",
        outcome: OscOutcome::Unsupported,
        test: "osc_selector_inventory_records_unsupported_operations",
    },
    OscSelectorEvidence {
        selector: "7,8,9,133",
        outcome: OscOutcome::Vendor,
        test: "vendor_osc_contracts_are_separate",
    },
    OscSelectorEvidence {
        selector: "1337",
        outcome: OscOutcome::Vendor,
        test: "osc1337_inline_image_is_typed_and_survives_input_chunk_boundaries",
    },
];

/// Forms of X color names: `rgb:h/h/h` (1–4 hexadecimal digits per component) and `#hhh` through
/// `#hhhhhhhhhhhh`.
fn parse_x_color(value: &str) -> Option<Rgb> {
    let scale = |digits: &str| -> Option<u8> {
        if digits.is_empty() || digits.len() > 4 {
            return None;
        }
        // silent: a malformed color text is not a color, and the function returns None to its
        // caller.
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = (1u32 << (4 * digits.len())) - 1;
        Some(((value * 255 + max / 2) / max) as u8)
    };
    if let Some(rest) = value.strip_prefix("rgb:") {
        let parts: Vec<_> = rest.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        return Some(Rgb {
            r: scale(parts[0])?,
            g: scale(parts[1])?,
            b: scale(parts[2])?,
        });
    }
    let hex = value.strip_prefix('#')?;
    if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 {
        return None;
    }
    let width = hex.len() / 3;
    Some(Rgb {
        r: scale(&hex[..width])?,
        g: scale(&hex[width..2 * width])?,
        b: scale(&hex[2 * width..])?,
    })
}

/// Converts an OSC 22 pointer name into a CSS cursor value. Accepts X cursor font names and CSS
/// names. An empty name means the default.
fn pointer_shape(name: &str) -> Option<&'static str> {
    Some(match name {
        "" | "default" | "left_ptr" | "arrow" | "top_left_arrow" => "default",
        "text" | "xterm" | "ibeam" => "text",
        "pointer" | "hand" | "hand1" | "hand2" => "pointer",
        "wait" | "watch" => "wait",
        "progress" => "progress",
        "crosshair" | "cross" | "tcross" => "crosshair",
        "move" | "fleur" => "move",
        "help" | "question_arrow" => "help",
        "not-allowed" | "X_cursor" => "not-allowed",
        "ew-resize" | "sb_h_double_arrow" => "ew-resize",
        "ns-resize" | "sb_v_double_arrow" => "ns-resize",
        "col-resize" => "col-resize",
        "row-resize" => "row-resize",
        "grab" => "grab",
        "grabbing" => "grabbing",
        "none" => "none",
        _ => return None,
    })
}

fn parse_vendor_osc(selector: &[u8], payload: &[u8]) -> Result<Option<EngineEvent>, String> {
    let selector = std::str::from_utf8(selector)
        .map_err(|_| "vendor OSC selector is not UTF-8".to_string())?;
    let payload =
        std::str::from_utf8(payload).map_err(|_| format!("OSC {selector} payload is not UTF-8"))?;
    match selector {
        "7" => {
            if payload.is_empty() {
                return Err("OSC 7 directory URI is empty".to_string());
            }
            Ok(Some(EngineEvent::Directory {
                uri: payload.to_string(),
                path: local_path(payload)?,
            }))
        }
        "8" => {
            let Some((params, uri)) = payload.split_once(';') else {
                return Err("OSC 8 hyperlink payload must contain params and URI".to_string());
            };
            let id = if params.is_empty() {
                String::new()
            } else {
                let mut id = None;
                for parameter in params.split(':') {
                    let Some(value) = parameter.strip_prefix("id=") else {
                        return Err(format!(
                            "OSC 8 hyperlink parameter is unsupported: {parameter}"
                        ));
                    };
                    if id.replace(value).is_some() {
                        return Err("OSC 8 hyperlink id is duplicated".to_string());
                    }
                }
                // default: an OSC 8 link without an id has an empty id (the id is an optional
                // parameter).
                id.unwrap_or_default().to_string()
            };
            Ok(Some(EngineEvent::Hyperlink {
                id,
                uri: (!uri.is_empty()).then_some(uri.to_string()),
            }))
        }
        "9" => {
            if payload.is_empty() {
                return Err("OSC 9 notification is empty".to_string());
            }
            Ok(Some(EngineEvent::Notification {
                message: payload.to_string(),
            }))
        }
        "133" => {
            // default: an OSC 133 mark without parameters has empty parameters.
            let (marker, params) = payload.split_once(';').unwrap_or((payload, ""));
            for parameter in params.split(';') {
                if let Some(value) = parameter.strip_prefix("redraw=") {
                    parse_redraw(value)?;
                }
            }
            let marker = match marker {
                "A" => ShellMarker::PromptStart,
                "B" => ShellMarker::PromptEnd,
                "C" => ShellMarker::CommandStart,
                "D" => ShellMarker::CommandFinished,
                value => return Err(format!("OSC 133 shell marker is unsupported: {value}")),
            };
            Ok(Some(EngineEvent::ShellState {
                marker,
                params: if params.is_empty() {
                    Vec::new()
                } else {
                    params.split(';').map(str::to_string).collect()
                },
            }))
        }
        "1337" => Ok(None),
        _ => Err(format!("unsupported vendor OSC selector {selector}")),
    }
}

/// OSC 133 `redraw` parameter: the area that the shell redraws after a size change. `0` does not
/// redraw. `1` redraws the whole prompt (zsh), so the cursor moves up to the first row of the
/// prompt. `last` redraws only the last line of the prompt (readline), so the cursor moves up to
/// the first row of the logical line that holds the cursor.
fn parse_redraw(value: &str) -> Result<Redraw, String> {
    match value {
        "0" => Ok(Redraw::Never),
        "1" => Ok(Redraw::Prompt),
        "last" => Ok(Redraw::LastLine),
        value => Err(format!("OSC 133 redraw value is unsupported: {value}")),
    }
}

/// Function that builds an OSC 52 reply from clipboard text. One is kept for each clipboard read
/// request number.
type ClipboardFormatter = Arc<dyn Fn(&str) -> String + Sync + Send + 'static>;

/// Shell state set by the OSC 133 marks. It decides whether the prompt rows are erased on a size
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellState {
    Output,
    /// At a prompt or during input. The value is the area that the shell redraws after a size
    /// change.
    Prompt(Redraw),
}

/// Area that the shell redraws after a size change (OSC 133 `redraw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redraw {
    Never,
    /// Redraws from the first row of the prompt.
    Prompt,
    /// Redraws from the first row of the logical line that holds the cursor.
    LastLine,
}

/// ED sequence that is erased directly, not by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Erase {
    All,
    Above,
    History,
}

/// Position where an OSC 133 mark ends within an input chunk, and the mark.
struct ShellMark {
    marker: ShellMarker,
    redraw: Option<Redraw>,
}

/// A sequence over this size is rejected: the largest encoded image and its header. `vte` with `std`
/// keeps an OSC in a `Vec` without a limit, so the engine bounds what it passes to the parsers.
const HELD_SEQUENCE_BYTES: usize = MAX_ENCODED_BYTES + 4096;

/// A sequence that the scan found.
enum Sequence {
    Osc {
        body: Vec<u8>,
        bell: bool,
    },
    Csi {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignored: bool,
        action: char,
    },
    Dcs(String),
    Esc {
        intermediates: Vec<u8>,
        byte: u8,
    },
}

/// The `Perform` that receives the first parse of the output. The parser stops at each sequence, so
/// the engine knows the position of the sequence in its input.
struct SequenceScan {
    found: Option<Sequence>,
    /// False from the last escape byte of an input until the parser returns to its ground state.
    left_sequence: bool,
}

impl Default for SequenceScan {
    fn default() -> Self {
        Self {
            found: None,
            left_sequence: true,
        }
    }
}

impl Perform for SequenceScan {
    fn print(&mut self, _c: char) {
        self.left_sequence = true;
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, 0x18 | 0x1a) {
            self.left_sequence = true;
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        self.left_sequence = true;
        self.found = Some(Sequence::Dcs(sequence_selector(
            &param_values(params),
            intermediates,
            action,
        )));
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.left_sequence = true;
        self.found = Some(Sequence::Osc {
            body: params.join(&b';'),
            bell: bell_terminated,
        });
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.left_sequence = true;
        self.found = Some(Sequence::Csi {
            params: param_values(params),
            intermediates: intermediates.to_vec(),
            ignored: ignore,
            action,
        });
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.left_sequence = true;
        self.found = Some(Sequence::Esc {
            intermediates: intermediates.to_vec(),
            byte,
        });
    }

    fn terminated(&self) -> bool {
        self.found.is_some()
    }
}

fn param_values(params: &Params) -> Vec<Vec<u16>> {
    params.iter().map(<[u16]>::to_vec).collect()
}

/// The text of a sequence without its payload: a private marker, the parameters, the other
/// intermediates and the final.
fn sequence_selector(params: &[Vec<u16>], intermediates: &[u8], action: char) -> String {
    let (marker, rest) = match intermediates.split_first() {
        Some((first, rest)) if (0x3c..=0x3f).contains(first) => {
            (String::from(*first as char), rest)
        }
        _ => (String::new(), intermediates),
    };
    // The parser reports an absent parameter as a single 0.
    let params = if params == [vec![0]] { &[] } else { params };
    let values = params
        .iter()
        .map(|param| {
            param
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(":")
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("{marker}{values}{}{action}", String::from_utf8_lossy(rest))
}

/// Whether the library executes this CSI. The arms are those of `csi_dispatch` in `vte::ansi`, which
/// reports another CSI only as a debug line.
fn csi_executed(params: &[Vec<u16>], intermediates: &[u8], ignored: bool, action: char) -> bool {
    if ignored || intermediates.len() > 2 {
        return false;
    }
    let first = params.first().and_then(|param| param.first()).copied();
    match (action, intermediates) {
        ('c', _) => first.is_none_or(|value| value == 0),
        ('W', b"?") => first == Some(5),
        ('@' | 'A' | 'B' | 'a' | 'b' | 'C' | 'D' | 'd' | 'e' | 'E' | 'F' | 'G' | '`', b"") => true,
        ('g' | 'H' | 'f' | 'h' | 'I' | 'J' | 'K' | 'L' | 'l' | 'M' | 'm' | 'n' | 'P', b"") => true,
        ('r' | 'S' | 's' | 'T' | 't' | 'u' | 'X' | 'Z', b"") => true,
        ('h' | 'l' | 'm' | 'u', b"?") | ('m' | 'u', b">") | ('u', b"=" | b"<") => true,
        ('k' | 'q', b" ") | ('p', b"$" | b"?$") => true,
        _ => false,
    }
}

/// Whether the library executes this escape sequence, from `esc_dispatch` in `vte::ansi`.
fn esc_executed(intermediates: &[u8], byte: u8) -> bool {
    matches!(
        (byte, intermediates),
        (b'B' | b'0', b"(" | b")" | b"*" | b"+")
            | (b'8', b"#")
            | (
                b'D' | b'E' | b'H' | b'M' | b'Z' | b'c' | b'7' | b'8' | b'=' | b'>' | b'\\',
                b""
            )
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsiOutcome {
    Implemented,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsiSelectorEvidence {
    pub selector: &'static str,
    pub outcome: CsiOutcome,
    pub test: &'static str,
}

/// Selector-level evidence for the CSI actions that this sidecar currently exposes. The inventory
/// is intentionally partial until the remaining XTerm categories have executable action and
/// rejection contracts.
pub const CSI_SELECTOR_INVENTORY: &[CsiSelectorEvidence] = &[
    CsiSelectorEvidence {
        selector: "A/B/C/D/G/H/f/s/u",
        outcome: CsiOutcome::Implemented,
        test: "csi_cursor_movement_and_save_restore_are_observable",
    },
    CsiSelectorEvidence {
        selector: "E/F",
        outcome: CsiOutcome::Implemented,
        test: "csi_cursor_next_and_previous_line_are_observable",
    },
    CsiSelectorEvidence {
        selector: "3C",
        outcome: CsiOutcome::Implemented,
        test: "display_points_are_used_as_cell_indices",
    },
    CsiSelectorEvidence {
        selector: "?12h/l",
        outcome: CsiOutcome::Implemented,
        test: "cursor_visibility_and_application_shape_are_exported",
    },
    CsiSelectorEvidence {
        selector: "?25h/l",
        outcome: CsiOutcome::Implemented,
        test: "cursor_visibility_and_application_shape_are_exported",
    },
    CsiSelectorEvidence {
        selector: "0,7 SP q",
        outcome: CsiOutcome::Implemented,
        test: "decscusr_initial_cursor_resources_are_observable",
    },
    CsiSelectorEvidence {
        selector: "1-6 SP q",
        outcome: CsiOutcome::Implemented,
        test: "decscusr_cursor_style_ids_are_observable",
    },
    CsiSelectorEvidence {
        selector: "CSI framing",
        outcome: CsiOutcome::Implemented,
        test: "csi_fragmentation_and_malformed_input_preserve_engine_state",
    },
    CsiSelectorEvidence {
        selector: "m",
        outcome: CsiOutcome::Implemented,
        test: "sgr_color_does_not_drop_the_character",
    },
    CsiSelectorEvidence {
        selector: "?1049h/l",
        outcome: CsiOutcome::Implemented,
        test: "alternate_screen_is_separate_from_primary_scrollback",
    },
    CsiSelectorEvidence {
        selector: "?47/?1047/?1048h/l",
        outcome: CsiOutcome::Unsupported,
        test: "unsupported_csi_alternate_modes_are_explicit_errors",
    },
    CsiSelectorEvidence {
        selector: "S/T;r",
        outcome: CsiOutcome::Implemented,
        test: "csi_scroll_moves_the_visible_grid_and_respects_a_scroll_region",
    },
    CsiSelectorEvidence {
        selector: "J/K",
        outcome: CsiOutcome::Implemented,
        test: "csi_erase_display_and_line_change_only_the_requested_cells",
    },
    CsiSelectorEvidence {
        selector: "@/P",
        outcome: CsiOutcome::Implemented,
        test: "csi_insert_delete_characters_and_lines_preserve_requested_cells",
    },
    CsiSelectorEvidence {
        selector: "L/M",
        outcome: CsiOutcome::Implemented,
        test: "csi_insert_delete_characters_and_lines_preserve_requested_cells",
    },
    CsiSelectorEvidence {
        selector: "I/Z",
        outcome: CsiOutcome::Implemented,
        test: "csi_tabulation_forward_and_backward_use_tab_stops",
    },
    CsiSelectorEvidence {
        selector: "6n/c",
        outcome: CsiOutcome::Implemented,
        test: "bel_and_st_terminated_effects_and_queries_preserve_response_order",
    },
    CsiSelectorEvidence {
        selector: "5n/6n",
        outcome: CsiOutcome::Implemented,
        test: "csi_device_status_reports_are_observable",
    },
    CsiSelectorEvidence {
        selector: "c/>c",
        outcome: CsiOutcome::Implemented,
        test: "csi_device_status_reports_are_observable",
    },
    CsiSelectorEvidence {
        selector: "b",
        outcome: CsiOutcome::Implemented,
        test: "csi_repeat_repeats_the_last_printed_character",
    },
    CsiSelectorEvidence {
        selector: "?1,?1000,?1002,?1003,?1004,?1005,?1006,?1007,?2004 h/l",
        outcome: CsiOutcome::Implemented,
        test: "csi_private_modes_export_keyboard_paste_and_mouse_state",
    },
    CsiSelectorEvidence {
        selector: "ESC =/>",
        outcome: CsiOutcome::Implemented,
        test: "csi_application_keypad_mode_uses_the_private_equals_prefix",
    },
    CsiSelectorEvidence {
        selector: "14t",
        outcome: CsiOutcome::Implemented,
        test: "text_area_callback_is_not_discarded",
    },
    CsiSelectorEvidence {
        selector: "other t",
        outcome: CsiOutcome::Unsupported,
        test: "unsupported_csi_window_report_is_an_explicit_error",
    },
    CsiSelectorEvidence {
        selector: "rectangle/protected/palette",
        outcome: CsiOutcome::Unsupported,
        test: "unsupported_csi_rectangle_protected_and_palette_reports_are_explicit_errors",
    },
];

#[derive(Clone, Copy)]
struct TermSize {
    columns: usize,
    lines: usize,
}

impl TermSize {
    fn new(columns: usize, lines: usize) -> Self {
        Self { columns, lines }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Keeps the events that the VT engine raised, in the order they occurred, until the owner takes
/// them. Callback events are converted to the values that the engine boundary needs, then passed on
/// as neutral events.
struct EventQueue {
    events: Mutex<VecDeque<QueuedEvent>>,
}

enum QueuedEvent {
    Alacritty(Event),
    Neutral(EngineEvent),
}

impl EventQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(VecDeque::new()),
        })
    }

    fn entries(&self) -> MutexGuard<'_, VecDeque<QueuedEvent>> {
        self.events
            .lock()
            // default: poison retains a valid VecDeque; report recovery before clearing the flag.
            .unwrap_or_else(|error| {
                let mut events = error.into_inner();
                events.push_back(QueuedEvent::Neutral(EngineEvent::RuntimeError(
                    "engine event queue lock was poisoned; queued events retained".to_string(),
                )));
                self.events.clear_poison();
                events
            })
    }

    fn push(&self, event: QueuedEvent) {
        self.entries().push_back(event);
    }

    fn drain(&self) -> Vec<QueuedEvent> {
        self.entries().drain(..).collect()
    }
}

#[derive(Clone)]
struct EventSink(Arc<EventQueue>);

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        self.0.push(QueuedEvent::Alacritty(event));
    }
}

pub struct AlacrittyEngine {
    term: Term<EventSink>,
    processor: Processor,
    events: Arc<EventQueue>,
    pending_clipboard: HashMap<u64, ClipboardFormatter>,
    next_clipboard_request: u64,
    cell_metrics: Option<(u16, u16)>,
    theme: TerminalTheme,
    /// Parses the output a first time, so that the engine acts on a sequence at its position.
    scanner: Parser,
    scan: SequenceScan,
    /// The start of a sequence that has not ended. The terminal receives it when it ends.
    held: Vec<u8>,
    /// The rest of a sequence over `HELD_SEQUENCE_BYTES`, which the engine removes from the output.
    discarding: bool,
    /// Cell where the selection started. Depending on the drag direction, it selects the side of
    /// the boundary so that both this cell and the pointer cell are included.
    selection_anchor: Option<(Point, Side)>,
    shell: ShellState,
    /// Absolute row number of the first prompt row (dropped + history row count + screen row).
    /// Present only in the prompt state.
    prompt_start: Option<i64>,
    /// Number of rows dropped past the history limit, and history rows erased by ED 3.
    dropped: i64,
    /// Limit on the number of history rows.
    history_limit: usize,
    /// Background and foreground colors of the selection, set by OSC 17 and 19. When unset,
    /// selected cells are drawn inverted.
    highlight_background: Option<Rgb>,
    highlight_foreground: Option<Rgb>,
    /// Special colors set by OSC 5 (0 bold, 1 underline, 2 blink, 3 inverse, 4 italic), and the
    /// enabled flags set by OSC 6 and 106.
    special_colors: [Option<Rgb>; 5],
    special_enabled: [bool; 5],
}

/// The rows above the history limit that the engine counts at one time. The library frees the rows
/// above its cache of 1000 rows when the limit returns (`Storage::shrink_lines`), so the count stays
/// inside that cache and a flood recycles rows instead of allocating them.
const COUNTED_ROWS: usize = 1000;

/// The input between two counts. One byte of a line feed pushes one row, so ordinary output stays
/// below `COUNTED_ROWS`, and reaching that number means that rows may have gone uncounted.
const ADVANCE_SLICE_BYTES: usize = COUNTED_ROWS - 1;

impl Default for AlacrittyEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AlacrittyEngine {
    pub fn new() -> Self {
        let events = EventQueue::new();
        let config = Config {
            osc52: Osc52::CopyPaste,
            ..Default::default()
        };
        let history_limit = config.scrolling_history;
        let term = Term::new(config, &TermSize::new(80, 24), EventSink(events.clone()));
        Self {
            term,
            processor: Processor::new(),
            events,
            pending_clipboard: HashMap::new(),
            next_clipboard_request: 1,
            cell_metrics: None,
            theme: TerminalTheme::dark(),
            scanner: Parser::new(),
            scan: SequenceScan::default(),
            held: Vec::new(),
            discarding: false,
            selection_anchor: None,
            shell: ShellState::Output,
            prompt_start: None,
            dropped: 0,
            history_limit,
            highlight_background: None,
            highlight_foreground: None,
            special_colors: [None; 5],
            special_enabled: [false; 5],
        }
    }

    /// Erases with the current background color: ED 2 (whole screen) and ED 1 (from the top of the
    /// screen to the cursor cell). History and cursor are unchanged.
    fn erase(&mut self, erase: Erase) {
        let bg = self.term.grid().cursor.template.bg;
        let cursor = self.term.grid().cursor.point;
        let columns = self.term.grid().columns();
        let grid = self.term.grid_mut();
        match erase {
            Erase::All => {
                grid.reset_region(..);
                // After the screen is cleared, the shell redraws the prompt on the top row without
                // OSC 133;A.
                if self.prompt_start.is_some() {
                    self.prompt_start = Some(self.dropped + grid.history_size() as i64);
                }
            }
            Erase::History => {
                // Erased history rows are counted as dropped rows, so the absolute numbers of the
                // screen rows stay the same.
                self.dropped += grid.history_size() as i64;
                grid.clear_history();
            }
            Erase::Above => {
                if cursor.line.0 > 0 {
                    grid.reset_region(..cursor.line);
                }
                let end = (cursor.column.0 + 1).min(columns);
                for cell in &mut grid[cursor.line][..Column(end)] {
                    *cell = bg.into();
                }
            }
        }
        self.term.selection = None;
    }

    /// Pushes the event of one OSC. An accepted OSC 133 mark is returned, so that the shell state is
    /// applied after the output before it is processed.
    fn handle_osc(&mut self, body: &[u8], bel: bool) -> Option<ShellMark> {
        let (selector, payload) = body
            .iter()
            .position(|byte| *byte == b';')
            .map(|position| (&body[..position], &body[position + 1..]))
            // default: an OSC without a semicolon has only a selector and an empty body.
            .unwrap_or((body, &[]));
        let outcome = osc_outcome(selector);
        let event = if let Some(result) = self.engine_osc(selector, payload, bel) {
            result
        } else if outcome == OscOutcome::Vendor {
            match parse_vendor_osc(selector, payload) {
                Ok(Some(event)) => Some(event),
                Ok(None) => None,
                Err(error) => Some(EngineEvent::Error(error)),
            }
        } else if outcome == OscOutcome::Unsupported {
            let selector = String::from_utf8_lossy(selector);
            Some(EngineEvent::Error(format!(
                "unsupported OSC selector {selector}"
            )))
        } else {
            None
        };
        let mark = if let Some(EngineEvent::ShellState { marker, params }) = &event {
            let redraw = params
                .iter()
                .find_map(|parameter| parameter.strip_prefix("redraw="))
                .map(|value| parse_redraw(value).expect("redraw was validated by the parser"));
            Some(ShellMark {
                marker: *marker,
                redraw,
            })
        } else {
            None
        };
        if let Some(event) = event {
            self.events.push(QueuedEvent::Neutral(event));
        }
        mark
    }

    /// OSCs that the engine handles directly: setting and querying the 17/19 highlight colors,
    /// resetting with 117/119, and the 22 pointer shape. A selector that is not handled returns
    /// None. A query reply ends with the same terminator as the request.
    fn engine_osc(
        &mut self,
        selector: &[u8],
        payload: &[u8],
        bel: bool,
    ) -> Option<Option<EngineEvent>> {
        // silent: a selector that is not UTF-8 is no known selector, and the function returns None.
        let selector = std::str::from_utf8(selector).ok()?;
        let payload = String::from_utf8_lossy(payload);
        let terminator = if bel { "\x07" } else { "\x1b\\" };
        Some(match selector {
            "17" | "19" if payload == "?" => {
                // An unset highlight background is the theme's selection background. An unset
                // highlight foreground is the default foreground color.
                let rgb = if selector == "17" {
                    self.highlight_background
                        .or(Some(self.theme_rgb(self.theme.selection)))
                } else {
                    self.highlight_foreground
                        .or_else(|| self.default_rgb(NamedColor::Foreground))
                };
                rgb.map(|rgb| {
                    EngineEvent::PtyWrite(
                        format!(
                    "\x1b]{selector};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}{terminator}",
                    rgb.r, rgb.r, rgb.g, rgb.g, rgb.b, rgb.b
                )
                        .into_bytes(),
                    )
                })
            }
            "17" | "19" => match parse_x_color(&payload) {
                Some(rgb) => {
                    if selector == "17" {
                        self.highlight_background = Some(rgb)
                    } else {
                        self.highlight_foreground = Some(rgb)
                    }
                    None
                }
                None => Some(EngineEvent::Error(format!(
                    "OSC {selector} color is not an X color: {payload}"
                ))),
            },
            "5" => self.special_color_osc(&payload, terminator),
            "6" | "106" => self.special_mode_osc(selector, &payload),
            "105" => {
                if payload.is_empty() {
                    self.special_colors = [None; 5];
                    None
                } else {
                    let mut error = None;
                    for part in payload.split(';') {
                        match part.parse::<usize>() {
                            Ok(index) if index < 5 => self.special_colors[index] = None,
                            _ => {
                                error = Some(EngineEvent::Error(format!(
                                    "OSC 105 special color number is not 0-4: {part}"
                                )))
                            }
                        }
                    }
                    error
                }
            }
            "117" => {
                self.highlight_background = None;
                None
            }
            "119" => {
                self.highlight_foreground = None;
                None
            }
            "22" => match pointer_shape(&payload) {
                Some(shape) => Some(EngineEvent::PointerShape(shape.to_string())),
                None => Some(EngineEvent::Error(format!(
                    "OSC 22 pointer shape is not supported: {payload}"
                ))),
            },
            _ => return None,
        })
    }

    /// OSC 5: pairs of special color number and color name. `?` replies with the current color,
    /// ending with the same terminator as the request. Blink (2) cannot be applied because the grid
    /// does not keep a blink attribute, so it is an explicit error.
    fn special_color_osc(&mut self, payload: &str, terminator: &str) -> Option<EngineEvent> {
        let parts: Vec<_> = payload.split(';').collect();
        if parts.len() % 2 != 0 {
            return Some(EngineEvent::Error(format!(
                "OSC 5 needs pairs of a color number and a color: {payload}"
            )));
        }
        let mut replies = String::new();
        for pair in parts.chunks(2) {
            let index = match pair[0].parse::<usize>() {
                Ok(index) if index < 5 => index,
                _ => {
                    return Some(EngineEvent::Error(format!(
                        "OSC 5 special color number is not 0-4: {}",
                        pair[0]
                    )));
                }
            };
            if index == 2 {
                return Some(EngineEvent::Error(
                    "OSC 5 blink color cannot apply: the grid does not keep the blink attribute"
                        .to_string(),
                ));
            }
            if pair[1] == "?" {
                let Some(rgb) =
                    self.special_colors[index].or_else(|| self.default_rgb(NamedColor::Foreground))
                else {
                    return Some(EngineEvent::Error(
                        "OSC 5 query has no foreground color".to_string(),
                    ));
                };
                replies.push_str(&format!(
                    "\x1b]5;{index};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}{terminator}",
                    rgb.r, rgb.r, rgb.g, rgb.g, rgb.b, rgb.b
                ));
            } else {
                match parse_x_color(pair[1]) {
                    Some(rgb) => self.special_colors[index] = Some(rgb),
                    None => {
                        return Some(EngineEvent::Error(format!(
                            "OSC 5 color is not an X color: {}",
                            pair[1]
                        )));
                    }
                }
            }
        }
        (!replies.is_empty()).then(|| EngineEvent::PtyWrite(replies.into_bytes()))
    }

    /// OSC 6 and 106: pairs of special color number and on (non-zero value) or off (0) state.
    fn special_mode_osc(&mut self, selector: &str, payload: &str) -> Option<EngineEvent> {
        let parts: Vec<_> = payload.split(';').collect();
        if parts.len() % 2 != 0 {
            return Some(EngineEvent::Error(format!(
                "OSC {selector} needs pairs of a color number and a flag: {payload}"
            )));
        }
        for pair in parts.chunks(2) {
            match (pair[0].parse::<usize>(), pair[1].parse::<u32>()) {
                (Ok(2), Ok(_)) => {
                    return Some(EngineEvent::Error(format!(
                        "OSC {selector} blink color cannot apply: the grid does not keep the blink attribute"
                    )));
                }
                (Ok(index), Ok(flag)) if index < 5 => self.special_enabled[index] = flag != 0,
                _ => {
                    return Some(EngineEvent::Error(format!(
                        "OSC {selector} pair is not a color number 0-4 and a flag: {};{}",
                        pair[0], pair[1]
                    )));
                }
            }
        }
        None
    }

    /// Applies an enabled special color to default-foreground text that has attributes. The first
    /// color is used in the order bold, underline, inverse, italic.
    fn special_foreground(&self, flags: Flags, foreground: Color) -> Option<Rgb> {
        if foreground != Color::Named(NamedColor::Foreground) {
            return None;
        }
        [
            (0, Flags::BOLD),
            (1, Flags::UNDERLINE),
            (3, Flags::INVERSE),
            (4, Flags::ITALIC),
        ]
        .into_iter()
        .find(|(index, flag)| {
            flags.contains(*flag)
                && self.special_enabled[*index]
                && self.special_colors[*index].is_some()
        })
        .and_then(|(index, _)| self.special_colors[index])
    }

    /// Default color set by the theme.
    fn default_rgb(&self, name: NamedColor) -> Option<Rgb> {
        self.term.colors()[name].or_else(|| {
            self.theme
                .color(name as usize)
                .or_else(|| default_terminal_color(name as usize))
                .map(|rgb| Rgb {
                    r: rgb[0],
                    g: rgb[1],
                    b: rgb[2],
                })
        })
    }

    fn apply_shell_mark(&mut self, marker: ShellMarker, redraw: Option<Redraw>) {
        self.shell = match marker {
            ShellMarker::PromptStart => {
                // A prompt starts on a new row. If the previous output ended without a line break,
                // the cursor moves to the next row.
                let cursor = &self.term.grid().cursor;
                if cursor.point.column.0 != 0 || cursor.input_needs_wrap {
                    self.processor.advance(&mut self.term, b"\r\n");
                }
                self.prompt_start = Some(self.absolute(self.term.grid().cursor.point.line.0));
                // default: an A mark without a redraw parameter has redraw=1
                // (docs/spec/terminal-runtime.md).
                ShellState::Prompt(redraw.unwrap_or(Redraw::Prompt))
            }
            ShellMarker::PromptEnd => match (self.shell, redraw) {
                (_, Some(redraw)) => ShellState::Prompt(redraw),
                (ShellState::Prompt(current), None) => ShellState::Prompt(current),
                (ShellState::Output, None) => ShellState::Prompt(Redraw::Prompt),
            },
            ShellMarker::CommandStart | ShellMarker::CommandFinished => {
                self.prompt_start = None;
                ShellState::Output
            }
        };
    }

    /// First row of the logical line that holds the cursor. Includes a row that reflow moved into
    /// history when that row belongs to the same logical line.
    fn cursor_logical_line_start(&self) -> i32 {
        let grid = self.term.grid();
        let last_column = Column(grid.columns() - 1);
        let top = -(grid.history_size() as i32);
        let mut start = grid.cursor.point.line.0;
        while start > top
            && grid[Line(start - 1)][last_column]
                .flags
                .contains(Flags::WRAPLINE)
        {
            start -= 1;
        }
        start
    }

    /// Before the shell redraws after a size change, erases from the first row (start) of the area
    /// that the shell redraws through the end of the screen, and moves the cursor to column 0 of
    /// the row that is `offset` rows below that first row. The shell moves up by the number of
    /// cursor rows at the previous width (offset) and writes again. When that row goes past the
    /// bottom of the screen, the screen is scrolled up by the amount that it goes past.
    fn clear_prompt_for_redraw(&mut self, mut start: i32, offset: i32) {
        let bottom = self.term.grid().screen_lines() as i32 - 1;
        let template = GridCell::default();
        let grid = self.term.grid_mut();
        for line in start..=bottom {
            grid[Line(line)].reset(&template);
        }
        let overflow = start + offset - bottom;
        if overflow > 0 {
            self.term.goto(bottom, 0);
            for _ in 0..overflow {
                self.term.linefeed();
            }
            start -= overflow;
        }
        self.term.goto((start + offset).clamp(0, bottom), 0);
    }

    /// Absolute number of a screen row (history rows are negative).
    fn absolute(&self, line: i32) -> i64 {
        self.dropped + self.term.grid().history_size() as i64 + i64::from(line)
    }

    /// Feeds input into the engine. The history limit is raised temporarily so that every row a
    /// slice of this input pushes up is kept in history. Then the rows past the limit are dropped
    /// and counted. Without this, the library drops the rows at the limit silently, and the absolute
    /// numbers of the screen rows and the scroll generation of inline images stop moving. One byte
    /// cannot push up more rows than the screen has.
    fn advance(&mut self, bytes: &[u8]) {
        for slice in bytes.chunks(ADVANCE_SLICE_BYTES) {
            self.term
                .grid_mut()
                .update_history(self.history_limit + COUNTED_ROWS);
            self.processor.advance(&mut self.term, slice);
            self.count_pushed_rows();
        }
    }

    /// Reduces the history to the limit and counts the dropped rows. Changes the grid size. The
    /// engine treats the screen as the last rows of storage, so a row added by a reflow that
    /// narrows the width pushes the rows above it into history, even when an empty row exists below
    /// the cursor. In the primary screen, the width is changed first with the row count unchanged.
    /// Then as many rows as there are empty rows below the cursor are returned from history to the
    /// screen (docs/spec/terminal-runtime.md). Empty rows are below the cursor, so reducing the row
    /// count by that amount removes only those rows and the screen does not move. When the row
    /// count grows again, the engine moves history rows back up.
    fn resize_grid(&mut self, primary: bool, cols: usize, rows: usize) {
        let lines = self.term.grid().screen_lines();
        if primary && cols < self.term.grid().columns() {
            // If a row at the history limit is dropped, the number of moved rows cannot be counted,
            // so the limit is raised by the screen row count only while the rows are counted.
            let history = self.term.grid().history_size();
            self.term
                .grid_mut()
                .update_history(self.history_limit.max(history) + lines);
            self.term.resize(TermSize::new(cols, lines));
            let moved = self.term.grid().history_size() - history;
            let back = moved.min(self.empty_rows_below_cursor());
            if back > 0 {
                self.term.resize(TermSize::new(cols, lines - back));
                self.term.resize(TermSize::new(cols, lines));
            }
            self.limit_history();
        }
        self.term.resize(TermSize::new(cols, rows));
    }

    /// Number of empty rows from below the cursor row to the bottom of the screen.
    fn empty_rows_below_cursor(&self) -> usize {
        let grid = self.term.grid();
        let cursor = grid.cursor.point.line.0;
        let bottom = grid.screen_lines() as i32 - 1;
        (cursor + 1..=bottom)
            .rev()
            .take_while(|row| grid[Line(*row)].is_clear())
            .count()
    }

    /// Counts the rows that the input pushed above the limit, and reports when more rows than the
    /// count covers may have passed, because the library drops those without a record.
    fn count_pushed_rows(&mut self) {
        if self.term.grid().history_size() >= self.history_limit + COUNTED_ROWS {
            self.events.push(QueuedEvent::Neutral(EngineEvent::Error(format!(
                "one input pushed {COUNTED_ROWS} or more rows above the history limit, so the row count may be low"
            ))));
        }
        self.limit_history();
    }

    fn limit_history(&mut self) {
        let excess = self
            .term
            .grid()
            .history_size()
            .saturating_sub(self.history_limit);
        self.term.grid_mut().update_history(self.history_limit);
        self.dropped += excess as i64;
    }

    /// Screen row of the first prompt row. If it was dropped past the history or lies below the
    /// cursor, the area that the shell redraws is unknown, so this is an error.
    fn prompt_row(&self) -> Result<i32, String> {
        let start = self.prompt_start.ok_or("no prompt start is recorded")?;
        let grid = self.term.grid();
        let row = start - self.dropped - grid.history_size() as i64;
        let top = -(grid.history_size() as i64);
        let cursor = i64::from(grid.cursor.point.line.0);
        if row < top || row > cursor {
            return Err(format!(
                "the prompt start row {row} lies outside the history and cursor rows {top}..={cursor}"
            ));
        }
        Ok(row as i32)
    }

    /// Number of logical lines that end from `row` up to, but not including, the logical line that
    /// holds the cursor.
    fn logical_lines_before_cursor(&self, row: i32) -> usize {
        let grid = self.term.grid();
        let last_column = Column(grid.columns() - 1);
        (row..self.cursor_logical_line_start())
            .filter(|line| {
                !grid[Line(*line)][last_column]
                    .flags
                    .contains(Flags::WRAPLINE)
            })
            .count()
    }

    /// First row of the logical line that is `count` logical lines above the logical line that
    /// holds the cursor.
    fn logical_line_start_above_cursor(&self, count: usize) -> i32 {
        let grid = self.term.grid();
        let last_column = Column(grid.columns() - 1);
        let top = -(grid.history_size() as i32);
        let mut start = self.cursor_logical_line_start();
        for _ in 0..count {
            if start <= top {
                break;
            }
            start -= 1;
            while start > top
                && grid[Line(start - 1)][last_column]
                    .flags
                    .contains(Flags::WRAPLINE)
            {
                start -= 1;
            }
        }
        start
    }

    /// The rejection of a CSI, or None for one that the library executes.
    fn csi_rejection(
        params: &[Vec<u16>],
        intermediates: &[u8],
        ignored: bool,
        action: char,
    ) -> Option<String> {
        let selector = sequence_selector(params, intermediates, action);
        let body = &selector[..selector.len() - action.len_utf8()];
        if action == 't' && body != "14" {
            Some(format!("window report {selector}"))
        } else if matches!(action, 'h' | 'l') && matches!(body, "?47" | "?1047" | "?1048") {
            Some(format!("alternate screen mode {selector}"))
        } else if action == 'x' && intermediates.contains(&b'$') {
            Some(format!("rectangle report {selector}"))
        } else if action == 'q' && intermediates.contains(&b'"') {
            Some(format!("protected-cell report {selector}"))
        } else if action == 'p' && intermediates.contains(&b'#') {
            Some(format!("palette report {selector}"))
        } else if !csi_executed(params, intermediates, ignored, action) {
            Some(selector)
        } else {
            None
        }
    }

    /// Acts on one sequence that ends at `at`, and returns the start of the input that the terminal
    /// has not received. An erase or a rewrite replaces the sequence, so the terminal never receives
    /// it: at ED 2 the library pushes the visible lines into history, and at ED 1 it does not erase
    /// the first line when the cursor is on the second line. An action that depends on the cursor
    /// runs after the output before it. An OSC ended by an escape byte leaves that byte to the
    /// sequence that it starts.
    fn apply_sequence(
        &mut self,
        sequence: Sequence,
        input: &[u8],
        from: usize,
        at: usize,
    ) -> usize {
        match sequence {
            Sequence::Csi {
                params,
                intermediates,
                ignored,
                action,
            } => {
                let value = match params.as_slice() {
                    [param] if !ignored => param.first().copied(),
                    _ => None,
                };
                let erase = match (action, intermediates.as_slice(), value) {
                    ('J', b"", Some(2)) => Some(Erase::All),
                    ('J', b"", Some(1)) => Some(Erase::Above),
                    ('J', b"", Some(3)) => Some(Erase::History),
                    _ => None,
                };
                let initial_cursor =
                    action == 'q' && intermediates.as_slice() == b" " && value == Some(7);
                if erase.is_none() && !initial_cursor {
                    if let Some(reason) =
                        Self::csi_rejection(&params, &intermediates, ignored, action)
                    {
                        self.events
                            .push(QueuedEvent::Neutral(EngineEvent::Error(format!(
                                "unsupported CSI {reason}"
                            ))));
                    }
                    return from;
                }
                let start = input[from..at]
                    .iter()
                    .rposition(|byte| *byte == 0x1b)
                    .expect("a CSI starts at an escape byte that the terminal has not received");
                self.advance(&input[from..from + start]);
                match erase {
                    Some(erase) => self.erase(erase),
                    None => self.advance(b"\x1b[0 q"),
                }
                at
            }
            Sequence::Osc { body, bell } => {
                let end = if input[at - 1] == 0x1b {
                    self.scan.left_sequence = false;
                    at - 1
                } else {
                    at
                };
                if body.starts_with(b"1337;") {
                    self.advance(&input[from..end]);
                    self.inline_image(&body[b"1337;".len()..]);
                    return end;
                }
                match self.handle_osc(&body, bell) {
                    Some(mark) => {
                        self.advance(&input[from..end]);
                        self.apply_shell_mark(mark.marker, mark.redraw);
                        end
                    }
                    None => from,
                }
            }
            Sequence::Dcs(selector) => {
                self.events
                    .push(QueuedEvent::Neutral(EngineEvent::Error(format!(
                        "unsupported DCS {selector}"
                    ))));
                from
            }
            Sequence::Esc {
                intermediates,
                byte,
            } => {
                if !esc_executed(&intermediates, byte) {
                    let intermediates = String::from_utf8_lossy(&intermediates);
                    self.events
                        .push(QueuedEvent::Neutral(EngineEvent::Error(format!(
                            "unsupported ESC {intermediates}{}",
                            byte as char
                        ))));
                }
                from
            }
        }
    }

    /// Stores or rejects one OSC 1337. The graphic is placed at the cursor where the sequence was
    /// met, which the caller reaches before a later output moves the cursor.
    fn inline_image(&mut self, payload: &[u8]) {
        let cursor = self.term.grid().cursor.point;
        let anchor = InlineAnchor {
            col: cursor.column.0 as u16,
            row: cursor.line.0.max(0) as u16,
            scroll: self.scroll_generation(),
        };
        let event = match parse_inline_image(payload) {
            Ok(command) => EngineEvent::InlineImage { command, anchor },
            Err(error) => EngineEvent::Error(error),
        };
        self.events.push(QueuedEvent::Neutral(event));
    }

    /// Scans `input` from `at` to `end`, acts on each sequence, and returns the new `from`.
    fn scan_range(&mut self, input: &[u8], mut from: usize, at: &mut usize, end: usize) -> usize {
        while *at < end {
            *at += self
                .scanner
                .advance_until_terminated(&mut self.scan, &input[*at..end]);
            if let Some(sequence) = self.scan.found.take() {
                from = self.apply_sequence(sequence, input, from, *at);
            }
        }
        from
    }

    /// The rest of a sequence over the bound ends at a terminator of a string. An escape byte stays
    /// in the output, because it starts the next sequence or the string terminator.
    fn skip_discarded<'a>(&mut self, bytes: &'a [u8]) -> &'a [u8] {
        if !self.discarding {
            return bytes;
        }
        let Some(end) = bytes
            .iter()
            .position(|byte| matches!(byte, 0x07 | 0x18 | 0x1a | 0x1b))
        else {
            return &[];
        };
        self.discarding = false;
        if bytes[end] == 0x1b {
            &bytes[end..]
        } else {
            &bytes[end + 1..]
        }
    }

    /// Passes the output to the scan and to the terminal. A sequence cut between two inputs waits in
    /// `held`, so that an erase or a rewrite can still replace it.
    fn feed_output(&mut self, bytes: &[u8]) {
        let bytes = self.skip_discarded(bytes);
        let mut input = std::mem::take(&mut self.held);
        let mut at = input.len();
        input.extend_from_slice(bytes);
        let last_escape = input[at..]
            .iter()
            .rposition(|byte| *byte == 0x1b)
            .map(|position| at + position);
        // default: an input without an escape byte is one range up to its end.
        let mut from = self.scan_range(&input, 0, &mut at, last_escape.unwrap_or(input.len()));
        if last_escape.is_some() {
            self.scan.left_sequence = false;
            from = self.scan_range(&input, from, &mut at, input.len());
        }
        let held_from = if self.scan.left_sequence {
            None
        } else {
            input[from..]
                .iter()
                .rposition(|byte| *byte == 0x1b)
                .map(|position| from + position)
        };
        // default: with no sequence open, the terminal receives the input up to its end.
        self.advance(&input[from..held_from.unwrap_or(input.len())]);
        let Some(held_from) = held_from else {
            return;
        };
        if input.len() - held_from > HELD_SEQUENCE_BYTES {
            self.events
                .push(QueuedEvent::Neutral(EngineEvent::Error(format!(
                    "a sequence of more than {HELD_SEQUENCE_BYTES} bytes is rejected"
                ))));
            self.scanner = Parser::new();
            self.scan.left_sequence = true;
            self.discarding = true;
            return;
        }
        input.drain(..held_from);
        self.held = input;
    }

    fn clipboard_selection(
        selection: alacritty_terminal::term::ClipboardType,
    ) -> ClipboardSelection {
        match selection {
            alacritty_terminal::term::ClipboardType::Clipboard => ClipboardSelection::Clipboard,
            alacritty_terminal::term::ClipboardType::Selection => ClipboardSelection::Selection,
        }
    }

    fn event(&self, event: Event) -> Option<EngineEvent> {
        match event {
            Event::Title(value) => Some(EngineEvent::Title(value)),
            Event::ResetTitle => Some(EngineEvent::ResetTitle),
            Event::ClipboardStore(selection, text) => Some(EngineEvent::ClipboardStore {
                selection: Self::clipboard_selection(selection),
                text,
            }),
            Event::ClipboardLoad(_, _) => None,
            Event::ColorRequest(_, _) | Event::TextAreaSizeRequest(_) => None,
            Event::PtyWrite(value) => Some(EngineEvent::PtyWrite(value.into_bytes())),
            Event::CursorBlinkingChange => Some(EngineEvent::CursorBlinkingChange),
            Event::Wakeup => Some(EngineEvent::Wakeup),
            Event::Bell => Some(EngineEvent::Bell),
            Event::Exit => Some(EngineEvent::Exit),
            Event::ChildExit(status) => Some(EngineEvent::ChildExit {
                success: status.success(),
                code: status.code(),
            }),
            Event::MouseCursorDirty => Some(EngineEvent::MouseCursorDirty),
        }
    }

    pub fn drain_events(&mut self) -> Vec<EngineEvent> {
        let raw_events = self.events.drain();
        let mut result = Vec::new();
        for queued in raw_events {
            let event = match queued {
                QueuedEvent::Neutral(event) => {
                    result.push(event);
                    continue;
                }
                QueuedEvent::Alacritty(event) => event,
            };
            match event {
                Event::ClipboardLoad(selection, callback) => {
                    let request_id = self.next_clipboard_request;
                    self.next_clipboard_request = self
                        .next_clipboard_request
                        .checked_add(1)
                        .expect("clipboard request id exhausted");
                    self.pending_clipboard.insert(request_id, callback);
                    result.push(EngineEvent::ClipboardQuery {
                        request_id,
                        selection: Self::clipboard_selection(selection),
                    });
                }
                Event::ColorRequest(index, callback) => {
                    let color = match self.color_request(index) {
                        Ok(color) => color,
                        Err(error) => {
                            result.push(EngineEvent::Error(error));
                            continue;
                        }
                    };
                    result.push(EngineEvent::PtyWrite(callback(color).into_bytes()));
                }
                Event::TextAreaSizeRequest(callback) => {
                    let Some((cell_width, cell_height)) = self.cell_metrics else {
                        result.push(EngineEvent::Error(
                            "text area size requested before renderer metrics were configured"
                                .to_string(),
                        ));
                        continue;
                    };
                    let reply = callback(WindowSize {
                        num_lines: self.term.grid().screen_lines() as u16,
                        num_cols: self.term.grid().columns() as u16,
                        cell_width,
                        cell_height,
                    });
                    result.push(EngineEvent::PtyWrite(reply.into_bytes()));
                }
                event => {
                    if let Some(event) = self.event(event) {
                        result.push(event);
                    }
                }
            }
        }
        result
    }

    pub fn resolve_clipboard(&mut self, request_id: u64, text: &str) -> Result<(), String> {
        let callback = self
            .pending_clipboard
            .remove(&request_id)
            .ok_or_else(|| format!("unknown clipboard request {request_id}"))?;
        self.events.push(QueuedEvent::Neutral(EngineEvent::PtyWrite(
            callback(text).into_bytes(),
        )));
        Ok(())
    }

    pub fn reject_clipboard(&mut self, request_id: u64, reason: &str) -> Result<(), String> {
        self.pending_clipboard
            .remove(&request_id)
            .ok_or_else(|| format!("unknown clipboard request {request_id}"))?;
        if reason.is_empty() {
            return Err("clipboard rejection reason is empty".to_string());
        }
        Ok(())
    }

    /// Converts a cell in viewport coordinates to grid coordinates. While the scrollback is shown,
    /// the upper rows of the viewport are history rows.
    fn viewport_point(&self, col: u16, row: u16) -> Option<Point> {
        if usize::from(col) >= self.term.grid().columns()
            || usize::from(row) >= self.term.grid().screen_lines()
        {
            return None;
        }
        let offset = self.term.grid().display_offset() as i32;
        Some(Point::new(
            Line(i32::from(row) - offset),
            Column(usize::from(col)),
        ))
    }

    /// Moves the viewport by `lines`. A positive value is toward older output, limited to the
    /// stored history range.
    pub fn scroll_viewport(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Returns the viewport to the newest output. Returns true when the viewport moved.
    pub fn scroll_to_newest(&mut self) -> bool {
        // If the viewport is already at the newest output, it does not move. scroll_display sends a
        // renderer notification even when it does not move.
        if self.term.grid().display_offset() == 0 {
            return false;
        }
        self.term.scroll_display(Scroll::Bottom);
        true
    }

    /// (Number of lines the viewport is above the newest output, number of stored history lines).
    pub fn scrollback(&self) -> (usize, usize) {
        (
            self.term.grid().display_offset(),
            self.term.grid().history_size(),
        )
    }

    /// Converts cell boundaries to alacritty cells and sides. Boundary c is the left side of cell
    /// c, and the column count is the right side of the last cell.
    fn edge_point(&self, edge: u16, row: u16) -> Result<(Point, Side), String> {
        let columns = self.term.grid().columns();
        let outside = || format!("selection edge is outside the terminal grid: {edge},{row}");
        if usize::from(edge) > columns {
            return Err(outside());
        }
        if usize::from(edge) == columns {
            let point = self.viewport_point(edge - 1, row).ok_or_else(outside)?;
            return Ok((point, Side::Right));
        }
        let point = self.viewport_point(edge, row).ok_or_else(outside)?;
        Ok((point, Side::Left))
    }

    pub fn selection_start(&mut self, edge: u16, row: u16) -> Result<(), String> {
        let (point, side) = self.edge_point(edge, row)?;
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, side));
        self.selection_anchor = Some((point, side));
        Ok(())
    }

    /// The selection is the cells between the start boundary and this boundary. The same boundary
    /// selects no cell.
    pub fn selection_update(&mut self, edge: u16, row: u16) -> Result<(), String> {
        let (point, side) = self.edge_point(edge, row)?;
        let (anchor, anchor_side) = self
            .selection_anchor
            .filter(|_| self.term.selection.is_some())
            .ok_or_else(|| "selection update without selection start".to_string())?;
        let mut selection = Selection::new(SelectionType::Simple, anchor, anchor_side);
        selection.update(point, side);
        self.term.selection = Some(selection);
        Ok(())
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    pub fn selection_end(&mut self) -> Result<Option<String>, String> {
        let text = self
            .term
            .selection_to_string()
            .filter(|text| !text.is_empty());
        if text.is_none() {
            self.term.selection = None;
        }
        Ok(text)
    }

    pub fn selection_clear(&mut self) -> bool {
        self.selection_anchor = None;
        self.term.selection.take().is_some()
    }

    fn color_request(&self, index: usize) -> Result<Rgb, String> {
        if index >= 269 {
            return Err(format!("unsupported terminal color index {index}"));
        }
        self.term.colors()[index]
            .or_else(|| {
                self.theme
                    .color(index)
                    .or_else(|| default_terminal_color(index))
                    .map(|rgb| Rgb {
                        r: rgb[0],
                        g: rgb[1],
                        b: rgb[2],
                    })
            })
            .ok_or_else(|| format!("unsupported terminal color index {index}"))
    }

    pub fn cursor(&self) -> Cursor {
        let renderable = self.term.renderable_content();
        // The cursor row is in grid coordinates, so it is converted to a viewport row. While the
        // scrollback is shown, the cursor is hidden when it is outside the viewport.
        let row = renderable.cursor.point.line.0 + renderable.display_offset as i32;
        let in_view = row >= 0 && (row as usize) < self.term.grid().screen_lines();
        Cursor {
            col: renderable.cursor.point.column.0 as u16,
            row: if in_view { row as u16 } else { 0 },
            shape: match renderable.cursor.shape {
                CursorShape::Block => ProtocolCursorShape::Block,
                CursorShape::Underline => ProtocolCursorShape::Underline,
                CursorShape::Beam => ProtocolCursorShape::Beam,
                CursorShape::HollowBlock => ProtocolCursorShape::HollowBlock,
                CursorShape::Hidden => ProtocolCursorShape::Hidden,
            },
            visible: in_view && renderable.cursor.shape != CursorShape::Hidden,
            blinking: self.term.cursor_style().blinking,
            blink_visible: true,
            focused: false,
            preedit: None,
        }
    }

    fn color(
        &self,
        color: Color,
        colors: &alacritty_terminal::term::color::Colors,
    ) -> Option<soksak_sidecar_vt_core::Rgb> {
        let rgb = match color {
            Color::Named(name) => colors[name].or_else(|| {
                self.theme
                    .color(name as usize)
                    .or_else(|| default_terminal_color(name as usize))
                    .map(|rgb| Rgb {
                        r: rgb[0],
                        g: rgb[1],
                        b: rgb[2],
                    })
            }),
            Color::Spec(rgb) => Some(rgb),
            Color::Indexed(index) => colors[index as usize].or_else(|| {
                self.theme
                    .color(index as usize)
                    .or_else(|| default_terminal_color(index as usize))
                    .map(|rgb| Rgb {
                        r: rgb[0],
                        g: rgb[1],
                        b: rgb[2],
                    })
            }),
        }?;
        Some(soksak_sidecar_vt_core::Rgb([rgb.r, rgb.g, rgb.b]))
    }

    fn cell(
        &self,
        cell: &alacritty_terminal::term::cell::Cell,
        colors: &alacritty_terminal::term::color::Colors,
    ) -> Cell {
        let mut text = compact_str::CompactString::default();
        if cell.c != '\0' && (cell.c != ' ' || cell.zerowidth().is_some()) {
            text.push(cell.c);
        }
        if let Some(combining) = cell.zerowidth() {
            text.extend(combining.iter().copied());
        }
        Cell {
            ch: (!text.is_empty()).then_some(text),
            width: if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                0
            } else {
                1
            },
            fg: match self.special_foreground(cell.flags, cell.fg) {
                Some(rgb) => Some(soksak_sidecar_vt_core::Rgb([rgb.r, rgb.g, rgb.b])),
                None => self.color(cell.fg, colors),
            },
            bg: self.color(cell.bg, colors),
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.contains(Flags::UNDERLINE),
            inverse: cell.flags.contains(Flags::INVERSE),
            link: cell.hyperlink().map(|link| link.uri().to_string()),
        }
    }

    fn is_trimmable_default_cell(&self, cell: &Cell) -> bool {
        cell.ch.is_none()
            && cell.width == 1
            && !cell.bold
            && !cell.italic
            && !cell.underline
            && !cell.inverse
            && cell.link.is_none()
            && cell.fg.is_none_or(|color| color.0 == self.theme.foreground)
            && cell.bg.is_none_or(|color| color.0 == self.theme.background)
    }

    fn theme_rgb(&self, rgb: [u8; 3]) -> Rgb {
        Rgb {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
        }
    }

    fn theme_hex(&self, rgb: [u8; 3]) -> String {
        format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
    }
}

impl Engine for AlacrittyEngine {
    fn set_theme(&mut self, theme: TerminalTheme) {
        self.theme = theme;
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let grid = self.term.grid();
        if grid.columns() == cols as usize && grid.screen_lines() == rows as usize {
            // At the same size the PTY sends no SIGWINCH, so the shell does not redraw either.
            return;
        }
        let primary = !self.term.mode().contains(TermMode::ALT_SCREEN);
        let cursor = self.term.grid().cursor.point.line.0;
        // The first prompt row is the first row of a logical line. Reflow keeps logical lines
        // intact, so the position after reflow is found again by the number of logical lines above
        // the cursor's logical line.
        let prompt = match self.prompt_row() {
            Ok(row) => Some((row, self.logical_lines_before_cursor(row))),
            Err(error) => {
                if self.prompt_start.is_some() {
                    self.events
                        .push(QueuedEvent::Neutral(EngineEvent::Error(format!(
                            "terminal resize: {error}"
                        ))));
                }
                None
            }
        };
        // Number of rows the shell moves up: the rows that were below the first row of the area the
        // shell redraws at the previous width.
        let region = match self.shell {
            ShellState::Prompt(Redraw::Prompt) if primary => {
                prompt.map(|(row, lines)| (cursor - row, lines))
            }
            ShellState::Prompt(Redraw::LastLine) if primary => {
                Some((cursor - self.cursor_logical_line_start(), 0))
            }
            _ => None,
        };
        if self.prompt_start.is_some() {
            let rows_now = self.term.grid().screen_lines();
            self.term
                .grid_mut()
                .update_history(self.history_limit + rows_now.max(rows as usize));
        }
        self.resize_grid(primary, cols as usize, rows as usize);
        if self.prompt_start.is_some() {
            self.limit_history();
        }
        if let Some((_, lines)) = prompt {
            let start = self.logical_line_start_above_cursor(lines);
            self.prompt_start = Some(self.absolute(start));
        }
        if let Some((offset, lines)) = region {
            let start = self.logical_line_start_above_cursor(lines);
            self.clear_prompt_for_redraw(start, offset);
            if self.prompt_start.is_some() {
                // The shell redraws from the first row of the erased area.
                let row = self.term.grid().cursor.point.line.0 - offset;
                self.prompt_start =
                    Some(self.absolute(row.max(-(self.term.grid().history_size() as i32))));
            }
        }
    }

    fn set_cell_metrics(&mut self, width: u16, height: u16) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("terminal cell metrics must be positive".to_string());
        }
        self.cell_metrics = Some((width, height));
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.feed_output(bytes);
    }

    fn sync_deadline(&self) -> Option<Instant> {
        self.processor
            .sync_timeout()
            .sync_timeout()
            .map(Instant::from_std)
    }

    fn stop_sync(&mut self) {
        // The library processes the held bytes in one call, so the count covers them as it covers a
        // slice of a feed.
        self.term
            .grid_mut()
            .update_history(self.history_limit + COUNTED_ROWS);
        self.processor.stop_sync(&mut self.term);
        self.count_pushed_rows();
    }

    fn drain_events(&mut self) -> Vec<EngineEvent> {
        AlacrittyEngine::drain_events(self)
    }

    fn resolve_clipboard(&mut self, request_id: u64, text: &str) -> Result<(), String> {
        AlacrittyEngine::resolve_clipboard(self, request_id, text)
    }

    fn reject_clipboard(&mut self, request_id: u64, reason: &str) -> Result<(), String> {
        AlacrittyEngine::reject_clipboard(self, request_id, reason)
    }

    fn selection_start(&mut self, edge: u16, row: u16) -> Result<(), String> {
        AlacrittyEngine::selection_start(self, edge, row)
    }

    fn selection_update(&mut self, edge: u16, row: u16) -> Result<(), String> {
        AlacrittyEngine::selection_update(self, edge, row)
    }

    fn selection_end(&mut self) -> Result<Option<String>, String> {
        AlacrittyEngine::selection_end(self)
    }

    fn selection_clear(&mut self) -> bool {
        AlacrittyEngine::selection_clear(self)
    }

    fn selection_text(&self) -> Option<String> {
        AlacrittyEngine::selection_text(self)
    }

    fn scroll_viewport(&mut self, lines: i32) {
        AlacrittyEngine::scroll_viewport(self, lines)
    }

    fn scroll_to_newest(&mut self) -> bool {
        AlacrittyEngine::scroll_to_newest(self)
    }

    fn viewport_offset(&self) -> u32 {
        self.term.grid().display_offset() as u32
    }

    fn cursor(&self) -> Cursor {
        AlacrittyEngine::cursor(self)
    }

    fn screen(&mut self) -> Screen {
        let cursor = self.cursor();
        let renderable = self.term.renderable_content();
        let cols = self.term.grid().columns() as u16;
        let rows = self.term.grid().screen_lines() as u16;
        let mut lines: Vec<Vec<Cell>> = (0..rows)
            .map(|_| Vec::with_capacity(usize::from(cols)))
            .collect();
        // The display point is in grid coordinates. While the scrollback is shown, history rows are
        // negative, so the viewport offset is added.
        let offset = renderable.display_offset as i32;

        for indexed in renderable.display_iter {
            let row = usize::try_from(indexed.point.line.0 + offset)
                .expect("display row must be non-negative");
            let col = indexed.point.column.0;
            if row >= lines.len() || col >= usize::from(cols) {
                panic!("display point outside terminal dimensions");
            }
            lines[row].resize_with(col + 1, Cell::default);
            let mut cell = self.cell(indexed.cell, renderable.colors);
            // The second argument is the cursor position. alacritty skips inverting the cell only
            // when the block cursor is at the selection boundary.
            if renderable.selection.as_ref().is_some_and(|selection| {
                selection.contains_cell(&indexed, renderable.cursor.point, renderable.cursor.shape)
            }) {
                // Selected cells are drawn with the highlight background (or the theme's selection
                // background when unset) and the highlight foreground (or the cell's foreground
                // color when unset).
                let hex = |rgb: Rgb| soksak_sidecar_vt_core::Rgb([rgb.r, rgb.g, rgb.b]);
                let foreground = if cell.inverse { cell.bg } else { cell.fg };
                let foreground =
                    foreground.or_else(|| self.default_rgb(NamedColor::Foreground).map(hex));
                cell.bg = Some(hex(self
                    .highlight_background
                    // default: without the OSC 17 highlight background a selected cell uses the
                    // selection background of the theme (docs/spec/terminal-runtime.md).
                    .unwrap_or_else(|| self.theme_rgb(self.theme.selection))));
                cell.fg = self.highlight_foreground.map(hex).or(foreground);
                cell.inverse = false;
            }
            lines[row][col] = cell;
        }

        for line in &mut lines {
            while line
                .last()
                .is_some_and(|cell| self.is_trimmable_default_cell(cell))
            {
                line.pop();
            }
        }

        let (offset, history) = self.scrollback();
        Screen {
            cols,
            rows,
            cursor,
            scrollback: soksak_sidecar_vt_core::Scrollback {
                offset: offset as u32,
                history: history as u32,
            },
            // The theme's default background applies when the program sets no background with OSC
            // 11.
            background: self.term.colors()[NamedColor::Background].map_or_else(
                || self.theme_hex(self.theme.background),
                |rgb| format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
            ),
            lines,
        }
    }

    /// Count of rows that have scrolled into the history, including the rows dropped past the
    /// history limit. It increases with each row pushed up and does not change when the history
    /// reaches its limit.
    fn scroll_generation(&self) -> i64 {
        self.dropped + self.term.grid().history_size() as i64
    }

    fn modes(&self) -> Modes {
        let mode = self.term.mode();
        Modes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse_click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            mouse_drag: mode.contains(TermMode::MOUSE_DRAG),
            mouse_motion: mode.contains(TermMode::MOUSE_MOTION),
            focus_in_out: mode.contains(TermMode::FOCUS_IN_OUT),
            utf8_mouse: mode.contains(TermMode::UTF8_MOUSE),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
        }
    }
}

#[cfg(test)]
#[path = "../tests/engine/poisoned_queue_test.rs"]
mod poisoned_queue_test;
