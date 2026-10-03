use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell as GridCell, Flags};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, Handler, NamedColor, Processor, Rgb};
use soksak_sidecar_vt_core::directory_uri::local_path;
use soksak_sidecar_vt_core::{
    default_terminal_color, inline_image::parse as parse_inline_image, Cell, ClipboardSelection,
    Cursor, CursorShape as ProtocolCursorShape, Engine, EngineEvent, InlineAnchor, Modes, Screen,
    ShellMarker, TerminalTheme,
};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

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

/// protocol audit가 사용하는 selector 수준 범위이다. 이 inventory는 parser 수락 여부에서
/// 도출하지 않는다. 각 행은 관찰 가능한 test 또는 명시적인 별도 vendor contract를
/// 지정한다.
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

/// X 색 이름의 `rgb:h/h/h`(성분마다 1–4자리 16진수)와 `#hhh`–`#hhhhhhhhhhhh` 형식.
fn parse_x_color(value: &str) -> Option<Rgb> {
    let scale = |digits: &str| -> Option<u8> {
        if digits.is_empty() || digits.len() > 4 {
            return None;
        }
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

/// OSC 22 의 포인터 이름을 CSS cursor 값으로 바꾼다. X 커서 글꼴 이름과 CSS 이름을 받는다. 빈 이름은 기본값이다.
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
                // 기본값: id 가 없는 OSC 8 링크는 빈 id 를 가진다(id 는 선택 인자다).
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
            // 기본값: 인자 없는 OSC 133 표시는 인자가 비어 있다.
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

/// OSC 133 `redraw` 매개변수: 크기 변경 뒤 셸이 다시 그리는 영역. `0` 은 다시 그리지 않는다. `1` 은 프롬프트
/// 전체를 다시 그리므로(zsh) 프롬프트 첫 행까지 올라가고, `last` 는 프롬프트의 마지막 줄만 다시 그리므로
/// (readline) 커서가 있는 논리적 행의 첫 행까지 올라간다.
fn parse_redraw(value: &str) -> Result<Redraw, String> {
    match value {
        "0" => Ok(Redraw::Never),
        "1" => Ok(Redraw::Prompt),
        "last" => Ok(Redraw::LastLine),
        value => Err(format!("OSC 133 redraw value is unsupported: {value}")),
    }
}

/// 클립보드 글을 OSC 52 응답으로 만드는 함수. 클립보드 읽기 요청 번호마다 둔다.
type ClipboardFormatter = Arc<dyn Fn(&str) -> String + Sync + Send + 'static>;

/// OSC 133 표시가 정한 셸 상태. 크기 변경 때 프롬프트 행을 지울지 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellState {
    Output,
    /// 프롬프트나 입력 중이다. 값은 셸이 크기 변경 뒤 다시 그리는 영역이다.
    Prompt(Redraw),
}

/// 크기 변경 뒤 셸이 다시 그리는 영역(OSC 133 `redraw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redraw {
    Never,
    /// 프롬프트 첫 행부터 다시 그린다.
    Prompt,
    /// 커서가 있는 논리적 행의 첫 행부터 다시 그린다.
    LastLine,
}

/// 엔진 대신 직접 지우는 ED 시퀀스.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Erase {
    All,
    Above,
    History,
}

/// 입력 조각 안에서 OSC 133 표시가 끝나는 위치와 그 표시.
struct ShellMark {
    end: usize,
    marker: ShellMarker,
    redraw: Option<Redraw>,
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

/// 이 sidecar가 현재 노출하는 CSI 동작에 대한 selector 수준 근거이다.
/// 나머지 XTerm category가 실행 가능한 동작 및 거부 contract를 갖출 때까지
/// 이 inventory는 의도적으로 부분적이다.
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

/// VT 엔진이 발생한 이벤트를 소유자가 회수할 때까지 발생 순서대로 보존한다.
/// 콜백 이벤트는 엔진 경계에서 필요한 값으로 변환한 뒤 중립 이벤트로 전달한다.
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

