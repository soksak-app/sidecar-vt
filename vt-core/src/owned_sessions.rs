// The owned sessions of the persistent service (core docs/spec/persistent-services.md#owned-sessions, sidecar
// docs/spec/vt-core.md#owned-session-operations). An owned session is a PTY session of its owner surface. Its output feeds an
// engine of its own, so a tab that attaches receives the bytes that draw the screen of that engine. While a tab shows the
// session, the surface of the tab draws it and its configure sets the grid.
use crate::encoding;
use crate::events::{Events, Level, Subject};
use crate::guard::{self, LineInput};
use crate::platform::{Metrics, default_font, metrics_for};
use crate::program_status::{MAX_RECORDS, ProgramStatusTracker, program_state};
use crate::protocol::{
    CursorShape, DEFAULT_FONT_SIZE, DaemonEvent, Dispatch, Engine, EngineEvent, OutputSink,
    Processes, RowScreen, SCREEN_ROWS_COUNT_MAX, SessionPort, ShellRequest, open_launch,
    screen_text, session_resize,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};

/// The factory of the engine of an owned session.
pub type EngineFactory = Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>;
/// The factory of the session port of an owned session.
pub type SessionPortFactory = Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>;

/// The size of the card of an owned session: its width and height in points and the backing scale of the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// Reads the `size` member of a request. Each member must be a positive number.
pub fn parse_size(value: Option<&Value>) -> Result<Size, String> {
    let member = |name: &str| {
        value
            .and_then(|size| size.get(name))
            .and_then(Value::as_f64)
            .filter(|number| number.is_finite() && *number > 0.0)
            .ok_or_else(|| "size width, height and scale must be positive numbers".to_string())
    };
    Ok(Size {
        width: member("width")?,
        height: member("height")?,
        scale: member("scale")?,
    })
}

/// Converts a size to a grid: each whole cell of the device pixel metrics that fits the size in device pixels
/// (width × scale and height × scale). A grid without a column or a row is refused, and so is a grid beyond the
/// `u16` range of the terminal.
pub fn grid(size: Size, cell_width: f32, cell_height: f32) -> Result<(u16, u16), String> {
    let columns = (size.width * size.scale / f64::from(cell_width)).floor();
    let rows = (size.height * size.scale / f64::from(cell_height)).floor();
    if columns < 1.0 || rows < 1.0 {
        return Err("size is smaller than one cell".to_string());
    }
    let too_large = || "size is larger than the terminal grid".to_string();
    let columns = u16::try_from(columns as u64).map_err(|_| too_large())?;
    let rows = u16::try_from(rows as u64).map_err(|_| too_large())?;
    Ok((columns, rows))
}

/// The device pixel cell metrics of the default font at the backing scale of a size.
pub fn default_cell_metrics(scale: f64) -> Result<Metrics, String> {
    metrics_for(&default_font(), DEFAULT_FONT_SIZE, scale as f32)
}

/// The connection of the owner of a session that its events go to, shared by the entry and the task of the session. Each
/// request of the owner replaces it, so an owner that connects again receives the events on its new connection.
type SinkCell = Arc<tokio::sync::Mutex<Option<OutputSink>>>;

/// The last program state of a session, shared by the entry and the task of the session: the task sets it with each
/// `session.program` event, and the `sessions` answer lists it, so an owner that adopts the session after a restart
/// receives the state that the program reported only once.
type ProgramCell = Arc<std::sync::Mutex<Option<&'static str>>>;

