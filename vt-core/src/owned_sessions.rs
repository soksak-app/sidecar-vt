// The owned sessions of the persistent service (core docs/spec/persistent-services.md#owned-sessions, sidecar
// docs/spec/vt-core.md#owned-session-operations). An owned session is a PTY session of its owner surface. Its output feeds an
// engine of its own and a buffer of the latest output, so a tab that attaches replays the screen the session held. While a
// tab shows the session, the surface of the tab draws it and its configure sets the grid.
use crate::events::Events;
use crate::platform::{Metrics, default_font, metrics_for};
use crate::protocol::{
    DEFAULT_FONT_SIZE, DaemonEvent, Dispatch, Engine, OutputSink, SessionPort, ShellRequest,
    open_launch, session_resize,
};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{mpsc, oneshot};

/// The factory of the engine of an owned session.
pub type EngineFactory = Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>;
/// The factory of the session port of an owned session.
pub type SessionPortFactory = Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>;

/// The bytes of the latest output that an owned session keeps for a replay at an attach. When the buffer is full, the oldest
/// bytes are dropped and the drop is written to the service log.
pub const REPLAY_LIMIT: usize = 1024 * 1024;

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

/// The commands that the task of a session receives from the service.
enum Control {
    /// The grid of the engine of the session, while no surface shows it.
    Resize(u16, u16),
    /// The attachment of a surface: the engine takes the grid, the buffer replays into `forward`, and later output goes to
    /// `forward` until the surface ends.
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
    /// The connection of the owner that sent the last request of the session: the event of its end goes there.
    sink: Option<OutputSink>,
}

/// The owned sessions of one persistent service. Its entries outlive each connection, so a restored owner reattaches.
#[derive(Default)]
pub struct OwnedSessions {
    entries: HashMap<String, Entry>,
    by_creation: HashMap<(String, String), String>,
    next: u64,
}

/// The result of an attachment: the PTY session, its grid, and the events that the attached surface receives.
pub(crate) struct Attached {
    /// The port that opened the PTY session: the session port keeps the sessions of its own port only.
    pub port: Arc<dyn SessionPort>,
    pub pty: String,
    pub columns: u16,
    pub rows: u16,
    pub events: mpsc::Receiver<DaemonEvent>,
}

/// Whether a request body is an owned-session request: an `open` with an object `session`, or a `resize` or `end`.
pub fn is_request(body: &Value) -> bool {
    match body.get("operation").and_then(Value::as_str) {
        Some("open") => body.get("session").is_some_and(Value::is_object),
        Some("resize" | "end") => body.get("session").is_some(),
        _ => false,
    }
}

/// Answers an owned-session request of `owner` and returns the reply body.
pub(crate) async fn answer(
    sessions: &Arc<tokio::sync::Mutex<OwnedSessions>>,
    owner: &str,
    body: &Value,
    engine_factory: &EngineFactory,
    session_port_factory: &SessionPortFactory,
    log: &Events,
    output: &OutputSink,
) -> Value {
    let result = match body.get("operation").and_then(Value::as_str) {
        Some("open") => {
            open(
                sessions,
                owner,
                body,
                engine_factory,
                session_port_factory,
                log,
                output,
            )
            .await
        }
        Some("resize") => resize(sessions, owner, body, log, output).await,
        Some("end") => end(sessions, owner, body, log, output).await,
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
    owner: &str,
    body: &Value,
    engine_factory: &EngineFactory,
    session_port_factory: &SessionPortFactory,
    log: &Events,
    output: &OutputSink,
) -> Result<Value, String> {
    let creation = body
        .get("session")
        .and_then(|session| session.get("creation"))
        .and_then(Value::as_str)
        .filter(|creation| !creation.is_empty())
        .ok_or_else(|| "open session requires a non-empty creation".to_string())?
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
            entry.sink = Some(output.clone());
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
    let session = format!("owned-{}", table.next + 1);
    tokio::spawn(feed_engine(
        events,
        engine,
        pty.clone(),
        control_rx,
        exited.clone(),
        log.clone(),
        (shared, session.clone()),
    ));
    table.next += 1;
    table.entries.insert(
        session.clone(),
        Entry {
            owner: owner.to_string(),
            pty,
            size,
            port: Some(port),
            control: Some(control_tx),
            exited,
            ended: false,
            shown_by: None,
            sink: Some(output.clone()),
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
    entry.sink = Some(output.clone());
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
    entry.sink = Some(output.clone());
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

/// Attaches a surface to an open session. The surface takes the output, the grid of the default font and the input of the
/// session; `surface` is the name of the surface and `commands` its command channel, which receives the sizes of the owner.
pub(crate) async fn attach(
    sessions: &tokio::sync::Mutex<OwnedSessions>,
    session: &str,
    surface: &str,
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
    let metrics = default_cell_metrics(entry.size.scale)?;
    let (columns, rows) = grid(entry.size, metrics.cell_width, metrics.cell_height)?;
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
        port,
        pty: entry.pty.clone(),
        columns,
        rows,
        events,
    })
}

/// Feeds the output of one PTY session to its engine and to the replay buffer until the session ends, and forwards it to
/// the attached surface while one is attached. The engine events of the session are discarded: the engine of the session
/// is used only for the screen that a replay of the buffer restores in the surface.
async fn feed_engine(
    mut events: mpsc::Receiver<DaemonEvent>,
    mut engine: Box<dyn Engine>,
    pty: String,
    mut control: mpsc::UnboundedReceiver<Control>,
    exited: Arc<AtomicBool>,
    log: Events,
    owner_of: (Arc<tokio::sync::Mutex<OwnedSessions>>, String),
) {
    let (table, session) = owner_of;
    let mut replay: VecDeque<u8> = VecDeque::new();
    let mut forward: Option<mpsc::Sender<DaemonEvent>> = None;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(DaemonEvent::Output { session_id, data }) if session_id == pty => {
                    engine.feed(&data);
                    let _ = engine.drain_events();
                    replay.extend(data.iter().copied());
                    let excess = replay.len().saturating_sub(REPLAY_LIMIT);
                    if excess > 0 {
                        replay.drain(..excess);
                        log.record_info("owned session", format!("session {pty}: the replay buffer dropped {excess} bytes"));
                    }
                    if let Some(sender) = &forward
                        && sender.send(DaemonEvent::Output { session_id, data }).await.is_err()
                    {
                        forward = None;
                    }
                }
                Some(DaemonEvent::Exit { session_id }) if session_id == pty => {
                    exited.store(true, Ordering::Release);
                    tokio::spawn(notify_ended(table.clone(), session.clone(), log.clone()));
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
                    tokio::spawn(notify_ended(table.clone(), session.clone(), log.clone()));
                    break;
                }
            },
            command = control.recv() => match command {
                Some(Control::Resize(columns, rows)) => engine.resize(columns, rows),
                Some(Control::Attach { columns, rows, forward: sender, reply }) => {
                    engine.resize(columns, rows);
                    let snapshot = DaemonEvent::Output {
                        session_id: pty.clone(),
                        data: replay.iter().copied().collect(),
                    };
                    let result = if sender.send(snapshot).await.is_ok() {
                        forward = Some(sender);
                        Ok(())
                    } else {
                        Err("the attached surface ended before the replay".to_string())
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
            .map(|entry| (entry.owner.clone(), entry.sink.clone()))
    };
    let Some((owner, sink)) = target else {
        log.record_error(
            "owned session",
            format!("session {session}: the end has no entry"),
        );
        return;
    };
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
