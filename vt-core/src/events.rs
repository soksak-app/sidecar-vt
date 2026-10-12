//! The event writer of the service (core docs/spec/logging.md#writers).
//!
//! A producer pushes an event to a queue bounded by count and by bytes without waiting. One writer thread assigns `seq`,
//! serializes the JSON line, writes batches, rotates the file and records the events that the queue refused as
//! `log.dropped`. The file is `<log-dir>/vt-<boot>.jsonl`; every event has the layer `sidecar` and a name that starts
//! with `sidecar.vt.`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use serde_json::json;
use serde_json::value::{to_raw_value, RawValue};

use crate::policy::{is_being_written, parse_policy, Class, Gate, Policy};

/// The schema number of the lines that this writer writes.
pub const SCHEMA: u64 = 2;

/// The role of this process in the name of its file and in `log.open`.
pub const ROLE: &str = "vt";

/// The layer of every event of this process.
const LAYER: &str = "sidecar";

/// The prefix of every event name of this process, except the events of the writer (`log.*`).
const PREFIX: &str = "sidecar.vt.";

/// The catalog of the events of this service, which names the class of each event
/// (core docs/spec/logging.md#classes-and-policy).
const CATALOG: &str = include_str!("../../vt-alacritty/events.json");

/// The name of the policy file in the parent folder of the log folder (core docs/spec/logging.md#policy-file).
pub const POLICY_FILE: &str = "log-policy.json";

/// The class that the catalog declares for the event `event`; none for an event that it does not declare.
fn class_of(event: &str) -> Option<Class> {
    static CLASSES: std::sync::OnceLock<std::collections::HashMap<String, Class>> =
        std::sync::OnceLock::new();
    CLASSES
        .get_or_init(|| {
            let catalog: serde_json::Value =
                serde_json::from_str(CATALOG).expect("the catalog of the service is JSON");
            catalog["events"]
                .as_object()
                .expect("the catalog declares events")
                .iter()
                .map(|(name, declaration)| {
                    let class = declaration["class"].as_str().and_then(Class::from_name);
                    let Some(class) = class else {
                        panic!("the catalog declares no class for {name}");
                    };
                    (name.clone(), class)
                })
                .collect()
        })
        .get(event)
        .copied()
}

/// The size in bytes of the JSON text of the fields of an event above which the writer stores only their head.
pub const FIELDS_BOUND: usize = 1_048_576;

/// The bytes of the head that an event over the bound keeps.
pub const HEAD_BYTES: usize = 4096;

/// The first `bound` bytes of `text`, cut on a character boundary.
pub fn head(text: &str, bound: usize) -> &str {
    let mut end = bound.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The level of an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// The levels from the most severe to the most verbose.
    pub const ALL: [Level; 5] = [
        Level::Error,
        Level::Warn,
        Level::Info,
        Level::Debug,
        Level::Trace,
    ];

    /// 1 for `error` to 5 for `trace`; the policy file's `off` is 0.
    pub fn rank(self) -> u8 {
        self as u8 + 1
    }

    pub fn from_name(name: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|level| level.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }
}

/// The objects an event concerns, written at the top level of the line. A request that continued inputs carries their
/// correlation identifiers in `cids`.
#[derive(Clone, Copy, Default)]
pub struct Subject<'a> {
    pub surface: Option<&'a str>,
    pub session: Option<&'a str>,
    pub cids: &'a [String],
}

impl<'a> Subject<'a> {
    /// An event that concerns no object.
    pub const NONE: Subject<'static> = Subject {
        surface: None,
        session: None,
        cids: &[],
    };

    /// The subject of a session and the inputs that caused the event.
    pub fn session(session: &'a str, cids: &'a [String]) -> Self {
        Self {
            surface: None,
            session: Some(session),
            cids,
        }
    }

    /// The subject of a surface.
    pub fn surface(surface: &'a str) -> Self {
        Self {
            surface: Some(surface),
            session: None,
            cids: &[],
        }
    }
}

/// How a writer is started.
pub struct Options {
    /// The log folder.
    pub directory: PathBuf,
    /// The identifier of this run.
    pub boot: String,
    /// The bound of the queue in events.
    pub queue_events: usize,
    /// The bound of the queue in bytes of serialized fields.
    pub queue_bytes: usize,
    /// How long the writer waits for more events before it writes a batch.
    pub batch_interval: Duration,
    /// The size at which the writer starts a new file; the file that was full becomes generation 1.
    pub rotate_bytes: u64,
    /// The number of earlier files that are kept (`.1` is the newest); the oldest one is removed.
    pub generations: usize,
    /// The policy file that the writer reads when it starts and watches (core docs/spec/logging.md#policy-file); none
    /// leaves the default policy in force.
    pub policy_file: Option<PathBuf>,
}

impl Options {
    /// The bounds of the service: events and bytes of serialized fields waiting for the batch interval, rotation at
    /// 100 MB with 5 earlier files.
    pub fn service(directory: PathBuf, boot: String) -> Self {
        Self {
            directory,
            boot,
            queue_events: 8192,
            queue_bytes: 16 << 20,
            batch_interval: Duration::from_millis(100),
            rotate_bytes: 100 << 20,
            generations: 5,
            policy_file: None,
        }
    }
}