/// The commands that the task of a session receives from the service.
enum Control {
    /// The grid of the engine of the session: each resize of the PTY of the session sets it, so the engine wraps the output at
    /// the width that the program draws for.
    Resize(u16, u16),
    /// The text of an `input` (`paste` false) or a `paste` of the owner. The task judges it with the line guard and the modes of
    /// the engine, and answers the bytes to write, or the error body of a refusal.
    Write {
        text: String,
        paste: bool,
        cids: Vec<String>,
        reply: oneshot::Sender<Result<Vec<u8>, Value>>,
    },
    /// The absolute line number of the first visible row, the visible rows of the screen as text and the cursor, or with `range` the retained rows `(from, count)`
    /// of the primary screen as text; a range outside the retained rows answers the error of the engine.
    Read {
        range: Option<(u64, u32)>,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    /// The attachment of a surface: the engine takes the grid, the bytes that draw the screen of the engine go to `forward`,
    /// and later output goes to `forward` until the surface ends.
    Attach {
        columns: u16,
        rows: u16,
        forward: mpsc::Sender<DaemonEvent>,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// One owned session.
struct Entry {
    owner: String,
    /// The project root of the owner window, from the envelope of the open.
    root: String,
    /// The creation identifier of the open.
    creation: String,
    /// The plugin whose surface opened the session, from the open, which the `sessions` answer names.
    plugin: String,
    pty: String,
    /// The last size that the owner or the attached view set, which the grid of a new attachment uses.
    size: Size,
    port: Option<Arc<dyn SessionPort>>,
    control: Option<mpsc::UnboundedSender<Control>>,
    /// Set by the task of the session when the shell or program of the session exits.
    exited: Arc<AtomicBool>,
    ended: bool,
    /// The surface that shows the session and its command channel, while that surface is attached.
    shown_by: Option<(String, mpsc::Sender<Dispatch>)>,
    /// Whether the session started a shell rather than a program (guard::follows_shell_line).
    starts_shell: bool,
    /// The connection of the owner that sent the last request of the session: the events of the session go there.
    sink: SinkCell,
    /// The last program state of the session, which the `sessions` answer lists.
    program: ProgramCell,
}

/// The owned sessions of one persistent service. Its entries outlive each connection, so a restored owner reattaches.
#[derive(Default)]
pub struct OwnedSessions {
    entries: HashMap<String, Entry>,
    by_creation: HashMap<(String, String), String>,
}

impl OwnedSessions {
    /// The open sessions as the entries of the `sessions` answer, `{session, surface, root, creation, size, plugin, program}`, ordered by
    /// session (core docs/spec/persistent-services.md#owned-sessions). A session whose shell or program ended is not listed.
    pub(crate) async fn list(&mut self, log: &Events) -> Vec<Value> {
        let mut listed = Vec::new();
        for (session, entry) in &mut self.entries {
            settle(entry, log).await;
            if entry.ended {
                continue;
            }
            listed.push(json!({
                "session": session,
                "surface": entry.owner,
                "root": entry.root,
                "creation": entry.creation,
                "size": {"width": entry.size.width, "height": entry.size.height, "scale": entry.size.scale},
                "plugin": entry.plugin,
                "program": *entry
                    .program
                    .lock()
                    // default: the state is one value that each holder replaces whole, so a panic of another holder
                    // leaves it consistent.
                    .unwrap_or_else(|error| error.into_inner()),
            }));
        }
        listed.sort_by(|a, b| a["session"].as_str().cmp(&b["session"].as_str()));
        listed
    }
}

/// The result of an attachment: the PTY session, its grid, and the events that the attached surface receives.
pub(crate) struct Attached {
    /// The port that opened the PTY session: the session port keeps the sessions of its own port only.
    pub port: Arc<dyn SessionPort>,
    pub pty: String,
    pub columns: u16,
    pub rows: u16,
    pub events: mpsc::Receiver<DaemonEvent>,
    /// Whether the session started a shell rather than a program (guard::follows_shell_line).
    pub starts_shell: bool,
}

/// Whether a request body is an owned-session request: an `open` with an object `session`, a `resize` or `end` with a
/// `session`, or an `input`, `paste` or `read` with a text `session`.
pub fn is_request(body: &Value) -> bool {
    match body.get("operation").and_then(Value::as_str) {
        Some("open") => body.get("session").is_some_and(Value::is_object),
        Some("resize" | "end") => body.get("session").is_some(),
        Some("input" | "paste" | "read") => body.get("session").is_some_and(Value::is_string),
        _ => false,
    }
}

/// The sender of an owned-session request: the owner surface, the correlation identifiers of the request, and the connection
/// that its events go to.
pub(crate) struct Request<'a> {
    pub owner: &'a str,
    /// The project root of the envelope, which an `open` records for the `sessions` answer.
    pub root: Option<&'a str>,
    pub cids: &'a [String],
    pub output: &'a OutputSink,
}

/// Answers an owned-session request and returns the reply body.
pub(crate) async fn answer(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    request: &Request<'_>,
    body: &Value,
    engine_factory: &EngineFactory,
    session_port_factory: &SessionPortFactory,
    log: &Events,
) -> Value {
    let Request {
        owner,
        cids,
        output,
        ..
    } = *request;
    let result = match body.get("operation").and_then(Value::as_str) {
        Some("open") => {
            open(
                sessions,
                request,
                body,
                engine_factory,
                session_port_factory,
                log,
            )
            .await
        }
        Some("resize") => resize(sessions, owner, body, log, output).await,
        Some("end") => end(sessions, owner, body, log, output).await,
        Some("input") => write_text(sessions, owner, cids, body, false, log, output).await,
        Some("paste") => write_text(sessions, owner, cids, body, true, log, output).await,
        Some("read") => read(sessions, owner, body, log).await,
        _ => Err("owned session request has no known operation".to_string()),
    };
    match result {
        Ok(reply) => reply,
        Err(error) => {
            log.record_error("owned session", &error);
            json!({"error": error})
        }
    }
}

fn session_member(body: &Value) -> Result<String, String> {
    body.get("session")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "owned session request names no session".to_string())
}

/// The launch request of an owned session. Its `params` take the fields of the terminal `open` request.
fn launch_request(owner: &str, params: &Value) -> Result<ShellRequest, String> {
    if !params.is_object() {
        return Err("open params must be an object".to_string());
    }
    let (shell, program, args) = open_launch(params)?;
    let directory = match params.get("directory") {
        None => None,
        Some(Value::String(directory)) if directory.starts_with('/') => Some(directory.clone()),
        Some(_) => return Err("open directory must be an absolute path".to_string()),
    };
    Ok(ShellRequest {
        shell,
        program,
        args,
        directory,
        surface: owner.to_string(),
    })
}

/// Records the end of a session whose shell or program has exited: the session is ended, its port is released and no
/// later request reaches it.
async fn settle(entry: &mut Entry, log: &Events) {
    if entry.ended || !entry.exited.load(Ordering::Acquire) {
        return;
    }
    entry.ended = true;
    entry.control = None;
    entry.shown_by = None;
    if let Some(port) = entry.port.take()
        && let Err(error) = port.close(&entry.pty).await
    {
        log.record_error(
            "owned session",
            format!("session {}: close after exit failed: {error}", entry.pty),
        );
    }
}

/// Returns the entry of a session after its end is settled. A missing session and an ended one are both not open.
async fn open_entry<'a>(
    table: &'a mut OwnedSessions,
    session: &str,
    log: &Events,
) -> Result<&'a mut Entry, String> {
    let entry = table
        .entries
        .get_mut(session)
        .ok_or_else(|| format!("session {session} is not open"))?;
    settle(entry, log).await;
    if entry.ended {
        return Err(format!("session {session} is not open"));
    }
    Ok(entry)
}

