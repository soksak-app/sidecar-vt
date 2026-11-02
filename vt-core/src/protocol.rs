use crate::encoding::{self, Key};
use crate::events::{Events, Level, Subject};
use crate::inline_image::{Dimension, InlineImageCommand};
use crate::policy::Class;
use crate::screen_stream::{ScreenCells, ScreenOrder, ScreenStream, SentScreen, screen_cells};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, MissedTickBehavior};
use unicode_segmentation::UnicodeSegmentation;

/// A request of a client while the service handles it: what a reply and the request event need. The
/// replies of the request carry its `cids` beside `surface` and `body`, and each reply is the event
/// `sidecar.vt.reply` in the result form of core (docs/spec/logging.md#result-form).
pub(crate) struct Request {
    cids: Vec<String>,
    operation: String,
    surface: Option<String>,
    started: std::time::Instant,
    log: Events,
    /// The wait that the request is in, as a `RequestStage` number.
    stage: std::sync::atomic::AtomicU8,
}

/// The wait that a request in progress is in. The event `sidecar.vt.request_waiting` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum RequestStage {
    /// The input loop handles the request.
    InputLoop,
    /// The input loop waits for room in the queue of the surface actor.
    SurfaceQueue,
    /// The request is in the queue of the surface actor.
    ActorQueue,
    /// The surface actor handles the request.
    Actor,
    /// A reply of the request waits for room in the output queue of the connection.
    OutputQueue,
    /// The request was answered.
    Answered,
}

impl RequestStage {
    const ALL: [RequestStage; 6] = [
        RequestStage::InputLoop,
        RequestStage::SurfaceQueue,
        RequestStage::ActorQueue,
        RequestStage::Actor,
        RequestStage::OutputQueue,
        RequestStage::Answered,
    ];

    fn name(self) -> &'static str {
        match self {
            RequestStage::InputLoop => "input loop",
            RequestStage::SurfaceQueue => "surface queue",
            RequestStage::ActorQueue => "actor queue",
            RequestStage::Actor => "actor",
            RequestStage::OutputQueue => "output queue",
            RequestStage::Answered => "answered",
        }
    }
}

/// A request still in progress this long after it arrived is the error event
/// `sidecar.vt.request_waiting`. The host waits 10 seconds for the answer to a `retain` and to an
/// image detach (core docs/spec/sidecars.md), so a request in progress at that time has already
/// failed there.
const REQUEST_WAIT_BOUND: Duration = Duration::from_secs(10);

/// The error of a reply line, as `<code>: <text>`, or None for an answer. `-32603` is the internal
/// code of the entrance (docs/spec/logging.md#result-form). The text is the member `error` of the
/// body, or of the line when it has no body, with the member `reason` after it; it is cut to 1,024
/// bytes.
fn reply_error(line: &Value) -> Option<String> {
    let holder = match line.get("body") {
        Some(body) if body.is_object() => body,
        _ => line,
    };
    let error = match holder.get("error")? {
        Value::Null => return None,
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let text = match holder.get("reason") {
        Some(Value::String(reason)) => format!("{error}: {reason}"),
        _ => error,
    };
    Some(format!("-32603: {}", crate::events::head(&text, 1024)))
}

impl Request {
    fn enter(&self, stage: RequestStage) {
        self.stage.store(stage as u8, Ordering::Release);
    }

    fn stage(&self) -> RequestStage {
        RequestStage::ALL[usize::from(self.stage.load(Ordering::Acquire))]
    }

    /// Writes the `cids` of the request into the reply line and records the reply.
    fn answer(&self, line: &mut Value) {
        let error = reply_error(line);
        if !self.cids.is_empty()
            && let Some(object) = line.as_object_mut()
        {
            object.insert("cids".to_string(), json!(self.cids));
        }
        let mut fields = json!({
            "operation": self.operation,
            "ok": error.is_none(),
            "micros": self.started.elapsed().as_micros() as u64,
        });
        if let Some(error) = &error {
            fields["error"] = json!(error);
        }
        let subject = Subject {
            surface: line
                .get("surface")
                .and_then(Value::as_str)
                .or(self.surface.as_deref()),
            session: None,
            cids: &self.cids,
        };
        let level = if error.is_none() {
            Level::Info
        } else {
            Level::Warn
        };
        self.log
            .emit_for(level, "sidecar.vt.reply", subject, &fields);
    }
}

/// A command for a surface task with the request that caused it; the commands that the service
/// sends on its own carry none.
struct Dispatch {
    command: SurfaceCommand,
    request: Option<Arc<Request>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ImageDetach {
    request: String,
    name: String,
    generation: u64,
}

struct ImageDetachReplay {
    surface: String,
    detach: ImageDetach,
    response: Value,
    owner_revision: u64,
}

#[derive(Default)]
struct ImageDetachRequestLedger {
    latest: tokio::sync::Mutex<Option<(String, ImageDetach)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageDetachRequestCheck {
    New,
    Replay,
    Mismatch,
}

impl ImageDetachRequestLedger {
    async fn check(&self, surface: &str, detach: &ImageDetach) -> ImageDetachRequestCheck {
        let mut latest = self.latest.lock().await;
        if let Some((latest_surface, latest_detach)) = latest.as_ref()
            && latest_detach.request == detach.request
        {
            return if latest_surface == surface && latest_detach == detach {
                ImageDetachRequestCheck::Replay
            } else {
                ImageDetachRequestCheck::Mismatch
            };
        }
        *latest = Some((surface.to_string(), detach.clone()));
        ImageDetachRequestCheck::New
    }
}

impl Dispatch {
    fn internal(command: SurfaceCommand) -> Self {
        Self {
            command,
            request: None,
        }
    }

    fn of(command: SurfaceCommand, request: &Arc<Request>) -> Self {
        Self {
            command,
            request: Some(Arc::clone(request)),
        }
    }
}

/// The sender of a surface task for the commands of one request.
struct RequestSender {
    tx: mpsc::Sender<Dispatch>,
    request: Arc<Request>,
}

impl RequestSender {
    async fn send(
        &self,
        command: SurfaceCommand,
    ) -> Result<(), Box<mpsc::error::SendError<Dispatch>>> {
        self.request.enter(RequestStage::SurfaceQueue);
        self.tx
            .send(Dispatch::of(command, &self.request))
            .await
            .map_err(Box::new)?;
        self.request.enter(RequestStage::ActorQueue);
        Ok(())
    }
}

/// Watches the requests of one connection in the order they arrived, and records each request that
/// is still in progress `REQUEST_WAIT_BOUND` after it arrived as the error event
/// `sidecar.vt.request_waiting`. A request is in progress while the input loop, a surface actor or
/// a reply sink holds it. A request that does not end produces no event, so the watch sleeps until
/// the bound of the oldest request. It ends when the input loop drops the sender of `arrived`.
async fn watch_requests(
    mut arrived: mpsc::UnboundedReceiver<std::sync::Weak<Request>>,
    connection: u64,
) {
    let mut waiting = std::collections::VecDeque::<std::sync::Weak<Request>>::new();
    loop {
        while waiting
            .front()
            .is_some_and(|request| request.strong_count() == 0)
        {
            waiting.pop_front();
        }
        let due = waiting
            .front()
            .and_then(std::sync::Weak::upgrade)
            .map(|request| Instant::from_std(request.started) + REQUEST_WAIT_BOUND);
        tokio::select! {
            next = arrived.recv() => match next {
                Some(request) => waiting.push_back(request),
                None => return,
            },
            _ = tokio::time::sleep_until(due.unwrap_or_else(Instant::now)), if due.is_some() => {
                let Some(request) = waiting.pop_front().and_then(|request| request.upgrade()) else {
                    continue;
                };
                let stage = request.stage();
                if stage == RequestStage::Answered {
                    continue;
                }
                request.log.emit_for(
                    Level::Error,
                    "sidecar.vt.request_waiting",
                    Subject {
                        surface: request.surface.as_deref(),
                        session: None,
                        cids: &request.cids,
                    },
                    &json!({
                        "operation": request.operation,
                        "stage": stage.name(),
                        "ms": Instant::now()
                            .saturating_duration_since(Instant::from_std(request.started))
                            .as_millis() as u64,
                        "connection": connection,
                    }),
                );
            }
        }
    }
}

/// The output sender of one connection, and what remains of the transfer images that the connection
/// carried while waiting for the host's reply.
#[derive(Clone)]
struct Connection {
    sender: mpsc::Sender<String>,
    transfers: Arc<TransferStore>,
}

impl Connection {
    fn new(sender: mpsc::Sender<String>, log: Events) -> Self {
        Self {
            sender,
            transfers: TransferStore::new(log),
        }
    }
}

/// A transfer image carried by one connection that still awaits the host's reply after the surface
/// that drew the image released it. The provider does not modify or rewrite the transfer image
/// before the reply (core docs/spec/native-surfaces.md#image-regions). If the image is released
/// before the reply, the host cannot find that IOSurface. It is released when the reply arrives, or
/// when the connection ends and no reply can come.
pub struct TransferStore {
    /// None means the connection has ended.
    images: std::sync::Mutex<Option<Vec<RetainedTransfer>>>,
    log: Events,
}

struct RetainedTransfer {
    surface: String,
    name: String,
    // The next raster of a surface rewrites the same name and sequence, so a reply belongs to this
    // transfer image only when generation and raster also match.
    generation: u64,
    raster: u64,
    sequence: u32,
    // Holds the IOSurface until it is released.
    _frame: crate::platform::Frame,
}

impl TransferStore {
    fn new(log: Events) -> Arc<Self> {
        Arc::new(Self {
            images: std::sync::Mutex::new(Some(Vec::new())),
            log,
        })
    }

    fn images(&self) -> std::sync::MutexGuard<'_, Option<Vec<RetainedTransfer>>> {
        self.images.lock().unwrap_or_else(|error| {
            let images = error.into_inner();
            self.log.record_error(
                "image transfer store",
                "transfer store lock was poisoned; transfer state retained",
            );
            self.images.clear_poison();
            images
        })
    }

    /// If the connection has ended, no reply can come, so the transfer image is released at once.
    fn retain(&self, transfer: RetainedTransfer) {
        if let Some(images) = self.images().as_mut() {
            images.push(transfer);
        }
    }

    /// Releases the transfer image that matches the reply, and returns whether such an image
    /// existed. The host answers in the order it received the display requests, so when several
    /// images have the same surface, name, and sequence, the image sent first is the image of that
    /// reply.
    fn answer(&self, surface: &str, answer: ImageAnswer<'_>) -> bool {
        let mut images = self.images();
        let Some(images) = images.as_mut() else {
            return false;
        };
        let Some(index) = images.iter().position(|image| {
            image.surface == surface
                && image.name == answer.name
                && image.generation == answer.generation
                && image.raster == answer.raster
                && u64::from(image.sequence) == answer.sequence
        }) else {
            return false;
        };
        images.remove(index);
        true
    }

    /// The input of the connection has ended. No reply can come, so all remaining transfer images
    /// are released.
    fn end(&self) {
        self.images().take();
    }
}

#[derive(Clone)]
struct OutputSink {
    connection: Arc<tokio::sync::Mutex<Option<Connection>>>,
    /// The connection of the persistent service. When the client's output closes, it is detached
    /// instead of reported as an error.
    detachable: bool,
    /// The request that the replies of this sink answer; a sink of events has none.
    request: Option<Arc<Request>>,
    submission_observer: Option<OutputSubmissionObserver>,
}

/// Submission stages reported to an explicitly supplied output observer.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputSubmissionStage {
    Attempt,
    Queued,
    Failed,
}

/// Test observation hook for messages at the connection queue boundary.
#[doc(hidden)]
pub type OutputSubmissionObserver = Arc<dyn Fn(OutputSubmissionStage, &str) + Send + Sync>;

impl OutputSink {
    fn direct(connection: Connection) -> Self {
        Self {
            connection: Arc::new(tokio::sync::Mutex::new(Some(connection))),
            detachable: false,
            request: None,
            submission_observer: None,
        }
    }

    /// The connection of the persistent service. A session stays alive when its client disconnects
    /// (docs/spec/terminal-runtime.md), so a client whose output side closed first is also detached
    /// before the end of its input is read. A client that attaches again receives the current
    /// screen.
    fn detachable(connection: Connection) -> Self {
        Self {
            connection: Arc::new(tokio::sync::Mutex::new(Some(connection))),
            detachable: true,
            request: None,
            submission_observer: None,
        }
    }

    /// Returns a copy of this sink that carries the replies of `request`. The sink that the registry
    /// keeps for events stays unbound.
    fn bound(&self, request: Arc<Request>) -> OutputSink {
        OutputSink {
            request: Some(request),
            ..self.clone()
        }
    }

    fn with_submission_observer(mut self, observer: Option<OutputSubmissionObserver>) -> Self {
        self.submission_observer = observer;
        self
    }

    /// Sends the reply to the request of this sink. The line gets the `cids` of the request, and
    /// the reply is recorded before it is written.
    async fn reply(&self, mut line: Value) -> Result<(), ()> {
        if let Some(request) = &self.request {
            request.answer(&mut line);
        }
        self.send(line.to_string()).await?;
        if let Some(request) = &self.request {
            request.enter(RequestStage::Answered);
        }
        Ok(())
    }

    async fn send(&self, message: String) -> Result<(), ()> {
        self.send_carried(message).await.map(|_| ())
    }

    /// Sends the message, and returns the transfer image store of the connection that carried it.
    /// If no connection is attached, there is no carrying connection.
    async fn send_carried(&self, message: String) -> Result<Option<Arc<TransferStore>>, ()> {
        let connection = self.connection.lock().await.clone();
        match connection {
            Some(connection) => {
                let observed_message = self.submission_observer.as_ref().map(|_| message.clone());
                if let Some(observer) = &self.submission_observer {
                    observer(OutputSubmissionStage::Attempt, &message);
                }
                let stage = self.request.as_ref().map(|request| {
                    let stage = request.stage();
                    request.enter(RequestStage::OutputQueue);
                    stage
                });
                let sent = connection.sender.send(message).await;
                if let (Some(request), Some(stage)) = (&self.request, stage) {
                    request.enter(stage);
                }
                match sent {
                    Ok(()) => {
                        if let Some(observer) = &self.submission_observer {
                            observer(
                                OutputSubmissionStage::Queued,
                                observed_message
                                    .as_deref()
                                    .expect("observer message is present"),
                            );
                        }
                        Ok(Some(connection.transfers))
                    }
                    Err(_) if self.detachable => {
                        if let Some(observer) = &self.submission_observer {
                            observer(
                                OutputSubmissionStage::Failed,
                                observed_message
                                    .as_deref()
                                    .expect("observer message is present"),
                            );
                        }
                        let mut current = self.connection.lock().await;
                        if current
                            .as_ref()
                            .is_some_and(|current| current.sender.same_channel(&connection.sender))
                        {
                            *current = None;
                        }
                        Ok(None)
                    }
                    Err(_) => {
                        if let Some(observer) = &self.submission_observer {
                            observer(
                                OutputSubmissionStage::Failed,
                                observed_message
                                    .as_deref()
                                    .expect("observer message is present"),
                            );
                        }
                        Err(())
                    }
                }
            }
            None => Ok(None),
        }
    }

    async fn connection(&self) -> Option<Connection> {
        self.connection.lock().await.clone()
    }

    async fn replace_connection(&self, connection: Option<Connection>) {
        *self.connection.lock().await = connection;
    }

    async fn detach_connection(&self, previous: &Option<Connection>) {
        let mut current = self.connection.lock().await;
        if let (Some(connection), Some(previous)) = (current.as_ref(), previous.as_ref())
            && connection.sender.same_channel(&previous.sender)
        {
            *current = None;
        }
    }
}

struct PersistentEntry {
    tx: mpsc::Sender<Dispatch>,
    output: OutputSink,
    actor: tokio::task::JoinHandle<()>,
    epoch: u64,
    owner: String,
}

pub struct PersistentRegistry {
    entries: tokio::sync::Mutex<HashMap<String, PersistentEntry>>,
    next_epoch: std::sync::atomic::AtomicU64,
    shutdown: AtomicBool,
    detach_requests: Arc<ImageDetachRequestLedger>,
}

const SURFACE_ACTOR_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Sends a close to the surface action, then waits for the result of the action's session close and
/// for the end of the action (if an actor exists). The closed answer and the cleanup answer are
/// sent after this result, so a host that receives the answer treats the PTY child of that surface
/// as already reaped (core docs/spec/sidecars.md#messages).
async fn close_surface_actor(
    tx: &mpsc::Sender<Dispatch>,
    actor: Option<tokio::task::JoinHandle<()>>,
    operation: &str,
) -> Result<(), String> {
    let (result, answer) = tokio::sync::oneshot::channel();
    let sent = tx
        .send(Dispatch::internal(SurfaceCommand::SessionClose { result }))
        .await;
    let closing = async {
        let closed = match sent {
            Ok(()) => match answer.await {
                Ok(closed) => closed,
                Err(_) => Err(format!(
                    "surface actor ended before it answered {operation}"
                )),
            },
            Err(_) => Err(format!("surface actor closed before {operation}")),
        };
        let joined = match actor {
            Some(actor) => actor
                .await
                .map_err(|error| format!("surface actor join failed: {error}")),
            None => Ok(()),
        };
        match (closed, joined) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(closed), Err(joined)) => Err(format!("{closed}; {joined}")),
        }
    };
    match tokio::time::timeout(SURFACE_ACTOR_CLOSE_TIMEOUT, closing).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "surface actor close exceeded {:?}",
            SURFACE_ACTOR_CLOSE_TIMEOUT
        )),
    }
}

async fn close_surface_entries(
    entries: Vec<(String, PersistentEntry)>,
    operation: &str,
) -> Result<usize, String> {
    let count = entries.len();
    let mut errors = Vec::new();
    for (key, entry) in entries {
        if let Err(error) = close_surface_actor(&entry.tx, Some(entry.actor), operation).await {
            errors.push(format!("{key}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(count)
    } else {
        Err(errors.join("; "))
    }
}

impl PersistentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: tokio::sync::Mutex::new(HashMap::new()),
            next_epoch: std::sync::atomic::AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
            detach_requests: Arc::new(ImageDetachRequestLedger::default()),
        })
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    pub(crate) fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    async fn close_surface(&self, key: &str, owner: &str) -> Result<(), String> {
        let entry = {
            let mut entries = self.entries.lock().await;
            let Some(entry) = entries.get(key) else {
                return Ok(());
            };
            if entry.owner != owner {
                return Err("stale attachment".to_string());
            }
            entries.remove(key).expect("registry entry disappeared")
        };
        close_surface_actor(&entry.tx, Some(entry.actor), "close").await
    }

    async fn close_owner(&self, owner: &str) -> Result<(), String> {
        let entries = {
            let mut registry = self.entries.lock().await;
            let keys = registry
                .iter()
                .filter_map(|(key, entry)| (entry.owner == owner).then_some(key.clone()))
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registry.remove(&key).map(|entry| (key, entry)))
                .collect::<Vec<_>>()
        };
        close_surface_entries(entries, "owner close")
            .await
            .map(|_| ())
    }

    /// Closes the sessions of this owner that are not in `keep`, and returns the number closed.
    /// After the app restarts, a session of a surface that is in no layout has no place to reattach
    /// (docs/spec/terminal-runtime.md).
    async fn retain(
        &self,
        owner: &str,
        keep: &std::collections::HashSet<String>,
    ) -> Result<usize, String> {
        let entries = {
            let mut registry = self.entries.lock().await;
            let keys = registry
                .iter()
                .filter_map(|(key, entry)| {
                    (entry.owner == owner && !keep.contains(key)).then_some(key.clone())
                })
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registry.remove(&key).map(|entry| (key, entry)))
                .collect::<Vec<_>>()
        };
        close_surface_entries(entries, "retain").await
    }

    async fn current_epoch(&self, key: &str, owner: &str) -> Option<u64> {
        let entries = self.entries.lock().await;
        entries
            .get(key)
            .filter(|entry| entry.owner == owner)
            .map(|entry| entry.epoch)
    }

    async fn contains(&self, key: &str) -> bool {
        self.entries.lock().await.contains_key(key)
    }

    async fn attach(
        &self,
        key: &str,
        owner: &str,
        output: &OutputSink,
    ) -> Result<(mpsc::Sender<Dispatch>, u64), String> {
        let mut entries = self.entries.lock().await;
        let entry = entries
            .get_mut(key)
            .ok_or_else(|| "session surface not found".to_string())?;
        if entry.owner != owner {
            return Err("stale attachment".to_string());
        }
        entry.epoch = self
            .next_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        entry
            .output
            .replace_connection(output.connection().await)
            .await;
        Ok((entry.tx.clone(), entry.epoch))
    }

    async fn insert(
        &self,
        key: String,
        owner: String,
        tx: mpsc::Sender<Dispatch>,
        output: OutputSink,
        actor: tokio::task::JoinHandle<()>,
    ) -> Result<u64, String> {
        let mut entries = self.entries.lock().await;
        if entries.contains_key(&key) {
            close_surface_actor(&tx, Some(actor), "duplicate close")
                .await
                .map_err(|error| format!("close duplicate surface: {error}"))?;
            return Err("session surface already exists".to_string());
        }
        let epoch = self
            .next_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        entries.insert(
            key,
            PersistentEntry {
                tx,
                output,
                actor,
                epoch,
                owner,
            },
        );
        Ok(epoch)
    }
}

/// The trait that an engine implements. It is the contract of the VT processing engine.
pub trait Engine: Send + 'static {
    fn resize(&mut self, cols: u16, rows: u16);
    fn set_theme(&mut self, theme: crate::palette::TerminalTheme);
    fn set_cell_metrics(&mut self, width: u16, height: u16) -> Result<(), String>;
    fn feed(&mut self, bytes: &[u8]);
    fn drain_events(&mut self) -> Vec<EngineEvent>;
    fn resolve_clipboard(&mut self, request_id: u64, text: &str) -> Result<(), String>;
    fn reject_clipboard(&mut self, request_id: u64, reason: &str) -> Result<(), String>;
    /// Starts a selection at a cell boundary. `edge` runs from 0 to the column count. Edge `c` is
    /// the left boundary of column `c`, and the column count is the right boundary of the last
    /// column.
    fn selection_start(&mut self, edge: u16, row: u16) -> Result<(), String>;
    /// Moves the end of the selection to a cell boundary. The selection includes the cells between
    /// the start boundary and the end boundary. If the two boundaries are the same, nothing is
    /// selected.
    fn selection_update(&mut self, edge: u16, row: u16) -> Result<(), String>;
    /// Ends the selection and returns its text. A selection that contains no text is cleared and
    /// returns `None`. This is a normal gesture, not an error.
    fn selection_end(&mut self) -> Result<Option<String>, String>;
    /// Clears the previous selection without copying it. Returns whether the pixels changed.
    fn selection_clear(&mut self) -> bool;
    /// The text of the current selection. `None` if there is no selection or it holds no
    /// characters.
    fn selection_text(&self) -> Option<String>;
    /// Moves the viewport of the primary screen by `lines`. A positive value moves toward the older
    /// output.
    fn scroll_viewport(&mut self, lines: i32);
    /// Returns the viewport to the newest output, and returns true if it moved.
    fn scroll_to_newest(&mut self) -> bool;
    fn cursor(&self) -> Cursor;
    fn screen(&mut self) -> Screen;
    fn scroll_generation(&self) -> i64 {
        0
    }
    /// The number of lines by which the viewport is above the newest output. An engine without
    /// scrollback returns 0.
    fn viewport_offset(&self) -> u32 {
        0
    }
    fn modes(&self) -> Modes;
    fn reset(&mut self);
}