/// The identifier of this run, shared by the files of the run: the start time in seconds and the process identifier.
pub fn process_boot() -> &'static str {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is before the Unix epoch")
            .as_secs();
        format!("{seconds}-{}", std::process::id())
    })
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is before the Unix epoch")
        .as_micros() as u64
}

/// The monotonic clock in nanoseconds, which `mach_absolute_time` also reads and which is comparable across the
/// processes of one boot.
fn mono_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid timespec that lives through the call.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut time) };
    assert_eq!(status, 0, "the monotonic clock cannot be read");
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

/// One event waiting for the writer. `seq` is assigned when the writer serializes it.
struct Pending {
    ts_us: u64,
    mono_ns: u64,
    level: Level,
    event: String,
    /// The class of the catalog; none for an event that the catalog does not declare.
    class: Option<Class>,
    surface: Option<String>,
    session: Option<String>,
    cids: Vec<String>,
    /// The fields as JSON text, serialized once by the producer.
    fields: Box<RawValue>,
    /// The size of the text of the fields before the cut, for an event over `FIELDS_BOUND`.
    truncated: Option<u64>,
    bytes: usize,
}

impl Pending {
    /// An event of the writer itself, which concerns no object. The events `log.*` are of the class `lifecycle`.
    fn of_writer(level: Level, event: &str, fields: Box<RawValue>) -> Self {
        Self {
            ts_us: now_us(),
            mono_ns: mono_ns(),
            level,
            event: event.to_string(),
            class: class_of(event).or(Some(Class::Lifecycle)),
            surface: None,
            session: None,
            cids: Vec::new(),
            fields,
            truncated: None,
            bytes: 0,
        }
    }
}

/// What the queue carries: an event, or a request of the owner. Two slots of the queue are reserved for requests, so a
/// flush or a stop is never refused because events filled the queue.
enum Message {
    Event(Pending),
    /// Write everything that is queued, then answer.
    Flush(SyncSender<()>),
    /// Write everything that is queued, then end.
    Stop,
    /// The file system reported a change of the policy file: read it and put the policy in force.
    PolicyChanged,
    /// The file system events of the policy file failed with this error.
    PolicyWatchFailed(String),
}

/// The slots of the queue that only requests use.
const RESERVED_FOR_REQUESTS: usize = 2;

/// Why an event never reached the file. The writer records each reason as its own `log.dropped`.
#[derive(Clone, Copy)]
enum Reason {
    QueueFull,
    OutsideNamespace,
    Unserializable,
}

impl Reason {
    const ALL: [Reason; 3] = [
        Reason::QueueFull,
        Reason::OutsideNamespace,
        Reason::Unserializable,
    ];

    fn text(self) -> &'static str {
        match self {
            Reason::QueueFull => "the queue was full",
            Reason::OutsideNamespace => "an event name outside the namespace of the service",
            Reason::Unserializable => "fields that cannot be serialized",
        }
    }
}

/// The events and bytes that producers refused since the writer last recorded them, for each reason.
struct Loss {
    counts: [(AtomicU64, AtomicU64); 3],
}

impl Loss {
    fn new() -> Self {
        Self {
            counts: std::array::from_fn(|_| (AtomicU64::new(0), AtomicU64::new(0))),
        }
    }

    fn add(&self, reason: Reason, events: u64, bytes: u64) {
        let (event_count, byte_count) = &self.counts[reason as usize];
        event_count.fetch_add(events, Ordering::Relaxed);
        byte_count.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Takes the counts of one reason.
    fn take(&self, reason: Reason) -> (u64, u64) {
        let (event_count, byte_count) = &self.counts[reason as usize];
        (
            event_count.swap(0, Ordering::Relaxed),
            byte_count.swap(0, Ordering::Relaxed),
        )
    }
}

pub struct EventLog {
    sender: SyncSender<Message>,
    queued_events: Arc<AtomicUsize>,
    queue_events: usize,
    queued_bytes: Arc<AtomicUsize>,
    queue_bytes: usize,
    loss: Arc<Loss>,
    gate: Arc<Gate>,
    path: PathBuf,
    boot: String,
    /// The watch of the policy file; it ends before the writer does.
    watcher: Mutex<Option<RecommendedWatcher>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

/// The writer that the connections, the surfaces and the sessions of the service share.
pub type Events = Arc<EventLog>;

impl EventLog {
    /// Starts the writer of the service in `directory` with the bounds of the service and the boot of this process. The
    /// policy file is `log-policy.json` in the parent folder of `directory`.
    pub fn start_service(directory: &Path) -> Result<Events, String> {
        if !directory.is_absolute() {
            return Err(format!(
                "{}: the log directory must be an absolute path",
                directory.display()
            ));
        }
        let parent = directory.parent().ok_or_else(|| {
            format!(
                "{}: the log directory has no parent folder for {POLICY_FILE}",
                directory.display()
            )
        })?;
        let mut options = Options::service(directory.to_path_buf(), process_boot().to_string());
        options.policy_file = Some(parent.join(POLICY_FILE));
        EventLog::start(options).map(Arc::new)
    }