async fn open(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    request: &Request<'_>,
    body: &Value,
    engine_factory: &EngineFactory,
    session_port_factory: &SessionPortFactory,
    log: &Events,
) -> Result<Value, String> {
    let Request {
        owner,
        root,
        output,
        ..
    } = *request;
    let creation = body
        .get("session")
        .and_then(|session| session.get("creation"))
        .and_then(Value::as_str)
        .filter(|creation| !creation.is_empty())
        .ok_or_else(|| "open session requires a non-empty creation".to_string())?
        .to_string();
    let plugin = body
        .get("plugin")
        .and_then(Value::as_str)
        .filter(|plugin| !plugin.is_empty())
        .ok_or_else(|| "open session requires a non-empty plugin".to_string())?
        .to_string();
    let root = root
        .filter(|root| !root.is_empty())
        .ok_or_else(|| "open session requires a non-empty root".to_string())?
        .to_string();
    let size = parse_size(body.get("size"))?;
    // default: an absent params member is {} and starts the login shell, as the spec states.
    let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
    let request = launch_request(owner, &params)?;
    let shared = Arc::clone(sessions);
    let mut table = sessions.lock().await;
    if let Some(session) = table
        .by_creation
        .get(&(owner.to_string(), creation.clone()))
        .cloned()
    {
        if let Some(entry) = table.entries.get_mut(&session) {
            *entry.sink.lock().await = Some(output.clone());
        }
        return Ok(json!({"session": session, "creation": creation}));
    }
    let metrics = default_cell_metrics(size.scale)?;
    let (columns, rows) = grid(size, metrics.cell_width, metrics.cell_height)?;
    let mut engine = engine_factory();
    engine.resize(columns, rows);
    engine.set_cell_metrics(
        metrics.cell_width.round() as u16,
        metrics.cell_height.round() as u16,
    )?;
    let port = session_port_factory();
    let events = port.get_events().await;
    let pty = port
        .open(&request, columns, rows)
        .await
        .map_err(|error| format!("open failed: {error}"))?;
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let exited = Arc::new(AtomicBool::new(false));
    // A random identifier: a later service process never names a session with the identifier of a session of an earlier
    // one, which a stored layout may still name (core docs/spec/persistent-services.md#owned-session-messages).
    let session = format!("owned-{}", uuid::Uuid::new_v4());
    let sink: SinkCell = Arc::new(tokio::sync::Mutex::new(Some(output.clone())));
    let program: ProgramCell = Arc::default();
    tokio::spawn(feed_engine(
        events,
        engine,
        pty.clone(),
        control_rx,
        exited.clone(),
        log.clone(),
        FeedContext {
            table: shared,
            port: Arc::clone(&port),
            session: session.clone(),
            owner: owner.to_string(),
            sink: Arc::clone(&sink),
            program: Arc::clone(&program),
            starts_shell: request.program.is_none(),
        },
    ));
    table.entries.insert(
        session.clone(),
        Entry {
            owner: owner.to_string(),
            root,
            creation: creation.clone(),
            plugin,
            pty,
            size,
            port: Some(port),
            control: Some(control_tx),
            exited,
            ended: false,
            shown_by: None,
            sink,
            program,
            starts_shell: request.program.is_none(),
        },
    );
    table
        .by_creation
        .insert((owner.to_string(), creation.clone()), session.clone());
    Ok(json!({"session": session, "creation": creation}))
}

async fn resize(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    owner: &str,
    body: &Value,
    log: &Events,
    output: &OutputSink,
) -> Result<Value, String> {
    let session = session_member(body)?;
    let size = parse_size(body.get("size"))?;
    let size_value = body
        .get("size")
        .cloned()
        .ok_or_else(|| "size width, height and scale must be positive numbers".to_string())?;
    let mut table = sessions.lock().await;
    let entry = open_entry(&mut table, &session, log).await?;
    if entry.owner != owner {
        return Err(format!("surface {owner} does not own session {session}"));
    }
    *entry.sink.lock().await = Some(output.clone());
    entry.size = size;
    if let Some((_, shown)) = entry.shown_by.clone() {
        // The attached surface holds the grid: it recomputes the grid from the size with its own metrics.
        drop(table);
        let sent = shown.send(session_resize(size)).await;
        if sent.is_err() {
            let mut table = sessions.lock().await;
            if let Some(entry) = table.entries.get_mut(&session) {
                entry.shown_by = None;
            }
        }
        return Ok(json!({"session": session, "size": size_value}));
    }
    let metrics = default_cell_metrics(size.scale)?;
    let (columns, rows) = grid(size, metrics.cell_width, metrics.cell_height)?;
    let port = entry
        .port
        .clone()
        .ok_or_else(|| format!("session {session} is not open"))?;
    port.resize(&entry.pty, columns, rows)
        .await
        .map_err(|error| format!("resize failed: {error}"))?;
    let control = entry
        .control
        .as_ref()
        .ok_or_else(|| format!("session {session} is not open"))?;
    control
        .send(Control::Resize(columns, rows))
        .map_err(|_| format!("resize failed: the task of session {session} has ended"))?;
    Ok(json!({"session": session, "size": size_value}))
}

async fn end(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    owner: &str,
    body: &Value,
    log: &Events,
    output: &OutputSink,
) -> Result<Value, String> {
    let session = session_member(body)?;
    let mut table = sessions.lock().await;
    let Some(entry) = table.entries.get_mut(&session) else {
        return Err(format!("session {session} is not open"));
    };
    if entry.owner != owner {
        return Err(format!("surface {owner} does not own session {session}"));
    }
    *entry.sink.lock().await = Some(output.clone());
    settle(entry, log).await;
    if entry.ended {
        return Ok(json!({"session": session, "ended": true}));
    }
    let port = entry
        .port
        .clone()
        .ok_or_else(|| format!("session {session} is not open"))?;
    port.close(&entry.pty)
        .await
        .map_err(|error| format!("end failed: {error}"))?;
    entry.ended = true;
    entry.port = None;
    entry.shown_by = None;
    // Dropping the sender ends the task of the session.
    entry.control = None;
    Ok(json!({"session": session, "ended": true}))
}