/// A neutral event passed between the engine and the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// An inline image command, with the cursor position and scroll generation at the point where
    /// its sequence was met.
    InlineImage {
        command: InlineImageCommand,
        anchor: InlineAnchor,
    },
    Title(String),
    ResetTitle,
    Directory {
        uri: String,
        /// The path if this is a directory of this computer, or None if it is a directory of
        /// another computer.
        path: Option<String>,
    },
    Hyperlink {
        id: String,
        uri: Option<String>,
    },
    Notification {
        message: String,
    },
    ShellState {
        marker: ShellMarker,
        params: Vec<String>,
    },
    ClipboardStore {
        selection: ClipboardSelection,
        text: String,
    },
    ClipboardQuery {
        request_id: u64,
        selection: ClipboardSelection,
    },
    PtyWrite(Vec<u8>),
    CursorBlinkingChange,
    Wakeup,
    Bell,
    Exit,
    ChildExit {
        success: bool,
        code: Option<i32>,
    },
    MouseCursorDirty,
    /// The pointer shape set by OSC 22. It is a CSS cursor value.
    PointerShape(String),
    Error(String),
    /// An internal engine failure, recorded independently of surface output.
    RuntimeError(String),
}

/// The cell column and scroll generation at the point where an inline image sequence was met. The
/// image is placed at this position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InlineAnchor {
    pub col: u16,
    pub row: u16,
    pub scroll: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellMarker {
    PromptStart,
    PromptEnd,
    CommandStart,
    CommandFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    Clipboard,
    Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CursorBlinkPolicy {
    Never,
    #[default]
    Off,
    On,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnfocusedCursor {
    #[default]
    Hollow,
    Solid,
    Underline,
    Beam,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPolicy {
    pub shape: CursorShape,
    pub blink: CursorBlinkPolicy,
    pub interval_ms: u64,
    pub idle_timeout_ms: u64,
    pub unfocused: UnfocusedCursor,
}

impl Default for CursorPolicy {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: CursorBlinkPolicy::Off,
            interval_ms: 750,
            idle_timeout_ms: 5000,
            unfocused: UnfocusedCursor::Hollow,
        }
    }
}

/// The attributes of one cell
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cell {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ch: Option<String>,
    pub width: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub inverse: bool,
    /// The URI of an OSC 8 hyperlink. A cell without a link has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: None,
            width: 1,
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            link: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    #[serde(default)]
    pub shape: CursorShape,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub blinking: bool,
    #[serde(rename = "blinkVisible", default = "default_blink_visible")]
    pub blink_visible: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preedit: Option<Preedit>,
}

fn default_visible() -> bool {
    true
}

fn default_blink_visible() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRange {
    pub location: usize,
    pub length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    #[serde(rename = "markedRange", deserialize_with = "deserialize_marked_range")]
    pub marked_range: Option<JsonRange>,
    #[serde(
        rename = "selectedRange",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_range: Option<JsonRange>,
    #[serde(
        rename = "replacementRange",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub replacement_range: Option<JsonRange>,
    #[serde(default)]
    pub attributed: bool,
}

impl Preedit {
    pub(crate) fn validate_marked_range(&self) -> Result<(), String> {
        let Some(JsonRange { location, length }) = self.marked_range else {
            return Ok(());
        };
        let end = location
            .checked_add(length)
            .ok_or_else(|| "markedRange JSON range overflows UTF-16 location".to_string())?;
        let text_length = self.text.encode_utf16().count();
        if end > text_length {
            return Err(format!(
                "markedRange JSON range [{location}, {length}] exceeds preedit UTF-16 length {text_length}"
            ));
        }
        if !preedit_grapheme_boundary(&self.text, location)
            || !preedit_grapheme_boundary(&self.text, end)
        {
            return Err("markedRange endpoint splits a preedit grapheme".to_string());
        }
        Ok(())
    }
}

fn preedit_grapheme_boundary(text: &str, target: usize) -> bool {
    if target == 0 {
        return true;
    }
    let mut offset = 0;
    for grapheme in text.graphemes(true) {
        offset += grapheme.encode_utf16().count();
        if offset == target {
            return true;
        }
        if offset > target {
            return false;
        }
    }
    false
}

fn deserialize_marked_range<'de, D>(deserializer: D) -> Result<Option<JsonRange>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<JsonRange>::deserialize(deserializer)
        .map_err(|error| serde::de::Error::custom(format!("markedRange: {error}")))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Modes {
    #[serde(default)]
    pub app_cursor: bool,
    #[serde(default)]
    pub app_keypad: bool,
    #[serde(default)]
    pub bracketed_paste: bool,
    /// ?1000: reports press and release.
    #[serde(default)]
    pub mouse_click: bool,
    /// ?1002: reports press and release, and motion while a button is held.
    #[serde(default)]
    pub mouse_drag: bool,
    /// ?1003: reports press and release, and all motion.
    #[serde(default)]
    pub mouse_motion: bool,
    #[serde(default)]
    pub focus_in_out: bool,
    #[serde(default)]
    pub utf8_mouse: bool,
    #[serde(default)]
    pub sgr_mouse: bool,
    #[serde(default)]
    pub alternate_scroll: bool,
    #[serde(default)]
    pub alt_screen: bool,
}

/// The phase of a pointer input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MousePhase {
    Down,
    Move,
    Up,
}

fn mouse_drag_motion_owned(gesture: bool, motion_owner: bool, pressed: bool) -> bool {
    gesture && motion_owner && pressed
}

impl Modes {
    /// Whether the program has turned on mouse reporting.
    pub fn mouse_report(&self) -> bool {
        self.mouse_click || self.mouse_drag || self.mouse_motion
    }
}

/// The number of lines by which the viewport is above the newest output, and the number of retained
/// history lines.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Scrollback {
    pub offset: u32,
    pub history: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    #[serde(default)]
    pub scrollback: Scrollback,
    /// The current default background color (`#rrggbb`). It includes the value that the program
    /// changed with OSC 11. The raster fills the margin outside the cells with this color.
    pub background: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImagePlacement {
    pub name: String,
    pub data: Vec<u8>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub preserve_aspect_ratio: bool,
    pub anchor_row: i32,
    pub anchor_scroll: i64,
    pub visible: bool,
}

/// Event received from the daemon
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    Output {
        session_id: String,
        data: Vec<u8>,
        sequence: i64,
        truncated: bool,
    },
    Exit {
        session_id: String,
    },
    Error {
        session_id: String,
        error: String,
    },
}

/// Session port: the abstract interface that communicates with the daemon. The shell and the start
/// directory of a request that opens a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellRequest {
    /// The value of the terminal setting shell. It is `login` or the absolute path of a shell.
    pub shell: String,
    /// The absolute path of the directory the shell starts in. Without it, the home directory of
    /// the account is used.
    pub directory: Option<String>,
}

#[async_trait]
pub trait SessionPort: Send + Sync {
    async fn open(&self, request: &ShellRequest, cols: u16, rows: u16) -> Result<String, String>;
    /// Writes `data` to the session. The surface task records the write as a replay input.
    async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String>;
    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String>;
    async fn detach(&self, session_id: &str) -> Result<(), String>;
    async fn close(&self, session_id: &str) -> Result<(), String>;
    /// Attaches a new view to an existing session and replays the retained output.
    async fn attach(&self, _session_id: &str, _from: i64) -> Result<String, String> {
        Err("session attach is not supported".to_string())
    }
    /// Measures the PTY transfer state at one point in time. pending is the number of input bytes
    /// that the master wrote and the child has not read yet, and written is the cumulative number
    /// of child output bytes that the reader has read. Because this is a measurement observation, a
    /// port that does not provide it fails explicitly.
    async fn pty_measurement(
        &self,
        _session_id: &str,
    ) -> Result<crate::pty::PtyMeasurement, String> {
        Err("session PTY measurement is not supported".to_string())
    }
    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent>;
}

/// Envelope of a message received from the host
#[derive(Debug, Deserialize)]
struct Envelope {
    surface: String,
    #[allow(dead_code)]
    root: Option<String>,
    body: Option<Value>,
    closed: Option<bool>,
    /// The correlation identifiers of the inputs the request continues (core
    /// docs/spec/sidecars.md#messages).
    #[serde(default)]
    cids: Vec<String>,
}

/// Commands sent to the surface action
#[derive(Debug)]
enum SurfaceCommand {
    Open {
        image: Option<String>,
        request: ShellRequest,
    },
    Reconnect,
    Configure(ImageConfiguration),
    /// The drawing state of the image region that the host sends at its attachment and at each
    /// change (core docs/spec/native-surfaces.md#image-regions).
    Drawing {
        name: String,
        generation: u64,
        drawing: bool,
    },
    ImageDetach {
        detach: ImageDetach,
        request_check: ImageDetachRequestCheck,
    },
    InputTransaction {
        actions: Vec<InputAction>,
        preedit: Preedit,
        cids: Vec<String>,
    },
    Paste {
        text: String,
        cids: Vec<String>,
    },
    Theme {
        // The theme is much larger than the other commands, so it is kept apart to avoid increasing
        // the size of the command.
        theme: Box<crate::palette::TerminalTheme>,
    },
    Font {
        font: std::sync::Arc<crate::platform::TerminalFont>,
        system: bool,
        skipped: Vec<String>,
        /// Font size (points).
        size: f32,
    },
    Cursor {
        policy: CursorPolicy,
    },
    ScreenRead,
    /// Closes the session, answers its result as `result`, and then ends the action.
    SessionClose {
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    SessionDetach,
    ImageResponse {
        body: Value,
        dispatched_at: Option<std::time::Instant>,
    },
    ClipboardResolve {
        request_id: u64,
        text: String,
    },
    ClipboardReject {
        request_id: u64,
        reason: String,
    },
    InlineImageDelete {
        name: String,
    },
    /// Pointer input of the page. If the program turned on mouse reporting, it is converted to a
    /// report.
    Mouse {
        input_id: String,
        phase: MousePhase,
        x: f64,
        y: f64,
        pressed: bool,
        shift: bool,
        alt: bool,
        ctrl: bool,
        cids: Vec<String>,
    },
    SelectionStart {
        x: f64,
        y: f64,
    },
    SelectionUpdate {
        x: f64,
        y: f64,
    },
    SelectionEnd,
    /// The user's copy command. It sends the text of the current selection as a copy event.
    Copy,
    /// Wheel scroll. `lines` is not 0, and a positive value moves toward the older output. `col`
    /// and `row` are the pointer cell. `shift` is true while Shift is held, which skips mouse reporting.
    Scroll {
        lines: i32,
        col: u16,
        row: u16,
        shift: bool,
        cids: Vec<String>,
    },
    /// Scrollbar drag. It moves the primary viewport to this offset, and it is not written to the
    /// program.
    Viewport {
        offset: u32,
    },
    /// A measurement operation that asks for the number of input bytes that the master wrote and
    /// the child has not read yet in the current session.
    PtyPending,
}

use crate::platform::ImageConfiguration;

/// Input key information
#[derive(Debug, Clone, Deserialize)]
struct InputKey {
    key: String,
    text: String,
    shift: bool,
    alt: bool,
    ctrl: bool,
}

#[derive(Debug)]
enum InputAction {
    Insert {
        text: String,
        replacement_range: Option<JsonRange>,
        attributed: bool,
    },
    Key(InputKey),
    Command(InputKey),
    Focus(bool),
}

fn require_exact_fields(value: &Value, fields: &[&str], description: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{description} must be an object"))?;
    if object.len() != fields.len() || object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err(format!(
            "{description} must contain exactly {}",
            fields.join(", ")
        ));
    }
    Ok(())
}

fn parse_required_nullable_range(
    object: &serde_json::Map<String, Value>,
    field: &str,
    description: &str,
) -> Result<Option<JsonRange>, String> {
    let value = object
        .get(field)
        .ok_or_else(|| format!("{description}.{field} is required"))?;
    match value {
        Value::Null => Ok(None),
        Value::Object(_) => {
            require_exact_fields(
                value,
                &["location", "length"],
                &format!("{description}.{field}"),
            )?;
            serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|error| format!("{description}.{field}: {error}"))
        }
        _ => Err(format!(
            "{description}.{field} must be null or a range object"
        )),
    }
}

fn parse_input_preedit(value: &Value) -> Result<Preedit, String> {
    require_exact_fields(
        value,
        &[
            "text",
            "markedRange",
            "selectedRange",
            "replacementRange",
            "attributed",
        ],
        "preedit",
    )?;
    let object = value.as_object().expect("exact object validation");
    parse_required_nullable_range(object, "markedRange", "preedit")?;
    parse_required_nullable_range(object, "selectedRange", "preedit")?;
    parse_required_nullable_range(object, "replacementRange", "preedit")?;
    if !object.get("attributed").is_some_and(Value::is_boolean) {
        return Err("preedit.attributed must be boolean".into());
    }
    let preedit: Preedit =
        serde_json::from_value(value.clone()).map_err(|error| format!("preedit: {error}"))?;
    preedit.validate_marked_range()?;
    Ok(preedit)
}

fn parse_input_key(value: &Value, description: &str) -> Result<InputKey, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{description} must be an object"))?;
    let key = object
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{description}.key must be a string"))?;
    let fields = if key == "Char" {
        &["key", "text", "shift", "alt", "ctrl"][..]
    } else {
        &["key", "shift", "alt", "ctrl"][..]
    };
    require_exact_fields(value, fields, description)?;
    let text = if key == "Char" {
        object
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{description}.text must be a string for Char"))?
            .to_string()
    } else {
        String::new()
    };
    let shift = object
        .get("shift")
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{description}.shift must be boolean"))?;
    let alt = object
        .get("alt")
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{description}.alt must be boolean"))?;
    let ctrl = object
        .get("ctrl")
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{description}.ctrl must be boolean"))?;
    Ok(InputKey {
        key: key.to_string(),
        text,
        shift,
        alt,
        ctrl,
    })
}

fn parse_input_action(value: &Value, index: usize) -> Result<InputAction, String> {
    let description = format!("actions[{index}]");
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{description}.type must be a string"))?;
    match kind {
        "insert" => {
            require_exact_fields(
                value,
                &["type", "text", "replacementRange", "attributed"],
                &description,
            )?;
            let object = value.as_object().expect("exact object validation");
            let text = object["text"]
                .as_str()
                .ok_or_else(|| format!("{description}.text must be a string"))?
                .to_string();
            let replacement_range =
                parse_required_nullable_range(object, "replacementRange", &description)?;
            let attributed = object["attributed"]
                .as_bool()
                .ok_or_else(|| format!("{description}.attributed must be boolean"))?;
            Ok(InputAction::Insert {
                text,
                replacement_range,
                attributed,
            })
        }
        "key" => {
            let object = value
                .as_object()
                .ok_or_else(|| format!("{description} must be an object"))?;
            let key = object
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{description}.key must be a string"))?;
            let allowed_fields = if key == "Char" {
                &["type", "key", "text", "shift", "alt", "ctrl"][..]
            } else {
                &["type", "key", "shift", "alt", "ctrl"][..]
            };
            require_exact_fields(value, allowed_fields, &description)?;
            let mut fields = object.clone();
            fields.remove("type");
            Ok(InputAction::Key(parse_input_key(
                &Value::Object(fields),
                &description,
            )?))
        }
        "command" => {
            require_exact_fields(value, &["type", "selector"], &description)?;
            let selector = value["selector"]
                .as_str()
                .ok_or_else(|| format!("{description}.selector must be a string"))?;
            key_for_native_command(selector)
                .map(InputAction::Command)
                .map_err(|error| format!("{description}: {error}"))
        }
        "focus" => {
            require_exact_fields(value, &["type", "focused"], &description)?;
            value["focused"]
                .as_bool()
                .map(InputAction::Focus)
                .ok_or_else(|| format!("{description}.focused must be boolean"))
        }
        _ => Err(format!("{description}.type is unsupported: {kind}")),
    }
}

fn parse_input_transaction(body: &Value) -> Result<(Vec<InputAction>, Preedit), String> {
    require_exact_fields(body, &["operation", "actions", "preedit"], "input")?;
    if body["operation"] != "input" {
        return Err("input.operation must be input".into());
    }
    let actions = body["actions"]
        .as_array()
        .ok_or_else(|| "input.actions must be an array".to_string())?
        .iter()
        .enumerate()
        .map(|(index, action)| parse_input_action(action, index))
        .collect::<Result<Vec<_>, _>>()?;
    let preedit = parse_input_preedit(&body["preedit"])?;
    Ok((actions, preedit))
}

fn preedit_changes_render(current: &Option<Preedit>, next: &Preedit) -> bool {
    if next.text.is_empty() {
        return current
            .as_ref()
            .is_some_and(|preedit| !preedit.text.is_empty());
    }
    current.as_ref() != Some(next)
}

fn key_for_native_command(selector: &str) -> Result<InputKey, String> {
    let key = match selector {
        "insertNewline:" => "Enter",
        "deleteBackward:" => "Backspace",
        "deleteForward:" => "Delete",
        "cancelOperation:" => "Escape",
        _ => return Err(format!("unsupported native command selector: {selector}")),
    };
    Ok(InputKey {
        key: key.to_string(),
        text: String::new(),
        shift: false,
        alt: false,
        ctrl: false,
    })
}

fn decorate_screen(
    mut screen: Screen,
    focused: bool,
    preedit: &Option<Preedit>,
    policy: &CursorPolicy,
    elapsed_ms: u64,
) -> Screen {
    let program_shape = screen.cursor.shape;
    if program_shape == CursorShape::Block {
        screen.cursor.shape = policy.shape;
    }
    screen.cursor.focused = focused;
    screen.cursor.blink_visible = crate::platform::darwin::frame::cursor_blink_visible(
        policy.blink,
        screen.cursor.blinking,
        focused,
        elapsed_ms,
        policy.interval_ms,
        policy.idle_timeout_ms,
    );
    if !focused {
        screen.cursor.shape = crate::platform::darwin::frame::effective_cursor_shape(
            screen.cursor.shape,
            false,
            policy.unfocused,
        );
    }
    screen.cursor.preedit = preedit.clone();
    screen
}

fn parse_cursor_policy(body: &Value) -> Result<CursorPolicy, String> {
    let defaults = CursorPolicy::default();
    let shape = match body.get("shape").and_then(Value::as_str) {
        None => defaults.shape,
        Some("block") => CursorShape::Block,
        Some("underline") => CursorShape::Underline,
        Some("beam") => CursorShape::Beam,
        Some(value) => {
            return Err(format!(
                "cursor.shape must be block, underline, or beam: {value}"
            ));
        }
    };
    let blink = match body.get("blink").and_then(Value::as_str) {
        None => defaults.blink,
        Some("Never") => CursorBlinkPolicy::Never,
        Some("Off") => CursorBlinkPolicy::Off,
        Some("On") => CursorBlinkPolicy::On,
        Some("Always") => CursorBlinkPolicy::Always,
        Some(value) => {
            return Err(format!(
                "cursor.blink must be Never, Off, On, or Always: {value}"
            ));
        }
    };
    let interval_ms = match body.get("interval") {
        None => defaults.interval_ms,
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| "cursor.interval must be a positive integer".to_string())?,
    };
    let idle_timeout_ms = match body.get("idleTimeout") {
        None => defaults.idle_timeout_ms,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| "cursor.idleTimeout must be a nonnegative integer".to_string())?,
    };
    let unfocused = match body.get("unfocused").and_then(Value::as_str) {
        None => defaults.unfocused,
        Some("hollow") => UnfocusedCursor::Hollow,
        Some("solid") => UnfocusedCursor::Solid,
        Some("underline") => UnfocusedCursor::Underline,
        Some("beam") => UnfocusedCursor::Beam,
        Some("unchanged") => UnfocusedCursor::Unchanged,
        Some(value) => return Err(format!("cursor.unfocused is invalid: {value}")),
    };
    Ok(CursorPolicy {
        shape,
        blink,
        interval_ms,
        idle_timeout_ms,
        unfocused,
    })
}

fn pixels_to_cells(pixels: u32, cell_size: f32) -> u16 {
    (pixels as f32 / cell_size) as u16
}

/// CSI 14t reports the character area in device pixels. The pixel size of an inline image uses the
/// same unit, so when a program computes the cell size from 14t and requests an image, the image
/// fits the cells.
fn set_engine_metrics(engine: &mut Box<dyn Engine>, state: &ImageState) -> Result<(), String> {
    let width = state.metrics.cell_width.round() as u16;
    let height = state.metrics.cell_height.round() as u16;
    engine.set_cell_metrics(width, height)
}

/// Validates and computes the terminal size. It returns an error if width or height is invalid or
/// the result is 0.
fn calculate_terminal_size(
    width: u32,
    height: u32,
    metrics: &crate::platform::Metrics,
) -> Result<(u16, u16), String> {
    if width == 0 || height == 0 {
        return Err("width and height must be positive".to_string());
    }

    let cols = pixels_to_cells(width, metrics.cell_width);
    let rows = pixels_to_cells(height, metrics.cell_height);

    if cols == 0 || rows == 0 {
        return Err("width and height must be positive".to_string());
    }

    Ok((cols, rows))
}

/// Records that a raster holds no whole cell, so the grid of the surface stays as it is until a
/// raster with a cell arrives.
fn record_empty_raster(log: &Events, surface_id: &str, width: u32, height: u32, reason: &str) {
    log.record_info(
        "raster",
        format!("surface {surface_id}: {reason} for {width}x{height} pixels; the grid stays"),
    );
}