    /// Creates the folder (mode 0700) and the file (mode 0600), writes `log.open` and starts the writer thread.
    pub fn start(options: Options) -> Result<EventLog, String> {
        if options.rotate_bytes == 0 {
            return Err("event writer: rotate_bytes must be positive".into());
        }
        if options.generations == 0 {
            return Err("event writer: generations must be positive".into());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&options.directory)
            .map_err(|e| format!("{}: {e}", options.directory.display()))?;
        // A folder that existed before keeps its mode; the log folder of the contract is private.
        std::fs::set_permissions(&options.directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", options.directory.display()))?;
        let path = options
            .directory
            .join(format!("{ROLE}-{}.jsonl", options.boot));
        let debug_path = options
            .directory
            .join(format!("{ROLE}-{}.debug.jsonl", options.boot));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (sender, receiver) = sync_channel(options.queue_events + RESERVED_FOR_REQUESTS);
        let queued_events = Arc::new(AtomicUsize::new(0));
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let loss = Arc::new(Loss::new());
        // The watch starts before the first read, so a change between the two reaches the writer as a notification.
        let watcher = match &options.policy_file {
            Some(file) => Some(watch_policy_file(file, &sender)?),
            None => None,
        };
        let policy = PolicyState::start(options.policy_file.clone());
        let gate = Arc::clone(&policy.gate);
        let open = Pending::of_writer(
            Level::Info,
            "log.open",
            to_raw_value(&json!({
                "schema": SCHEMA,
                "pid": std::process::id(),
                "role": ROLE,
                "boot": options.boot,
                "version": env!("CARGO_PKG_VERSION"),
            }))
            .map_err(|e| format!("log.open: {e}"))?,
        );
        let writer = {
            let queued_bytes = Arc::clone(&queued_bytes);
            let queued_events = Arc::clone(&queued_events);
            let loss = Arc::clone(&loss);
            let interval = options.batch_interval;
            let path_for_writer = path.clone();
            let debug_path_for_writer = debug_path.clone();
            let rotate_bytes = options.rotate_bytes;
            let generations = options.generations;
            std::thread::Builder::new()
                .name("event-log-writer".into())
                .spawn(move || {
                    write_loop(
                        Output::new(
                            file,
                            path_for_writer,
                            debug_path_for_writer,
                            rotate_bytes,
                            generations,
                            &open,
                            policy,
                        ),
                        receiver,
                        queued_bytes,
                        queued_events,
                        loss,
                        interval,
                        open,
                    )
                })
                .map_err(|e| format!("event writer thread: {e}"))?
        };
        Ok(EventLog {
            sender,
            queued_events,
            queue_events: options.queue_events,
            queued_bytes,
            queue_bytes: options.queue_bytes,
            loss,
            gate,
            path,
            boot: options.boot.clone(),
            watcher: Mutex::new(watcher),
            writer: Mutex::new(Some(writer)),
        })
    }

    /// Whether the policy stores the events of `class` at `level`: one atomic read, so a producer checks it before it
    /// builds fields. A refusal is counted, and the writer states the count as `log.gated`.
    pub fn gate(&self, class: Class, level: Level) -> bool {
        self.gate.allows(class, level)
    }

    /// Whether the policy stores the events of `class` at `level`, without counting a refusal.
    pub fn enabled(&self, class: Class, level: Level) -> bool {
        self.gate.enabled(class, level)
    }

    /// The file this writer writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The identifier of the run in the name of the file, which is the replay segment of its frame records (core
    /// docs/spec/replay.md#segments).
    pub fn boot(&self) -> &str {
        &self.boot
    }

    /// Queues an event that concerns no object. See [`EventLog::emit_for`].
    pub fn emit<T: Serialize>(&self, level: Level, event: &str, fields: &T) -> bool {
        // events: forwarded
        self.emit_for(level, event, Subject::NONE, fields)
    }

    /// Queues an event. Returns false when the policy does not store the class of the event at its level (the refusal is
    /// counted as a gated event), and false, with the loss counted, when the queue has no room or the name does not start
    /// with `sidecar.vt.`. It never waits. The fields are serialized once, and only after the queue is known to have room
    /// for another event.
    pub fn emit_for<T: Serialize>(
        &self,
        level: Level,
        event: &str,
        subject: Subject<'_>,
        fields: &T,
    ) -> bool {
        if !event.starts_with(PREFIX) {
            self.loss
                .add(Reason::OutsideNamespace, 1, event.len() as u64);
            self.record_error(
                "event writer",
                format!("the event {event:?} does not start with {PREFIX:?}"),
            );
            return false;
        }
        let class = class_of(event);
        if let Some(class) = class {
            if !self.gate.allows(class, level) {
                return false;
            }
        }
        if self.queued_events.load(Ordering::Relaxed) >= self.queue_events {
            self.loss.add(Reason::QueueFull, 1, event.len() as u64);
            return false;
        }
        let fields = match to_raw_value(fields) {
            Ok(fields) => fields,
            Err(error) => {
                // A value that cannot be serialized is a defect of the caller: it is counted in place of the event.
                self.loss.add(Reason::Unserializable, 1, event.len() as u64);
                self.record_error(
                    "event writer",
                    format!("the fields of {event} cannot be serialized: {error}"),
                );
                return false;
            }
        };
        // Fields over the bound are stored by their head, and the event states the size they had.
        let size = fields.get().len();
        let (fields, truncated) = if size > FIELDS_BOUND {
            let cut = to_raw_value(&json!({"head": head(fields.get(), HEAD_BYTES)}))
                .expect("a json value serializes");
            (cut, Some(size as u64))
        } else {
            (fields, None)
        };
        self.enqueue(Pending {
            ts_us: now_us(),
            mono_ns: mono_ns(),
            level,
            event: event.to_string(),
            class,
            surface: subject.surface.map(str::to_string),
            session: subject.session.map(str::to_string),
            cids: subject.cids.to_vec(),
            fields,
            truncated,
            bytes: 0,
        })
    }

    fn enqueue(&self, mut pending: Pending) -> bool {
        let bytes = pending.fields.get().len()
            + pending.event.len()
            + pending.cids.iter().map(String::len).sum::<usize>();
        if self.queued_bytes.load(Ordering::Relaxed) + bytes > self.queue_bytes {
            self.loss.add(Reason::QueueFull, 1, bytes as u64);
            return false;
        }
        pending.bytes = bytes;
        self.queued_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.queued_events.fetch_add(1, Ordering::Relaxed);
        match self.sender.try_send(Message::Event(pending)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
                self.queued_events.fetch_sub(1, Ordering::Relaxed);
                self.loss.add(Reason::QueueFull, 1, bytes as u64);
                false
            }
        }
    }

