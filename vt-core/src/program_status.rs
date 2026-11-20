//! The program status of a session (core docs/spec/terminal-state.md#program-status).
//!
//! A program that runs as one command reports its own state with the Program Status Protocol (OSC 7501, revision 0.4):
//! [`parse_report`] reads one report into an [`Action`] and the reasons of the parts that it rejects, and
//! [`ProgramStatusTracker`] keeps the records of the session together with the notice, the call of a program that sent a
//! notification or a bell while the surface had no keyboard focus. The tracker depends on no engine: each engine gives it
//! the parsed reports, the OSC 133 marks, the notifications, the full resets and the end of the session, and the service
//! gives it the focus and the input of the person. Each method returns a [`Change`] only when the records or the notice
//! change, so a repeated report gives no event.

use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde_json::{Value, json};

use crate::protocol::{EngineEvent, ShellMarker};

/// The longest report, from its `ESC` through its terminator.
pub const MAX_SEQUENCE_BYTES: usize = 4096;
/// The most records that a session keeps.
pub const MAX_RECORDS: usize = 256;
const MAX_KEY_BYTES: usize = 16;
const MAX_MSG_ENCODED: usize = 2732;
const MAX_MSG_DECODED: usize = 2048;
const MAX_TITLE_ENCODED: usize = 256;
const MAX_TITLE_DECODED: usize = 192;
const MAX_ID_BYTES: usize = 128;
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_SEGMENTS: usize = 8;
const MAX_APP_BYTES: usize = 32;

/// Standard base64 whose padding is optional, as the protocol states. Trailing bits that are not zero are refused.
static BASE64: LazyLock<GeneralPurpose> = LazyLock::new(|| {
    GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
});

/// The state of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

impl ProgramState {
    pub fn name(self) -> &'static str {
        match self {
            ProgramState::Idle => "idle",
            ProgramState::Working => "working",
            ProgramState::Blocked => "blocked",
            ProgramState::Done => "done",
            ProgramState::Error => "error",
        }
    }
}

/// What a `blocked` record waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Permission,
    Question,
    Auth,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Auth => "auth",
        }
    }
}

/// One record as a report gives it. `id` is None for the root record; `app` is the reported value only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub id: Option<String>,
    pub state: ProgramState,
    pub kind: Option<Kind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// What an accepted report does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Replaces the record of its id.
    Set(Record),
    /// Removes the named record and every record below it, or every record without an id.
    Clear { id: Option<String> },
}

/// A parsed report: the action, or None when the report is rejected whole, and the reason of each rejected part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub action: Option<Action>,
    pub rejections: Vec<String>,
}

fn rejected(reason: String) -> Parsed {
    Parsed {
        action: None,
        rejections: vec![reason],
    }
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-')
}

fn valid_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('/').collect();
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && segments.len() <= MAX_ID_SEGMENTS
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment.len() <= MAX_ID_SEGMENT_BYTES
                && segment.bytes().all(is_name_byte)
        })
}