/// Answers an `input` (`paste` false) or a `paste` of the owner: the task judges the text as the input of a surface does
/// (guard.rs), the bytes are written to the PTY, and the reply is sent when the write completes. A refusal of the guard
/// writes nothing and answers its error body.
async fn write_text(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    owner: &str,
    cids: &[String],
    body: &Value,
    paste: bool,
    log: &Events,
    output: &OutputSink,
) -> Result<Value, String> {
    let operation = if paste { "paste" } else { "input" };
    let session = session_member(body)?;
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{operation} requires text string"))?;
    let (control, port, pty) = {
        let mut table = sessions.lock().await;
        let entry = open_entry(&mut table, &session, log).await?;
        if entry.owner != owner {
            return Err(format!("surface {owner} does not own session {session}"));
        }
        *entry.sink.lock().await = Some(output.clone());
        let control = entry
            .control
            .clone()
            .ok_or_else(|| format!("session {session} is not open"))?;
        let port = entry
            .port
            .clone()
            .ok_or_else(|| format!("session {session} is not open"))?;
        (control, port, entry.pty.clone())
    };
    let (reply, judged) = oneshot::channel();
    control
        .send(Control::Write {
            text: text.to_string(),
            paste,
            cids: cids.to_vec(),
            reply,
        })
        .map_err(|_| format!("session {session} is not open"))?;
    let bytes = match judged
        .await
        .map_err(|_| format!("session {session} is not open"))?
    {
        Ok(bytes) => bytes,
        Err(refusal) => return Ok(refusal),
    };
    if bytes.is_empty() {
        return Ok(json!({"session": session}));
    }
    let completion = port
        .write(&pty, crate::pty::WriteOrigin::Person, Arc::from(bytes))
        .await
        .map_err(|error| format!("{operation} failed: {error}"))?;
    completion
        .await
        .map_err(|_| WRITE_LOST.to_string())?
        .map_err(|error| format!("{operation} failed: {error}"))?;
    Ok(json!({"session": session}))
}

/// The result of a write whose session ended before the write completed.
const WRITE_LOST: &str = "the session ended before the write completed";

/// The range of a `read` with `from` and `count`, bounded as the `from` and `count` of `screen.read`: both together, `from`
/// an integer of 0 or more and `count` an integer from 1 to SCREEN_ROWS_COUNT_MAX; None without both.
fn read_range(body: &Value) -> Result<Option<(u64, u32)>, String> {
    match (body.get("from"), body.get("count")) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err("read count requires from".to_string()),
        (Some(from), count) => {
            let from = from.as_u64();
            let count = count
                .and_then(Value::as_u64)
                // silent: a count that does not fit is refused by the error that follows.
                .and_then(|count| u32::try_from(count).ok())
                .filter(|count| (1..=SCREEN_ROWS_COUNT_MAX).contains(count));
            match (from, count) {
                (Some(from), Some(count)) => Ok(Some((from, count))),
                _ => Err(format!(
                    "read requires an integer from and a count from 1 to {SCREEN_ROWS_COUNT_MAX}"
                )),
            }
        }
    }
}

/// Answers a `read` of the owner: the absolute line number `from` of the first visible row, the visible rows of the screen
/// as text and the cursor, zero-based, as `{row, column}`, or
/// with `from` and `count` the retained rows `{from, count, rows}` of the primary screen as text.
async fn read(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    owner: &str,
    body: &Value,
    log: &Events,
) -> Result<Value, String> {
    let session = session_member(body)?;
    let range = read_range(body)?;
    let control = {
        let mut table = sessions.lock().await;
        let entry = open_entry(&mut table, &session, log).await?;
        if entry.owner != owner {
            return Err(format!("surface {owner} does not own session {session}"));
        }
        entry
            .control
            .clone()
            .ok_or_else(|| format!("session {session} is not open"))?
    };
    let (reply, answer) = oneshot::channel();
    control
        .send(Control::Read { range, reply })
        .map_err(|_| format!("session {session} is not open"))?;
    answer
        .await
        .map_err(|_| format!("session {session} is not open"))?
}

/// Attaches a surface to an open session. The surface takes the output, the grid and the input of the session; the grid is
/// the size of the session in the cells of `metrics`, the cell size of the surface, or of the default font when the surface
/// has none. `surface` is the name of the surface and `commands` its command channel, which receives the sizes of the owner.
/// The port of the attachment applies each resize of the PTY to the engine of the session as well.
pub(crate) async fn attach(
    sessions: &tokio::sync::Mutex<OwnedSessions>,
    session: &str,
    surface: &str,
    metrics: Option<(f32, f32)>,
    commands: mpsc::Sender<Dispatch>,
    log: &Events,
) -> Result<Attached, String> {
    let mut table = sessions.lock().await;
    let entry = open_entry(&mut table, session, log).await?;
    if let Some((shown, channel)) = &entry.shown_by
        && !channel.is_closed()
    {
        return Err(format!("session {session} is shown by surface {shown}"));
    }
    let (cell_width, cell_height) = match metrics {
        Some(metrics) => metrics,
        None => {
            let metrics = default_cell_metrics(entry.size.scale)?;
            (metrics.cell_width, metrics.cell_height)
        }
    };
    let (columns, rows) = grid(entry.size, cell_width, cell_height)?;
    let port = entry
        .port
        .clone()
        .ok_or_else(|| format!("session {session} is not open"))?;
    port.resize(&entry.pty, columns, rows)
        .await
        .map_err(|error| format!("attach failed: {error}"))?;
    let control = entry
        .control
        .clone()
        .ok_or_else(|| format!("session {session} is not open"))?;
    let (forward, events) = crate::pty::output_channel();
    let (reply, answer) = oneshot::channel();
    control
        .send(Control::Attach {
            columns,
            rows,
            forward,
            reply,
        })
        .map_err(|_| format!("session {session} is not open"))?;
    answer
        .await
        .map_err(|_| format!("session {session} is not open"))??;
    entry.shown_by = Some((surface.to_string(), commands));
    Ok(Attached {
        port: Arc::new(AttachedPort {
            port,
            control: control.clone(),
        }),
        pty: entry.pty.clone(),
        columns,
        rows,
        events,
        starts_shell: entry.starts_shell,
    })
}