    fn drain(&self) -> Vec<QueuedEvent> {
        let mut events = self.events.lock().expect("engine event queue poisoned");
        events.drain(..).collect()
    }
}

#[derive(Clone)]
struct EventSink(Arc<EventQueue>);

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        self.0
            .events
            .lock()
            .expect("engine event queue poisoned")
            .push_back(QueuedEvent::Alacritty(event));
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
    pending_input: Vec<u8>,
    pending_osc: Vec<u8>,
    pending_csi: Vec<u8>,
    pending_sequence: Vec<u8>,
    /// 선택을 시작한 칸. 끄는 방향에 따라 이 칸과 포인터 칸을 모두 포함하도록 선택의 경계 쪽을 정한다.
    selection_anchor: Option<(Point, Side)>,
    shell: ShellState,
    /// 프롬프트 첫 행의 절대 행 번호(dropped + 기록 행 수 + 화면 행). 프롬프트 상태에서만 있다.
    prompt_start: Option<i64>,
    /// 기록 한도를 넘어 버려진 행의 수. 프롬프트 상태에서 센다.
    dropped: i64,
    /// 기록 행 수의 한도.
    history_limit: usize,
    /// OSC 17 과 19 가 정한 선택 영역의 배경과 글자 색. 없으면 선택 칸을 반전해 그린다.
    highlight_background: Option<Rgb>,
    highlight_foreground: Option<Rgb>,
    /// OSC 5 가 정한 특수 색(0 굵게, 1 밑줄, 2 깜빡임, 3 반전, 4 기울임)과 OSC 6/106 이 켠 사용 여부.
    special_colors: [Option<Rgb>; 5],
    special_enabled: [bool; 5],
}

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
            pending_input: Vec::new(),
            pending_osc: Vec::new(),
            pending_csi: Vec::new(),
            pending_sequence: Vec::new(),
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

    /// 입력을 엔진에 넣기 전에 바꾸는 시퀀스. 입력 조각 사이에서 끊긴 앞부분은 다음 조각까지 보관한다.
    /// - DECSCUSR 7 은 초기 커서 모양(0)으로 바꾼다.
    /// - ED 2 와 ED 1 은 엔진에 넣지 않고 위치를 돌려주어 그 자리에서 직접 지운다. 엔진은 ED 2 에서 보이는 줄을
    ///   기록으로 올려 clear 뒤에도 기록이 남고, ED 1 에서 커서가 둘째 줄이면 첫 줄을 지우지 않는다.
    fn normalize_sequences(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<(usize, Erase)>) {
        // 찾을 시퀀스, 대신 넣을 시퀀스, 그 자리에서 직접 지우는 범위.
        type Rewrite = (&'static [u8], Option<&'static [u8]>, Option<Erase>);
        const REWRITES: &[Rewrite] = &[
            (b"\x1b[7 q", Some(b"\x1b[0 q"), None),
            (b"\x1b[2J", None, Some(Erase::All)),
            (b"\x1b[1J", None, Some(Erase::Above)),
            (b"\x1b[3J", None, Some(Erase::History)),
        ];
        let mut input = std::mem::take(&mut self.pending_sequence);
        input.extend_from_slice(bytes);
        let mut normalized = Vec::with_capacity(input.len());
        let mut erases = Vec::new();
        let mut index = 0;
        'input: while index < input.len() {
            let remaining = &input[index..];
            if remaining[0] == 0x1b {
                for (from, to, erase) in REWRITES {
                    if remaining.len() < from.len() && from.starts_with(remaining) {
                        self.pending_sequence.extend_from_slice(remaining);
                        break 'input;
                    }
                    if remaining.starts_with(from) {
                        if let Some(to) = to {
                            normalized.extend_from_slice(to);
                        }
                        if let Some(erase) = erase {
                            erases.push((normalized.len(), *erase));
                        }
                        index += from.len();
                        continue 'input;
                    }
                }
            }
            normalized.push(input[index]);
            index += 1;
        }
        (normalized, erases)
    }

    /// ED 2(화면 전체)와 ED 1(화면 처음부터 커서 칸까지)을 현재 배경색으로 지운다. 기록과 커서는 그대로다.
    fn erase(&mut self, erase: Erase) {
        let bg = self.term.grid().cursor.template.bg;
        let cursor = self.term.grid().cursor.point;
        let columns = self.term.grid().columns();
        let grid = self.term.grid_mut();
        match erase {
            Erase::All => {
                grid.reset_region(..);
                // 화면을 지운 뒤 셸은 OSC 133;A 없이 프롬프트를 맨 위 행에서 다시 그린다.
                if self.prompt_start.is_some() {
                    self.prompt_start = Some(self.dropped + grid.history_size() as i64);
                }
            }
            Erase::History => {
                // 지운 기록 행은 버려진 행으로 세어 화면 행의 절대 번호를 그대로 둔다.
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

    /// OSC 시퀀스를 검사하고 이벤트를 쌓는다. 받아들인 OSC 133 표시는 끝 위치와 함께 돌려주어
    /// 앞의 출력을 처리한 뒤에 셸 상태에 적용하게 한다.
    fn audit_osc(&mut self, bytes: &[u8]) -> Vec<ShellMark> {
        let mut marks = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            if self.pending_osc.is_empty() {
                if bytes[index..].starts_with(b"\x1b]") {
                    self.pending_osc.extend_from_slice(b"\x1b]");
                    index += 2;
                } else {
                    index += 1;
                }
                continue;
            }

            self.pending_osc.push(bytes[index]);
            let terminated = bytes[index] == b'\x07'
                || (self.pending_osc.len() >= 2
                    && self.pending_osc[self.pending_osc.len() - 2..] == *b"\x1b\\");
            index += 1;
            if !terminated {
                continue;
            }

            let terminator = if self.pending_osc.last() == Some(&b'\x07') {
                self.pending_osc.len() - 1
            } else {
                self.pending_osc.len() - 2
            };
            let owned = self.pending_osc[2..terminator].to_vec();
            let body = owned.as_slice();
            let (selector, payload) = body
                .iter()
                .position(|byte| *byte == b';')
                .map(|position| (&body[..position], &body[position + 1..]))
                // 기본값: ; 가 없는 OSC 는 선택자만 있고 내용이 비어 있다.
                .unwrap_or((body, &[]));
            let outcome = osc_outcome(selector);
            let bel = self.pending_osc.last() == Some(&b'\x07');
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
            if let Some(EngineEvent::ShellState { marker, params }) = &event {
                let redraw = params
                    .iter()
                    .find_map(|parameter| parameter.strip_prefix("redraw="))
                    .map(|value| parse_redraw(value).expect("redraw was validated by the parser"));
                marks.push(ShellMark {
                    end: index,
                    marker: *marker,
                    redraw,
                });
            }
            if let Some(event) = event {
                self.events
                    .events
                    .lock()
                    .expect("engine event queue poisoned")
                    .push_back(QueuedEvent::Neutral(event));
            }
            self.pending_osc.clear();
        }
        marks
    }

    /// 엔진이 직접 처리하는 OSC: 17/19 강조 색의 설정과 조회, 117/119 초기화, 22 포인터 모양. 처리하지 않는
    /// 선택자는 None 이다. 조회의 답은 요청과 같은 종결자로 끝난다.
    fn engine_osc(
        &mut self,
        selector: &[u8],
        payload: &[u8],
        bel: bool,
    ) -> Option<Option<EngineEvent>> {
        let selector = std::str::from_utf8(selector).ok()?;
        let payload = String::from_utf8_lossy(payload);
        let terminator = if bel { "\x07" } else { "\x1b\\" };
        Some(match selector {
            "17" | "19" if payload == "?" => {
                // 정하지 않은 강조 배경은 테마의 선택 배경, 강조 글자는 기본 전경색이다.
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

    /// OSC 5: 특수 색 번호와 색 이름의 쌍들. `?` 는 현재 색을 요청과 같은 종결자로 답한다. 깜빡임(2)은 격자가
    /// 깜빡임 속성을 보관하지 않아 적용할 수 없으므로 명시적 오류다.
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
                    )))
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
                        )))
                    }
                }
            }
        }
        (!replies.is_empty()).then(|| EngineEvent::PtyWrite(replies.into_bytes()))
    }

    /// OSC 6 과 106: 특수 색 번호와 켜기(0 이 아닌 값)·끄기(0)의 쌍들.
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
                    return Some(EngineEvent::Error(format!("OSC {selector} blink color cannot apply: the grid does not keep the blink attribute")));
                }
                (Ok(index), Ok(flag)) if index < 5 => self.special_enabled[index] = flag != 0,
                _ => {
                    return Some(EngineEvent::Error(format!(
                        "OSC {selector} pair is not a color number 0-4 and a flag: {};{}",
                        pair[0], pair[1]
                    )))
                }
            }
        }
        None
    }

    /// 속성이 있는 기본 전경색 글자에 켜진 특수 색을 적용한다. 굵게, 밑줄, 반전, 기울임 순으로 첫 색을 쓴다.
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

    /// 테마가 정한 기본 색.
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
                // 프롬프트는 새 행에서 시작한다. 앞의 출력이 개행 없이 끝났으면 다음 행으로 옮긴다.
                let cursor = &self.term.grid().cursor;
                if cursor.point.column.0 != 0 || cursor.input_needs_wrap {
                    self.processor.advance(&mut self.term, b"\r\n");
                }
                self.prompt_start = Some(self.absolute(self.term.grid().cursor.point.line.0));
                // 기본값: redraw 인자가 없는 A 표시는 redraw=1 이다(docs/spec/terminal-runtime.md).
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

    /// 커서가 있는 논리적 행의 첫 행. 재배치가 기록으로 옮긴 행도 같은 논리적 행이면 포함한다.
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

    /// 크기 변경 뒤 셸이 다시 그리기 전에, 셸이 다시 그릴 영역의 첫 행(start)부터 화면 끝까지 지우고,
    /// 커서를 그 첫 행에서 offset 만큼 아래 행의 0열로 옮긴다. 셸은 이전 폭에서의 커서 행 수(offset)만큼
    /// 올라가 다시 출력한다. 그 행이 화면 아래를 넘으면 넘는 만큼 화면을 올린다.
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

    /// 화면 행(기록은 음수)의 절대 번호.
    fn absolute(&self, line: i32) -> i64 {
        self.dropped + self.term.grid().history_size() as i64 + i64::from(line)
    }

    /// 입력을 엔진에 넣는다. 프롬프트 상태에서는 기록 한도를 잠시 넓혀 이 입력이 올린 행을 모두 기록에 둔 뒤,
    /// 한도를 넘은 행을 버리고 그 수를 센다. 그래서 프롬프트 첫 행의 절대 번호가 스크롤 뒤에도 맞다. 한 바이트는
    /// 화면 행 수보다 많이 올리지 못한다.
    fn advance(&mut self, bytes: &[u8]) {
        if self.prompt_start.is_none() {
            self.processor.advance(&mut self.term, bytes);
            return;
        }
        let rows = self.term.grid().screen_lines();
        self.term
            .grid_mut()
            .update_history(self.history_limit + bytes.len() * rows);
        self.processor.advance(&mut self.term, bytes);
        self.limit_history();
    }

    /// 기록을 한도로 줄이고 버린 행을 센다.
    /// 격자 크기를 바꾼다. 엔진은 화면을 저장소의 마지막 행들로 보므로, 폭을 좁히는 재배치로 늘어난 행은 커서 아래 빈
    /// 행이 있어도 위의 행을 기록으로 민다. 기본 화면에서는 행 수를 그대로 둔 채 폭을 먼저 바꾸고, 기록으로 간 행을 커서 아래
    /// 빈 행 수만큼 화면으로 되돌린다(docs/spec/terminal-runtime.md). 빈 행은 커서보다 아래이므로 행 수를 그만큼 줄이면
    /// 화면은 움직이지 않고 그 행만 빠지며, 다시 늘리면 엔진이 기록의 행을 위로 되돌린다.
    fn resize_grid(&mut self, primary: bool, cols: usize, rows: usize) {
        let lines = self.term.grid().screen_lines();
        if primary && cols < self.term.grid().columns() {
            // 기록 상한에 걸린 행이 버려지면 옮겨진 행 수를 셀 수 없으므로, 셀 동안만 상한을 화면 행 수만큼 늘린다.
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

    /// 커서 행 아래에서 화면 맨 아래까지 이어지는 빈 행 수.
    fn empty_rows_below_cursor(&self) -> usize {
        let grid = self.term.grid();
        let cursor = grid.cursor.point.line.0;
        let bottom = grid.screen_lines() as i32 - 1;
        (cursor + 1..=bottom)
            .rev()
            .take_while(|row| grid[Line(*row)].is_clear())
            .count()
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

    /// 프롬프트 첫 행의 화면 행. 기록 밖으로 버려졌거나 커서보다 아래면 셸이 다시 그릴 영역을 알 수 없으므로 오류다.
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

    /// row 부터 커서가 있는 논리적 행 앞까지 끝나는 논리적 행의 수.
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

    /// 커서가 있는 논리적 행에서 count 개 위의 논리적 행의 첫 행.
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

    fn audit_csi(&mut self, bytes: &[u8]) {
        let mut input = std::mem::take(&mut self.pending_csi);
        input.extend_from_slice(bytes);
        let mut index = 0;
        while index < input.len() {
            let Some(relative) = input[index..].windows(2).position(|pair| pair == b"\x1b[") else {
                if input.last() == Some(&0x1b) {
                    self.pending_csi.push(0x1b);
                }
                return;
            };
            let start = index + relative;
            let Some(final_offset) = input[start + 2..]
                .iter()
                .position(|byte| (0x40..=0x7e).contains(byte))
            else {
                self.pending_csi.extend_from_slice(&input[start..]);
                return;
            };
            let final_index = start + 2 + final_offset;
            let body = &input[start + 2..final_index];
            let unsupported = if input[final_index] == b't' && body != b"14" {
                Some(format!("window report {}t", String::from_utf8_lossy(body)))
            } else if (input[final_index] == b'h' || input[final_index] == b'l')
                && matches!(body, b"?47" | b"?1047" | b"?1048")
            {
                Some(format!(
                    "alternate screen mode {}{}",
                    String::from_utf8_lossy(body),
                    input[final_index] as char
                ))
            } else if input[final_index] == b'x' && body.contains(&b'$') {
                Some(format!(
                    "rectangle report {}x",
                    String::from_utf8_lossy(body)
                ))
            } else if input[final_index] == b'q' && body.contains(&b'"') {
                Some(format!(
                    "protected-cell report {}q",
                    String::from_utf8_lossy(body)
                ))
            } else if input[final_index] == b'p' && body.starts_with(b"#") {
                Some(format!("palette report {}p", String::from_utf8_lossy(body)))
            } else {
                None
            };
            if let Some(selector) = unsupported {
                self.events
                    .events
                    .lock()
                    .expect("engine event queue poisoned")
                    .push_back(QueuedEvent::Neutral(EngineEvent::Error(format!(
                        "unsupported CSI {selector}"
                    ))));
            }
            index = final_index + 1;
        }
    }

    fn feed_plain(&mut self, bytes: &[u8]) {
        let (bytes, erases) = self.normalize_sequences(bytes);
        let marks = self.audit_osc(&bytes);
        self.audit_csi(&bytes);
        // 셸 표시와 지우기를 입력 안의 위치 순서대로 적용한다.
        enum Step {
            Mark(ShellMark),
            Erase(Erase),
        }
        let mut steps: Vec<(usize, Step)> = marks
            .into_iter()
            .map(|mark| (mark.end, Step::Mark(mark)))
            .collect();
        steps.extend(
            erases
                .into_iter()
                .map(|(at, erase)| (at, Step::Erase(erase))),
        );
        steps.sort_by_key(|(at, _)| *at);
        let mut from = 0;
        for (at, step) in steps {
            self.advance(&bytes[from..at]);
            from = at;
            match step {
                Step::Mark(mark) => self.apply_shell_mark(mark.marker, mark.redraw),
                Step::Erase(erase) => self.erase(erase),
            }
        }
        self.advance(&bytes[from..]);
    }

    fn feed_with_inline_images(&mut self, bytes: &[u8]) {
        const PREFIX: &[u8] = b"\x1b]1337;";
        self.pending_input.extend_from_slice(bytes);
        loop {
            let Some(start) = self
                .pending_input
                .windows(PREFIX.len())
                .position(|candidate| candidate == PREFIX)
            else {
                let keep = (1..PREFIX.len())
                    .rev()
                    .find(|length| self.pending_input.ends_with(&PREFIX[..*length]))
                    // 기본값: 입력 끝이 접두어의 앞부분과 겹치지 않으면 남길 바이트가 없다.
                    .unwrap_or(0);
                let split = self.pending_input.len().saturating_sub(keep);
                let plain = self.pending_input[..split].to_vec();
                self.pending_input.drain(..split);
                self.feed_plain(&plain);
                return;
            };

            let before = self.pending_input[..start].to_vec();
            self.pending_input.drain(..start);
            self.feed_plain(&before);

            let body_start = PREFIX.len();
            let terminator = self.pending_input[body_start..]
                .iter()
                .enumerate()
                .find_map(|(offset, byte)| (*byte == b'\x07').then_some((body_start + offset, 1)))
                .or_else(|| {
                    self.pending_input[body_start..]
                        .windows(2)
                        .position(|pair| pair == b"\x1b\\")
                        .map(|offset| (body_start + offset, 2))
                });
            let Some((end, terminator_len)) = terminator else {
                return;
            };
            let payload = self.pending_input[body_start..end].to_vec();
            self.pending_input.drain(..end + terminator_len);
            // 그림은 시퀀스를 만난 자리의 커서에 놓인다. 뒤의 출력이 커서를 옮기기 전에 위치를 싣는다.
            let cursor = self.term.grid().cursor.point;
            let anchor = InlineAnchor {
                col: cursor.column.0 as u16,
                row: cursor.line.0.max(0) as u16,
                scroll: self.scroll_generation(),
            };
            match parse_inline_image(&payload) {
                Ok(command) => self
                    .events
                    .events
                    .lock()
                    .expect("engine event queue poisoned")
                    .push_back(QueuedEvent::Neutral(EngineEvent::InlineImage {
                        command,
                        anchor,
                    })),
                Err(error) => self
                    .events
                    .events
                    .lock()
                    .expect("engine event queue poisoned")
                    .push_back(QueuedEvent::Neutral(EngineEvent::Error(error))),
            }
        }
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
        self.events
            .events
            .lock()
            .expect("engine event queue poisoned")
            .push_back(QueuedEvent::Neutral(EngineEvent::PtyWrite(
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

    /// 뷰포트 좌표의 칸을 격자 좌표로 바꾼다. 스크롤백을 보는 동안 뷰포트의 위쪽 행은 기록 행이다.
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

    /// 뷰포트를 lines 만큼 움직인다. 양수는 오래된 출력 쪽이며 보관된 기록 범위 안으로 제한된다.
    pub fn scroll_viewport(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// 뷰포트를 가장 새 출력으로 되돌리고, 움직였으면 true 를 반환한다.
    pub fn scroll_to_newest(&mut self) -> bool {
        // 이미 가장 새 출력이면 움직이지 않는다. scroll_display 는 움직이지 않아도 렌더러 알림을 낸다.
        if self.term.grid().display_offset() == 0 {
            return false;
        }
        self.term.scroll_display(Scroll::Bottom);
        true
    }

    /// (뷰포트가 가장 새 출력보다 위에 있는 줄 수, 보관된 기록 줄 수).
    pub fn scrollback(&self) -> (usize, usize) {
        (
            self.term.grid().display_offset(),
            self.term.grid().history_size(),
        )
    }

    /// 칸 경계를 alacritty 의 칸과 쪽으로 바꾼다. 경계 c 는 칸 c 의 왼쪽이고, 열 수는 마지막 칸의 오른쪽이다.
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

    /// 선택은 시작 경계와 이 경계 사이의 칸이다. 같은 경계는 아무 칸도 선택하지 않는다.
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
        // 커서 행은 격자 좌표이므로 뷰포트 행으로 바꾼다. 스크롤백을 보는 동안 뷰포트 밖이면 숨긴다.
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
    ) -> Option<String> {
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
        Some(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b))
    }

    fn cell(
        &self,
        cell: &alacritty_terminal::term::cell::Cell,
        colors: &alacritty_terminal::term::color::Colors,
    ) -> Cell {
        let mut text = String::new();
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
                Some(rgb) => Some(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b)),
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
            && cell
                .fg
                .as_deref()
                .is_none_or(|color| color == self.theme_hex(self.theme.foreground).as_str())
            && cell
                .bg
                .as_deref()
                .is_none_or(|color| color == self.theme_hex(self.theme.background).as_str())
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
            // 같은 크기에는 PTY 가 SIGWINCH 를 보내지 않으므로 셸도 다시 그리지 않는다.
            return;
        }
        let primary = !self.term.mode().contains(TermMode::ALT_SCREEN);
        let cursor = self.term.grid().cursor.point.line.0;
        // 프롬프트 첫 행은 논리적 행의 첫 행이다. 재배치는 논리적 행을 유지하므로, 커서의 논리적 행에서 몇 개 위의
        // 논리적 행인지로 재배치 뒤 위치를 다시 찾는다.
        let prompt = match self.prompt_row() {
            Ok(row) => Some((row, self.logical_lines_before_cursor(row))),
            Err(error) => {
                if self.prompt_start.is_some() {
                    self.events
                        .events
                        .lock()
                        .expect("engine event queue poisoned")
                        .push_back(QueuedEvent::Neutral(EngineEvent::Error(format!(
                            "terminal resize: {error}"
                        ))));
                }
                None
            }
        };
        // 셸이 올라갈 행 수: 이전 폭에서 커서가 다시 그릴 영역의 첫 행보다 아래에 있던 행 수.
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
                // 셸은 지운 영역의 첫 행에서 다시 그린다.
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
        self.feed_with_inline_images(bytes);
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
        let mut lines = vec![Vec::<Cell>::new(); rows as usize];
        // 표시 점은 격자 좌표다. 스크롤백을 보는 동안 기록 행은 음수이므로 뷰포트 오프셋을 더한다.
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
            // 두 번째 인자는 커서 위치다. alacritty 는 블록 커서가 선택 경계에 있을 때만 그 칸을 반전하지 않는다.
            if renderable.selection.as_ref().is_some_and(|selection| {
                selection.contains_cell(&indexed, renderable.cursor.point, renderable.cursor.shape)
            }) {
                // 선택한 칸은 강조 배경(없으면 테마의 선택 배경) 위에 강조 글자(없으면 칸의 글자 색)로 그린다.
                let hex = |rgb: Rgb| format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b);
                let foreground = if cell.inverse {
                    cell.bg.clone()
                } else {
                    cell.fg.clone()
                };
                let foreground =
                    foreground.or_else(|| self.default_rgb(NamedColor::Foreground).map(hex));
                cell.bg = Some(hex(self
                    .highlight_background
                    // 기본값: OSC 17 강조 배경이 없으면 선택 칸은 테마의 선택 배경을 쓴다(docs/spec/terminal-runtime.md).
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
            // 프로그램이 OSC 11 로 정한 배경이 없으면 테마의 기본 배경이다.
            background: self.term.colors()[NamedColor::Background].map_or_else(
                || self.theme_hex(self.theme.background),
                |rgb| format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
            ),
            lines,
        }
    }

    fn scroll_generation(&self) -> i64 {
        self.term.grid().history_size() as i64
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

    fn reset(&mut self) {
        *self = Self::new();
    }
}