    /// Records what the service reports to its operator as an error: the event `sidecar.vt.record.error` with the
    /// place (`where`) and the text.
    pub fn record_error(&self, place: &str, text: impl std::fmt::Display) {
        self.emit(
            Level::Error,
            "sidecar.vt.record.error",
            &json!({"where": place, "text": text.to_string()}),
        );
    }

    /// Records what the service reports to its operator as information: the event `sidecar.vt.record.info`.
    pub fn record_info(&self, place: &str, text: impl std::fmt::Display) {
        self.emit(
            Level::Info,
            "sidecar.vt.record.info",
            &json!({"where": place, "text": text.to_string()}),
        );
    }

    /// Writes everything that is queued and waits until it is on disk, at most `deadline`.
    pub fn flush(&self, deadline: Duration) -> Result<(), String> {
        let (done, wait) = sync_channel(1);
        // A request has a reserved slot, so this send is not refused by a full queue of events.
        self.sender
            .try_send(Message::Flush(done))
            .map_err(|e| format!("the event writer did not take the flush request: {e}"))?;
        wait.recv_timeout(deadline)
            .map_err(|_| format!("the event writer did not flush within {deadline:?}"))
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        // The watch ends first, so no notification is sent to a queue that is being closed.
        if let Ok(mut watcher) = self.watcher.lock() {
            watcher.take();
        }
        // The stop waits for room in the queue: the writer takes events while it runs. A writer that ended can no longer
        // record its own end, so the standard error of the process is the only place left for these two reports.
        if let Err(error) = self.sender.send(Message::Stop) {
            eprintln!("event writer: the stop request was not taken: {error}");
        }
        if let Ok(mut writer) = self.writer.lock() {
            if let Some(handle) = writer.take() {
                if handle.join().is_err() {
                    eprintln!("event writer: the writer thread panicked");
                }
            }
        }
    }
}

/// The line of one event, with the `seq` that the writer gives it.
fn line(seq: u64, pending: &Pending) -> String {
    let text = |value: &str| serde_json::to_string(value).expect("a string serializes");
    // The fields are already JSON text, so the line is assembled without parsing them again.
    let mut line = format!(
        "{{\"ts_us\":{},\"mono_ns\":{},\"seq\":{},\"level\":\"{}\"",
        pending.ts_us,
        pending.mono_ns,
        seq,
        pending.level.name(),
    );
    if let Some(class) = pending.class {
        line.push_str(&format!(",\"class\":\"{}\"", class.name()));
    }
    line.push_str(&format!(
        ",\"layer\":\"{LAYER}\",\"event\":{}",
        text(&pending.event)
    ));
    if let Some(surface) = &pending.surface {
        line.push_str(&format!(",\"surface\":{}", text(surface)));
    }
    if let Some(session) = &pending.session {
        line.push_str(&format!(",\"session\":{}", text(session)));
    }
    if !pending.cids.is_empty() {
        line.push_str(&format!(
            ",\"cids\":{}",
            serde_json::to_string(&pending.cids).expect("strings serialize")
        ));
    }
    if pending.class.is_none() {
        line.push_str(",\"undeclared\":true");
    }
    line.push_str(&format!(",\"fields\":{}", pending.fields.get()));
    if let Some(size) = pending.truncated {
        line.push_str(&format!(",\"truncated\":{size}"));
    }
    line.push('}');
    line
}

/// A range of `seq` whose lines a failed write lost.
struct Lost {
    from_seq: u64,
    to_seq: u64,
    bytes: u64,
    reason: String,
}

/// What the policy file holds now.
enum PolicyFile {
    /// There is no file: the default policy is in force.
    Absent,
    /// The file is empty or holds white space: its writer is still writing it.
    Waiting,
    Policy(Policy),
}

/// Reads the policy file. A refusal is the error text of the contract, which names the file and what was found.
fn read_policy_file(file: &Path) -> Result<PolicyFile, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PolicyFile::Absent)
        }
        Err(error) => return Err(format!("{}: {error}", file.display())),
    };
    if is_being_written(&text) {
        return Ok(PolicyFile::Waiting);
    }
    parse_policy(&file.display().to_string(), &text).map(PolicyFile::Policy)
}