/// The port of an attached surface: the port of the session, whose resize also sets the grid of the engine of the session,
/// so the engine of the session holds the grid of the PTY while a surface sets it.
struct AttachedPort {
    port: Arc<dyn SessionPort>,
    control: mpsc::UnboundedSender<Control>,
}

#[async_trait]
impl SessionPort for AttachedPort {
    async fn open(&self, request: &ShellRequest, cols: u16, rows: u16) -> Result<String, String> {
        self.port.open(request, cols, rows).await
    }
    async fn write(
        &self,
        session_id: &str,
        origin: crate::pty::WriteOrigin,
        data: Arc<[u8]>,
    ) -> Result<crate::pty::WriteCompletion, String> {
        self.port.write(session_id, origin, data).await
    }
    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        self.port.resize(session_id, cols, rows).await?;
        self.control
            .send(Control::Resize(cols, rows))
            .map_err(|_| format!("the task of session {session_id} has ended"))
    }
    async fn close(&self, session_id: &str) -> Result<(), String> {
        self.port.close(session_id).await
    }
    async fn pty_measurement(
        &self,
        session_id: &str,
    ) -> Result<crate::pty::PtyMeasurement, String> {
        self.port.pty_measurement(session_id).await
    }
    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent> {
        self.port.get_events().await
    }
    fn processes(&self, session_id: &str) -> Result<Processes, String> {
        self.port.processes(session_id)
    }
    fn process_surface(&self, pid: u32) -> Result<Option<crate::pty::SurfaceProcess>, String> {
        self.port.process_surface(pid)
    }
}

/// The plain text of a row of cells, as `screen_text` reads the rows of the viewport.
fn row_text(row: &[crate::protocol::Cell]) -> String {
    let text: String = row
        .iter()
        .filter(|cell| cell.width != 0)
        // default: a cell without a character is blank, as screen_text reads it.
        .map(|cell| cell.ch.as_deref().unwrap_or(" "))
        .collect();
    text.trim_end().to_string()
}

/// The bytes that draw the screen of `engine` in an engine of the same grid that has received no output: the retained rows
/// of the primary screen, the kept rows and the screen of the alternate screen while it is active, the cursor and the modes
/// of the engine (docs/spec/vt-core.md#owned-session-operations). A row is written as its cells with their colors,
/// attributes and links, so the receiving engine holds the cells of the session; the wrap of a row into the next row, the
/// scroll region, the character sets, the tab stops and the attributes of the next output of the program are not part of
/// the cells and are not written.
pub fn screen_bytes(engine: &mut dyn Engine) -> Result<Vec<u8>, String> {
    let screen = engine.screen();
    let modes = engine.modes();
    let mut out = Vec::new();
    let primary = engine.primary_retained();
    let count = u32::try_from(primary.end - primary.start).map_err(|_| {
        format!(
            "the primary screen retains {} rows",
            primary.end - primary.start
        )
    })?;
    let rows = if count == 0 {
        Vec::new()
    } else {
        engine.screen_rows(RowScreen::Primary, primary.start, count)?
    };
    write_rows(&mut out, &rows);
    if screen.alternate.active {
        out.extend_from_slice(b"\x1b[?1049h");
        let kept = screen.alternate.history as u64;
        let count = u32::try_from(kept + u64::from(screen.rows))
            .map_err(|_| format!("the alternate screen keeps {kept} rows"))?;
        let rows = engine.screen_rows(RowScreen::Alternate, screen.alternate.dropped, count)?;
        write_rows(&mut out, &rows);
    }
    let cursor = &screen.cursor;
    out.extend_from_slice(format!("\x1b[{};{}H", cursor.row + 1, cursor.col + 1).as_bytes());
    let shape = match (cursor.shape, cursor.blinking) {
        (CursorShape::Underline, true) => Some(3),
        (CursorShape::Underline, false) => Some(4),
        (CursorShape::Beam, true) => Some(5),
        (CursorShape::Beam, false) => Some(6),
        (CursorShape::Block | CursorShape::HollowBlock, true) => Some(1),
        (CursorShape::Block | CursorShape::HollowBlock, false) => Some(2),
        // A hidden cursor is written as the reset of the visible cursor mode below.
        (CursorShape::Hidden, _) => None,
    };
    if let Some(shape) = shape {
        out.extend_from_slice(format!("\x1b[{shape} q").as_bytes());
    }
    let visible = cursor.visible && cursor.shape != CursorShape::Hidden;
    for (mode, on) in [
        (25, visible),
        (1, modes.app_cursor),
        (2004, modes.bracketed_paste),
        (1000, modes.mouse_click),
        (1002, modes.mouse_drag),
        (1003, modes.mouse_motion),
        (1004, modes.focus_in_out),
        (1005, modes.utf8_mouse),
        (1006, modes.sgr_mouse),
        (1007, modes.alternate_scroll),
    ] {
        out.extend_from_slice(format!("\x1b[?{mode}{}", if on { 'h' } else { 'l' }).as_bytes());
    }
    out.extend_from_slice(if modes.app_keypad { b"\x1b=" } else { b"\x1b>" });
    Ok(out)
}

