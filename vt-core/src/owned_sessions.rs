// The owned sessions of the persistent service (core docs/spec/persistent-services.md#owned-sessions, sidecar
// docs/spec/vt-core.md#owned-session-operations). An owned session is a PTY session of its owner surface that no surface
// draws. Its engine is fed with the output of the PTY, so a later attachment starts from the screen that the session holds.
use crate::events::Events;
use crate::platform::{default_font, metrics_for};
use crate::protocol::{
    DEFAULT_FONT_SIZE, DaemonEvent, Engine, SessionPort, ShellRequest, open_launch,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

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

/// The device pixel cell size of the default font at the backing scale of a size.
fn cell_metrics(scale: f64) -> Result<crate::platform::Metrics, String> {
    metrics_for(&default_font(), DEFAULT_FONT_SIZE, scale as f32)
}

/// The commands that the task of a session receives from the service.
enum Control {
    Resize(u16, u16),
}

/// One owned session. After its end, the port and the control channel are released and only the record stays, so a
/// repeated request still gets its answer.
struct Entry {
    owner: String,
    pty: String,
    port: Option<Arc<dyn SessionPort>>,
    control: Option<mpsc::UnboundedSender<Control>>,
    ended: bool,
}

/// The owned sessions of one persistent service. Its entries outlive each connection, so a restored owner reattaches.
#[derive(Default)]
pub struct OwnedSessions {
    entries: HashMap<String, Entry>,
    by_creation: HashMap<(String, String), String>,
    next: u64,
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
pub async fn answer(
    sessions: &tokio::sync::Mutex<OwnedSessions>,
    owner: &str,
    body: &Value,
    engine_factory: &EngineFactory,
    session_port_factory: &SessionPortFactory,
    log: &Events,
) -> Value {
    let result = match body.get("operation").and_then(Value::as_str) {
        Some("open") => open(sessions, owner, body, engine_factory, session_port_factory).await,
        Some("resize") => resize(sessions, owner, body).await,
        Some("end") => end(sessions, owner, body).await,
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

async fn open(
    sessions: &tokio::sync::Mutex<OwnedSessions>,
    owner: &str,
    body: &Value,
    engine_factory: &EngineFactory,
    session_port_factory: &SessionPortFactory,
) -> Result<Value, String> {
    let creation = body
        .get("session")
        .and_then(|session| session.get("creation"))
        .and_then(Value::as_str)
        .filter(|creation| !creation.is_empty())
        .ok_or_else(|| "open session requires a non-empty creation".to_string())?
        .to_string();
    let size = parse_size(body.get("size"))?;
    let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
    let request = launch_request(owner, &params)?;
    let mut table = sessions.lock().await;
    if let Some(session) = table
        .by_creation
        .get(&(owner.to_string(), creation.clone()))
    {
        return Ok(json!({"session": session, "creation": creation}));
    }
    let metrics = cell_metrics(size.scale)?;
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
    tokio::spawn(feed_engine(events, engine, pty.clone(), control_rx));
    table.next += 1;
    let session = format!("owned-{}", table.next);
    table.entries.insert(
        session.clone(),
        Entry {
            owner: owner.to_string(),
            pty,
            port: Some(port),
            control: Some(control_tx),
            ended: false,
        },
    );
    table
        .by_creation
        .insert((owner.to_string(), creation.clone()), session.clone());
    Ok(json!({"session": session, "creation": creation}))
}

/// Checks that a request names a session that the owner holds and that is open. A session of another owner is refused.
fn open_session<'a>(
    table: &'a mut OwnedSessions,
    owner: &str,
    session: &str,
) -> Result<&'a mut Entry, String> {
    let entry = table
        .entries
        .get_mut(session)
        .ok_or_else(|| format!("session {session} is not open"))?;
    if entry.owner != owner {
        return Err(format!("surface {owner} does not own session {session}"));
    }
    if entry.ended {
        return Err(format!("session {session} is not open"));
    }
    Ok(entry)
}

async fn resize(
    sessions: &tokio::sync::Mutex<OwnedSessions>,
    owner: &str,
    body: &Value,
) -> Result<Value, String> {
    let session = session_member(body)?;
    let size_value = body.get("size").cloned().unwrap_or(Value::Null);
    let size = parse_size(body.get("size"))?;
    let mut table = sessions.lock().await;
    let entry = open_session(&mut table, owner, &session)?;
    let metrics = cell_metrics(size.scale)?;
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
    sessions: &tokio::sync::Mutex<OwnedSessions>,
    owner: &str,
    body: &Value,
) -> Result<Value, String> {
    let session = session_member(body)?;
    let mut table = sessions.lock().await;
    let Some(entry) = table.entries.get_mut(&session) else {
        return Err(format!("session {session} is not open"));
    };
    if entry.owner != owner {
        return Err(format!("surface {owner} does not own session {session}"));
    }
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
    // Dropping the sender ends the task of the session.
    entry.control = None;
    Ok(json!({"session": session, "ended": true}))
}

/// Feeds the output of one PTY session to its engine until the session ends. The engine events of the session are
/// discarded, because no surface draws the session; the owned-session specification states this.
async fn feed_engine(
    mut events: mpsc::Receiver<DaemonEvent>,
    mut engine: Box<dyn Engine>,
    pty: String,
    mut control: mpsc::UnboundedReceiver<Control>,
) {
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(DaemonEvent::Output { session_id, data }) if session_id == pty => {
                    engine.feed(&data);
                    let _ = engine.drain_events();
                }
                Some(_) => {}
                None => break,
            },
            command = control.recv() => match command {
                Some(Control::Resize(columns, rows)) => engine.resize(columns, rows),
                None => break,
            },
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
    fn cell_metrics_of_the_default_font_fit_the_grid_bounds() {
        // The grid uses the real device metrics: the cells fit the device size, and one more cell would not.
        let metrics = cell_metrics(2.0).expect("the default font has cell metrics");
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