/// Encodes key input as bytes
fn encode_keys(keys: &[InputKey], modes: &Modes) -> Result<Vec<u8>, String> {
    let mut all_bytes = Vec::new();

    for input_key in keys {
        let key_name = &input_key.key;
        let text = &input_key.text;

        // Computes the modifier bits: shift=1, alt=2, ctrl=4
        let mut modifiers = 0u8;
        if input_key.shift {
            modifiers |= 1;
        }
        if input_key.alt {
            modifiers |= 2;
        }
        if input_key.ctrl {
            modifiers |= 4;
        }

        // Converts the key name to the Key enumeration
        let key = match key_name.as_str() {
            "Up" => Key::Up,
            "Down" => Key::Down,
            "Left" => Key::Left,
            "Right" => Key::Right,
            "Home" => Key::Home,
            "End" => Key::End,
            "Insert" => Key::Insert,
            "Delete" => Key::Delete,
            "PageUp" => Key::PageUp,
            "PageDown" => Key::PageDown,
            "F1" => Key::F1,
            "F2" => Key::F2,
            "F3" => Key::F3,
            "F4" => Key::F4,
            "F5" => Key::F5,
            "F6" => Key::F6,
            "F7" => Key::F7,
            "F8" => Key::F8,
            "F9" => Key::F9,
            "F10" => Key::F10,
            "F11" => Key::F11,
            "F12" => Key::F12,
            "Enter" => Key::Enter,
            "Tab" => Key::Tab,
            "Backspace" => Key::Backspace,
            "Escape" => Key::Escape,
            "KeypadEnter" => Key::KeypadEnter,
            name if name.starts_with("Keypad") => match &name["Keypad".len()..] {
                digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                    Key::Keypad(digit.as_bytes()[0] as char)
                }
                "Decimal" => Key::Keypad('.'),
                "Plus" => Key::Keypad('+'),
                "Minus" => Key::Keypad('-'),
                "Multiply" => Key::Keypad('*'),
                "Divide" => Key::Keypad('/'),
                "Equals" => Key::Keypad('='),
                _ => return Err(format!("unknown key: {}", key_name)),
            },
            "Char" => {
                // Special handling of "Char"
                if text.is_empty() {
                    return Err("unknown key: Char with empty text".to_string());
                }
                let ch = text.chars().next().unwrap();
                let encoded_bytes = if modifiers & 4 != 0 {
                    // The ctrl bit is set
                    encoding::encode_ctrl_char(ch).map_err(|error| {
                        format!(
                            "unknown key: Char with ctrl: {:?} (text {:?}, U+{:04X})",
                            error, ch, ch as u32
                        )
                    })?
                } else if modifiers & 2 != 0 {
                    // The alt bit is set
                    encoding::encode_alt_char(ch)
                } else {
                    // Plain text
                    encoding::encode_text(text)
                };
                all_bytes.extend_from_slice(&encoded_bytes);
                continue;
            }
            _ => {
                return Err(format!("unknown key: {}", key_name));
            }
        };

        // Encodes a standard key
        let encoded_bytes = encoding::encode_key(key, modifiers, modes)
            .map_err(|e| format!("unknown key: {} ({:?})", key_name, e))?;
        all_bytes.extend_from_slice(&encoded_bytes);
    }

    Ok(all_bytes)
}

pub use crate::platform::ImageState;

/// Draws the screen into the image, sends the envelope, and hands the image to the host. If drawing
/// fails, it sends an error event and returns true. If the output channel has closed, it returns
/// false. The default and the range of the font size (points). The default size multiplied by the
/// text scale of the frame and of the card (each at most 3) stays within the range
/// (docs/spec/text-size.md).
const DEFAULT_FONT_SIZE: f32 = 13.0;
const FONT_SIZE_MIN: f64 = 4.0;
const FONT_SIZE_MAX: f64 = 128.0;

/// The `screen` message of core (docs/spec/persistent-services.md#screen-message): the members that
/// the screen hash covers, with every cell in the full form or the rows that changed since the sent
/// message of `order` in the change form, so the page applies the message to the screen it holds,
/// hashes the full screen and compares it with the frame record of the same `segment` and
/// `sequence`. The scrollback state and the background color are further members of the service.
/// The `frame` member names the frame that shows the screen (`shown_frame`). A screen that cannot
/// be hashed gets no message. Returns the message and the screen that it carries.
pub fn screen_message(
    segment: &str,
    sequence: u64,
    order: ScreenOrder,
    frame: Value,
    render: Value,
    screen: &Screen,
    frame_accepted: bool,
) -> Result<(Value, SentScreen), String> {
    let layout = crate::platform::darwin::frame::render_layout(screen)?;
    let (mut members, cells) = screen_message_with_layout(
        segment,
        sequence,
        order,
        frame,
        render,
        screen,
        &layout,
        frame_accepted,
    )?;
    cells.insert_into(&mut members);
    Ok((Value::Object(members), cells.sent()))
}

/// The members of a `screen` message other than its cells, and its cells.
#[allow(clippy::too_many_arguments)]
fn screen_message_with_layout(
    segment: &str,
    sequence: u64,
    order: ScreenOrder,
    frame: Value,
    render: Value,
    screen: &Screen,
    layout: &crate::platform::darwin::frame::RenderLayout,
    frame_accepted: bool,
) -> Result<(serde_json::Map<String, Value>, ScreenCells), String> {
    if sequence == 0 {
        return Err("screen message: sequence must be an integer from 1".to_string());
    }
    if frame_accepted && frame.is_null() {
        return Err("screen message: accepted frame identity is required".to_string());
    }
    if !render.is_null() && (frame.is_null() || render["frame"] != frame) {
        return Err("screen message: render.frame must match frame".to_string());
    }
    let cells = crate::screen_hash::message_cells(screen)?;
    let mut cursor = json!(screen.cursor);
    cursor["caret"] = if frame_accepted {
        json!({
            "row": layout.screen.cursor.row,
            "col": layout.screen.cursor.col,
        })
    } else {
        Value::Null
    };
    let mut body = serde_json::Map::from_iter([
        ("event".to_string(), Value::String("screen".to_string())),
        ("sequence".to_string(), json!(sequence)),
        ("segment".to_string(), json!(segment)),
        ("rows".to_string(), json!(screen.rows)),
        ("cols".to_string(), json!(screen.cols)),
        ("scrollback".to_string(), json!(screen.scrollback)),
        ("background".to_string(), json!(screen.background)),
    ]);
    body.insert("frame".to_string(), frame);
    body.insert("render".to_string(), render);
    body.insert("cursor".to_string(), cursor);
    let cells = screen_cells(order, screen.rows, screen.cols, cells);
    Ok((body, cells))
}

/// The line of the `screen` message of a surface and the screen that it carries.
fn screen_event(
    surface_id: &str,
    log: &Events,
    sequence: u64,
    order: ScreenOrder,
    image: Option<&ImageState>,
    screen: &Screen,
    layout: &crate::platform::darwin::frame::RenderLayout,
) -> Result<(String, SentScreen), String> {
    let frame = shown_frame(image);
    let render = image
        .and_then(|state| state.render.as_ref())
        .filter(|metadata| metadata["frame"] == frame)
        .cloned()
        // default: a frame without matching diagnostic metadata reports no render metadata.
        .unwrap_or(Value::Null);
    let frame_accepted = image.is_some_and(|state| !state.dirty && state.sequence > 0);
    let (members, cells) = screen_message_with_layout(
        log.boot(),
        sequence,
        order,
        frame,
        render,
        screen,
        layout,
        frame_accepted,
    )?;
    let line = cells.line(surface_id, &members)?;
    Ok((line, cells.sent()))
}

fn collect_render_metadata(trace_dir: Option<&std::path::Path>) -> bool {
    cfg!(feature = "diagnostics") && trace_dir.is_some()
}

/// The `frame` member of a `screen` message: the identity of the frame that shows the screen of the
/// surface. A frame is named by its sequence, and the screen of a frame is the screen of its
/// sequence. The screen names the next sequence while it has changes that no sent frame holds
/// (`dirty`), and also when no frame was sent for the image state (`sequence` 0). Otherwise it
/// names the last sent frame. Without an image state no frame exists, so the member is null.
fn shown_frame(image: Option<&ImageState>) -> Value {
    match image {
        None => Value::Null,
        Some(state) => {
            let sequence = if state.dirty || state.sequence == 0 {
                state.sequence + 1
            } else {
                state.sequence
            };
            json!({"generation": state.generation, "raster": state.raster, "sequence": sequence})
        }
    }
}

/// Records the frame record of a screen (core docs/spec/replay.md#records): the sequence of the
/// last input that it includes, its size and its hash. A raster adds the time to draw it, its size
/// in pixels and its number.
fn record_frame(
    log: &Events,
    surface_id: &str,
    session: Option<&str>,
    screen: &Screen,
    reason: &str,
    sequence: u64,
    raster: Option<(u64, String, u32)>,
) {
    let hash = match crate::screen_hash::screen_hash(screen) {
        Ok(hash) => hash,
        Err(error) => return log.record_error("frame", error),
    };
    let mut fields = serde_json::json!({
        "reason": reason,
        "sequence": sequence,
        "rows": screen.rows,
        "cols": screen.cols,
        "hash": hash,
    });
    if let Some((draw_us, raster, image)) = raster {
        fields["draw_us"] = json!(draw_us);
        fields["raster"] = json!(raster);
        fields["image"] = json!(image);
    }
    log.emit_for(
        Level::Debug,
        "sidecar.vt.frame",
        Subject {
            surface: Some(surface_id),
            session,
            cids: &[],
        },
        &fields,
    );
}

/// Sends the screen of a surface that has no raster: the frame record, then the message. A screen
/// that cannot be hashed is an error record and nothing is sent. Returns false when the output is
/// closed.
#[allow(clippy::too_many_arguments)]
async fn send_plain_screen(
    surface_id: &str,
    session: Option<&str>,
    screen: &Screen,
    output_tx: &OutputSink,
    log: &Events,
    screens: &mut ScreenStream,
    sequence: u64,
    image: Option<&ImageState>,
) -> bool {
    let layout = match crate::platform::darwin::frame::render_layout(screen) {
        Ok(layout) => layout,
        Err(error) => {
            log.record_error("screen", error);
            return true;
        }
    };
    let (line, sent) = match screen_event(
        surface_id,
        log,
        sequence,
        screens.order(),
        image,
        screen,
        &layout,
    ) {
        Ok(message) => message,
        Err(error) => {
            log.record_error("screen", error);
            return true;
        }
    };
    record_frame(log, surface_id, session, screen, "screen", sequence, None);
    if output_tx.send(line).await.is_err() {
        return false;
    }
    screens.sent(sent);
    true
}

/// The state of the view that decorates the screen of the engine.
struct ScreenView<'a> {
    focused: bool,
    preedit: &'a Option<Preedit>,
    policy: &'a CursorPolicy,
}

/// Ends the selection gesture: the engine copies the selected text, and the event of the release
/// (`selection.copy` or `selection.end`) is sent, after the raster that shows the end when the
/// surface has one. Returns false when the output is closed.
#[allow(clippy::too_many_arguments)]
async fn end_selection(
    surface_id: &str,
    engine: &mut dyn Engine,
    inputs: &mut ReplayInputs,
    session_id: Option<&str>,
    image_state: &mut Option<ImageState>,
    view: ScreenView<'_>,
    output_tx: &OutputSink,
    log: &Events,
    screens: &mut ScreenStream,
    pending_selection_event: &mut Option<String>,
) -> bool {
    match inputs.selection_end(engine, session_id) {
        Ok(text) => {
            let body = match text {
                Some(text) => {
                    json!({"event": "selection.copy", "text": text, "userInitiated": true})
                }
                None => json!({"event": "selection.end", "copied": false}),
            };
            if let Some(state) = image_state.as_mut() {
                let screen =
                    decorate_screen(engine.screen(), view.focused, view.preedit, view.policy, 0);
                if !present_screen(
                    surface_id,
                    screen,
                    state,
                    output_tx,
                    "selection",
                    log,
                    screens,
                    inputs.sequence,
                )
                .await
                {
                    return false;
                }
                *pending_selection_event =
                    Some(json!({"surface": surface_id, "body": body}).to_string());
                return true;
            }
            // A surface without a raster reports the release at once.
            let response = json!({"surface": surface_id, "body": body});
            output_tx.send(response.to_string()).await.is_ok()
        }
        Err(error) => {
            let response =
                json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
            output_tx.reply(response).await.is_ok()
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn present_screen(
    surface_id: &str,
    screen: Screen,
    state: &mut ImageState,
    output_tx: &OutputSink,
    reason: &str,
    log: &Events,
    screens: &mut ScreenStream,
    input_sequence: u64,
) -> bool {
    // The host answers consumed after it copies the raster of a display request. If the same raster
    // is drawn again before the answer arrives, the pixels being copied are overwritten and a frame
    // with missing glyphs is shown. The screen in the meantime stays dirty and is drawn after the
    // answer arrives.
    if state.pending_draw || !state.drawing {
        state.dirty = true;
        return true;
    }
    let draw_started = std::time::Instant::now();
    let collect_render = collect_render_metadata(log.trace_dir());
    let draw = crate::platform::darwin::frame::render_layout(&screen).and_then(|layout| {
        state
            .frame
            .draw_render_layout(
                &layout,
                &state.metrics,
                crate::platform::darwin::frame::CursorRender::from_protocol(&screen.cursor),
                &state.theme,
                &state.inline_images,
                collect_render,
            )
            .map(|render| (layout, render))
    });
    let (layout, render) = match draw {
        Ok(result) => result,
        Err(reason) => {
            // The failed screen is in no frame. It stays dirty, so the `screen` event names the
            // next frame, and the next change draws that frame. Clearing it would name the last
            // sent frame, which shows an earlier screen.
            state.dirty = true;
            let response = json!({
                "surface": surface_id,
                "body": {"event": "error", "reason": reason}
            });
            if output_tx.send(response.to_string()).await.is_err() {
                return false;
            }
            // A failed draw does not discard the state events. The error reports that the screen
            // has no drawing.
            for event in std::mem::take(&mut state.presentation_events) {
                if output_tx.send(event).await.is_err() {
                    return false;
                }
            }
            return true;
        }
    };
    // The instrument for one frame (V5-104): what caused the draw, how long it took, and for which
    // raster. The frame record (core docs/spec/replay.md#records) holds the sequence number of the
    // last input that the screen includes, and the size and hash of the screen.
    record_frame(
        log,
        surface_id,
        state.session.as_deref(),
        &screen,
        reason,
        input_sequence,
        Some((
            draw_started.elapsed().as_micros() as u64,
            format!("{}x{}", state.width_px, state.height_px),
            state.sequence + 1,
        )),
    );
    state.sequence += 1;
    let frame = json!({
        "generation": state.generation,
        "raster": state.raster,
        "sequence": state.sequence
    });
    state.render = render.map(|render| render.value(frame));
    state.pending_draw = true;
    state.sent_at = Some(std::time::Instant::now());
    state.dirty = false;
    let nonce = state.frame.nonce();
    let nonce_b64 = base64_encode(&nonce);
    let image_envelope = json!({
        "surface": surface_id,
        "body": {
            "image": {
                "name": state.name.clone(),
                "generation": state.generation,
                "raster": state.raster,
                "token": {
                    "kind": "iosurface-global",
                    "id": state.frame.id(),
                    "nonce": nonce_b64
                },
                "width": state.width_px,
                "height": state.height_px,
                "scale": state.scale,
                "format": "bgra8",
                "sequence": state.sequence
            }
        }
    });
    // The screen of the page is the screen that this raster drew. Both must be sent together so
    // that the cursor and glyph state match the screen pixels. The state events of the inline
    // images that this raster drew are sent after them.
    match output_tx.send_carried(image_envelope.to_string()).await {
        Ok(carrier) => state.carrier = carrier,
        Err(()) => return false,
    }
    let surface = surface_id.to_string();
    let segment = log.boot().to_string();
    let frame = shown_frame(Some(state));
    // default: render metadata is absent when diagnostic frame tracing is disabled.
    let render = state.render.clone().unwrap_or(Value::Null);
    let work = screen_message_work(
        surface,
        segment,
        input_sequence,
        screens.order(),
        frame,
        render,
        screen,
        layout,
    );
    match prepare_screen_line(work).await {
        Ok((line, sent)) => {
            if output_tx.send(line).await.is_err() {
                return false;
            }
            screens.sent(sent);
        }
        Err(error) => log.record_error("screen", error),
    }
    for event in std::mem::take(&mut state.presentation_events) {
        if output_tx.send(event).await.is_err() {
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn screen_message_work(
    surface: String,
    segment: String,
    sequence: u64,
    order: ScreenOrder,
    frame: Value,
    render: Value,
    screen: Screen,
    layout: crate::platform::darwin::frame::RenderLayout,
) -> impl FnOnce() -> Result<(String, SentScreen), String> + Send + 'static {
    move || {
        let (members, cells) = screen_message_with_layout(
            &segment, sequence, order, frame, render, &screen, &layout, true,
        )?;
        let line = cells.line(&surface, &members)?;
        Ok((line, cells.sent()))
    }
}

/// Records the drawing mode that an `open` or an applied `image.configure` leaves on a surface:
/// `image` draws the output of the session, `headless` feeds it without drawing (core checklist
/// item F192).
fn record_surface_mode(log: &Events, surface_id: &str, headless: bool, cause: &str) {
    log.emit_for(
        Level::Info,
        "sidecar.vt.surface_mode",
        Subject::surface(surface_id),
        &json!({"mode": if headless { "headless" } else { "image" }, "cause": cause}),
    );
}

/// The modes that decide the route of a wheel operation, as the fields of `sidecar.vt.mouse_modes`
/// and `sidecar.vt.wheel` (docs/spec/vt-core.md).
fn wheel_modes(modes: &Modes) -> Value {
    json!({
        "mouse_click": modes.mouse_click,
        "mouse_drag": modes.mouse_drag,
        "mouse_motion": modes.mouse_motion,
        "sgr_mouse": modes.sgr_mouse,
        "utf8_mouse": modes.utf8_mouse,
        "alternate_scroll": modes.alternate_scroll,
        "alt_screen": modes.alt_screen,
    })
}

/// Records `sidecar.vt.mouse_modes` when the wheel modes of the engine differ from `recorded`, the
/// modes of the last record of the surface.
fn record_mouse_modes(log: &Events, surface_id: &str, recorded: &mut Value, modes: &Modes) {
    let current = wheel_modes(modes);
    if current != *recorded {
        log.emit_for(
            Level::Info,
            "sidecar.vt.mouse_modes",
            Subject::surface(surface_id),
            &current,
        );
        *recorded = current;
    }
}

/// Where a wheel operation goes under the modes in force and its Shift state: the mouse reporting of
/// the program, the cursor keys of the alternate scroll, or the primary viewport (docs/spec/vt-core.md).
#[derive(Clone, Copy)]
enum WheelRoute {
    Program,
    AlternateScroll,
    Viewport,
}

impl WheelRoute {
    fn of(modes: &Modes, shift: bool) -> Self {
        if modes.mouse_report() && !shift {
            WheelRoute::Program
        } else if modes.alt_screen && modes.alternate_scroll {
            WheelRoute::AlternateScroll
        } else {
            WheelRoute::Viewport
        }
    }

    fn name(self) -> &'static str {
        match self {
            WheelRoute::Program => "program",
            WheelRoute::AlternateScroll => "alternate_scroll",
            WheelRoute::Viewport => "viewport",
        }
    }
}

/// Records the routing decision of a scroll operation with the modes in force, and returns the route
/// that the operation takes.
fn record_wheel(
    log: &Events,
    surface_id: &str,
    lines: i32,
    col: u16,
    row: u16,
    shift: bool,
    modes: &Modes,
) -> WheelRoute {
    let route = WheelRoute::of(modes, shift);
    let mut fields = wheel_modes(modes);
    fields["lines"] = json!(lines);
    fields["col"] = json!(col);
    fields["row"] = json!(row);
    fields["shift"] = json!(shift);
    fields["route"] = json!(route.name());
    log.emit_for(
        Level::Info,
        "sidecar.vt.wheel",
        Subject::surface(surface_id),
        &fields,
    );
    route
}

/// Whether the host is copying the previous raster. Screen changes in the meantime stay dirty, and
/// the screen is neither drawn nor sent as JSON. If JSON is sent for every screen while output
/// piles up, the page spends its time on past screens, and input and scrolling fall behind. When
/// the host answers, the screen at that time is drawn once and sent.
fn hold_while_presenting(image_state: &mut Option<ImageState>) -> bool {
    match image_state.as_mut() {
        Some(state) if state.pending_draw || !state.drawing => {
            state.dirty = true;
            true
        }
        _ => false,
    }
}

/// The surface releases its image state. A transfer image that the host has not answered yet is
/// left to the connection that carried it, until the answer arrives or the connection ends. If no
/// connection carried it, no answer can come. It returns the remaining inline images.
fn release_image_state(surface_id: &str, state: ImageState) -> Vec<InlineImagePlacement> {
    let ImageState {
        name,
        frame,
        generation,
        raster,
        sequence,
        pending_draw,
        carrier,
        inline_images,
        ..
    } = state;
    if let (true, Some(carrier)) = (pending_draw, carrier) {
        carrier.retain(RetainedTransfer {
            surface: surface_id.to_string(),
            name,
            generation,
            raster,
            sequence,
            _frame: frame,
        });
    }
    inline_images
}

/// The transfer image that the host's answer (consumed or error) to a display request refers to.
struct ImageAnswer<'a> {
    name: &'a str,
    generation: u64,
    raster: u64,
    sequence: u64,
}

/// The name, generation, raster, and sequence number of the host's answer (consumed or error) to a
/// display request.
fn image_answer(image: &serde_json::Map<String, Value>) -> Option<ImageAnswer<'_>> {
    let answer = match image.get("consumed") {
        Some(consumed) => consumed.as_object()?,
        None if image.contains_key("error") => image,
        None => return None,
    };
    Some(ImageAnswer {
        name: answer.get("name")?.as_str()?,
        generation: answer.get("generation")?.as_u64()?,
        raster: answer.get("raster")?.as_u64()?,
        sequence: answer.get("sequence")?.as_u64()?,
    })
}

const MAX_JAVASCRIPT_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn parse_image_detach(value: &Value) -> Result<ImageDetach, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "image.detach must be an object".to_string())?;
    if object.len() != 3
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "request" | "name" | "generation"))
    {
        return Err("image.detach requires only request, name, and generation".to_string());
    }
    let request = object
        .get("request")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "image.detach request must be a non-empty string".to_string())?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "image.detach name must be a non-empty string".to_string())?;
    let generation = object
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0 && *value <= MAX_JAVASCRIPT_SAFE_INTEGER)
        .ok_or_else(|| {
            "image.detach generation must be a positive JavaScript-safe integer".to_string()
        })?;
    Ok(ImageDetach {
        request: request.to_string(),
        name: name.to_string(),
        generation,
    })
}

fn image_detach_response(
    surface: &str,
    detach: &ImageDetach,
    result: Option<&str>,
    error: Option<&str>,
    current_generation: Option<u64>,
) -> Value {
    let mut detached = json!({
        "request": detach.request,
        "name": detach.name,
        "generation": detach.generation,
        "ok": error.is_none(),
    });
    if let Some(result) = result {
        detached["result"] = json!(result);
    }
    if let Some(error) = error {
        detached["error"] = json!(error);
    }
    if let Some(generation) = current_generation {
        detached["currentGeneration"] = json!(generation);
    }
    json!({"surface": surface, "body": {"image": {"detached": detached}}})
}