/// Writes rows from the top of a screen: each row as its cells, the rows separated by a carriage return and a line feed, so
/// that the rows past the last row of the screen move into the history.
fn write_rows(out: &mut Vec<u8>, rows: &[Vec<crate::protocol::Cell>]) {
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        let mut pen: Option<String> = None;
        let mut link: Option<&str> = None;
        for cell in row.iter().filter(|cell| cell.width != 0) {
            let sgr = cell_sgr(cell);
            if pen.as_deref() != Some(sgr.as_str()) {
                out.extend_from_slice(sgr.as_bytes());
                pen = Some(sgr);
            }
            if link != cell.link.as_deref() {
                link = cell.link.as_deref();
                // default: a cell without a link ends the link of the cells before it with an empty URI.
                out.extend_from_slice(format!("\x1b]8;;{}\x1b\\", link.unwrap_or("")).as_bytes());
            }
            // default: a cell without a character is blank, as screen_text reads it.
            out.extend_from_slice(cell.ch.as_deref().unwrap_or(" ").as_bytes());
        }
        if link.is_some() {
            out.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        out.extend_from_slice(b"\x1b[0m");
    }
}

/// The select graphic rendition sequence that sets the colors and attributes of a cell from the default rendition.
fn cell_sgr(cell: &crate::protocol::Cell) -> String {
    let mut sgr = String::from("\x1b[0");
    for (on, code) in [
        (cell.bold, ";1"),
        (cell.dim, ";2"),
        (cell.italic, ";3"),
        (cell.underline, ";4"),
        (cell.inverse, ";7"),
    ] {
        if on {
            sgr.push_str(code);
        }
    }
    if let Some(crate::protocol::Rgb([red, green, blue])) = cell.fg {
        sgr.push_str(&format!(";38;2;{red};{green};{blue}"));
    }
    if let Some(crate::protocol::Rgb([red, green, blue])) = cell.bg {
        sgr.push_str(&format!(";48;2;{red};{green};{blue}"));
    }
    sgr.push('m');
    sgr
}

/// Feeds the output of one PTY session to its engine until the session ends, and forwards it to the attached surface while
/// one is attached. The engine of the session answers the queries of its program while no surface is attached, and its
/// events give the program status of the session; a surface that attaches receives the bytes that draw the screen of the
/// engine (`screen_bytes`) as `DaemonEvent::Replay`, which that surface shows without answering.
async fn feed_engine(
    mut events: mpsc::Receiver<DaemonEvent>,
    mut engine: Box<dyn Engine>,
    pty: String,
    mut control: mpsc::UnboundedReceiver<Control>,
    exited: Arc<AtomicBool>,
    log: Events,
    context: FeedContext,
) {
    let table = Arc::clone(&context.table);
    let mut forward: Option<mpsc::Sender<DaemonEvent>> = None;
    let mut program = ProgramStatusTracker::default();
    let mut line_guard = guard::LineGuard::default();
    let mut shell_state = crate::shell_state::ShellStateTracker::default();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(DaemonEvent::Output { session_id, data }) if session_id == pty => {
                    engine.feed(&data);
                    let events = engine.drain_events();
                    for event in &events {
                        if let EngineEvent::ShellState { marker, params } = event
                            && let Err(reason) = shell_state.apply(&pty, *marker, params)
                        {
                            log.record_error("owned session", format!("session {pty}: {reason}"));
                        }
                    }
                    // The engine of the session answers the queries of its program while no surface shows it, in the order of
                    // their bytes, as the engine of a surface does; an attached surface answers the output that it receives.
                    if !forward.as_ref().is_some_and(|sender| !sender.is_closed()) {
                        answer_program(&events, &context, &pty, &log).await;
                    }
                    follow_program(events, &mut program, &context, &pty, &log).await;
                    if let Some(sender) = &forward
                        && sender.send(DaemonEvent::Output { session_id, data }).await.is_err()
                    {
                        forward = None;
                    }
                }
                Some(DaemonEvent::Exit { session_id }) if session_id == pty => {
                    exited.store(true, Ordering::Release);
                    tokio::spawn(notify_ended(table.clone(), context.session.clone(), log.clone()));
                    if let Some(sender) = &forward {
                        let _ = sender.send(DaemonEvent::Exit { session_id }).await;
                    }
                    break;
                }
                Some(DaemonEvent::Error { session_id, error }) if session_id == pty => {
                    log.record_error("owned session", format!("session {pty}: {error}"));
                    if let Some(sender) = &forward {
                        let _ = sender.send(DaemonEvent::Error { session_id, error }).await;
                    }
                }
                Some(DaemonEvent::InputHeld { session_id, queued, held }) if session_id == pty => {
                    if let Some(sender) = &forward {
                        let _ = sender.send(DaemonEvent::InputHeld { session_id, queued, held }).await;
                    }
                }
                Some(_) => {}
                None => {
                    exited.store(true, Ordering::Release);
                    tokio::spawn(notify_ended(table.clone(), context.session.clone(), log.clone()));
                    break;
                }
            },
            command = control.recv() => match command {
                Some(Control::Resize(columns, rows)) => engine.resize(columns, rows),
                Some(Control::Write { text, paste, cids, reply }) => {
                    let shell_line = guard::follows_shell_line(
                        context.starts_shell,
                        shell_state.current(&pty).state,
                        &engine.modes(),
                    );
                    let judged = judge_input(engine.as_ref(), shell_line, &mut line_guard, &pty, &cids, &text, paste, &log);
                    let _ = reply.send(judged);
                }
                Some(Control::Read { range: None, reply }) => {
                    let screen = engine.screen();
                    // The absolute number of the first visible row in the numbering of the rows of its screen: the kept
                    // rows of the alternate screen while it is active, else the retained rows of the primary screen.
                    let from = if screen.alternate.active {
                        screen.alternate.dropped + screen.alternate.history as u64
                    } else {
                        screen.scrollback.dropped + u64::from(screen.scrollback.history)
                            - u64::from(screen.scrollback.offset)
                    };
                    let body = json!({
                        "from": from,
                        "rows": screen_text(&screen),
                        "cursor": {"row": screen.cursor.row, "column": screen.cursor.col},
                    });
                    let _ = reply.send(Ok(body));
                }
                Some(Control::Read { range: Some((from, count)), reply }) => {
                    let body = engine.screen_rows(RowScreen::Primary, from, count).map(|rows| {
                        let rows: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
                        json!({"from": from, "count": count, "rows": rows})
                    });
                    let _ = reply.send(body);
                }
                Some(Control::Attach { columns, rows, forward: sender, reply }) => {
                    engine.resize(columns, rows);
                    let result = match screen_bytes(engine.as_mut()) {
                        Ok(data) => {
                            let snapshot = DaemonEvent::Replay { session_id: pty.clone(), data };
                            if sender.send(snapshot).await.is_ok() {
                                forward = Some(sender);
                                Ok(())
                            } else {
                                Err("the attached surface ended before the screen of the session".to_string())
                            }
                        }
                        Err(reason) => Err(format!("the screen of session {pty} was not read: {reason}")),
                    };
                    let _ = reply.send(result);
                }
                None => break,
            },
        }
    }
}