/// Starts the watch of the policy file: a notification of the file system for the file puts a request to read it in the
/// queue of the writer. The folder is watched and not the file, because the host replaces the file by a rename and the
/// file may not exist yet. No timer reads the file. The send waits for room in the queue; it fails only when the writer
/// has ended.
fn watch_policy_file(
    file: &Path,
    sender: &SyncSender<Message>,
) -> Result<RecommendedWatcher, String> {
    let folder = file
        .parent()
        .ok_or_else(|| format!("{}: the policy file has no folder", file.display()))?;
    let name = file
        .file_name()
        .ok_or_else(|| format!("{}: the policy file has no name", file.display()))?
        .to_os_string();
    let sender = sender.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let message = match result {
            Ok(event) => {
                if !event
                    .paths
                    .iter()
                    .any(|path| path.file_name() == Some(&name))
                {
                    return;
                }
                Message::PolicyChanged
            }
            Err(error) => Message::PolicyWatchFailed(error.to_string()),
        };
        // silent: the send fails only after the writer ended, when there is no file to record the notification in.
        let _ = sender.send(message);
    })
    .map_err(|error| format!("{}: watch the policy file: {error}", file.display()))?;
    watcher
        .watch(folder, RecursiveMode::NonRecursive)
        .map_err(|error| format!("{}: watch the policy file: {error}", folder.display()))?;
    Ok(watcher)
}

/// The policy of the writer: the file it reads, the policy in force and the gate that the producers ask.
struct PolicyState {
    file: Option<PathBuf>,
    gate: Arc<Gate>,
    current: Policy,
    /// How the first policy was found, which `log.policy` states when the writer starts.
    start: Applied,
}

/// The policy that the writer applied when it started, and the refusal of a file that could not be applied.
struct Applied {
    source: &'static str,
    refused: Option<String>,
}

impl PolicyState {
    /// Puts the policy of the file in force, or the default policy when there is no file or it cannot be applied.
    fn start(file: Option<PathBuf>) -> PolicyState {
        let (current, source, refused) = match file.as_deref().map(read_policy_file) {
            Some(Ok(PolicyFile::Policy(policy))) => (policy, "file", None),
            Some(Err(error)) => (Policy::default(), "default", Some(error)),
            _ => (Policy::default(), "default", None),
        };
        PolicyState {
            file,
            gate: Arc::new(Gate::new(&current)),
            current,
            start: Applied { source, refused },
        }
    }
}

/// The file of the writer with its batch, its size and the sequence number of the last line.
/// One file of the writer: the main file of the levels error, warn and info, or the debug file of the levels debug and
/// trace. The debug file is `None` until the first line of its channel is written, so a run that stores no such level
/// leaves no empty file.
struct LogFile {
    path: PathBuf,
    file: Option<File>,
    /// The bytes of the file, written or waiting in the batch.
    size: u64,
    /// The bytes of the file that reached it as whole lines.
    file_len: u64,
    /// The size at which a rotation that failed is tried again.
    retry_rotation_at: u64,
    /// The lines that wait for the next write, with the `seq` of the first and their number.
    batch: String,
    batch_first_seq: u64,
    batch_lines: u64,
    /// The lines that failed writes lost; `log.dropped` states them when writing works again.
    lost: Option<Lost>,
}

impl LogFile {
    fn new(path: PathBuf, file: Option<File>) -> LogFile {
        LogFile {
            path,
            file,
            size: 0,
            file_len: 0,
            retry_rotation_at: 0,
            batch: String::new(),
            batch_first_seq: 0,
            batch_lines: 0,
            lost: None,
        }
    }
}

/// The channel of a level: 0 holds error, warn and info in the main file, 1 holds debug and trace in the debug file.
fn channel_of(level: Level) -> usize {
    match level {
        Level::Debug | Level::Trace => 1,
        Level::Error | Level::Warn | Level::Info => 0,
    }
}

/// The two files of the writer with the sequence number of the last line, which runs across both.
struct Output {
    policy: PolicyState,
    files: [LogFile; 2],
    rotate_bytes: u64,
    generations: usize,
    /// The fields of `log.open`, written again at the start of every file.
    open_fields: String,
    seq: u64,
}