/// Decodes `title` or `msg`; an error rejects the report whole.
fn decode_text(
    key: &str,
    value: &str,
    max_encoded: usize,
    max_decoded: usize,
) -> Result<String, String> {
    if value.len() > max_encoded {
        return Err(format!(
            "OSC 7501 {key} is longer than {max_encoded} encoded bytes"
        ));
    }
    let bytes = BASE64
        .decode(value)
        .map_err(|error| format!("OSC 7501 {key} is not base64: {error}"))?;
    if bytes.len() > max_decoded {
        return Err(format!(
            "OSC 7501 {key} is longer than {max_decoded} decoded bytes"
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("OSC 7501 {key} is not UTF-8"))?;
    if text.chars().any(char::is_control) {
        return Err(format!("OSC 7501 {key} holds a control character"));
    }
    Ok(text)
}

/// Parses the payload of one OSC 7501 report other than the query. `sequence_bytes` is the length of the whole sequence
/// from its `ESC` through its terminator.
pub fn parse_report(payload: &str, sequence_bytes: usize) -> Parsed {
    if sequence_bytes > MAX_SEQUENCE_BYTES {
        return rejected(format!(
            "OSC 7501 report is longer than {MAX_SEQUENCE_BYTES} bytes"
        ));
    }
    let mut rejections = Vec::new();
    let (mut state, mut id, mut kind, mut progress, mut app, mut title, mut msg) =
        (None, None, None, None, None, None, None);
    for (index, pair) in payload.split(':').enumerate() {
        let Some((key, value)) = pair.split_once('=') else {
            rejections.push(format!("OSC 7501 pair {} has no =", index + 1));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty()
            || key.len() > MAX_KEY_BYTES
            || !key.bytes().all(|byte| byte.is_ascii_lowercase())
        {
            rejections.push(format!("OSC 7501 pair {} has an invalid key", index + 1));
            continue;
        }
        // A `title` or `msg` of another byte is base64 that does not decode, which rejects the report whole.
        if !value.bytes().all(is_value_byte) && key != "title" && key != "msg" {
            rejections.push(format!("OSC 7501 pair {} has an invalid value", index + 1));
            continue;
        }
        let slot = match key {
            "state" => &mut state,
            "id" => &mut id,
            "kind" => &mut kind,
            "progress" => &mut progress,
            "app" => &mut app,
            "title" => &mut title,
            "msg" => &mut msg,
            // silent: the protocol requires a terminal to ignore a key that it does not define, so that a later
            // revision can add keys (core docs/spec/terminal-state.md#records).
            _ => continue,
        };
        *slot = Some(value.to_string());
    }
    let state = match state.as_deref() {
        None => return rejected("OSC 7501 report has no state".to_string()),
        Some("clear") => None,
        Some("idle") => Some(ProgramState::Idle),
        Some("working") => Some(ProgramState::Working),
        Some("blocked") => Some(ProgramState::Blocked),
        Some("done") => Some(ProgramState::Done),
        Some("error") => Some(ProgramState::Error),
        Some(other) => return rejected(format!("OSC 7501 state is unknown: {other}")),
    };
    if let Some(id) = id.as_deref()
        && !valid_id(id)
    {
        return rejected("OSC 7501 id is invalid".to_string());
    }
    let title = match title {
        Some(value) => match decode_text("title", &value, MAX_TITLE_ENCODED, MAX_TITLE_DECODED) {
            Ok(text) => Some(text),
            Err(reason) => return rejected(reason),
        },
        None => None,
    };
    let msg = match msg {
        Some(value) => match decode_text("msg", &value, MAX_MSG_ENCODED, MAX_MSG_DECODED) {
            Ok(text) => Some(text),
            Err(reason) => return rejected(reason),
        },
        None => None,
    };
    let Some(state) = state else {
        for (key, present) in [
            ("kind", kind.is_some()),
            ("progress", progress.is_some()),
            ("app", app.is_some()),
            ("title", title.is_some()),
            ("msg", msg.is_some()),
        ] {
            if present {
                rejections.push(format!("OSC 7501 clear takes no key {key}"));
            }
        }
        return Parsed {
            action: Some(Action::Clear { id }),
            rejections,
        };
    };
    let kind = kind.and_then(|value| {
        let parsed = match value.as_str() {
            "permission" => Kind::Permission,
            "question" => Kind::Question,
            "auth" => Kind::Auth,
            _ => {
                rejections.push(format!("OSC 7501 kind is unknown: {value}"));
                return None;
            }
        };
        if state != ProgramState::Blocked {
            rejections.push(format!(
                "OSC 7501 kind applies only to blocked, not to {}",
                state.name()
            ));
            return None;
        }
        Some(parsed)
    });
    let progress = progress.and_then(|value| {
        let number = (!value.is_empty()
            && value.len() <= 3
            && value.bytes().all(|byte| byte.is_ascii_digit()))
        // At most three digits, so the value fits in u16 and a value of at most 100 fits in u8.
        .then(|| {
            value
                .bytes()
                .fold(0u16, |number, byte| number * 10 + u16::from(byte - b'0'))
        })
        .filter(|number| *number <= 100)
        .map(|number| number as u8);
        let Some(number) = number else {
            rejections.push(format!("OSC 7501 progress is not 0 to 100: {value}"));
            return None;
        };
        if !matches!(state, ProgramState::Working | ProgramState::Blocked) {
            rejections.push(format!(
                "OSC 7501 progress applies only to working and blocked, not to {}",
                state.name()
            ));
            return None;
        }
        Some(number)
    });
    let app = app.and_then(|value| {
        if value.is_empty() || value.len() > MAX_APP_BYTES || !value.bytes().all(is_name_byte) {
            rejections.push("OSC 7501 app is invalid".to_string());
            return None;
        }
        Some(value)
    });
    Parsed {
        action: Some(Action::Set(Record {
            id,
            state,
            kind,
            progress,
            app,
            title,
            msg,
        })),
        rejections,
    }
}

/// A record as the change event and the read show it: `app` is the reported or the inherited value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordView {
    pub record: Record,
    pub app: Option<String>,
}

impl RecordView {
    pub fn to_json(&self) -> Value {
        let record = &self.record;
        json!({
            "id": record.id,
            "state": record.state.name(),
            "kind": record.kind.map(Kind::name),
            "progress": record.progress,
            "app": self.app,
            "title": record.title,
            "msg": record.msg,
        })
    }

    /// The members that the event log keeps, without `title` and `msg`.
    pub fn to_log_json(&self) -> Value {
        json!({
            "id": self.record.id,
            "state": self.record.state.name(),
            "kind": self.record.kind.map(Kind::name),
            "app": self.app,
        })
    }
}

/// The records and the notice of a session at one change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub records: Vec<RecordView>,
    pub notice: bool,
    pub sequence: u64,
}

impl Current {
    /// The members of the change event and of the read reply, without `event`.
    pub fn to_json(&self) -> Value {
        json!({
            "records": self.records.iter().map(RecordView::to_json).collect::<Vec<_>>(),
            "notice": self.notice,
            "sequence": self.sequence,
        })
    }

    pub fn to_log_json(&self) -> Value {
        json!({
            "records": self.records.iter().map(RecordView::to_log_json).collect::<Vec<_>>(),
            "notice": self.notice,
            "sequence": self.sequence,
        })
    }
}

/// A change of the records or the notice, and the ids of the records that the limit removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub current: Current,
    pub evicted: Vec<Option<String>>,
}