/// Sends the event `session.ended` of an owned session to the connection of its owner, with the reason `ended`
/// (core docs/spec/persistent-services.md#owned-sessions). It runs as its own task, so the task of the session never
/// waits for the table while it holds a reply of an attach.
async fn notify_ended(table: Arc<tokio::sync::Mutex<OwnedSessions>>, session: String, log: Events) {
    let target = {
        let table = table.lock().await;
        table
            .entries
            .get(&session)
            .map(|entry| (entry.owner.clone(), Arc::clone(&entry.sink)))
    };
    let Some((owner, sink)) = target else {
        log.record_error(
            "owned session",
            format!("session {session}: the end has no entry"),
        );
        return;
    };
    let sink = sink.lock().await.clone();
    let Some(sink) = sink else {
        log.record_error(
            "owned session",
            format!("session {session}: the owner has no connection for the end"),
        );
        return;
    };
    let line = json!({"surface": owner, "body": {"event": "session.ended", "session": session, "reason": "ended"}});
    if sink.send(line.to_string()).await.is_err() {
        log.record_error(
            "owned session",
            format!("session {session}: the session.ended event was not delivered"),
        );
    }
}

/// What the task of an owned session needs of its entry: the table for the end, the identifiers of the session and its owner,
/// and the connection of the owner for the events.
struct FeedContext {
    table: Arc<tokio::sync::Mutex<OwnedSessions>>,
    /// The port of the PTY session, which takes the answers of the engine to the program.
    port: Arc<dyn SessionPort>,
    session: String,
    owner: String,
    sink: SinkCell,
    /// The last program state of the session, which the task sets before it sends `session.program`.
    program: ProgramCell,
    /// Whether the session started a shell rather than a program (guard::follows_shell_line).
    starts_shell: bool,
}

/// Judges the text of an `input` or a `paste` as the input of a surface judges its own (guard.rs): on a shell line the guard
/// follows the line and refuses a submission it does not allow, and the refusal writes nothing. A paste is encoded with the
/// bracketed-paste mode of the engine; an input is written as given. Returns the bytes to write, or the error body.
#[allow(clippy::too_many_arguments)]
fn judge_input(
    engine: &dyn Engine,
    shell_line: bool,
    line: &mut guard::LineGuard,
    session: &str,
    cids: &[String],
    text: &str,
    paste: bool,
    log: &Events,
) -> Result<Vec<u8>, Value> {
    if !shell_line {
        *line = guard::LineGuard::default();
    } else {
        let inputs = if paste {
            vec![LineInput::Text(text)]
        } else {
            line_inputs_of_text(text)
        };
        if let Some((rule, reason)) = guard::judge_transaction(log, line, session, cids, &inputs) {
            return Err(json!({"error": "guardRefused", "rule": rule, "reason": reason}));
        }
    }
    if paste {
        encoding::encode_paste(text, &engine.modes())
            .map_err(|reason| json!({"error": "invalidParams", "reason": reason}))
    } else {
        Ok(text.as_bytes().to_vec())
    }
}

/// The keys that a text written as input gives the command line: a carriage return or line feed submits, a delete control
/// deletes, Ctrl-C and Ctrl-U discard, another control character moves or recalls the line (`Other`), and each run of other
/// characters is text.
fn line_inputs_of_text(text: &str) -> Vec<LineInput<'_>> {
    let mut inputs = Vec::new();
    let mut run_start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        let key = match ch {
            '\r' | '\n' => LineInput::Enter,
            '\u{7f}' | '\u{8}' => LineInput::Backspace,
            '\u{3}' | '\u{15}' => LineInput::Discard,
            ch if ch.is_control() => LineInput::Other,
            _ => {
                run_start.get_or_insert(index);
                continue;
            }
        };
        if let Some(start) = run_start.take() {
            inputs.push(LineInput::Text(&text[start..index]));
        }
        inputs.push(key);
    }
    if let Some(start) = run_start {
        inputs.push(LineInput::Text(&text[start..]));
    }
    inputs
}