impl Output {
    #[allow(clippy::too_many_arguments)]
    fn new(
        file: File,
        path: PathBuf,
        debug_path: PathBuf,
        rotate_bytes: u64,
        generations: usize,
        open: &Pending,
        policy: PolicyState,
    ) -> Output {
        Output {
            policy,
            files: [
                LogFile::new(path, Some(file)),
                LogFile::new(debug_path, None),
            ],
            rotate_bytes,
            generations,
            open_fields: open.fields.get().to_string(),
            seq: 0,
        }
    }

    /// Adds the line of an event with the next `seq` to the batch of a file.
    fn append(&mut self, channel: usize, pending: &Pending) {
        self.seq += 1;
        let text = format!("{}\n", line(self.seq, pending));
        let file = &mut self.files[channel];
        if file.batch_lines == 0 {
            file.batch_first_seq = self.seq;
        }
        file.batch_lines += 1;
        file.size += text.len() as u64;
        file.batch.push_str(&text);
    }

    /// Records a failure of the writer itself as an error record of the file.
    fn record_failure(&mut self, text: String) {
        self.record_error("event writer", text);
    }

    fn record_error(&mut self, place: &str, text: String) {
        let record = Pending::of_writer(
            Level::Error,
            "sidecar.vt.record.error",
            to_raw_value(&json!({"where": place, "text": text})).expect("a json value serializes"),
        );
        self.push(&record);
    }

    /// States the policy that the writer found when it started, and the refusal of its file.
    fn start_policy(&mut self) {
        if let Some(error) = self.policy.start.refused.take() {
            self.record_error("log policy", error);
        }
        self.state_policy(self.policy.start.source);
    }

    /// Stores the event `log.policy` for the policy in force.
    fn state_policy(&mut self, source: &str) {
        let event = Pending::of_writer(
            Level::Info,
            "log.policy",
            to_raw_value(&json!({"source": source, "classes": self.policy.current.classes()}))
                .expect("a json value serializes"),
        );
        self.push(&event);
    }

    /// The file system reported a change of the policy file: the file is read and its policy put in force. A refused file
    /// is an error record and the policy in force stays, an empty file is waited on, a removed file puts the default in
    /// force, and the same policy again stores nothing.
    fn policy_changed(&mut self) {
        let Some(file) = self.policy.file.clone() else {
            return;
        };
        let (policy, source) = match read_policy_file(&file) {
            Ok(PolicyFile::Waiting) => return,
            Ok(PolicyFile::Absent) => (Policy::default(), "default"),
            Ok(PolicyFile::Policy(policy)) => (policy, "file"),
            Err(error) => {
                self.record_error("log policy", error);
                return;
            }
        };
        if policy == self.policy.current {
            return;
        }
        self.policy.current = policy;
        self.policy.gate.set(&policy);
        self.state_policy(source);
    }

    /// Stores one `log.gated` for each class and level whose events the policy refused since the last report.
    fn gated(&mut self) {
        for (class, level, count) in self.policy.gate.take_refused() {
            let event = Pending::of_writer(
                Level::Info,
                "log.gated",
                to_raw_value(
                    &json!({"class": class.name(), "level": level.name(), "count": count}),
                )
                .expect("a json value serializes"),
            );
            self.push(&event);
        }
    }

    /// The `log.open` that starts every file, with the fields of the run.
    fn open_event(&self) -> Pending {
        Pending::of_writer(
            Level::Info,
            "log.open",
            RawValue::from_string(self.open_fields.clone())
                .expect("the fields of log.open are JSON"),
        )
    }

    /// Adds an event to the file of its channel. A file that the line would take past the bound is closed first; the new
    /// file opens with `log.open` and the record of the rotation, so every file of the contract starts with `log.open` and
    /// `seq` continues. A rotation that fails is tried again after another `rotate_bytes` of lines.
    fn push(&mut self, pending: &Pending) {
        let channel = channel_of(pending.level);
        if self.files[channel].file.is_none() {
            match open_log_file(&self.files[channel].path) {
                Ok(file) => {
                    self.files[channel].file = Some(file);
                    let open = self.open_event();
                    self.append(channel, &open);
                }
                Err(error) => {
                    // The event is not written; its seq stays taken, so the audit shows the gap next to this report.
                    self.seq += 1;
                    eprintln!(
                        "event writer: {} cannot be written: {}: {error}",
                        pending.event,
                        self.files[channel].path.display()
                    );
                    return;
                }
            }
        }
        let next = line(self.seq + 1, pending).len() as u64 + 1;
        if self.files[channel].size > 0
            && self.files[channel].size + next > self.rotate_bytes
            && self.files[channel].size >= self.files[channel].retry_rotation_at
        {
            self.write_batch(channel);
            let full = self.files[channel].size;
            match self.rotate(channel) {
                Ok(removed) => {
                    let file = &mut self.files[channel];
                    file.size = 0;
                    file.file_len = 0;
                    file.retry_rotation_at = 0;
                    let open = self.open_event();
                    self.append(channel, &open);
                    let rotated = Pending::of_writer(
                        Level::Info,
                        "log.rotated",
                        to_raw_value(&json!({
                            "bytes": full,
                            "kept": self.generations,
                            "removed": removed,
                        }))
                        .expect("a json value serializes"),
                    );
                    self.append(channel, &rotated);
                }
                Err(error) => {
                    self.files[channel].retry_rotation_at =
                        self.files[channel].size + self.rotate_bytes;
                    self.record_failure(error);
                }
            }
        }
        self.append(channel, pending);
    }