/// Answers a detach that is refused before it reaches the surface. The answer echoes the request and
/// the name and generation that were received, so the host correlates it with its detach. A detach
/// without a nonempty string request has no answer, and the caller keeps its error envelope.
fn image_detach_refusal(
    surface: &str,
    detach: &Value,
    error: &str,
    reason: Option<&str>,
) -> Option<Value> {
    let object = detach.as_object()?;
    let request = object
        .get("request")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())?;
    let mut detached = json!({"request": request, "ok": false, "error": error});
    for field in ["name", "generation"] {
        if let Some(value) = object.get(field) {
            detached[field] = value.clone();
        }
    }
    if let Some(reason) = reason {
        detached["reason"] = json!(reason);
    }
    Some(json!({"surface": surface, "body": {"image": {"detached": detached}}}))
}

async fn send_state(
    surface_id: &str,
    session_id: &str,
    cols: u16,
    rows: u16,
    state: &ImageState,
    output_tx: &OutputSink,
) -> bool {
    let response = json!({
        "surface": surface_id,
        "body": {
            "event": "state", "sessionId": session_id, "cols": cols, "rows": rows,
            "cursor": {"col": 0, "row": 0},
            "cellWidth": state.metrics.cell_width / state.scale,
            "cellHeight": state.metrics.cell_height / state.scale
        }
    });
    output_tx.send(response.to_string()).await.is_ok()
}

fn clipboard_selection_name(selection: ClipboardSelection) -> &'static str {
    match selection {
        ClipboardSelection::Clipboard => "clipboard",
        ClipboardSelection::Selection => "selection",
    }
}

struct MultipartAssembly {
    name: String,
    data: Vec<u8>,
}

fn resolve_inline_dimension(dimension: &Dimension, cell_size: u32, frame_size: u32) -> u32 {
    match dimension {
        Dimension::Auto => 0,
        Dimension::Cells(value) => cell_size.saturating_mul(*value),
        Dimension::Pixels(value) => *value,
        Dimension::Percent(value) => frame_size.saturating_mul(u32::from(*value)) / 100,
    }
}

fn store_inline_display(
    command: InlineImageCommand,
    anchor: InlineAnchor,
    image_state: &mut Option<ImageState>,
) -> Result<(), String> {
    let InlineImageCommand::Display {
        name,
        data,
        width,
        height,
        preserve_aspect_ratio,
    } = command
    else {
        return Err("inline image command is not a display record".to_string());
    };
    let Some(state) = image_state.as_mut() else {
        return Err("inline image received before the image region was configured".to_string());
    };
    let placement = InlineImagePlacement {
        name: name.clone(),
        data,
        x: u32::from(anchor.col).saturating_mul(state.metrics.cell_width.round() as u32),
        y: u32::from(anchor.row).saturating_mul(state.metrics.cell_height.round() as u32),
        width: resolve_inline_dimension(
            &width,
            state.metrics.cell_width.round() as u32,
            state.width_px,
        ),
        height: resolve_inline_dimension(
            &height,
            state.metrics.cell_height.round() as u32,
            state.height_px,
        ),
        preserve_aspect_ratio,
        anchor_row: i32::from(anchor.row),
        anchor_scroll: anchor.scroll,
        visible: true,
    };
    if let Some(existing) = state
        .inline_images
        .iter_mut()
        .find(|image| image.name == name)
    {
        *existing = placement;
    } else {
        state.inline_images.push(placement);
    }
    Ok(())
}

fn refresh_inline_image_positions(
    engine: &mut Box<dyn Engine>,
    image_state: &mut Option<ImageState>,
) {
    let Some(state) = image_state.as_mut() else {
        return;
    };
    let current_scroll = engine.scroll_generation();
    // While scrollback is viewed, the viewport shows the rows moved down by that amount.
    let offset = engine.viewport_offset() as i32;
    let cell_height = state.metrics.cell_height.round() as u32;
    for image in &mut state.inline_images {
        let row = image.anchor_row - (current_scroll - image.anchor_scroll) as i32 + offset;
        image.visible = row >= 0;
        image.y = if row >= 0 {
            (row as u32).saturating_mul(cell_height)
        } else {
            state.height_px
        };
    }
}

fn apply_inline_image_command(
    command: InlineImageCommand,
    anchor: InlineAnchor,
    image_state: &mut Option<ImageState>,
    multipart: &mut Option<MultipartAssembly>,
) -> Result<(), String> {
    match command {
        InlineImageCommand::Display { .. } => store_inline_display(command, anchor, image_state),
        InlineImageCommand::Transfer { .. } => Ok(()),
        InlineImageCommand::MultipartStart { name } => {
            if multipart.is_some() {
                return Err("inline image multipart transfer is already active".to_string());
            }
            *multipart = Some(MultipartAssembly {
                name,
                data: Vec::new(),
            });
            Ok(())
        }
        InlineImageCommand::MultipartPart(data) => {
            let Some(assembly) = multipart.as_mut() else {
                return Err("inline image multipart part has no active transfer".to_string());
            };
            if assembly.data.len().saturating_add(data.len()) > crate::inline_image::MAX_IMAGE_BYTES
            {
                return Err(format!(
                    "inline image multipart payload exceeds {} bytes",
                    crate::inline_image::MAX_IMAGE_BYTES
                ));
            }
            assembly.data.extend_from_slice(&data);
            Ok(())
        }
        InlineImageCommand::MultipartEnd => {
            let Some(assembly) = multipart.take() else {
                return Err("inline image multipart end has no active transfer".to_string());
            };
            if assembly.data.is_empty() {
                return Err("inline image multipart transfer has no data".to_string());
            }
            store_inline_display(
                InlineImageCommand::Display {
                    name: assembly.name,
                    data: assembly.data,
                    width: Dimension::Auto,
                    height: Dimension::Auto,
                    preserve_aspect_ratio: true,
                },
                anchor,
                image_state,
            )
        }
    }
}

/// The objects that the surface actor uses for replies, the session, and performance records. They
/// do not change while the actor lives.
struct SurfacePorts<'a> {
    surface_id: &'a str,
    session_port: &'a Arc<dyn SessionPort>,
    output_tx: &'a OutputSink,
    log: &'a Events,
    inputs: &'a mut ReplayInputs,
    screens: &'a mut ScreenStream,
}

/// The inputs of the engine of one surface, recorded in the order in which the surface task applies
/// them (core docs/spec/replay.md#records): the theme, the output of the program, the input to the
/// program, each resize and each view change that took effect (the selection and the viewport).
/// `sequence` counts them from 1 without a gap, and a frame names the sequence of the last input
/// that its screen includes. The view changes are applied to the engine through the methods of this
/// type, so the engine call and its record cannot differ.
struct ReplayInputs {
    log: Events,
    sequence: u64,
    /// The inputs before the session existed: the event and its fields with `sequence`. They are
    /// recorded when it opens.
    deferred: Vec<(&'static str, Value)>,
}

impl ReplayInputs {
    fn new(log: Events) -> Self {
        Self {
            log,
            sequence: 0,
            deferred: Vec::new(),
        }
    }

    /// Gives the next `sequence` to an input and records it, or keeps it until the session exists.
    fn input(&mut self, session: Option<&str>, event: &'static str, mut fields: Value) {
        self.sequence += 1;
        fields["sequence"] = json!(self.sequence);
        match session {
            Some(session) => self.record(session, event, &fields),
            None => self.deferred.push((event, fields)),
        }
    }

    fn record(&self, session: &str, event: &'static str, fields: &Value) {
        // events: forwarded
        self.log
            .emit_for(Level::Info, event, Subject::session(session, &[]), fields);
    }

    /// The output of the program is fed to the engine. It is stored when the policy stores the
    /// class `io`.
    fn output(&mut self, session: &str, data: &[u8]) {
        self.sequence += 1;
        if self.log.gate(crate::policy::Class::Io, Level::Debug) {
            self.log.emit_for(
                Level::Debug,
                "sidecar.vt.pty_read",
                Subject::session(session, &[]),
                &json!({"sequence": self.sequence, "hex": crate::events::hex(data)}),
            );
        }
    }

    /// The engine was resized.
    fn resized(&mut self, session: Option<&str>, rows: u16, cols: u16) {
        self.input(
            session,
            "sidecar.vt.resize",
            json!({"rows": rows, "cols": cols}),
        );
    }

    /// The session exists: the inputs before it are recorded.
    fn opened(&mut self, session: &str) {
        for (event, fields) in std::mem::take(&mut self.deferred) {
            self.record(session, event, &fields);
        }
    }

    /// Puts `theme` in force in the engine and records the complete set of colors.
    fn set_theme(
        &mut self,
        engine: &mut dyn Engine,
        session: Option<&str>,
        theme: crate::palette::TerminalTheme,
    ) {
        engine.set_theme(theme);
        let hex = |color: [u8; 3]| format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]);
        let mut colors = serde_json::Map::new();
        colors.insert("foreground".into(), json!(hex(theme.foreground)));
        colors.insert("background".into(), json!(hex(theme.background)));
        colors.insert("cursor".into(), json!(hex(theme.cursor)));
        colors.insert("selection".into(), json!(hex(theme.selection)));
        for (index, color) in theme.palette.iter().enumerate() {
            colors.insert(format!("ansi{index}"), json!(hex(*color)));
        }
        self.input(session, "sidecar.vt.theme", json!({"colors": colors}));
    }

    fn selection(&mut self, session: Option<&str>, fields: Value) {
        self.input(session, "sidecar.vt.selection", fields);
    }

    /// Starts the selection at a cell boundary. A call that the engine refuses is an error and no
    /// input.
    fn selection_start(
        &mut self,
        engine: &mut dyn Engine,
        session: Option<&str>,
        edge: u16,
        row: u16,
    ) -> Result<(), String> {
        engine.selection_start(edge, row)?;
        self.selection(
            session,
            json!({"action": "start", "edge": edge, "row": row}),
        );
        Ok(())
    }

    /// Moves the end of the selection to a cell boundary.
    fn selection_update(
        &mut self,
        engine: &mut dyn Engine,
        session: Option<&str>,
        edge: u16,
        row: u16,
    ) -> Result<(), String> {
        engine.selection_update(edge, row)?;
        self.selection(
            session,
            json!({"action": "update", "edge": edge, "row": row}),
        );
        Ok(())
    }

    /// Ends the gesture and returns the text of the selection.
    fn selection_end(
        &mut self,
        engine: &mut dyn Engine,
        session: Option<&str>,
    ) -> Result<Option<String>, String> {
        let text = engine.selection_end()?;
        self.selection(session, json!({"action": "end"}));
        Ok(text)
    }

    /// Clears the selection; only a clear that removed a selection is an input.
    fn selection_clear(&mut self, engine: &mut dyn Engine, session: Option<&str>) -> bool {
        let cleared = engine.selection_clear();
        if cleared {
            self.selection(session, json!({"action": "clear"}));
        }
        cleared
    }

    /// Moves the viewport by `lines` (positive toward older lines). The input holds the change that
    /// the engine applied, which its limit to the retained lines may make smaller, and a move that
    /// changed nothing is no input.
    fn scroll_viewport(&mut self, engine: &mut dyn Engine, session: Option<&str>, lines: i32) {
        let before = i64::from(engine.viewport_offset());
        engine.scroll_viewport(lines);
        let moved = i64::from(engine.viewport_offset()) - before;
        if moved != 0 {
            self.input(session, "sidecar.vt.viewport", json!({"lines": moved}));
        }
    }

    /// Moves the viewport to the newest output; true when it moved, which is an input.
    fn scroll_to_newest(&mut self, engine: &mut dyn Engine, session: Option<&str>) -> bool {
        let moved = engine.scroll_to_newest();
        if moved {
            self.input(session, "sidecar.vt.viewport", json!({"newest": true}));
        }
        moved
    }

    /// Writes `data` to the session and records the write with the `cids` of the inputs that caused
    /// it.
    async fn write(
        &mut self,
        port: &Arc<dyn SessionPort>,
        session: &str,
        data: &[u8],
        cids: &[String],
    ) -> Result<(), String> {
        self.sequence += 1;
        let result = port.write(session, data).await;
        let mut fields = json!({
            "sequence": self.sequence,
            "hex": crate::events::hex(data),
            "ok": result.is_ok(),
        });
        if let Err(error) = &result {
            fields["error"] = json!(error);
        }
        self.log.emit_for(
            Level::Info,
            "sidecar.vt.pty_write",
            Subject::session(session, cids),
            &fields,
        );
        result
    }
}