/// Writes the answers of the engine to the queries of one output to the program, in the order of the output. A write that
/// fails is reported in the log.
async fn answer_program(events: &[EngineEvent], context: &FeedContext, pty: &str, log: &Events) {
    for event in events {
        let EngineEvent::PtyWrite(bytes) = event else {
            continue;
        };
        let completion = match context
            .port
            .write(
                pty,
                crate::pty::WriteOrigin::Engine,
                Arc::from(bytes.as_slice()),
            )
            .await
        {
            Ok(completion) => completion,
            Err(error) => {
                log.record_error(
                    "owned session",
                    format!("session {pty}: the answer to the program failed: {error}"),
                );
                continue;
            }
        };
        let log = log.clone();
        let pty = pty.to_string();
        tokio::spawn(async move {
            match completion.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log.record_error(
                    "owned session",
                    format!("session {pty}: the answer to the program failed: {error}"),
                ),
                Err(_) => log.record_error("owned session", format!("session {pty}: {WRITE_LOST}")),
            }
        });
    }
}

/// Applies the engine events of one output to the program status of the session. When the records change, the state that
/// the records give is sent to the owner as `session.program` if it differs from the state sent last (core
/// docs/spec/persistent-services.md#owned-sessions), and is kept for the `sessions` answer. The status is read from the records only, so a session whose records
/// give no state sends nothing.
async fn follow_program(
    events: Vec<EngineEvent>,
    program: &mut ProgramStatusTracker,
    context: &FeedContext,
    pty: &str,
    log: &Events,
) {
    let FeedContext {
        session,
        owner,
        sink,
        program: state_sent,
        ..
    } = context;
    for event in events {
        let change = match &event {
            EngineEvent::ShellState { marker, params } => program.mark(pty, *marker, params),
            other => program.apply(pty, other),
        };
        let Some(change) = change else {
            continue;
        };
        for id in &change.evicted {
            log.emit_for(
                Level::Warn,
                "sidecar.vt.program.status.evicted",
                Subject::session(pty, &[]),
                &json!({"id": id, "records": MAX_RECORDS}),
            );
        }
        log.emit_for(
            Level::Info,
            "sidecar.vt.program.status",
            Subject::session(pty, &[]),
            &change.current.to_log_json(),
        );
        let Some(state) = program_state(&change.current.records) else {
            continue;
        };
        {
            let mut sent = state_sent
                .lock()
                // default: the state is one value that each holder replaces whole, so a panic of another holder leaves it
                // consistent.
                .unwrap_or_else(|error| error.into_inner());
            if *sent == Some(state) {
                continue;
            }
            *sent = Some(state);
        }
        let line = json!({"surface": owner, "body": {"event": "session.program", "session": session, "state": state}});
        let connection = sink.lock().await.clone();
        let Some(connection) = connection else {
            log.record_error(
                "owned session",
                format!("session {session}: the owner has no connection for the program state"),
            );
            continue;
        };
        if connection.send(line.to_string()).await.is_err() {
            log.record_error(
                "owned session",
                format!("session {session}: the session.program event was not delivered"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: f64, height: f64, scale: f64) -> Size {
        Size {
            width,
            height,
            scale,
        }
    }

    #[test]
    fn grid_counts_whole_cells_of_the_device_size() {
        // A cell of 8 by 16 device pixels: 80 by 48 device pixels hold 10 by 3 cells, and the scale of 2 doubles the pixels.
        assert_eq!(grid(size(80.0, 48.0, 1.0), 8.0, 16.0), Ok((10, 3)));
        assert_eq!(grid(size(40.0, 24.0, 2.0), 8.0, 16.0), Ok((10, 3)));
        // A partial cell does not count.
        assert_eq!(grid(size(87.0, 55.0, 1.0), 8.0, 16.0), Ok((10, 3)));
    }

    #[test]
    fn grid_refuses_a_size_with_no_whole_cell() {
        assert_eq!(
            grid(size(7.0, 48.0, 1.0), 8.0, 16.0),
            Err("size is smaller than one cell".to_string())
        );
        assert_eq!(
            grid(size(80.0, 15.0, 1.0), 8.0, 16.0),
            Err("size is smaller than one cell".to_string())
        );
    }

    #[test]
    fn grid_refuses_a_grid_beyond_the_terminal_range() {
        assert_eq!(
            grid(size(1e9, 48.0, 1.0), 1.0, 16.0),
            Err("size is larger than the terminal grid".to_string())
        );
    }

    #[test]
    fn default_cell_metrics_fit_the_grid_bounds() {
        // The grid uses the real device metrics: the cells fit the device size, and one more cell would not.
        let metrics = default_cell_metrics(2.0).expect("the default font has cell metrics");
        let (columns, rows) = grid(
            size(400.0, 200.0, 2.0),
            metrics.cell_width,
            metrics.cell_height,
        )
        .expect("a 400 by 200 point card holds cells");
        let device_width = 400.0 * 2.0;
        let device_height = 200.0 * 2.0;
        assert!(f64::from(columns) * f64::from(metrics.cell_width) <= device_width);
        assert!((f64::from(columns) + 1.0) * f64::from(metrics.cell_width) > device_width);
        assert!(f64::from(rows) * f64::from(metrics.cell_height) <= device_height);
        assert!((f64::from(rows) + 1.0) * f64::from(metrics.cell_height) > device_height);
    }

    #[test]
    fn parse_size_refuses_a_member_that_is_not_positive() {
        let zero = json!({"width": 400, "height": 200, "scale": 0});
        assert_eq!(
            parse_size(Some(&zero)),
            Err("size width, height and scale must be positive numbers".to_string())
        );
        let missing = json!({"width": 400, "height": 200});
        assert_eq!(
            parse_size(Some(&missing)),
            Err("size width, height and scale must be positive numbers".to_string())
        );
    }
}