    /// Stores one `log.dropped` after the range of `seq` that the refused events would have had. The refused events had no
    /// `seq`; their numbers are skipped (core docs/spec/logging.md#event).
    fn dropped(&mut self, events: u64, bytes: u64, reason: &str) {
        if events == 0 {
            return;
        }
        let from = self.seq + 1;
        self.seq += events;
        let pending = Pending::of_writer(
            Level::Warn,
            "log.dropped",
            to_raw_value(&json!({
                "from_seq": from,
                "to_seq": self.seq,
                "count": events,
                "bytes": bytes,
                "reason": reason,
                // The queue holds the events of both channels, so its loss names the channel other.
                "channel": "other",
            }))
            .expect("a json value serializes"),
        );
        self.push(&pending);
    }

    /// Writes the batch. A write that fails loses the lines of the batch: the file returns to its last whole line, and the
    /// range of `seq` is stated by `log.dropped` after the next write that succeeds.
    fn flush(&mut self) {
        for channel in 0..2 {
            if !self.write_batch(channel) {
                continue;
            }
            let Some(lost) = self.files[channel].lost.take() else {
                continue;
            };
            let pending = Pending::of_writer(
                Level::Warn,
                "log.dropped",
                to_raw_value(&json!({
                    "from_seq": lost.from_seq,
                    "to_seq": lost.to_seq,
                    "count": lost.to_seq - lost.from_seq + 1,
                    "bytes": lost.bytes,
                    "reason": lost.reason,
                    // The batch held the lines of one file, so its loss names that file.
                    "channel": if channel == 0 { "other" } else { "debug" },
                }))
                .expect("a json value serializes"),
            );
            self.append(0, &pending);
            self.write_batch(0);
        }
    }

    /// Writes the batch of one file; false when the write failed and the batch was counted as lost.
    fn write_batch(&mut self, channel: usize) -> bool {
        if self.files[channel].batch.is_empty() {
            return true;
        }
        let Some(file) = self.files[channel].file.as_mut() else {
            return true;
        };
        match file.write_all(self.files[channel].batch.as_bytes()) {
            Ok(()) => {
                let file = &mut self.files[channel];
                file.file_len += file.batch.len() as u64;
                file.batch.clear();
                file.batch_lines = 0;
                true
            }
            Err(error) => {
                let mut reason = format!("the write failed: {error}");
                // A partial write leaves a torn line; the file goes back to its last whole line.
                if let Err(truncate) = file.set_len(self.files[channel].file_len) {
                    reason.push_str(&format!("; the partial line stays in the file: {truncate}"));
                }
                let file = &mut self.files[channel];
                let lost = Lost {
                    from_seq: file
                        .lost
                        .as_ref()
                        .map_or(file.batch_first_seq, |lost| lost.from_seq),
                    to_seq: self.seq,
                    bytes: file.lost.as_ref().map_or(0, |lost| lost.bytes)
                        + file.batch.len() as u64,
                    reason,
                };
                file.lost = Some(lost);
                file.size = file.file_len;
                file.batch.clear();
                file.batch_lines = 0;
                false
            }
        }
    }

    /// Moves the full file of a channel to generation 1 and the older generations one step up, removes the generation
    /// beyond the kept ones, and opens a new file. Returns the name of the removed generation, if there was one, or the
    /// step that failed; a failed step leaves the writer with the file it has.
    fn rotate(&mut self, channel: usize) -> Result<Option<String>, String> {
        let path = self.files[channel].path.clone();
        let generation = |n: usize| {
            let mut name = path.clone().into_os_string();
            name.push(format!(".{n}"));
            PathBuf::from(name)
        };
        let oldest = generation(self.generations);
        let removed = match std::fs::remove_file(&oldest) {
            Ok(()) => oldest
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "rotation could not remove {}: {error}",
                    oldest.display()
                ));
            }
        };
        for n in (1..self.generations).rev() {
            match std::fs::rename(generation(n), generation(n + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "rotation could not move {}: {error}",
                        generation(n).display()
                    ));
                }
            }
        }
        std::fs::rename(&path, generation(1))
            .map_err(|error| format!("rotation could not move {}: {error}", path.display()))?;
        // The new file is opened before the old one is released, so a failure leaves the writer with a file to write.
        let next = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| format!("rotation could not open {}: {error}", path.display()))?;
        let full = self.files[channel].file.replace(next);
        if let Some(file) = full {
            if let Err(error) = file.sync_all() {
                self.record_failure(format!("rotation could not sync the full file: {error}"));
            }
        }
        Ok(removed)
    }
}

/// Opens a file of the writer close-on-exec.
fn open_log_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
}