async fn send_engine_events(
    ports: SurfacePorts<'_>,
    session_id: Option<&str>,
    engine: &mut Box<dyn Engine>,
    emit_surface_events: bool,
    image_state: &mut Option<ImageState>,
    multipart: &mut Option<MultipartAssembly>,
) -> bool {
    let SurfacePorts {
        surface_id,
        session_port,
        output_tx,
        log,
        inputs,
        screens: _,
    } = ports;
    for event in engine.drain_events() {
        match event {
            EngineEvent::InlineImage { command, anchor } => {
                let command_for_event = command.clone();
                if let Err(error) =
                    apply_inline_image_command(command, anchor, image_state, multipart)
                {
                    // The command is dropped whether or not the page is told, so the service
                    // records it.
                    log.record_error("inline image", &error);
                    if !emit_surface_events {
                        continue;
                    }
                    let response = json!({
                        "surface": surface_id,
                        "body": {"event": "error", "reason": error}
                    });
                    if output_tx.send(response.to_string()).await.is_err() {
                        return false;
                    }
                    continue;
                }
                if !emit_surface_events {
                    continue;
                }
                let body = match command_for_event {
                    InlineImageCommand::Display {
                        name,
                        data,
                        width,
                        height,
                        preserve_aspect_ratio,
                    } => json!({
                        "event": "image.inline",
                        "command": "display",
                        "name": name,
                        "data": base64_encode(&data),
                        "width": inline_dimension(&width),
                        "height": inline_dimension(&height),
                        "preserveAspectRatio": preserve_aspect_ratio,
                    }),
                    InlineImageCommand::Transfer { name, data } => json!({
                        "event": "image.inline",
                        "command": "transfer",
                        "name": name,
                        "data": base64_encode(&data),
                    }),
                    InlineImageCommand::MultipartStart { name } => json!({
                        "event": "image.inline",
                        "command": "multipart.start",
                        "name": name,
                    }),
                    InlineImageCommand::MultipartPart(data) => json!({
                        "event": "image.inline",
                        "command": "multipart.part",
                        "data": base64_encode(&data),
                    }),
                    InlineImageCommand::MultipartEnd => json!({
                        "event": "image.inline",
                        "command": "multipart.end",
                    }),
                };
                let event = json!({"surface": surface_id, "body": body}).to_string();
                // When there is an image area, the event is sent with the display that drew that
                // image. Output processing draws the screen after this event.
                match image_state.as_mut() {
                    Some(state) => state.presentation_events.push(event),
                    None => {
                        if output_tx.send(event).await.is_err() {
                            return false;
                        }
                    }
                }
            }
            EngineEvent::PtyWrite(bytes) => {
                let Some(session_id) = session_id else {
                    let response = json!({"surface": surface_id, "body": {"error": "engine response without session"}});
                    return output_tx.reply(response).await.is_ok();
                };
                if let Err(error) = inputs.write(session_port, session_id, &bytes, &[]).await {
                    let response = json!({"surface": surface_id, "body": {"error": "engine response write failed", "reason": error}});
                    return output_tx.reply(response).await.is_ok();
                }
            }
            EngineEvent::Title(title) => {
                if !emit_surface_events {
                    continue;
                }
                let response =
                    json!({"surface": surface_id, "body": {"event": "title", "title": title}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ResetTitle => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "title.reset"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Directory { uri, path } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "directory", "uri": uri, "path": path}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Hyperlink { id, uri } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "hyperlink", "id": id, "uri": uri}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::PointerShape(shape) => {
                if !emit_surface_events {
                    continue;
                }
                let response =
                    json!({"surface": surface_id, "body": {"event": "pointer", "shape": shape}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Notification { message } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "notification", "message": message}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ShellState { marker, params } => {
                if !emit_surface_events {
                    continue;
                }
                let marker = match marker {
                    ShellMarker::PromptStart => "prompt.start",
                    ShellMarker::PromptEnd => "prompt.end",
                    ShellMarker::CommandStart => "command.start",
                    ShellMarker::CommandFinished => "command.finished",
                };
                let response = json!({"surface": surface_id, "body": {"event": "vendor.shell.state", "marker": marker, "params": params}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ClipboardStore { selection, text } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "clipboard.store", "selection": clipboard_selection_name(selection), "text": text}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ClipboardQuery {
                request_id,
                selection,
            } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "clipboard.query", "requestId": request_id, "selection": clipboard_selection_name(selection)}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::CursorBlinkingChange => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "cursor.blinking"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Wakeup => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "wakeup"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Bell => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "bell"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Exit => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "exit"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ChildExit { success, code } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "child.exit", "success": success, "code": code}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::MouseCursorDirty => {
                if !emit_surface_events {
                    continue;
                }
                let response =
                    json!({"surface": surface_id, "body": {"event": "mouse.cursor.dirty"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::RuntimeError(reason) => {
                log.record_error("engine", reason);
            }
            EngineEvent::Error(reason) => {
                if !emit_surface_events {
                    continue;
                }
                // An engine rejection concerns the sequence of the program output. The page records
                // it as a record, not as a terminal error.
                let response = json!({"surface": surface_id, "body": {"event": "sequence.rejected", "reason": reason}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
        }
    }
    refresh_inline_image_positions(engine, image_state);
    true
}

fn inline_dimension(dimension: &Dimension) -> Value {
    match dimension {
        Dimension::Auto => Value::String("auto".to_string()),
        Dimension::Cells(value) => json!(value),
        Dimension::Pixels(value) => Value::String(format!("{value}px")),
        Dimension::Percent(value) => Value::String(format!("{value}%")),
    }
}

async fn open_headless(
    surface_id: &str,
    shell: &ShellRequest,
    session_id: &mut Option<String>,
    engine: &mut Box<dyn Engine>,
    session_port: &Arc<dyn SessionPort>,
    output_tx: &OutputSink,
    inputs: &mut ReplayInputs,
) -> bool {
    if session_id.is_some() {
        return true;
    }
    engine.resize(80, 24);
    inputs.resized(None, 24, 80);
    match session_port.open(shell, 80, 24).await {
        Ok(id) => {
            inputs.opened(&id);
            *session_id = Some(id);
            true
        }
        Err(error) => {
            let response = json!({"surface": surface_id, "body": {"error": format!("Failed to open headless session: {error}")}});
            output_tx.reply(response).await.is_ok()
        }
    }
}

async fn open_if_configured(
    ports: SurfacePorts<'_>,
    requested: bool,
    requested_image: &Option<String>,
    shell: &ShellRequest,
    session_id: &mut Option<String>,
    engine: &mut Box<dyn Engine>,
    image_state: &mut Option<ImageState>,
) -> bool {
    let SurfacePorts {
        surface_id,
        session_port,
        output_tx,
        log,
        inputs,
        screens,
    } = ports;
    if !requested || session_id.is_some() {
        return true;
    }
    let Some(state) = image_state.as_mut() else {
        return true;
    };
    if requested_image
        .as_ref()
        .is_some_and(|name| name != &state.name)
    {
        return true;
    }
    let (cols, rows) =
        match calculate_terminal_size(state.width_px, state.height_px, &state.metrics) {
            Ok(size) => size,
            // A raster with no room for one cell is a transient state in which the card collapses
            // during a drag. An error here would discard the open request, and the session would
            // not reopen at a later size. Without opening, it waits, and the next configure opens
            // the session (V5-96-14-6-4-8).
            Err(reason) => {
                record_empty_raster(log, surface_id, state.width_px, state.height_px, &reason);
                return true;
            }
        };
    crate::engine_trace::record(log, surface_id, || format!("resize {cols} {rows}"));
    engine.resize(cols, rows);
    inputs.resized(session_id.as_deref(), rows, cols);
    if let Err(error) = set_engine_metrics(engine, state) {
        let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
        return output_tx.reply(response).await.is_ok();
    }
    match session_port.open(shell, cols, rows).await {
        Ok(sid) => {
            *session_id = Some(sid.clone());
            state.session = Some(sid.clone());
            inputs.opened(&sid);
            if !send_state(surface_id, &sid, cols, rows, state, output_tx).await {
                return false;
            }
            let screen = engine.screen();
            present_screen(
                surface_id,
                screen,
                state,
                output_tx,
                "open",
                log,
                screens,
                inputs.sequence,
            )
            .await
        }
        Err(error) => {
            let response = json!({"surface": surface_id, "body": {"error": format!("Failed to open: {error}")}});
            output_tx.reply(response).await.is_ok()
        }
    }
}

fn newer_configuration(configuration: &ImageConfiguration, state: &ImageState) -> bool {
    (configuration.generation, configuration.raster) > (state.generation, state.raster)
}

/// Converts the surfaces list `[{surface, root}]` of a retain request to session keys. A malformed
/// list is an error.
fn retain_keys(value: &Value) -> Result<std::collections::HashSet<String>, String> {
    let surfaces = value
        .get("surfaces")
        .and_then(Value::as_array)
        .ok_or("retain requires a surfaces array")?;
    surfaces
        .iter()
        .map(|item| {
            let surface = item.get("surface").and_then(Value::as_str);
            let root = item.get("root").and_then(Value::as_str);
            match (surface, root) {
                (Some(surface), Some(root)) if !surface.is_empty() && !root.is_empty() => {
                    Ok(format!("{root}\0{surface}"))
                }
                _ => Err(format!("retain surface entry is invalid: {item}")),
            }
        })
        .collect()
}

/// Closes one surface of this connection. If a persistent registry exists, it closes the surface
/// there, and it also clears the sender and the attachment epoch of this connection. If the entry
/// stays, reopening the same surface compares it with the epoch of the cleared registry entry, and
/// the stale attachment is rejected (closed notice and close request).
async fn close_connection_surface(
    registry: Option<&Arc<PersistentRegistry>>,
    surface_txs: &mut HashMap<String, mpsc::Sender<Dispatch>>,
    surface_epochs: &mut HashMap<String, u64>,
    registry_key: &str,
    owner: &str,
) -> Result<(), String> {
    if let Some(registry) = registry {
        let closed = registry.close_surface(registry_key, owner).await;
        if closed.is_ok() {
            surface_txs.remove(registry_key);
            surface_epochs.remove(registry_key);
        }
        closed
    } else if let Some(tx) = surface_txs.remove(registry_key) {
        close_surface_actor(&tx, None, "close").await
    } else {
        Ok(())
    }
}

/// Clears the sender and the attachment epoch of each surface of this connection whose registry
/// entry an owner close or a retain removed. If they stay, the next message of the surface compares
/// its epoch with the removed entry and is refused as a stale attachment, although no other
/// attachment took the surface.
async fn forget_closed_surfaces(
    registry: &Arc<PersistentRegistry>,
    surface_txs: &mut HashMap<String, mpsc::Sender<Dispatch>>,
    surface_epochs: &mut HashMap<String, u64>,
) {
    let mut closed = Vec::new();
    for key in surface_txs.keys() {
        if !registry.contains(key).await {
            closed.push(key.clone());
        }
    }
    for key in closed {
        surface_txs.remove(&key);
        surface_epochs.remove(&key);
    }
}

fn local_surface_key(
    surface_txs: &HashMap<String, mpsc::Sender<Dispatch>>,
    root: Option<&str>,
    surface: &str,
) -> String {
    if let Some(root) = root {
        return format!("{root}\0{surface}");
    }
    surface_txs
        .keys()
        .find(|key| key.ends_with(&format!("\0{surface}")))
        .cloned()
        // default: a surface that came without a root and is not found among the registered keys
        // gets a key without a root, which the lookup reports as an unknown surface.
        .unwrap_or_else(|| format!("\0{surface}"))
}

/// Asynchronous work per surface. It owns the engine and the daemon connection, and processes
/// commands.
async fn surface_task(
    surface_id: String,
    log: Events,
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    session_port: Arc<dyn SessionPort>,
    mut cmd_rx: mpsc::Receiver<Dispatch>,
    output_tx: OutputSink,
) {
    let mut engine = engine_factory();
    let mut inputs = ReplayInputs::new(log.clone());
    let mut screens = ScreenStream::default();
    let mut session_id: Option<String> = None;
    let mut daemon_events_rx = session_port.get_events().await;
    let mut image_state: Option<ImageState> = None;
    let mut preserved_inline_images = Vec::new();
    let mut multipart: Option<MultipartAssembly> = None;
    // The wheel modes of the last `sidecar.vt.mouse_modes` record of this surface.
    let mut recorded_mouse_modes = wheel_modes(&Modes::default());
    let mut open_requested = false;
    let mut requested_image: Option<String> = None;
    let mut requested_shell = ShellRequest::default();
    let mut headless = false;
    let mut pending_configuration: Option<ImageConfiguration> = None;
    // The drawing state that the host last sent for the image region of the surface. A surface
    // draws until the first state arrives, because the host sends it at the attachment of the
    // region, before the first configure.
    let mut drawing = true;
    // The generation of the last applied drawing state. A state of an earlier generation belongs to
    // an earlier attachment of the region and is ignored.
    let mut drawing_generation = 0;
    let mut latest_detached_image: Option<(String, u64)> = None;
    let mut last_image_detach: Option<ImageDetachReplay> = None;
    let mut image_owner_revision = 0_u64;
    let mut focused = false;
    // A press that started with mouse reporting. Selection operations select nothing until it is
    // released.
    let mut mouse_gesture = false;
    // The motion ownership is fixed at button-down time. The terminal can change the mouse mode
    // while a drag is in progress. If the mode is read again at every move, one gesture is split
    // between the TUI and the page selection.
    let mut mouse_gesture_motion = false;
    let mut mouse_cell: Option<(u16, u16)> = None;
    // A selection release describes the raster that was created just before it. The event is held
    // until that raster is consumed, so the client cannot observe the copy acknowledgement while
    // the previous selection remains on the screen.
    let mut pending_selection_event: Option<String> = None;
    // A release that arrived while the event of the previous release waited, with the output that
    // answers its request.
    let mut deferred_selection_end: Option<OutputSink> = None;
    let mut preedit: Option<Preedit> = None;
    let mut current_theme = crate::palette::TerminalTheme::dark();
    // The first input of a session is the theme in force, so a replay starts from it and not from a
    // default of its own.
    inputs.set_theme(engine.as_mut(), None, current_theme);
    let mut cursor_policy = CursorPolicy::default();
    // The terminal font of this surface. Until a font request, it is the system fixed-pitch font.
    let mut terminal_font = crate::platform::default_font();
    // Font size (points). The font operation sets it (docs/spec/text-size.md).
    let mut terminal_font_size = DEFAULT_FONT_SIZE;
    let mut cursor_activity = Instant::now();
    let mut cursor_tick = tokio::time::interval(Duration::from_millis(50));
    cursor_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_cursor_frame: Option<(CursorShape, bool, bool, bool)> = None;

    loop {
        tokio::select! {
            _ = cursor_tick.tick(), if image_state.is_some() && !headless => {
                // If cursor blinking is turned off entirely, this tick does nothing
                // (V5-96-14-6-4-9). engine.screen() copies the whole grid, so copying the whole
                // screen every 50ms to check whether it changed was the cause of the idle CPU.
                // Measured: while idle, blink frames occurred 0.15 times per second (negligible),
                // but the tick ran 20 times per second and copied the screen each time. If the
                // cursor does not blink, the screen does not change, so there is no need to copy
                // it.
                let blink_enabled = focused
                    && !matches!(cursor_policy.blink, crate::platform::darwin::frame::CursorBlinkPolicy::Never)
                    && (cursor_policy.idle_timeout_ms == 0
                        || (cursor_activity.elapsed().as_millis() as u64) < cursor_policy.idle_timeout_ms)
                    && engine.screen().cursor.visible;
                if !blink_enabled {
                    continue;
                }
                let screen = decorate_screen(
                    engine.screen(),
                    focused,
                    &preedit,
                    &cursor_policy,
                    cursor_activity.elapsed().as_millis() as u64,
                );
                let signature = (
                    screen.cursor.shape,
                    screen.cursor.blink_visible,
                    screen.cursor.focused,
                    screen.cursor.visible,
                );
                if last_cursor_frame != Some(signature)
                    && let Some(state) = image_state.as_mut() {
                        if state.pending_draw {
                            state.dirty = true;
                        } else if !present_screen(&surface_id, screen, state, &output_tx, "blink", &log, &mut screens, inputs.sequence).await {
                            return;
                        } else {
                            last_cursor_frame = Some(signature);
                        }
                    }
            }
            Some(Dispatch { command: cmd, request }) = cmd_rx.recv() => {
                // The replies of this command answer the request that sent it.
                let output_tx = match request {
                    Some(request) => {
                        request.enter(RequestStage::Actor);
                        output_tx.bound(request)
                    }
                    None => output_tx.clone(),
                };
                match cmd {
                    SurfaceCommand::Open { image, request } => {
                        // An open without image for a surface that was opened with an image or
                        // holds a configured image comes from a second owner of the surface. It
                        // would make the surface headless while its image stays shown, so the
                        // output would be fed and not drawn; it is refused and changes nothing.
                        if image.is_none() && (requested_image.is_some() || image_state.is_some()) {
                            let shown = requested_image.as_deref()
                                .or(image_state.as_ref().map(|state| state.name.as_str()))
                                .unwrap_or_default();
                            let reason = format!("surface {surface_id} has the image {shown}; an open without image is refused");
                            log.record_error("open", &reason);
                            let response = json!({"surface": surface_id, "body": {"error": "imageAttached", "reason": reason}});
                            if output_tx.reply(response).await.is_err() { return; }
                            continue;
                        }
                        if image.is_some() {
                            image_owner_revision = image_owner_revision.saturating_add(1);
                        }
                        open_requested = true;
                        requested_image = image;
                        requested_shell = request;
                        headless = requested_image.is_none();
                        record_surface_mode(&log, &surface_id, headless, "open");
                        if headless && !open_headless(&surface_id, &requested_shell, &mut session_id, &mut engine, &session_port, &output_tx, &mut inputs).await { return; }
                        if !open_if_configured(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, log: &log, inputs: &mut inputs, screens: &mut screens }, open_requested, &requested_image, &requested_shell, &mut session_id,
                            &mut engine, &mut image_state).await {
                            return;
                        }
                    }
                    SurfaceCommand::Reconnect => {
                        // PTY and VT state belong to the persistent session, and the IOSurface
                        // belongs to the application instance. Discarding only the previous native
                        // image lets the client that reconnects provide a new Configure message.
                        if let Some(previous_state) = image_state.take() {
                            preserved_inline_images = release_image_state(&surface_id, previous_state);
                        }
                        pending_configuration = None;
                        // The reconnected client holds no screen of the surface, so its first screen is the full form.
                        screens.forget();
                        if !headless
                            && let Some(session_id) = session_id.as_deref() {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"event": "session", "sessionId": session_id}
                            });
                            if output_tx.send(response.to_string()).await.is_err() {
                                return;
                            }
                            }
                    }
                    SurfaceCommand::ImageDetach { detach, request_check } => {
                        let current = [
                            image_state
                                .as_ref()
                                .map(|state| (state.name.as_str(), state.generation)),
                            pending_configuration
                                .as_ref()
                                .map(|configuration| {
                                    (configuration.name.as_str(), configuration.generation)
                                }),
                        ]
                        .into_iter()
                        .flatten()
                        .max_by_key(|(_, generation)| *generation);
                        let current_generation = current.map(|(_, generation)| generation);
                        let request_replay = (request_check == ImageDetachRequestCheck::Replay)
                            .then(|| {
                                last_image_detach
                                    .as_ref()
                                    .filter(|replay| replay.detach.request == detach.request)
                            })
                            .flatten();

                        let request_mismatch = request_check == ImageDetachRequestCheck::Mismatch;
                        let response = if request_mismatch {
                            image_detach_response(
                                &surface_id,
                                &detach,
                                None,
                                Some("requestMismatch"),
                                current_generation,
                            )
                        } else if let Some(replay) = request_replay {
                            if replay.surface != surface_id
                                || replay.detach.name != detach.name
                                || replay.detach.generation != detach.generation
                            {
                                image_detach_response(
                                    &surface_id,
                                    &detach,
                                    None,
                                    Some("requestMismatch"),
                                    current_generation,
                                )
                            } else if replay.owner_revision != image_owner_revision
                            {
                                image_detach_response(
                                    &surface_id,
                                    &detach,
                                    None,
                                    Some(if current_generation
                                        .is_some_and(|generation| generation > detach.generation)
                                    {
                                        "staleGeneration"
                                    } else {
                                        "requestMismatch"
                                    }),
                                    current_generation,
                                )
                            } else {
                                replay.response.clone()
                            }
                        } else {
                            let failure = if let Some((current_name, generation)) = current {
                                if current_name != detach.name
                                    || requested_image
                                        .as_deref()
                                        .is_some_and(|requested| requested != detach.name)
                                {
                                    Some("imageNameMismatch")
                                } else if detach.generation < generation {
                                    Some("staleGeneration")
                                } else if detach.generation > generation {
                                    Some("futureGeneration")
                                } else {
                                    None
                                }
                            } else if let Some(requested) = requested_image.as_deref() {
                                if requested != detach.name {
                                    Some("imageNameMismatch")
                                } else if latest_detached_image.as_ref().is_some_and(
                                    |(name, generation)| {
                                        name == &detach.name && detach.generation <= *generation
                                    },
                                ) {
                                    Some("staleGeneration")
                                } else {
                                    None
                                }
                            } else if latest_detached_image.as_ref().is_some_and(
                                |(name, generation)| {
                                    name == &detach.name && detach.generation < *generation
                                },
                            ) {
                                Some("staleGeneration")
                            } else {
                                None
                            };

                            if let Some(error) = failure {
                                image_detach_response(
                                    &surface_id,
                                    &detach,
                                    None,
                                    Some(error),
                                    current_generation.or_else(|| {
                                        latest_detached_image.as_ref().and_then(
                                            |(name, generation)| {
                                                (name == &detach.name).then_some(*generation)
                                            },
                                        )
                                    }),
                                )
                            } else if current.is_none()
                                && requested_image.is_none()
                                && latest_detached_image.as_ref().is_some_and(
                                    |(name, generation)| {
                                        name == &detach.name && *generation == detach.generation
                                    },
                                )
                            {
                                image_detach_response(
                                    &surface_id,
                                    &detach,
                                    Some("alreadyDetached"),
                                    None,
                                    current_generation,
                                )
                            } else if current.is_none() && requested_image.is_none() {
                                image_detach_response(
                                    &surface_id,
                                    &detach,
                                    Some("alreadyAbsent"),
                                    None,
                                    current_generation,
                                )
                            } else {
                                if let Some(state) = image_state.take() {
                                    preserved_inline_images
                                        .extend(release_image_state(&surface_id, state));
                                }
                                if pending_configuration.as_ref().is_some_and(|configuration| {
                                    configuration.name == detach.name
                                        && configuration.generation == detach.generation
                                }) {
                                    pending_configuration = None;
                                }
                                if requested_image.as_deref() == Some(detach.name.as_str()) {
                                    requested_image = None;
                                }
                                headless = true;
                                latest_detached_image =
                                    Some((detach.name.clone(), detach.generation));
                                image_owner_revision = image_owner_revision.saturating_add(1);
                                record_surface_mode(&log, &surface_id, true, "image.detach");
                                image_detach_response(
                                    &surface_id,
                                    &detach,
                                    Some("detached"),
                                    None,
                                    None,
                                )
                            }
                        };
                        if !request_mismatch {
                            last_image_detach = Some(ImageDetachReplay {
                                surface: surface_id.clone(),
                                detach,
                                response: response.clone(),
                                owner_revision: image_owner_revision,
                            });
                        }
                        if output_tx.reply(response).await.is_err() {
                            return;
                        }
                    }
                    SurfaceCommand::Drawing { name, generation, drawing: next } => {
                        let current = image_state.as_ref().map_or(drawing_generation, |state| state.generation.max(drawing_generation));
                        if generation < current {
                            continue;
                        }
                        drawing_generation = generation;
                        drawing = next;
                        let Some(state) = image_state.as_mut() else { continue };
                        if state.name != name {
                            continue;
                        }
                        state.drawing = next;
                        // The region draws again: the screen that changed while it did not is drawn
                        // and sent once. A frame in flight draws it after its answer.
                        if next && state.dirty && !state.pending_draw && session_id.is_some() {
                            let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                            if !present_screen(&surface_id, screen, state, &output_tx, "drawing", &log, &mut screens, inputs.sequence).await {
                                return;
                            }
                        }
                    }
                    SurfaceCommand::Configure(configuration) => {
                        if image_state.is_none()
                            && pending_configuration.is_none()
                            && latest_detached_image.as_ref().is_some_and(|(name, generation)| {
                                name == &configuration.name && configuration.generation <= *generation
                            })
                        {
                            let current_generation = latest_detached_image
                                .as_ref()
                                .map(|(_, generation)| *generation);
                            let response = json!({"surface": surface_id,
                                "body": {"error": "staleGeneration", "currentGeneration": current_generation}});
                            if output_tx.reply(response).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        if let Some(state) = image_state.as_ref() {
                            if !newer_configuration(&configuration, state) {
                                continue;
                            }
                            if state.pending_draw {
                                let replace = pending_configuration.as_ref().is_none_or(|pending|
                                    (configuration.generation, configuration.raster) > (pending.generation, pending.raster));
                                if replace {
                                    pending_configuration = Some(configuration);
                                    image_owner_revision = image_owner_revision.saturating_add(1);
                                }
                                continue;
                            }
                        }
                        let new_state = match ImageState::new(&configuration, &terminal_font, terminal_font_size) {
                            Ok(state) => state,
                            Err(reason) => {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"error": "image creation failed", "reason": reason}
                                });
                                if output_tx.reply(response).await.is_err() { return; }
                                continue;
                            }
                        };
                        let mut new_state = new_state;
                        new_state.theme = current_theme;
                        new_state.drawing = drawing;
                        new_state.session = session_id.clone();
                        new_state.inline_images = image_state.as_ref()
                            .map(|previous_state| previous_state.inline_images.clone())
                            // default: without an earlier image state the surface takes over the
                            // inline images that it kept in between.
                            .unwrap_or_else(|| std::mem::take(&mut preserved_inline_images));
                        let (cols, rows) = match calculate_terminal_size(
                            configuration.width, configuration.height, &new_state.metrics) {
                            Ok(size) => size,
                            // A raster with no room for one cell is a transient state in which the
                            // card collapses during a drag. Keeping the current grid and passing
                            // through the normal path (a resize to the same size changes nothing)
                            // clips the drawing to the IOSurface boundary and displays the frame,
                            // so the host's display barrier does not stall (V5-96-14-6-4-8).
                            Err(reason) => {
                                record_empty_raster(&log, &surface_id, configuration.width, configuration.height, &reason);
                                let screen = engine.screen();
                                (screen.cols, screen.rows)
                            }
                        };
                        crate::engine_trace::record(&log, &surface_id, || format!("resize {cols} {rows}"));
                        engine.resize(cols, rows);
                        inputs.resized(session_id.as_deref(), rows, cols);
                        if let Err(error) = set_engine_metrics(&mut engine, &new_state) {
                            let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
                            if output_tx.reply(response).await.is_err() { return; }
                            continue;
                        }
                        image_state = Some(new_state);
                        image_owner_revision = image_owner_revision.saturating_add(1);
                        record_surface_mode(&log, &surface_id, headless, "configure");
                        refresh_inline_image_positions(&mut engine, &mut image_state);
                        if let Some(sid) = session_id.as_ref() {
                            if let Err(error) = session_port.resize(sid, cols, rows).await {
                                let response = json!({"surface": surface_id,
                                    "body": {"error": format!("Resize failed: {error}")}});
                                if output_tx.reply(response).await.is_err() { return; }
                            } else if !send_state(&surface_id, sid, cols, rows, image_state.as_ref().unwrap(), &output_tx).await {
                                return;
                            }
                            let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                            if !present_screen(&surface_id, screen, image_state.as_mut().unwrap(), &output_tx, "resize", &log, &mut screens, inputs.sequence).await {
                                return;
                            }
                        } else if !open_if_configured(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, log: &log, inputs: &mut inputs, screens: &mut screens }, open_requested, &requested_image, &requested_shell, &mut session_id,
                            &mut engine, &mut image_state).await {
                            return;
                        }
                    }
                    SurfaceCommand::InputTransaction { actions, preedit: next_preedit, cids } => {
                        let modes = engine.modes();
                        let mut next_focused = focused;
                        let mut bytes = Vec::new();
                        let encoding = (|| -> Result<(), String> {
                            for action in &actions {
                                match action {
                                    InputAction::Insert { text, replacement_range, attributed } => {
                                        let _ = (replacement_range, attributed);
                                        bytes.extend_from_slice(text.as_bytes());
                                    }
                                    InputAction::Key(key) | InputAction::Command(key) => {
                                        bytes.extend_from_slice(&encode_keys(std::slice::from_ref(key), &modes)?);
                                    }
                                    InputAction::Focus(next) => {
                                        if *next != next_focused && modes.focus_in_out {
                                            bytes.extend_from_slice(if *next { b"\x1b[I" } else { b"\x1b[O" });
                                        }
                                        next_focused = *next;
                                    }
                                }
                            }
                            Ok(())
                        })();
                        if let Err(error) = encoding {
                            let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                            if output_tx.reply(response).await.is_err() { return; }
                            continue;
                        }
                        if !bytes.is_empty() && session_id.is_none() {
                            let response = json!({"surface": surface_id, "body": {"error": "Session not open"}});
                            if output_tx.reply(response).await.is_err() { return; }
                            continue;
                        }

                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        let viewport_changed =
                            inputs.scroll_to_newest(engine.as_mut(), session_id.as_deref());
                        if viewport_changed {
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                        }
                        let presentation_changed = viewport_changed
                            || next_focused != focused
                            || preedit_changes_render(&preedit, &next_preedit);
                        let write_result = match (session_id.as_deref(), bytes.is_empty()) {
                            (Some(session), false) => inputs.write(&session_port, session, &bytes, &cids).await,
                            _ => Ok(()),
                        };
                        focused = next_focused;
                        preedit = Some(next_preedit);

                        let mut queued_output = false;
                        let mut exited = false;
                        while let Ok(event) = daemon_events_rx.try_recv() {
                            match event {
                                DaemonEvent::Output { session_id: ref received, data, truncated, .. }
                                    if session_id.as_deref() == Some(received) => {
                                        queued_output = true;
                                        cursor_activity = Instant::now();
                                        last_cursor_frame = None;
                                        if truncated {
                                            crate::engine_trace::record(&log, &surface_id, || "reset".to_string());
                                            engine.reset();
                                        }
                                        crate::engine_trace::record(&log, &surface_id, || format!("feed {}", base64_encode(&data)));
                                        inputs.output(received, &data);
                                        engine.feed(&data);
                                        record_mouse_modes(&log, &surface_id, &mut recorded_mouse_modes, &engine.modes());
                                        if !send_engine_events(
                                            SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, log: &log, inputs: &mut inputs, screens: &mut screens },
                                            session_id.as_deref(), &mut engine, !headless, &mut image_state, &mut multipart,
                                        ).await { return; }
                                    }
                                DaemonEvent::Exit { session_id: ref received }
                                    if session_id.as_deref() == Some(received) => exited = true,
                                DaemonEvent::Error { session_id: ref received, error }
                                    if session_id.as_deref() == Some(received)
                                        && output_tx.send(json!({"surface": surface_id, "body": {"event": "error", "reason": error}}).to_string()).await.is_err() => return,
                                _ => {}
                            }
                        }

                        if presentation_changed || (queued_output && !headless) {
                            if let Some(state) = image_state.as_mut() {
                                if state.pending_draw {
                                    state.dirty = true;
                                } else {
                                    let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                    if !present_screen(&surface_id, screen, state, &output_tx, "input", &log, &mut screens, inputs.sequence).await { return; }
                                }
                            } else {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if !send_plain_screen(&surface_id, session_id.as_deref(), &screen, &output_tx, &log, &mut screens, inputs.sequence, image_state.as_ref()).await { return; }
                            }
                        }

                        let body = match write_result {
                            Ok(()) => json!({"ack": true}),
                            Err(error) => json!({"error": format!("Write failed for submitted action batch; PTY write progress is unknown: {error}")}),
                        };
                        if output_tx.reply(json!({"surface": surface_id, "body": body})).await.is_err() { return; }
                        if exited {
                            if output_tx.send(json!({"surface": surface_id, "body": {"event": "exit"}}).to_string()).await.is_err() { return; }
                            return;
                        }
                    }
                    SurfaceCommand::Paste { text, cids } => {
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        // Input is written after the viewport is returned to the newest output.
                        if inputs.scroll_to_newest(engine.as_mut(), session_id.as_deref()) {
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                            if !hold_while_presenting(&mut image_state) {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if let Some(state) = image_state.as_mut() {
                                    if !present_screen(&surface_id, screen, state, &output_tx, "paste", &log, &mut screens, inputs.sequence).await { return; }
                                } else if !send_plain_screen(&surface_id, session_id.as_deref(), &screen, &output_tx, &log, &mut screens, inputs.sequence, image_state.as_ref()).await { return; }
                            }
                        }
                        if let Some(ref sid) = session_id {
                            let bytes = match encoding::encode_paste(&text, &engine.modes()) {
                                Ok(bytes) => bytes,
                                Err(error) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": error}
                                    });
                                    if output_tx.reply(response).await.is_err() {
                                        return;
                                    }
                                    continue;
                                }
                            };
                            match inputs.write(&session_port, sid, &bytes, &cids).await {
                                Ok(()) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"ack": true, "event": "paste"}
                                    });
                                    if output_tx.reply(response).await.is_err() {
                                        return;
                                    }
                                }
                                Err(error) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": format!("Paste failed: {error}")}
                                    });
                                    if output_tx.reply(response).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        } else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "Session not open"}
                            });
                            if output_tx.reply(response).await.is_err() {
                                return;
                            }
                        }
                    }
                    SurfaceCommand::Mouse { input_id, phase, x, y, pressed, shift, alt, ctrl, cids } => {
                        let modes = engine.modes();
                        let cell = match image_state.as_ref()
                            .ok_or_else(|| "mouse image is not configured".to_string())
                            .and_then(|state| state.selection_cell(x, y)) {
                            Ok(cell) => cell,
                            Err(reason) => {
                                // A failure is also the mouse result of that operation. The same
                                // inputId is carried, so the page does not lose its pairing with
                                // the next result.
                                let phase_name = match phase { MousePhase::Down => "down", MousePhase::Move => "move", MousePhase::Up => "up" };
                                let response = json!({"surface": surface_id, "body": {
                                    "event": "mouse", "inputId": input_id, "phase": phase_name, "x": null, "y": null,
                                    "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                    "reported": false, "written": false,
                                    "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                    "bytes": null, "error": format!("invalidParams: {reason}")
                                }});
                                if output_tx.reply(response).await.is_err() { return; }
                                continue;
                            }
                        };
                        // When the program turns on mouse reporting, an ordinary drag is sent to
                        // the program, and a Shift drag selects text.
                        let report = match phase {
                            MousePhase::Down if modes.mouse_report() && !shift => {
                                mouse_gesture = true;
                                mouse_gesture_motion = modes.mouse_drag || modes.mouse_motion;
                                if inputs.selection_clear(engine.as_mut(), session_id.as_deref()) {
                                    last_cursor_frame = None;
                                    if let Some(state) = image_state.as_mut() {
                                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                        if !present_screen(&surface_id, screen, state, &output_tx, "mouse", &log, &mut screens, inputs.sequence).await { return; }
                                    }
                                }
                                Some((encoding::MouseButton::Left, encoding::MouseAction::Press))
                            }
                            MousePhase::Down => None,
                            MousePhase::Move if mouse_cell == Some(cell) => None,
                            MousePhase::Move if mouse_drag_motion_owned(mouse_gesture, mouse_gesture_motion, pressed) =>
                                Some((encoding::MouseButton::Left, encoding::MouseAction::Motion)),
                            MousePhase::Move if !pressed && !mouse_gesture && modes.mouse_motion =>
                                Some((encoding::MouseButton::None, encoding::MouseAction::Motion)),
                            MousePhase::Move => None,
                            MousePhase::Up if mouse_gesture => {
                                mouse_gesture = false;
                                mouse_gesture_motion = false;
                                Some((encoding::MouseButton::Left, encoding::MouseAction::Release))
                            }
                            MousePhase::Up => None,
                        };
                        mouse_cell = Some(cell);
                        if let Some((button, action)) = report {
                            let encoded = encoding::encode_mouse(&encoding::MouseReport {
                                button, action, col: cell.0, row: cell.1, alt, ctrl,
                            }, &modes);
                            let phase_name = match phase { MousePhase::Down => "down", MousePhase::Move => "move", MousePhase::Up => "up" };
                            match encoded {
                                Ok(bytes) => {
                                    let written = if let Some(ref sid) = session_id {
                                        if let Err(error) = inputs.write(&session_port, sid, &bytes, &cids).await {
                                            let response = json!({"surface": surface_id, "body": {"error": "mouse write failed", "reason": error}});
                                            if output_tx.reply(response).await.is_err() { return; }
                                            false
                                        } else { true }
                                    } else { false };
                                    let response = json!({"surface": surface_id, "body": {
                                        "event": "mouse", "inputId": input_id, "phase": phase_name, "x": cell.0, "y": cell.1,
                                        "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                        "reported": true, "written": written,
                                        "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                        "bytes": base64_encode(&bytes)
                                    }});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                }
                                Err(reason) => {
                                    let response = json!({"surface": surface_id, "body": {"event": "mouse", "inputId": input_id, "phase": phase_name,
                                        "x": cell.0, "y": cell.1, "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                        "reported": true, "written": false,
                                        "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                        "bytes": null, "error": reason}});
                                    if output_tx.reply(response).await.is_err() { return; }
                                }
                            }
                        } else {
                            // The measurement is sent for every pointer phase, including the phase
                            // that the current mouse mode deliberately sends to text selection.
                            // This way an unreported phase does not disappear, and its ownership
                            // can be observed.
                            let phase_name = match phase {
                                MousePhase::Down => "down",
                                MousePhase::Move => "move",
                                MousePhase::Up => "up",
                            };
                            let response = json!({"surface": surface_id, "body": {
                                "event": "mouse", "inputId": input_id, "phase": phase_name, "x": cell.0, "y": cell.1,
                                "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                "reported": false, "written": false,
                                "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                "bytes": null, "error": null
                            }});
                            if output_tx.reply(response).await.is_err() { return; }
                        }
                    }
                    // Measures, in one step, the number of input bytes that the current session of
                    // the surface has not read and the cumulative number of child output bytes that
                    // the reader has read, then answers. It is a diagnostic observation that
                    // distinguishes a successful write, the child's receipt, and the child's
                    // output.
                    SurfaceCommand::PtyPending => {
                        // An error reply also carries the same event marker. The reply alone must
                        // show that it answers a measurement request.
                        let response = match session_id.as_ref() {
                            Some(sid) => match session_port.pty_measurement(sid).await {
                                Ok(measurement) => json!({"surface": surface_id, "body": {"event": "pty.pending", "pending": measurement.pending, "written": measurement.written}}),
                                Err(error) => json!({"surface": surface_id, "body": {"event": "pty.pending", "error": error}}),
                            },
                            None => json!({"surface": surface_id, "body": {"event": "pty.pending", "error": "Session not open"}}),
                        };
                        if output_tx.reply(response).await.is_err() { return; }
                    }
                    // A mouse report gesture selects no text. The page waits for the answer of the
                    // selection operation.
                    SurfaceCommand::SelectionStart { .. } if mouse_gesture => {
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.start"}});
                        if output_tx.reply(response).await.is_err() { return; }
                    }
                    SurfaceCommand::SelectionUpdate { .. } if mouse_gesture => {
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.update"}});
                        if output_tx.reply(response).await.is_err() { return; }
                    }
                    SurfaceCommand::SelectionEnd if mouse_gesture => {
                        let response = json!({"surface": surface_id, "body": {"event": "selection.end", "copied": false}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::SelectionStart { x, y } => {
                        let result = image_state.as_ref().ok_or_else(|| "selection image is not configured".to_string())
                            .and_then(|state| state.selection_edge(x, y))
                            .and_then(|(edge, row)| inputs.selection_start(engine.as_mut(), session_id.as_deref(), edge, row));
                        if let Err(error) = result {
                            let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                            if output_tx.reply(response).await.is_err() { return; }
                        } else {
                            cursor_activity = Instant::now();
                            last_cursor_frame = None;
                            if let Some(state) = image_state.as_mut() {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if !present_screen(&surface_id, screen, state, &output_tx, "selection", &log, &mut screens, inputs.sequence).await { return; }
                            }
                            let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.start"}});
                            if output_tx.reply(response).await.is_err() { return; }
                        }
                    }
                    SurfaceCommand::SelectionUpdate { x, y } => {
                        let result = image_state.as_ref().ok_or_else(|| "selection image is not configured".to_string())
                            .and_then(|state| state.selection_edge(x, y))
                            .and_then(|(edge, row)| inputs.selection_update(engine.as_mut(), session_id.as_deref(), edge, row));
                        if let Err(error) = result {
                            let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                            if output_tx.reply(response).await.is_err() { return; }
                        } else {
                            cursor_activity = Instant::now();
                            last_cursor_frame = None;
                            if let Some(state) = image_state.as_mut() {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if !present_screen(&surface_id, screen, state, &output_tx, "selection", &log, &mut screens, inputs.sequence).await { return; }
                            }
                            let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.update"}});
                            if output_tx.reply(response).await.is_err() { return; }
                        }
                    }
                    SurfaceCommand::SelectionEnd => {
                        // The event of the previous release waits for its raster; this release is
                        // applied after that event is sent, so the two gestures report their ends
                        // in order.
                        if pending_selection_event.is_some() {
                            deferred_selection_end = Some(output_tx.clone());
                            continue;
                        }
                        let view = ScreenView { focused, preedit: &preedit, policy: &cursor_policy };
                        if !end_selection(&surface_id, engine.as_mut(), &mut inputs, session_id.as_deref(), &mut image_state, view, &output_tx, &log, &mut screens, &mut pending_selection_event).await {
                            return;
                        }
                    }
                    SurfaceCommand::Viewport { offset } => {
                        // Limits the viewport to the range of history that the engine retains.
                        let delta = i64::from(offset) - i64::from(engine.viewport_offset());
                        if delta != 0 {
                            inputs.scroll_viewport(engine.as_mut(), session_id.as_deref(), delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
                        }
                        refresh_inline_image_positions(&mut engine, &mut image_state);
                        if hold_while_presenting(&mut image_state) { continue; }
                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                        if let Some(state) = image_state.as_mut() {
                            if !present_screen(&surface_id, screen, state, &output_tx, "viewport", &log, &mut screens, inputs.sequence).await { return; }
                        } else if !send_plain_screen(&surface_id, session_id.as_deref(), &screen, &output_tx, &log, &mut screens, inputs.sequence, image_state.as_ref()).await { return; }
                    }
                    SurfaceCommand::Scroll { lines, col, row, shift, cids } => {
                        let modes = engine.modes();
                        // Applies, in this order: mouse reporting unless Shift is held, the alternate
                        // scroll of the alternate screen, and the viewport of the primary screen.
                        let route = record_wheel(&log, &surface_id, lines, col, row, shift, &modes);
                        let bytes = match route {
                            WheelRoute::Program => {
                            match encoding::encode_wheel(&modes, lines > 0, col, row) {
                                Ok(event) => Some(event.repeat(lines.unsigned_abs() as usize)),
                                Err(error) => {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                                    if output_tx.reply(response).await.is_err() { return; }
                                    continue;
                                }
                            }
                            }
                            WheelRoute::AlternateScroll => {
                            let key = if lines > 0 { "Up" } else { "Down" };
                            let keys = vec![InputKey { key: key.to_string(), text: String::new(), shift: false, alt: false, ctrl: false }; lines.unsigned_abs() as usize];
                            match encode_keys(&keys, &modes) {
                                Ok(bytes) => Some(bytes),
                                Err(error) => {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                                    if output_tx.reply(response).await.is_err() { return; }
                                    continue;
                                }
                            }
                            }
                            WheelRoute::Viewport => None,
                        };
                        match bytes {
                            Some(bytes) => {
                                if let Some(ref sid) = session_id
                                    && let Err(error) = inputs.write(&session_port, sid, &bytes, &cids).await {
                                        let response = json!({"surface": surface_id, "body": {"error": "scroll write failed", "reason": error}});
                                        if output_tx.reply(response).await.is_err() { return; }
                                    }
                            }
                            None => {
                                inputs.scroll_viewport(engine.as_mut(), session_id.as_deref(), lines);
                                refresh_inline_image_positions(&mut engine, &mut image_state);
                                if hold_while_presenting(&mut image_state) { continue; }
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if let Some(state) = image_state.as_mut() {
                                    if !present_screen(&surface_id, screen, state, &output_tx, "scroll", &log, &mut screens, inputs.sequence).await { return; }
                                } else if !send_plain_screen(&surface_id, session_id.as_deref(), &screen, &output_tx, &log, &mut screens, inputs.sequence, image_state.as_ref()).await { return; }
                            }
                        }
                    }
                    SurfaceCommand::Copy => {
                        // If there is no selection, nothing is copied and the reply says so. The
                        // clipboard is not changed.
                        let body = match engine.selection_text() {
                            Some(text) => json!({"event": "copy", "text": text, "userInitiated": true}),
                            None => json!({"event": "copy", "copied": false}),
                        };
                        let response = json!({"surface": surface_id, "body": body});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::Theme { theme } => {
                        let theme = *theme;
                        current_theme = theme;
                        inputs.set_theme(engine.as_mut(), session_id.as_deref(), theme);
                        if let Some(state) = image_state.as_mut() {
                            state.theme = theme;
                            if state.pending_draw {
                                state.dirty = true;
                            } else {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                if !present_screen(&surface_id, screen, state, &output_tx, "theme", &log, &mut screens, inputs.sequence).await { return; }
                            }
                        }
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "theme", "mode": theme.mode.name(), "background": format!("#{:02x}{:02x}{:02x}", theme.background[0], theme.background[1], theme.background[2])}});
                        if output_tx.reply(response).await.is_err() { return; }
                    }
                    SurfaceCommand::Font { font, system, skipped, size } => {
                        let family = match font.family() {
                            Ok(family) => family,
                            Err(reason) => {
                                let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                if output_tx.reply(response).await.is_err() { return; }
                                continue;
                            }
                        };
                        // Keeps the current raster size and recalculates the columns and rows with
                        // the cell metrics of the new font.
                        if let Some(state) = image_state.as_mut() {
                            let metrics = match crate::platform::metrics_for(&font, size, state.scale) {
                                Ok(metrics) => metrics,
                                Err(reason) => {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                    if output_tx.reply(response).await.is_err() { return; }
                                    continue;
                                }
                            };
                            let (cols, rows) = match calculate_terminal_size(state.width_px, state.height_px, &metrics) {
                                Ok(size) => size,
                                // Keeps the grid even when the font changes on a sub-cell raster.
                                // The next valid raster recalculates the grid with the new font
                                // (V5-96-14-6-4-8).
                                Err(reason) => {
                                    record_empty_raster(&log, &surface_id, state.width_px, state.height_px, &reason);
                                    let screen = engine.screen();
                                    (screen.cols, screen.rows)
                                }
                            };
                            state.metrics = metrics;
                            crate::engine_trace::record(&log, &surface_id, || format!("resize {cols} {rows}"));
                            engine.resize(cols, rows);
                            inputs.resized(session_id.as_deref(), rows, cols);
                            if let Err(error) = set_engine_metrics(&mut engine, state) {
                                let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
                                if output_tx.reply(response).await.is_err() { return; }
                                continue;
                            }
                            terminal_font = font;
                            terminal_font_size = size;
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                            if let Some(sid) = session_id.as_ref() {
                                if let Err(error) = session_port.resize(sid, cols, rows).await {
                                    let response = json!({"surface": surface_id, "body": {"error": format!("Resize failed: {error}")}});
                                    if output_tx.reply(response).await.is_err() { return; }
                                } else if !send_state(&surface_id, sid, cols, rows, image_state.as_ref().unwrap(), &output_tx).await {
                                    return;
                                }
                            }
                            let state = image_state.as_mut().unwrap();
                            if state.pending_draw {
                                state.dirty = true;
                            } else {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                if !present_screen(&surface_id, screen, state, &output_tx, "metrics", &log, &mut screens, inputs.sequence).await { return; }
                            }
                        } else {
                            terminal_font = font;
                            terminal_font_size = size;
                        }
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "font", "family": family, "system": system, "skipped": skipped, "size": size}});
                        if output_tx.reply(response).await.is_err() { return; }
                    }
                    SurfaceCommand::Cursor { policy } => {
                        cursor_policy = policy;
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        let screen = decorate_screen(
                            engine.screen(),
                            focused,
                            &preedit,
                            &cursor_policy,
                            0,
                        );
                        if let Some(state) = image_state.as_mut() {
                            if state.pending_draw {
                                state.dirty = true;
                            } else if !present_screen(&surface_id, screen, state, &output_tx, "cursor", &log, &mut screens, inputs.sequence).await {
                                return;
                            }
                        }
                        let response = json!({
                            "surface": surface_id,
                            "body": {
                                "ack": true,
                                "event": "cursor",
                                "shape": match policy.shape {
                                    CursorShape::Block => "block",
                                    CursorShape::Underline => "underline",
                                    CursorShape::Beam => "beam",
                                    CursorShape::HollowBlock => "block",
                                    CursorShape::Hidden => "block",
                                },
                                "blink": match policy.blink {
                                    CursorBlinkPolicy::Never => "Never",
                                    CursorBlinkPolicy::Off => "Off",
                                    CursorBlinkPolicy::On => "On",
                                    CursorBlinkPolicy::Always => "Always",
                                },
                                "interval": policy.interval_ms,
                                "idleTimeout": policy.idle_timeout_ms,
                                "unfocused": match policy.unfocused {
                                    UnfocusedCursor::Hollow => "hollow",
                                    UnfocusedCursor::Solid => "solid",
                                    UnfocusedCursor::Underline => "underline",
                                    UnfocusedCursor::Beam => "beam",
                                    UnfocusedCursor::Unchanged => "unchanged",
                                },
                            }
                        });
                        if output_tx.reply(response).await.is_err() { return; }
                    }
                    SurfaceCommand::ClipboardResolve { request_id, text } => {
                        if let Err(error) = engine.resolve_clipboard(request_id, &text) {
                            let response = json!({"surface": surface_id, "body": {"error": error}});
                            if output_tx.reply(response).await.is_err() { return; }
                        } else if !send_engine_events(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, log: &log, inputs: &mut inputs, screens: &mut screens }, session_id.as_deref(), &mut engine, true, &mut image_state, &mut multipart).await {
                            return;
                        }
                    }
                    SurfaceCommand::ClipboardReject { request_id, reason } => {
                        if let Err(error) = engine.reject_clipboard(request_id, &reason) {
                            let response = json!({"surface": surface_id, "body": {"error": error}});
                            if output_tx.reply(response).await.is_err() { return; }
                        } else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"event": "clipboard.rejected", "requestId": request_id, "reason": reason}
                            });
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                    SurfaceCommand::ScreenRead => {
                        // The reply of a screen read is the full form, from which the page continues.
                        screens.forget();
                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                        if !send_plain_screen(&surface_id, session_id.as_deref(), &screen, &output_tx, &log, &mut screens, inputs.sequence, image_state.as_ref()).await {
                            return;
                        }
                    }
                    SurfaceCommand::InlineImageDelete { name } => {
                        let Some(state) = image_state.as_mut() else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "imageNotConfigured", "reason": "inline image region is not configured"}
                            });
                            if output_tx.reply(response).await.is_err() { return; }
                            continue;
                        };
                        let Some(index) = state.inline_images.iter().position(|image| image.name == name) else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "imageNotFound", "reason": format!("inline image {name:?} is not owned by this surface")}
                            });
                            if output_tx.reply(response).await.is_err() { return; }
                            continue;
                        };
                        state.inline_images.remove(index);
                        // The delete event is sent with the display that cleared the image.
                        state.presentation_events.push(json!({
                            "surface": surface_id,
                            "body": {"event": "image.inline.deleted", "name": name}
                        }).to_string());
                        if state.pending_draw {
                            state.dirty = true;
                        } else {
                            let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                            if !present_screen(&surface_id, screen, state, &output_tx, "image", &log, &mut screens, inputs.sequence).await { return; }
                        }
                    }
                    SurfaceCommand::SessionClose { result } => {
                        // The image must be released before the close reply, so that the display
                        // reply that the host sends after receiving the close reply finds that
                        // transfer image.
                        if let Some(state) = image_state.take() {
                            release_image_state(&surface_id, state);
                        }
                        let closed = match session_id.as_deref() {
                            Some(sid) => session_port
                                .close(sid)
                                .await
                                .map_err(|error| format!("Close failed: {error}")),
                            None => Ok(()),
                        };
                        // If the sender of the close has stopped waiting, that sender has already
                        // reported the timeout. A close error has no receiver, so it is kept as an
                        // error record event.
                        if let Err(Err(error)) = result.send(closed) {
                            log.record_error(&format!("surface {surface_id}"), error);
                        }
                        break;
                    }
                    SurfaceCommand::SessionDetach => {
                        if let Some(ref sid) = session_id
                            && let Err(error) = session_port.detach(sid).await {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"error": format!("Detach failed: {error}")}
                                });
                                if output_tx.reply(response).await.is_err() {
                                    return;
                                }
                            }
                        break;
                    }
                    SurfaceCommand::ImageResponse { body, dispatched_at } => {
                        let image_obj = body.get("image").and_then(|v| v.as_object());
                        let consumed = image_obj.and_then(|o| o.get("consumed")).and_then(|v| v.as_object());
                        let is_error = image_obj.is_some_and(|o| o.get("error").is_some());
                        let answer = image_obj.and_then(image_answer);
                        // Only the complete identity of the frame in flight may release this draw.
                        // A late answer for a detached image must not unblock a newer generation
                        // with the same name.
                        let releases_pending = image_state.as_ref().is_some_and(|state|
                            state.pending_draw
                                && answer.as_ref().is_some_and(|answer| {
                                    answer.name == state.name
                                        && answer.generation == state.generation
                                        && answer.raster == state.raster
                                        && answer.sequence == u64::from(state.sequence)
                                })
                                && (consumed.is_some() || is_error));
                        if releases_pending {
                            // Instrument of the wait for transfer completion (V5-104): from the
                            // moment of sending to the consumed or error reply.
                            let state = image_state.as_mut().unwrap();
                            if let Some(sent) = state.sent_at.take() {
                                let handled_at = std::time::Instant::now();
                                log.emit_for(Level::Debug, "sidecar.vt.consumed_wait", Subject::surface(&surface_id), &serde_json::json!({
                                    "name": state.name,
                                    "generation": state.generation,
                                    "raster": state.raster,
                                    "sequence": state.sequence,
                                    "wait_us": handled_at.duration_since(sent).as_micros() as u64,
                                    "queue_us": dispatched_at.map(|at| handled_at.duration_since(at).as_micros() as u64),
                                    "answer": if consumed.is_some() { "consumed" } else { "error" },
                                }));
                            }
                            let state = image_state.as_mut().unwrap();
                            state.pending_draw = false;
                            state.carrier = None;
                            if let Some(configuration) = pending_configuration.take() {
                                let new_state = match ImageState::new(&configuration, &terminal_font, terminal_font_size) {
                                    Ok(state) => state,
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id,
                                            "body": {"error": "image creation failed", "reason": reason}});
                                        if output_tx.reply(response).await.is_err() { return; }
                                        continue;
                                    }
                                };
                                let mut new_state = new_state;
                                new_state.theme = current_theme;
                                new_state.drawing = drawing;
                        new_state.session = session_id.clone();
                                let (cols, rows) = match calculate_terminal_size(
                                    configuration.width, configuration.height, &new_state.metrics) {
                                    Ok(size) => size,
                                    // The same retention rule as Configure: the sub-cell raster
                                    // keeps the current grid and displays the frame
                                    // (V5-96-14-6-4-8).
                                    Err(reason) => {
                                        record_empty_raster(&log, &surface_id, configuration.width, configuration.height, &reason);
                                        let screen = engine.screen();
                                        (screen.cols, screen.rows)
                                    }
                                };
                                crate::engine_trace::record(&log, &surface_id, || format!("resize {cols} {rows}"));
                                engine.resize(cols, rows);
                                inputs.resized(session_id.as_deref(), rows, cols);
                                if let Err(error) = set_engine_metrics(&mut engine, &new_state) {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
                                    if output_tx.reply(response).await.is_err() { return; }
                                    continue;
                                }
                                image_state = Some(new_state);
                                record_surface_mode(&log, &surface_id, headless, "configure");
                                if let Some(sid) = session_id.as_ref() {
                                    if let Err(error) = session_port.resize(sid, cols, rows).await {
                                        let response = json!({"surface": surface_id,
                                            "body": {"error": format!("Resize failed: {error}")}});
                                        if output_tx.reply(response).await.is_err() { return; }
                                    } else if !send_state(&surface_id, sid, cols, rows, image_state.as_ref().unwrap(), &output_tx).await {
                                        return;
                                    }
                                    let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                    if !present_screen(&surface_id, screen, image_state.as_mut().unwrap(), &output_tx, "image", &log, &mut screens, inputs.sequence).await {
                                        return;
                                    }
                                }
                            } else if image_state.as_ref().unwrap().dirty {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                // Sends one screen that holds the changes made during the wait,
                                // together with the raster.
                                if !present_screen(&surface_id, screen, image_state.as_mut().unwrap(), &output_tx, "image", &log, &mut screens, inputs.sequence).await {
                                    return;
                                }
                            }
                            if image_state.as_ref().is_none_or(|state| !state.pending_draw)
                                && let Some(event) = pending_selection_event.take() {
                                    if output_tx.send(event).await.is_err() { return; }
                                    if let Some(deferred) = deferred_selection_end.take() {
                                        let view = ScreenView { focused, preedit: &preedit, policy: &cursor_policy };
                                        if !end_selection(&surface_id, engine.as_mut(), &mut inputs, session_id.as_deref(), &mut image_state, view, &deferred, &log, &mut screens, &mut pending_selection_event).await {
                                            return;
                                        }
                                    }
                                }
                        }
                    }
                }
            }
            Some(event) = daemon_events_rx.recv() => {
                match event {
                    DaemonEvent::Output { session_id: ref recv_sid, data, sequence: _, truncated } => {
                        if let Some(ref sid) = session_id
                            && recv_sid == sid {
                                cursor_activity = Instant::now();
                                last_cursor_frame = None;
                                if truncated {
                                    crate::engine_trace::record(&log, &surface_id, || "reset".to_string());
                                    engine.reset();
                                }
                                crate::engine_trace::record(&log, &surface_id, || format!("feed {}", base64_encode(&data)));
                                inputs.output(sid, &data);
                                engine.feed(&data);
                                record_mouse_modes(&log, &surface_id, &mut recorded_mouse_modes, &engine.modes());
                                if !send_engine_events(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, log: &log, inputs: &mut inputs, screens: &mut screens }, session_id.as_deref(), &mut engine, !headless, &mut image_state, &mut multipart).await { return; }
                                if headless || hold_while_presenting(&mut image_state) { continue; }
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);

                                if let Some(ref mut img_state) = image_state {
                                        // If the image is on the host, it is not drawn until it
                                        // comes back, and only the fact of the change is recorded.
                                        if img_state.pending_draw {
                                            img_state.dirty = true;
                                        } else if !present_screen(&surface_id, screen, img_state, &output_tx, "image", &log, &mut screens, inputs.sequence).await {
                                            return;
                                        }
                                    continue;
                                }

                                if headless { continue; }
                                if !send_plain_screen(&surface_id, session_id.as_deref(), &screen, &output_tx, &log, &mut screens, inputs.sequence, image_state.as_ref()).await {
                                    return;
                                }
                            }
                    }
                    DaemonEvent::Exit { session_id: ref recv_sid } => {
                        if let Some(ref sid) = session_id
                            && recv_sid == sid {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"event": "exit"}
                                });
                                if output_tx.send(response.to_string()).await.is_err() {
                                    // The output channel has closed, so this exits as it is
                                }
                                break;
                            }
                    }
                    DaemonEvent::Error { session_id: ref recv_sid, error } => {
                        if session_id.as_deref() == Some(recv_sid) {
                            let response = json!({"surface": surface_id, "body": {"event": "error", "reason": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                }
            }
            else => break,
        }
    }
}