#[derive(Debug)]
struct Entry {
    record: Record,
    updated: u64,
}

/// Tracks the program status of the session of one surface. A session that differs from the tracked one starts without
/// records, without a notice and at sequence 0.
#[derive(Debug, Default)]
pub struct ProgramStatusTracker {
    session: Option<String>,
    records: Vec<Entry>,
    notice: bool,
    sequence: u64,
    clock: u64,
    /// The keyboard focus of the surface, which a new session keeps.
    focused: bool,
}

fn below(id: &str, parent: &str) -> bool {
    id.len() > parent.len() && id.starts_with(parent) && id.as_bytes()[parent.len()] == b'/'
}

impl ProgramStatusTracker {
    /// Applies an accepted report.
    pub fn report(&mut self, session: &str, action: &Action) -> Option<Change> {
        self.follow(session);
        let before = self.views();
        let mut evicted = Vec::new();
        match action {
            Action::Set(record) => {
                self.clock += 1;
                if let Some(entry) = self
                    .records
                    .iter_mut()
                    .find(|entry| entry.record.id == record.id)
                {
                    entry.record = record.clone();
                    entry.updated = self.clock;
                } else {
                    if self.records.len() == MAX_RECORDS
                        && let Some(oldest) = self
                            .records
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, entry)| entry.updated)
                            .map(|(index, _)| index)
                    {
                        evicted.push(self.records.remove(oldest).record.id);
                    }
                    self.records.push(Entry {
                        record: record.clone(),
                        updated: self.clock,
                    });
                }
            }
            Action::Clear { id: None } => self.records.clear(),
            Action::Clear { id: Some(id) } => {
                self.records
                    .retain(|entry| match entry.record.id.as_deref() {
                        Some(own) => own != id && !below(own, id),
                        None => true,
                    })
            }
        }
        self.changed(before, self.notice, evicted)
    }

    /// Applies an OSC 133 mark: `D` and a prompt start of kind `i` end the records of a running program and the notice,
    /// `C` ends the `done` and `error` records.
    pub fn mark(
        &mut self,
        session: &str,
        marker: ShellMarker,
        params: &[String],
    ) -> Option<Change> {
        self.follow(session);
        let before = self.views();
        let notice = self.notice;
        let prompt = || {
            params
                .iter()
                .find_map(|parameter| parameter.strip_prefix("k="))
                .is_none_or(|kind| kind == "i")
        };
        match marker {
            ShellMarker::CommandFinished => self.end_program(),
            ShellMarker::PromptStart if prompt() => self.end_program(),
            ShellMarker::CommandStart => self.records.retain(|entry| {
                !matches!(entry.record.state, ProgramState::Done | ProgramState::Error)
            }),
            ShellMarker::PromptStart | ShellMarker::PromptEnd => {}
        }
        self.changed(before, notice, Vec::new())
    }

    fn end_program(&mut self) {
        self.records.retain(|entry| {
            !matches!(
                entry.record.state,
                ProgramState::Working | ProgramState::Blocked | ProgramState::Idle
            )
        });
        self.notice = false;
    }

    /// Applies the engine event that is a cause of the program status: a report, an OSC 133 mark, a notification, a
    /// bell or a full reset. Another event changes nothing.
    pub fn apply(&mut self, session: &str, event: &EngineEvent) -> Option<Change> {
        match event {
            EngineEvent::ProgramStatus(action) => self.report(session, action),
            EngineEvent::ShellState { marker, params } => self.mark(session, *marker, params),
            EngineEvent::Notification { .. } | EngineEvent::Bell => self.notify(session),
            EngineEvent::FullReset => self.reset(session),
            _ => None,
        }
    }

    /// A notification or a bell of the program: a notice while the surface has no keyboard focus.
    pub fn notify(&mut self, session: &str) -> Option<Change> {
        self.follow(session);
        let before = self.views();
        let notice = self.notice;
        if !self.focused {
            self.notice = true;
        }
        self.changed(before, notice, Vec::new())
    }

    /// The keyboard focus of the surface: gaining it ends the notice. A surface starts without the focus.
    pub fn focus(&mut self, session: &str, focused: bool) -> Option<Change> {
        self.focused = focused;
        self.follow(session);
        let before = self.views();
        let notice = self.notice;
        if focused {
            self.notice = false;
        }
        self.changed(before, notice, Vec::new())
    }

    /// The person typed into the surface: the notice ends.
    pub fn typed(&mut self, session: &str) -> Option<Change> {
        self.follow(session);
        let before = self.views();
        let notice = self.notice;
        self.notice = false;
        self.changed(before, notice, Vec::new())
    }

    /// A full reset (RIS) removes every record.
    pub fn reset(&mut self, session: &str) -> Option<Change> {
        self.follow(session);
        let before = self.views();
        let notice = self.notice;
        self.records.clear();
        self.changed(before, notice, Vec::new())
    }

    /// The end of the session removes every record and the notice.
    pub fn end(&mut self, session: &str) -> Option<Change> {
        self.follow(session);
        let before = self.views();
        let notice = self.notice;
        self.records.clear();
        self.notice = false;
        self.changed(before, notice, Vec::new())
    }

    /// The current records and notice of the session.
    pub fn current(&mut self, session: &str) -> Current {
        self.follow(session);
        Current {
            records: self.views(),
            notice: self.notice,
            sequence: self.sequence,
        }
    }

    fn changed(
        &mut self,
        before: Vec<RecordView>,
        notice: bool,
        evicted: Vec<Option<String>>,
    ) -> Option<Change> {
        let records = self.views();
        if records == before && self.notice == notice {
            return None;
        }
        self.sequence += 1;
        Some(Change {
            current: Current {
                records,
                notice: self.notice,
                sequence: self.sequence,
            },
            evicted,
        })
    }

    /// The records, the root record first and then by id in code point order, each with its inherited `app`.
    fn views(&self) -> Vec<RecordView> {
        let mut records: Vec<&Record> = self.records.iter().map(|entry| &entry.record).collect();
        records.sort_by(|a, b| a.id.cmp(&b.id));
        records
            .iter()
            .map(|record| RecordView {
                record: (*record).clone(),
                app: record
                    .app
                    .clone()
                    .or_else(|| self.inherited_app(record.id.as_deref())),
            })
            .collect()
    }

    fn inherited_app(&self, id: Option<&str>) -> Option<String> {
        let mut id = id?;
        let app_of = |wanted: Option<&str>| {
            self.records
                .iter()
                .find(|entry| entry.record.id.as_deref() == wanted)
                .and_then(|entry| entry.record.app.clone())
        };
        while let Some((parent, _)) = id.rsplit_once('/') {
            if let Some(app) = app_of(Some(parent)) {
                return Some(app);
            }
            id = parent;
        }
        app_of(None)
    }

    fn follow(&mut self, session: &str) {
        if self.session.as_deref() != Some(session) {
            *self = Self {
                session: Some(session.to_string()),
                focused: self.focused,
                ..Self::default()
            };
        }
    }
}