fn write_loop(
    mut out: Output,
    receiver: Receiver<Message>,
    queued_bytes: Arc<AtomicUsize>,
    queued_events: Arc<AtomicUsize>,
    loss: Arc<Loss>,
    interval: Duration,
    open: Pending,
) {
    out.push(&open);
    out.start_policy();
    let record_loss = |out: &mut Output| {
        for reason in Reason::ALL {
            let (events, bytes) = loss.take(reason);
            out.dropped(events, bytes, reason.text());
        }
        out.gated();
    };
    let take = |out: &mut Output, pending: &Pending| {
        queued_bytes.fetch_sub(pending.bytes, Ordering::Relaxed);
        queued_events.fetch_sub(1, Ordering::Relaxed);
        out.push(pending);
    };
    // Takes every event that is queued now, then the loss counted so far, and writes them.
    let drain = |out: &mut Output| -> Option<Message> {
        let mut request = None;
        while let Ok(message) = receiver.try_recv() {
            match message {
                Message::Event(pending) => take(out, &pending),
                Message::PolicyChanged => out.policy_changed(),
                Message::PolicyWatchFailed(text) => out.record_error("log policy", text),
                other => {
                    request = Some(other);
                    break;
                }
            }
        }
        record_loss(out);
        out.flush();
        request
    };
    loop {
        let message = match receiver.recv_timeout(interval) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => {
                // The interval ended: write what has gathered, and handle a request that was queued behind it.
                match drain(&mut out) {
                    Some(request) => request,
                    None => continue,
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                drain(&mut out);
                return;
            }
        };
        match message {
            Message::Event(pending) => {
                let urgent = pending.level == Level::Error;
                take(&mut out, &pending);
                if urgent {
                    record_loss(&mut out);
                    out.flush();
                }
            }
            Message::Flush(done) => {
                let following = drain(&mut out);
                if done.send(()).is_err() {
                    out.record_failure("the flush was answered after its deadline".into());
                }
                if matches!(following, Some(Message::Stop)) {
                    return;
                }
            }
            Message::Stop => {
                drain(&mut out);
                return;
            }
            Message::PolicyChanged => out.policy_changed(),
            Message::PolicyWatchFailed(text) => out.record_error("log policy", text),
        }
    }
}

/// Reads the lines of an event file. A file whose first line is not `log.open` of the schema of this writer is rejected with
/// an error that names the file and what it found; no earlier schema is read.
/// Reads the lines of both files of a writer of one run in the order of their `seq`: the main file and the debug file,
/// which exists once the first line of its channel was written.
pub fn read_run_lines(main: &Path) -> Result<Vec<serde_json::Value>, String> {
    let mut lines = read_lines(main)?;
    let debug = main.with_extension("debug.jsonl");
    if debug.exists() {
        lines.extend(read_lines(&debug)?);
        // default: a line without a readable seq keeps its place after the lines before it.
        lines.sort_by_key(|line| line["seq"].as_u64().unwrap_or(0));
    }
    Ok(lines)
}

pub fn read_lines(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "{}:{}: not a JSON object: {error}",
                path.display(),
                index + 1
            )
        })?;
        lines.push(value);
    }
    let first = lines
        .first()
        .ok_or_else(|| format!("{}: the file is empty", path.display()))?;
    if first["event"] != "log.open" {
        return Err(format!(
            "{}: the first line is {}, not log.open",
            path.display(),
            first["event"]
        ));
    }
    if first["fields"]["schema"] != SCHEMA {
        return Err(format!(
            "{}: schema {} is not {SCHEMA}",
            path.display(),
            first["fields"]["schema"]
        ));
    }
    Ok(lines)
}

/// The bytes in lowercase hexadecimal, built in one allocation with a table of the sixteen digits.
pub fn hex(data: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = Vec::with_capacity(data.len() * 2);
    for byte in data {
        text.push(DIGITS[usize::from(byte >> 4)]);
        text.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    String::from_utf8(text).expect("hexadecimal digits are ASCII")
}

/// A writer in a folder of its own for the tests of this crate; the folder is removed when the value is dropped.
#[cfg(test)]
pub mod test_support {
    use super::*;

    pub struct TestEvents {
        handle: Events,
        _directory: tempfile::TempDir,
    }

    impl Default for TestEvents {
        fn default() -> Self {
            Self::new()
        }
    }

    impl TestEvents {
        pub fn new() -> Self {
            Self::with_policy(Policy::default())
        }

        /// A writer that reads `policy` from the policy file in the parent of its log folder.
        pub fn with_policy(policy: Policy) -> Self {
            let directory = tempfile::tempdir().expect("create the folder of the events");
            let file = directory.path().join(POLICY_FILE);
            std::fs::write(
                &file,
                json!({"schema": 1, "classes": policy.classes()}).to_string(),
            )
            .expect("write the policy file");
            let mut options = Options::service(directory.path().join("logs"), "test".to_string());
            options.policy_file = Some(file);
            options.batch_interval = Duration::from_millis(20);
            let handle = Arc::new(EventLog::start(options).expect("start the writer"));
            Self {
                handle,
                _directory: directory,
            }
        }

        pub fn handle(&self) -> Events {
            self.handle.clone()
        }

        /// The events written so far, after the writer put everything it holds on disk.
        pub fn lines(&self) -> Vec<serde_json::Value> {
            self.handle
                .flush(Duration::from_secs(10))
                .expect("flush the writer");
            read_lines(self.handle.path()).expect("read the file of the events")
        }
    }
}