/// Service loop. Reads JSON from stdin and writes replies to stdout. The events of frames,
/// requests, and PTY input and output are written to `log`.
pub async fn serve<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    log: Events,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_options(
        reader,
        writer,
        ServeOptions {
            engine_factory,
            session_port_factory,
            owner_close: None,
            registry: None,
            owner: String::new(),
            log,
            detach_requests: Arc::new(ImageDetachRequestLedger::default()),
            submission_observer: None,
        },
    )
    .await
}

/// Runs the service with an observer called at the connection queue boundary.
/// This interface is used by owning contract tests; normal services use `serve`.
#[doc(hidden)]
pub async fn serve_with_submission_observer<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    log: Events,
    observer: OutputSubmissionObserver,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_options(
        reader,
        writer,
        ServeOptions {
            engine_factory,
            session_port_factory,
            owner_close: None,
            registry: None,
            owner: String::new(),
            log,
            detach_requests: Arc::new(ImageDetachRequestLedger::default()),
            submission_observer: Some(observer),
        },
    )
    .await
}

/// The owner of a persistent connection. The surfaces of the owner remain in the registry after the
/// connection ends.
pub struct PersistentOwner {
    /// The function called when the owner requests a close.
    pub close: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub registry: Arc<PersistentRegistry>,
    pub owner: String,
    pub log: Events,
}

pub async fn serve_with_registry<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    owner: PersistentOwner,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_options(
        reader,
        writer,
        ServeOptions {
            engine_factory,
            session_port_factory,
            owner_close: Some(owner.close),
            detach_requests: Arc::clone(&owner.registry.detach_requests),
            registry: Some(owner.registry),
            owner: owner.owner,
            log: owner.log,
            submission_observer: None,
        },
    )
    .await
}

/// The factories and the owner used to process the input of one connection. Without a registry, the
/// surfaces also end when the connection ends.
struct ServeOptions {
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    owner_close: Option<Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
    registry: Option<Arc<PersistentRegistry>>,
    owner: String,
    log: Events,
    detach_requests: Arc<ImageDetachRequestLedger>,
    submission_observer: Option<OutputSubmissionObserver>,
}

async fn serve_with_options<R, W>(
    reader: R,
    writer: W,
    options: ServeOptions,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let buf_reader = BufReader::new(PolledReader::new(reader, options.log.clone()));
    let (output_sender, output_rx) = mpsc::channel::<String>(100);
    let connection = Connection::new(output_sender, options.log.clone());
    let transfers = connection.transfers.clone();
    let output_tx = if options.registry.is_some() {
        OutputSink::detachable(connection)
    } else {
        OutputSink::direct(connection)
    }
    .with_submission_observer(options.submission_observer.clone());

    let output_trace = options.log.clone();
    let input_task = async {
        let result = run_input_loop(buf_reader, output_tx, options).await;
        // No reply comes to a connection whose input has ended.
        transfers.end();
        result
    };
    let output_task = run_output_loop(writer, output_rx, output_trace);

    tokio::try_join!(input_task, output_task)?;
    Ok(())
}

async fn run_input_loop<R>(
    mut buf_reader: BufReader<R>,
    output_tx: OutputSink,
    options: ServeOptions,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let ServeOptions {
        engine_factory,
        session_port_factory,
        owner_close,
        registry,
        owner,
        log,
        detach_requests,
        submission_observer: _,
    } = options;
    // Connection sequence number. It distinguishes the connections of the same owner in the trace.
    static CONNECTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let connection = CONNECTIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let (watched, arrived) = mpsc::unbounded_channel();
    tokio::spawn(watch_requests(arrived, connection));
    let mut surface_txs: HashMap<String, mpsc::Sender<Dispatch>> = HashMap::new();
    let mut surface_epochs: HashMap<String, u64> = HashMap::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut line = String::new();
    let connection_output = output_tx.connection().await;
    // Of the transfer images that this connection carried, the ones that a surface released. Their
    // replies come on this connection.
    let transfers = connection_output
        .as_ref()
        .map(|connection| connection.transfers.clone());
    // Surface actions that had already ended when the input ended. They are reported as errors at
    // the end of serve.
    let mut ended_surfaces = Vec::new();

    loop {
        line.clear();
        let waiting = std::time::Instant::now();
        let n = buf_reader.read_line(&mut line).await?;
        let idle = waiting.elapsed();

        if n == 0 {
            if registry.is_none() {
                // The surface actions of a connection that is not persistent are detached from
                // their sessions and end.
                for (key, tx) in &surface_txs {
                    if tx
                        .send(Dispatch::internal(SurfaceCommand::SessionDetach))
                        .await
                        .is_err()
                    {
                        ended_surfaces.push(key.clone());
                    }
                }
            } else {
                output_tx.detach_connection(&connection_output).await;
            }
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // The processing record of this request. It is written when this iteration ends (before the
        // next request is read), including the hand-off to a surface action. silent: a line that is
        // not JSON is recorded as an unparsed request and answered with a parse error below.
        let parsed = serde_json::from_str::<Value>(trimmed).ok();
        let request = Arc::new(Request {
            operation: parsed
                .as_ref()
                .map_or_else(|| "unparsed".to_string(), request_operation),
            surface: parsed
                .as_ref()
                .and_then(|value| value.get("surface"))
                .and_then(Value::as_str)
                .map(str::to_string),
            // A `cids` that is not a list of strings is answered with a parse error below; the
            // event holds the body.
            cids: parsed
                .as_ref()
                .and_then(|value| value.get("cids"))
                // silent: cids that are not a list of strings are answered with a parse error
                // below, and the request event holds the body.
                .and_then(|cids| serde_json::from_value::<Vec<String>>(cids.clone()).ok())
                // default: a request without usable cids has none.
                .unwrap_or_default(),
            started: std::time::Instant::now(),
            log: log.clone(),
            stage: std::sync::atomic::AtomicU8::new(RequestStage::InputLoop as u8),
        });
        // The watch ends with the input loop, so it is gone only when this loop has ended.
        if watched.send(Arc::downgrade(&request)).is_err() {
            log.record_error(
                "request watch",
                "the watch of the requests ended before the input loop",
            );
        }
        let _trace = RequestTrace {
            request: Arc::clone(&request),
            body: RequestBody::of(trimmed, parsed.is_some()),
            idle,
            owner: owner.clone(),
            connection,
        };
        let reply_tx = output_tx.bound(Arc::clone(&request));
        if let Some(value) = parsed {
            if value.get("operation").and_then(Value::as_str) == Some("close-owner") {
                let Some(request) = value.get("request").and_then(Value::as_str) else {
                    let reply = json!({"error": "invalidParams", "reason": "the owner request has no request id"});
                    if reply_tx.reply(reply).await.is_err() {
                        break;
                    }
                    continue;
                };
                // Closes the surface actions first. Each action closes its own session and replies.
                // If the owner's sessions end first, their actions end by themselves and cannot
                // reply to the close (core F98). The owner close then closes the sessions that
                // remain.
                let closed = match registry.as_ref() {
                    Some(registry) => {
                        let closed = registry.close_owner(&owner).await;
                        forget_closed_surfaces(registry, &mut surface_txs, &mut surface_epochs)
                            .await;
                        closed
                    }
                    None => Ok(()),
                };
                // The owner close waits for each child to be reaped, so it runs outside the runtime
                // workers.
                let owner_closed = match owner_close.clone() {
                    Some(close) => tokio::task::spawn_blocking(move || close())
                        .await
                        .map_err(|error| format!("close owner task failed: {error}"))
                        .and_then(|result| result),
                    // default: a service without an owner close answers that request with an error.
                    None => Err("owner close is unavailable".to_string()),
                };
                let result = match (closed, owner_closed) {
                    (Ok(()), Ok(())) => Ok(()),
                    (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                    (Err(closed), Err(owner)) => Err(format!("{closed}; {owner}")),
                };
                let reply = match result {
                    Ok(()) => json!({"operation": "closed-owner", "request": request, "ok": true}),
                    Err(error) => {
                        json!({"operation": "closed-owner", "request": request, "ok": false, "error": error})
                    }
                };
                if reply_tx.reply(reply).await.is_err() {
                    break;
                }
                continue;
            }
            if value.get("operation").and_then(Value::as_str) == Some("retain") {
                let Some(request) = value.get("request").and_then(Value::as_str) else {
                    let reply = json!({"error": "invalidParams", "reason": "the owner request has no request id"});
                    if reply_tx.reply(reply).await.is_err() {
                        break;
                    }
                    continue;
                };
                let result = match (registry.as_ref(), retain_keys(&value)) {
                    (Some(registry), Ok(keep)) => {
                        let closed = registry.retain(&owner, &keep).await;
                        forget_closed_surfaces(registry, &mut surface_txs, &mut surface_epochs)
                            .await;
                        closed
                    }
                    (None, _) => Err("retain is unavailable".to_string()),
                    (_, Err(error)) => Err(error),
                };
                let reply = match result {
                    Ok(closed) => {
                        json!({"operation": "retained", "request": request, "ok": true, "closed": closed})
                    }
                    Err(error) => {
                        json!({"operation": "retained", "request": request, "ok": false, "error": error})
                    }
                };
                if reply_tx.reply(reply).await.is_err() {
                    break;
                }
                continue;
            }
            if value.get("operation").and_then(Value::as_str) == Some("shutdown") {
                let Some(request) = value.get("request").and_then(Value::as_str) else {
                    let reply = json!({"error": "invalidParams", "reason": "the owner request has no request id"});
                    if reply_tx.reply(reply).await.is_err() {
                        break;
                    }
                    continue;
                };
                let result = if let Some(registry) = registry.as_ref() {
                    registry.request_shutdown();
                    Ok(())
                } else {
                    Err("shutdown is unavailable".to_string())
                };
                let reply = match result {
                    Ok(()) => json!({"operation": "shutdown", "request": request, "ok": true}),
                    Err(error) => {
                        json!({"operation": "shutdown", "request": request, "ok": false, "error": error})
                    }
                };
                if reply_tx.reply(reply).await.is_err() {
                    break;
                }
                break;
            }
        }

        match serde_json::from_str::<Envelope>(trimmed) {
            Ok(env) => {
                let surface_id = env.surface.clone();
                let cids = env.cids.clone();

                if env.closed == Some(true) {
                    let registry_key =
                        local_surface_key(&surface_txs, env.root.as_deref(), &surface_id);
                    let close_result = close_connection_surface(
                        registry.as_ref(),
                        &mut surface_txs,
                        &mut surface_epochs,
                        &registry_key,
                        &owner,
                    )
                    .await;
                    // Replies to every closed. On failure, the reason is sent with it (core
                    // docs/spec/sidecars.md#messages).
                    if let Err(error) = close_result {
                        let response =
                            json!({"surface": surface_id, "closed": true, "error": error});
                        if reply_tx.reply(response).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    let response = json!({"surface": surface_id, "closed": true});
                    if reply_tx.reply(response).await.is_err() {
                        // The output channel has closed, so serve ends
                        break;
                    }
                } else if let Some(body) = env.body {
                    // If this is the reply to a transfer image that a closed surface left, the
                    // image is released. The reply does not recreate the surface.
                    if let Some(answer) = body
                        .get("image")
                        .and_then(Value::as_object)
                        .and_then(image_answer)
                        && transfers
                            .as_ref()
                            .is_some_and(|transfers| transfers.answer(&surface_id, answer))
                    {
                        continue;
                    }
                    let registry_key =
                        local_surface_key(&surface_txs, env.root.as_deref(), &surface_id);
                    let tx = if let Some(tx) = surface_txs.get(&registry_key) {
                        if let Some(registry) = registry.as_ref() {
                            // default: a surface that recorded no attachment epoch has epoch 0, and
                            // an epoch that differs from the registry is refused as a stale
                            // attachment.
                            let epoch = surface_epochs.get(&registry_key).copied().unwrap_or(0);
                            if registry.current_epoch(&registry_key, &owner).await != Some(epoch) {
                                let response = body
                                    .get("image")
                                    .and_then(|image| image.get("detach"))
                                    .and_then(|detach| {
                                        image_detach_refusal(&surface_id, detach, "stale attachment", None)
                                    })
                                    .unwrap_or_else(|| json!({"surface": surface_id, "body": {"error": "stale attachment"}}));
                                if reply_tx.reply(response).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                        }
                        tx.clone()
                    } else if let Some(registry) = registry.as_ref() {
                        match registry.attach(&registry_key, &owner, &output_tx).await {
                            Ok((tx, epoch)) => {
                                surface_epochs.insert(registry_key.clone(), epoch);
                                if tx
                                    .send(Dispatch::of(SurfaceCommand::Reconnect, &request))
                                    .await
                                    .is_err()
                                {
                                    let response = json!({"surface": surface_id, "body": {"error": "persistent surface actor closed before reconnect"}});
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                tx
                            }
                            Err(error) if registry.contains(&registry_key).await => {
                                let response = json!({"surface": surface_id, "body": {"error": "persistent attach failed", "reason": error}});
                                if reply_tx.reply(response).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                            Err(error) => {
                                if error != "session surface not found" {
                                    let response = json!({"surface": surface_id, "body": {"error": "persistent attach failed", "reason": error}});
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }

                                // A persistent service must create a new actor for a surface that
                                // is actually new. It can only attach again to an existing registry
                                // entry. No error is converted into an implicit replacement of an
                                // existing session.
                                let (cmd_tx, cmd_rx) = mpsc::channel(10);
                                let session_port = session_port_factory();
                                let factory = engine_factory.clone();
                                let out_tx = output_tx.clone();
                                let sid = surface_id.clone();
                                let task_trace = log.clone();
                                let actor = tokio::spawn(async move {
                                    surface_task(
                                        sid,
                                        task_trace,
                                        factory,
                                        session_port,
                                        cmd_rx,
                                        out_tx,
                                    )
                                    .await;
                                });
                                match registry
                                    .insert(
                                        registry_key.clone(),
                                        owner.clone(),
                                        cmd_tx.clone(),
                                        output_tx.clone(),
                                        actor,
                                    )
                                    .await
                                {
                                    Ok(epoch) => {
                                        surface_epochs.insert(registry_key.clone(), epoch);
                                        cmd_tx
                                    }
                                    Err(insert_error) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "persistent surface creation failed", "reason": insert_error}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                        continue;
                                    }
                                }
                            }
                        }
                    } else {
                        // New surface: creates the action
                        let (cmd_tx, cmd_rx) = mpsc::channel(10);
                        let session_port = session_port_factory();
                        let factory = engine_factory.clone();
                        let out_tx = output_tx.clone();
                        let sid = surface_id.clone();
                        let sid_for_monitor = sid.clone();
                        let task_trace = log.clone();

                        // Spawns the actual surface task as a separate handle
                        let surface_handle = tokio::spawn(async move {
                            surface_task(sid, task_trace, factory, session_port, cmd_rx, out_tx)
                                .await;
                        });

                        // Spawns a monitor task that watches for panics
                        let out_tx_monitor = output_tx.clone();
                        let log_monitor = log.clone();
                        tasks.spawn(async move {
                            match surface_handle.await {
                                Ok(_) => {},
                                Err(join_err) if join_err.is_panic() => {
                                    // The task panicked, so an error event is sent
                                    let panic_msg = if let Ok(panic_obj) = join_err.try_into_panic() {
                                        if let Some(s) = panic_obj.downcast_ref::<String>() {
                                            s.clone()
                                        } else if let Some(&s) = panic_obj.downcast_ref::<&str>() {
                                            s.to_string()
                                        } else {
                                            "unknown".to_string()
                                        }
                                    } else {
                                        "unknown".to_string()
                                    };
                                    let response = json!({
                                        "surface": sid_for_monitor,
                                        "body": {"event": "error", "reason": format!("surface task ended: {}", panic_msg)}
                                    });
                                    if let Err(error) = out_tx_monitor.send(response.to_string()).await {
                                        log_monitor.record_error("surface task", format!("the panic report was not delivered: {error:?}"));
                                    }
                                }
                                Err(error) => {
                                    log_monitor.record_error("surface task", format!("cancelled: {error}"));
                                }
                            }
                        });

                        surface_txs.insert(registry_key.clone(), cmd_tx.clone());
                        cmd_tx
                    };
                    surface_txs.insert(registry_key.clone(), tx.clone());
                    // The commands below answer this request.
                    let tx = RequestSender {
                        tx,
                        request: Arc::clone(&request),
                    };

                    // Checks the operation field first (it is a request).
                    if let Some(operation) = body.get("operation").and_then(|v| v.as_str()) {
                        match operation {
                            "reconnect" => {
                                if tx.send(SurfaceCommand::Reconnect).await.is_err() {
                                    break;
                                }
                            }
                            "open" => {
                                let image = body
                                    .get("image")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                let Some(shell) = body
                                    .get("shell")
                                    .and_then(|v| v.as_str())
                                    .filter(|shell| !shell.is_empty())
                                else {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "open requires a shell"}});
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                    continue;
                                };
                                let directory = match body.get("directory") {
                                    None => None,
                                    Some(Value::String(directory))
                                        if directory.starts_with('/') =>
                                    {
                                        Some(directory.clone())
                                    }
                                    Some(_) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "open directory must be an absolute path"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                        continue;
                                    }
                                };
                                let request = ShellRequest {
                                    shell: shell.to_string(),
                                    directory,
                                };
                                if tx
                                    .send(SurfaceCommand::Open { image, request })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "input" => {
                                let (actions, preedit) = match parse_input_transaction(&body) {
                                    Ok(transaction) => transaction,
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                        continue;
                                    }
                                };
                                if tx
                                    .send(SurfaceCommand::InputTransaction {
                                        actions,
                                        preedit,
                                        cids: cids.clone(),
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "theme" => {
                                let text = |name: &str| body.get(name).and_then(Value::as_str);
                                // Applies only when the mode and all four colors match. If any one
                                // does not match, nothing is changed.
                                match crate::palette::TerminalTheme::from_request(
                                    text("mode"),
                                    text("background"),
                                    text("foreground"),
                                    text("cursor"),
                                    text("selection"),
                                ) {
                                    Ok(theme) => {
                                        if tx
                                            .send(SurfaceCommand::Theme {
                                                theme: Box::new(theme),
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "font" => {
                                let size = match body.get("size").and_then(Value::as_f64) {
                                    Some(size)
                                        if (FONT_SIZE_MIN..=FONT_SIZE_MAX).contains(&size) =>
                                    {
                                        Ok(size as f32)
                                    }
                                    _ => Err(format!(
                                        "font.size must be a number from {FONT_SIZE_MIN} to {FONT_SIZE_MAX} points"
                                    )),
                                };
                                let resolved = match body.get("family").and_then(Value::as_str) {
                                    Some(list) => crate::platform::resolve_font_list(list),
                                    None => Err("font.family must be a string".to_string()),
                                };
                                match size
                                    .and_then(|size| resolved.map(|selection| (selection, size)))
                                {
                                    Ok((selection, size)) => {
                                        // A family that is not installed is recorded in the log,
                                        // not reported as an error.
                                        for family in &selection.skipped {
                                            log.record_info(
                                                "font",
                                                format!("terminal font family is not installed: {family}"),
                                            );
                                        }
                                        if selection.system {
                                            log.record_info("font", "no listed terminal font family is installed; using the system fixed-pitch font");
                                        }
                                        let (font, system, skipped) =
                                            (selection.font, selection.system, selection.skipped);
                                        if tx
                                            .send(SurfaceCommand::Font {
                                                font,
                                                system,
                                                skipped,
                                                size,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason, "operation": "font"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "cursor" => match parse_cursor_policy(&body) {
                                Ok(policy) => {
                                    if tx.send(SurfaceCommand::Cursor { policy }).await.is_err() {
                                        break;
                                    }
                                }
                                Err(reason) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": reason}
                                    });
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                }
                            },
                            "paste" => match body.get("text").and_then(Value::as_str) {
                                Some(text) => {
                                    if tx
                                        .send(SurfaceCommand::Paste {
                                            text: text.to_string(),
                                            cids: cids.clone(),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                None => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": "paste requires text string"}
                                    });
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                }
                            },
                            "mouse" => {
                                let phase = match body.get("phase").and_then(Value::as_str) {
                                    Some("down") => Some(MousePhase::Down),
                                    Some("move") => Some(MousePhase::Move),
                                    Some("up") => Some(MousePhase::Up),
                                    _ => None,
                                };
                                let x = body
                                    .get("x")
                                    .and_then(Value::as_f64)
                                    .filter(|x| x.is_finite());
                                let y = body
                                    .get("y")
                                    .and_then(Value::as_f64)
                                    .filter(|y| y.is_finite());
                                let flag = |name: &str| body.get(name).and_then(Value::as_bool);
                                let input_id = body
                                    .get("inputId")
                                    .and_then(Value::as_str)
                                    .filter(|id| !id.is_empty());
                                match (
                                    input_id,
                                    phase,
                                    x,
                                    y,
                                    flag("pressed"),
                                    flag("shift"),
                                    flag("alt"),
                                    flag("ctrl"),
                                ) {
                                    (
                                        Some(input_id),
                                        Some(phase),
                                        Some(x),
                                        Some(y),
                                        Some(pressed),
                                        Some(shift),
                                        Some(alt),
                                        Some(ctrl),
                                    ) => {
                                        if tx
                                            .send(SurfaceCommand::Mouse {
                                                input_id: input_id.to_string(),
                                                phase,
                                                x,
                                                y,
                                                pressed,
                                                shift,
                                                alt,
                                                ctrl,
                                                cids: cids.clone(),
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams",
                                            "reason": "mouse requires phase down, move, or up, finite x and y, boolean pressed, shift, alt, and ctrl, and non-empty inputId"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "selection.start" | "selection.update" => {
                                let x = body.get("x").and_then(Value::as_f64);
                                let y = body.get("y").and_then(Value::as_f64);
                                match (x, y) {
                                    (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                                        let command = if operation == "selection.start" {
                                            SurfaceCommand::SelectionStart { x, y }
                                        } else {
                                            SurfaceCommand::SelectionUpdate { x, y }
                                        };
                                        if tx.send(command).await.is_err() {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "selection requires finite x and y"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "selection.end" => {
                                if tx.send(SurfaceCommand::SelectionEnd).await.is_err() {
                                    break;
                                }
                            }
                            "copy" => {
                                if tx.send(SurfaceCommand::Copy).await.is_err() {
                                    break;
                                }
                            }
                            "viewport" => {
                                let Some(offset) = body
                                    .get("offset")
                                    .and_then(Value::as_u64)
                                    // silent: a value that does not fit is refused by the
                                    // invalidParams reply that follows.
                                    .and_then(|v| u32::try_from(v).ok())
                                else {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "viewport requires a nonnegative integer offset"}});
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                    continue;
                                };
                                if tx.send(SurfaceCommand::Viewport { offset }).await.is_err() {
                                    break;
                                }
                            }
                            "scroll" => {
                                let lines = body
                                    .get("lines")
                                    .and_then(Value::as_i64)
                                    .filter(|lines| *lines != 0 && i32::try_from(*lines).is_ok());
                                let col = body
                                    .get("col")
                                    .and_then(Value::as_u64)
                                    // silent: a value that does not fit is refused by the
                                    // invalidParams reply that follows.
                                    .and_then(|v| u16::try_from(v).ok());
                                let row = body
                                    .get("row")
                                    .and_then(Value::as_u64)
                                    // silent: a value that does not fit is refused by the
                                    // invalidParams reply that follows.
                                    .and_then(|v| u16::try_from(v).ok());
                                // An absent shift is a scroll without Shift; a present shift must be a boolean.
                                let shift = match body.get("shift") {
                                    None => Some(false),
                                    Some(value) => value.as_bool(),
                                };
                                let (Some(lines), Some(col), Some(row), Some(shift)) =
                                    (lines, col, row, shift)
                                else {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "scroll requires a nonzero integer lines and cell col and row, and a boolean shift when it is present"}});
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                    continue;
                                };
                                if tx
                                    .send(SurfaceCommand::Scroll {
                                        lines: lines as i32,
                                        col,
                                        row,
                                        shift,
                                        cids: cids.clone(),
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "screen.read" => {
                                if tx.send(SurfaceCommand::ScreenRead).await.is_err() {
                                    break;
                                }
                            }
                            "clipboard.resolve" => {
                                let request_id = body.get("requestId").and_then(Value::as_u64);
                                let text =
                                    body.get("text").and_then(Value::as_str).map(str::to_string);
                                match (request_id, text) {
                                    (Some(request_id), Some(text)) => {
                                        if tx
                                            .send(SurfaceCommand::ClipboardResolve {
                                                request_id,
                                                text,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "clipboard.resolve requires requestId and text"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "clipboard.reject" => {
                                let request_id = body.get("requestId").and_then(Value::as_u64);
                                let reason = body
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .map(str::to_string);
                                match (request_id, reason) {
                                    (Some(request_id), Some(reason)) if !reason.is_empty() => {
                                        if tx
                                            .send(SurfaceCommand::ClipboardReject {
                                                request_id,
                                                reason,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "clipboard.reject requires requestId and non-empty reason"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "image.inline.delete" => {
                                match body
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .filter(|name| !name.is_empty())
                                {
                                    Some(name) => {
                                        if tx
                                            .send(SurfaceCommand::InlineImageDelete {
                                                name: name.to_string(),
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    None => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "image.inline.delete requires a non-empty name"}});
                                        if reply_tx.reply(response).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "pty.pending" => {
                                if tx.send(SurfaceCommand::PtyPending).await.is_err() {
                                    break;
                                }
                            }
                            "close" => {
                                let close_result = close_connection_surface(
                                    registry.as_ref(),
                                    &mut surface_txs,
                                    &mut surface_epochs,
                                    &registry_key,
                                    &owner,
                                )
                                .await;
                                // After the close, it replies with an empty body, or with the
                                // reason when the close fails.
                                let body = match close_result {
                                    Ok(()) => json!({}),
                                    Err(error) => json!({"error": error}),
                                };
                                let response = json!({"surface": surface_id, "body": body});
                                if output_tx.send(response.to_string()).await.is_err() {
                                    break;
                                }
                            }
                            _ => {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"error": format!("Unknown operation: {}", operation)}
                                });
                                if reply_tx.reply(response).await.is_err() {
                                    break;
                                }
                            }
                        }
                    } else if let Some(image) = body.get("image").and_then(|v| v.as_object()) {
                        if image.contains_key("detach") {
                            let parsed = (image.len() == 1)
                                .then(|| image.get("detach"))
                                .flatten()
                                .ok_or_else(|| {
                                    "image.detach cannot share an image envelope with another action"
                                        .to_string()
                                })
                                .and_then(parse_image_detach);
                            match parsed {
                                Ok(detach) => {
                                    let request_check =
                                        detach_requests.check(&surface_id, &detach).await;
                                    if tx
                                        .send(SurfaceCommand::ImageDetach {
                                            detach,
                                            request_check,
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                Err(reason) => {
                                    let response = image
                                        .get("detach")
                                        .and_then(|detach| {
                                            image_detach_refusal(
                                                &surface_id,
                                                detach,
                                                "invalidParams",
                                                Some(&reason),
                                            )
                                        })
                                        .unwrap_or_else(|| {
                                            json!({"surface": surface_id,
                                            "body": {"error": "invalidParams", "reason": reason}})
                                        });
                                    if reply_tx.reply(response).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        } else if let Some(drawing) =
                            image.get("drawing").and_then(|v| v.as_object())
                        {
                            let parsed = (|| {
                                Some(SurfaceCommand::Drawing {
                                    name: drawing.get("name")?.as_str()?.to_string(),
                                    generation: drawing.get("generation")?.as_u64()?,
                                    drawing: drawing.get("drawing")?.as_bool()?,
                                })
                            })();
                            if let Some(command) = parsed {
                                if tx.send(command).await.is_err() {
                                    break;
                                }
                            } else {
                                let response = json!({"surface": surface_id,
                                    "body": {"error": "invalidParams", "reason": "invalid image drawing"}});
                                if reply_tx.reply(response).await.is_err() {
                                    break;
                                }
                            }
                        } else if let Some(configure) =
                            image.get("configure").and_then(|v| v.as_object())
                        {
                            let parsed = (|| {
                                let name = configure.get("name")?.as_str()?.to_string();
                                let generation = configure.get("generation")?.as_u64()?;
                                let raster = configure.get("raster")?.as_u64()?;
                                let width =
                                    // silent: a width that does not fit u32 makes the configure
                                    // invalid, which is answered with invalidParams below.
                                    u32::try_from(configure.get("width")?.as_u64()?).ok()?;
                                let height =
                                    // silent: a height that does not fit u32 makes the configure
                                    // invalid, which is answered with invalidParams below.
                                    u32::try_from(configure.get("height")?.as_u64()?).ok()?;
                                let scale = configure.get("scale")?.as_f64()? as f32;
                                (generation > 0
                                    && raster > 0
                                    && width > 0
                                    && height > 0
                                    && scale.is_finite()
                                    && scale > 0.0)
                                    .then_some(ImageConfiguration {
                                        name,
                                        generation,
                                        raster,
                                        width,
                                        height,
                                        scale,
                                    })
                            })();
                            if let Some(configuration) = parsed {
                                if tx
                                    .send(SurfaceCommand::Configure(configuration))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            } else {
                                let response = json!({"surface": surface_id,
                                    "body": {"error": "invalidParams", "reason": "invalid image configure"}});
                                if reply_tx.reply(response).await.is_err() {
                                    break;
                                }
                            }
                        } else if tx
                            .send(SurfaceCommand::ImageResponse {
                                body: body.clone(),
                                dispatched_at: log
                                    .enabled(Class::Frame, Level::Debug)
                                    .then(std::time::Instant::now),
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        // A message with neither an operation nor an image object is an unknown
                        // message.
                        let response = json!({
                            "surface": surface_id,
                            "body": {"error": "unknown operation"}
                        });
                        if reply_tx.reply(response).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                // The surface and the cids are read from the line again, so an envelope that does
                // not parse is answered on its surface when the surface is a string; a line without
                // one is answered with an empty surface.
                let line = serde_json::from_str::<Value>(trimmed).ok();
                let surface = line
                    .as_ref()
                    .and_then(|value| value.get("surface"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let cids_invalid = line
                    .as_ref()
                    .and_then(|value| value.get("cids"))
                    .is_some_and(|cids| {
                        serde_json::from_value::<Vec<String>>(cids.clone()).is_err()
                    });
                let response = match surface {
                    Some(surface) if cids_invalid => json!({
                        "surface": surface,
                        "body": {"error": "invalidParams", "reason": "cids must be a list of strings"}
                    }),
                    Some(surface) => json!({
                        "surface": surface,
                        "body": {"error": "invalidParams", "reason": format!("Parse error: {}", e)}
                    }),
                    None => json!({
                        "surface": "",
                        "body": {"error": format!("Parse error: {}", e)}
                    }),
                };
                if reply_tx.reply(response).await.is_err() {
                    // The output channel has closed, so serve ends
                    break;
                }
            }
        }
    }

    // Waits until every monitor task completes
    while tasks.join_next().await.is_some() {}
    if !ended_surfaces.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            format!(
                "surface actor ended before serve shutdown: {}",
                ended_surfaces.join(", ")
            ),
        ));
    }
    Ok(())
}

/// The read side of the connection. When data arrives after a read has waited for one second or
/// more, the number of read attempts made during that wait and the time since the last attempt are
/// recorded as `read_wait`. If there was only one attempt, the task was not woken after the data
/// arrived. If there were several, the task was woken but was told there was nothing to read.
struct PolledReader<R> {
    inner: R,
    log: Events,
    waiting_since: Option<std::time::Instant>,
    last_poll: std::time::Instant,
    pending_polls: u64,
}

impl<R> PolledReader<R> {
    fn new(inner: R, log: Events) -> Self {
        Self {
            inner,
            log,
            waiting_since: None,
            last_poll: std::time::Instant::now(),
            pending_polls: 0,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for PolledReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let now = std::time::Instant::now();
        let since_last_poll = now.duration_since(self.last_poll);
        self.last_poll = now;
        let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
        match &result {
            std::task::Poll::Pending => {
                if self.waiting_since.is_none() {
                    self.waiting_since = Some(now);
                }
                self.pending_polls += 1;
            }
            std::task::Poll::Ready(_) => {
                if let Some(started) = self.waiting_since.take() {
                    let waited = now.duration_since(started);
                    if waited >= std::time::Duration::from_secs(1) {
                        self.log.emit(
                            Level::Info,
                            "sidecar.vt.read_wait",
                            &serde_json::json!({
                                "waited_us": waited.as_micros() as u64,
                                "pending_polls": self.pending_polls,
                                "since_last_poll_us": since_last_poll.as_micros() as u64,
                                "bytes": buf.filled().len(),
                            }),
                        );
                    }
                }
                self.pending_polls = 0;
            }
        }
        result
    }
}

/// If writing one output line takes at least this long, it is recorded as blocked because the peer
/// does not read it.
const OUTPUT_BLOCKED: std::time::Duration = std::time::Duration::from_millis(10);

async fn run_output_loop<W>(
    mut writer: W,
    mut output_rx: mpsc::Receiver<String>,
    log: Events,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(output) = output_rx.recv().await {
        let started = std::time::Instant::now();
        writer.write_all(output.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        let elapsed = started.elapsed();
        // Output-blocked instrument: if the peer does not read, this write waits, and when the
        // output channel is full, the surface actions and the input loop stop.
        if elapsed >= OUTPUT_BLOCKED {
            log.emit(
                Level::Info,
                "sidecar.vt.output_blocked",
                &serde_json::json!({"us": elapsed.as_micros() as u64, "bytes": output.len()}),
            );
        }
    }
    Ok(())
}

/// The processing record of one request. It records the time from when the input loop read the
/// request until it reads the next request.
struct RequestTrace<'a> {
    request: Arc<Request>,
    /// The time the input loop waited for a read before this request arrived.
    idle: std::time::Duration,
    /// The owner of the connection that sent the request.
    owner: String,
    /// The sequence number of the connection that this service received.
    connection: u64,
    /// The request as it arrived.
    body: RequestBody<'a>,
}

/// The longest line that the request event holds whole. A longer line is recorded by its head and
/// its size, so the cost of recording does not depend on the size of the request (core
/// docs/spec/logging.md#bounds).
const REQUEST_BODY_BOUND: usize = 1_000_000;

/// The body of the request event: the text of the line, which is JSON, a line that is not JSON as
/// `{"unparsed": <line>}`, or a line over the bound as `{"head": <first 4,096 bytes>, "size":
/// <bytes>}`. The text is borrowed from the line.
enum RequestBody<'a> {
    Raw(&'a serde_json::value::RawValue),
    Unparsed(&'a str),
    Cut { head: &'a str, size: usize },
}

impl<'a> RequestBody<'a> {
    /// `json` tells that the line parsed, so it is valid JSON text.
    fn of(line: &'a str, json: bool) -> Self {
        if line.len() > REQUEST_BODY_BOUND {
            return Self::Cut {
                head: crate::events::head(line, crate::events::HEAD_BYTES),
                size: line.len(),
            };
        }
        if json && let Ok(raw) = serde_json::from_str::<&serde_json::value::RawValue>(line) {
            return Self::Raw(raw);
        }
        Self::Unparsed(line)
    }
}

impl Serialize for RequestBody<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::Raw(raw) => raw.serialize(serializer),
            Self::Unparsed(line) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("unparsed", line)?;
                map.end()
            }
            Self::Cut { head, size } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("head", head)?;
                map.serialize_entry("size", size)?;
                map.end()
            }
        }
    }
}

#[derive(Serialize)]
struct RequestFields<'a> {
    operation: &'a str,
    owner: &'a str,
    connection: u64,
    idle_us: u64,
    us: u64,
    body: &'a RequestBody<'a>,
}

impl Drop for RequestTrace<'_> {
    fn drop(&mut self) {
        let request = &self.request;
        let subject = Subject {
            surface: request.surface.as_deref(),
            session: None,
            cids: &request.cids,
        };
        request.log.emit_for(
            Level::Info,
            "sidecar.vt.request",
            subject,
            &RequestFields {
                operation: &request.operation,
                owner: &self.owner,
                connection: self.connection,
                idle_us: self.idle.as_micros() as u64,
                us: request.started.elapsed().as_micros() as u64,
                body: &self.body,
            },
        );
    }
}

/// The operation name of a request: operation, closed, the operation in body, or the first key of
/// body followed by the first key of its value (for example, image.configure).
pub fn request_operation(value: &Value) -> String {
    if let Some(operation) = value.get("operation").and_then(Value::as_str) {
        return operation.to_string();
    }
    if value.get("closed") == Some(&Value::Bool(true)) {
        return "closed".to_string();
    }
    let body = value.get("body").and_then(Value::as_object);
    if let Some(operation) = body
        .and_then(|body| body.get("operation"))
        .and_then(Value::as_str)
    {
        return operation.to_string();
    }
    let Some((key, inner)) = body.and_then(|body| body.iter().next()) else {
        return "unknown".to_string();
    };
    match inner.as_object().and_then(|object| object.keys().next()) {
        Some(sub) => format!("{key}.{sub}"),
        None => key.clone(),
    }
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|error| format!("invalid base64 input: {error}"))
}

/// Creates the default SessionPort factory (uses DaemonFinder)
pub fn make_default_session_port_factory(
    log: Events,
) -> Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> {
    let service = Arc::new(crate::pty::PtyService::new(log));
    Arc::new(move || Arc::new(LocalSessionPort::new(service.clone())) as Arc<dyn SessionPort>)
}

/// The in-process PTY session port. The service is shared by all surface ports. Each `open` call
/// still creates one independent PTY session.
pub struct LocalSessionPort {
    service: Arc<crate::pty::PtyService>,
    owner: String,
    events_tx: mpsc::UnboundedSender<DaemonEvent>,
    events_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<DaemonEvent>>>>,
    attachments: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
}

impl LocalSessionPort {
    fn new(service: Arc<crate::pty::PtyService>) -> Self {
        Self::new_with_owner(service, String::new())
    }

    pub fn new_with_owner(service: Arc<crate::pty::PtyService>, owner: String) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            service,
            owner,
            events_tx,
            events_rx: Arc::new(tokio::sync::Mutex::new(Some(events_rx))),
            attachments: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl SessionPort for LocalSessionPort {
    async fn open(&self, request: &ShellRequest, cols: u16, rows: u16) -> Result<String, String> {
        let service = Arc::clone(&self.service);
        let owner = self.owner.clone();
        let shell = crate::pty::resolve_shell(&request.shell)?;
        let directory = request
            .directory
            .as_deref()
            .map(crate::pty::resolve_directory)
            .transpose()?;
        let events = self.events_tx.clone();
        let (session_id, attachment_id) = tokio::task::spawn_blocking(move || {
            service.open_shell(&owner, &shell, directory.as_deref(), cols, rows, events)
        })
        .await
        .map_err(|error| format!("open PTY task failed: {error}"))??;
        self.attachments
            .lock()
            .await
            .insert(session_id.clone(), attachment_id);
        Ok(session_id)
    }

    async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        // A write waits while the child does not read, so it runs outside the runtime workers.
        let service = Arc::clone(&self.service);
        let session_id = session_id.to_string();
        let data = data.to_vec();
        tokio::task::spawn_blocking(move || service.write(&session_id, &data))
            .await
            .map_err(|error| format!("write PTY task failed: {error}"))?
    }

    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        self.service.resize(session_id, cols, rows)
    }

    async fn detach(&self, session_id: &str) -> Result<(), String> {
        if let Some(attachment_id) = self.attachments.lock().await.remove(session_id) {
            self.service.detach(session_id, &attachment_id)?;
        }
        Ok(())
    }

    async fn close(&self, session_id: &str) -> Result<(), String> {
        self.attachments.lock().await.remove(session_id);
        let service = Arc::clone(&self.service);
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || service.close(&session_id))
            .await
            .map_err(|error| format!("close PTY task failed: {error}"))?
    }

    async fn attach(&self, session_id: &str, from: i64) -> Result<String, String> {
        let attachment_id = self
            .service
            .attach(session_id, from, self.events_tx.clone())?;
        self.attachments
            .lock()
            .await
            .insert(session_id.to_string(), attachment_id.clone());
        Ok(attachment_id)
    }

    async fn pty_measurement(
        &self,
        session_id: &str,
    ) -> Result<crate::pty::PtyMeasurement, String> {
        self.service.pty_measurement(session_id)
    }

    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent> {
        let (tx, rx) = mpsc::channel(128);
        let mut events_rx = self.events_rx.lock().await;
        if let Some(mut source) = events_rx.take() {
            tokio::spawn(async move {
                while let Some(event) = source.recv().await {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
            });
        }
        rx
    }
}

/// Base64 encode
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
#[path = "../tests/support/protocol_test.rs"]
mod tests;

/// Prepares one complete screen line before its actor submits subsequent events.
async fn prepare_screen_line<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| format!("screen preparation task failed: {error}"))?
}

#[cfg(test)]
#[path = "../tests/support/screen_preparation_test.rs"]
mod screen_preparation_test;

#[cfg(test)]
#[path = "../tests/support/transfer_store_poison_test.rs"]
mod transfer_store_poison_test;
