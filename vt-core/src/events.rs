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

use serde::Serialize;
use serde_json::json;
use serde_json::value::{to_raw_value, RawValue};

/// The schema number of the lines that this writer writes.
pub const SCHEMA: u64 = 2;

/// The role of this process in the name of its file and in `log.open`.
pub const ROLE: &str = "vt";

/// The layer of every event of this process.
const LAYER: &str = "sidecar";

/// The prefix of every event name of this process, except the events of the writer (`log.*`).
const PREFIX: &str = "sidecar.vt.";

/// The classes whose events the policy stores (core docs/spec/logging.md#classes-and-policy). A process that cannot read the
/// policy stores `lifecycle`, `input`, `call` and `io`.
pub const POLICY: [&str; 4] = ["lifecycle", "input", "call", "io"];

/// The level of an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    fn name(self) -> &'static str {
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
    surface: Option<String>,
    session: Option<String>,
    cids: Vec<String>,
    /// The fields as JSON text, serialized once by the producer.
    fields: Box<RawValue>,
    bytes: usize,
}

impl Pending {
    /// An event of the writer itself, which concerns no object.
    fn of_writer(level: Level, event: &str, fields: Box<RawValue>) -> Self {
        Self {
            ts_us: now_us(),
            mono_ns: mono_ns(),
            level,
            event: event.to_string(),
            surface: None,
            session: None,
            cids: Vec::new(),
            fields,
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
}

/// The slots of the queue that only requests use.
const RESERVED_FOR_REQUESTS: usize = 2;

/// The loss that the queue counted since the writer last recorded it.
struct Loss {
    events: AtomicU64,
    bytes: AtomicU64,
}

pub struct EventLog {
    sender: SyncSender<Message>,
    queued_events: Arc<AtomicUsize>,
    queue_events: usize,
    queued_bytes: Arc<AtomicUsize>,
    queue_bytes: usize,
    loss: Arc<Loss>,
    path: PathBuf,
    writer: Mutex<Option<JoinHandle<()>>>,
}

/// The writer that the connections, the surfaces and the sessions of the service share.
pub type Events = Arc<EventLog>;

impl EventLog {
    /// Starts the writer of the service in `directory` with the bounds of the service and the boot of this process.
    pub fn start_service(directory: &Path) -> Result<Events, String> {
        if !directory.is_absolute() {
            return Err(format!(
                "{}: the log directory must be an absolute path",
                directory.display()
            ));
        }
        EventLog::start(Options::service(
            directory.to_path_buf(),
            process_boot().to_string(),
        ))
        .map(Arc::new)
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
        let loss = Arc::new(Loss {
            events: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        });
        let open = Pending::of_writer(
            Level::Info,
            "log.open",
            to_raw_value(&json!({
                "schema": SCHEMA,
                "pid": std::process::id(),
                "role": ROLE,
                "boot": options.boot,
                "version": env!("CARGO_PKG_VERSION"),
                "policy": POLICY,
            }))
            .map_err(|e| format!("log.open: {e}"))?,
        );
        let writer = {
            let queued_bytes = Arc::clone(&queued_bytes);
            let queued_events = Arc::clone(&queued_events);
            let loss = Arc::clone(&loss);
            let interval = options.batch_interval;
            let path_for_writer = path.clone();
            let rotate_bytes = options.rotate_bytes;
            let generations = options.generations;
            std::thread::Builder::new()
                .name("event-log-writer".into())
                .spawn(move || {
                    write_loop(
                        Output::new(file, path_for_writer, rotate_bytes, generations, &open),
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
            path,
            writer: Mutex::new(Some(writer)),
        })
    }

    /// The file this writer writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Queues an event that concerns no object. See [`EventLog::emit_for`].
    pub fn emit<T: Serialize>(&self, level: Level, event: &str, fields: &T) -> bool {
        self.emit_for(level, event, Subject::NONE, fields)
    }

    /// Queues an event. Returns false, and counts the loss, when the queue has no room or the name does not start with
    /// `sidecar.vt.`; the name is reported on the standard error. It never waits. The fields are serialized once, and
    /// only after the queue is known to have room for another event.
    pub fn emit_for<T: Serialize>(
        &self,
        level: Level,
        event: &str,
        subject: Subject<'_>,
        fields: &T,
    ) -> bool {
        if !event.starts_with(PREFIX) {
            self.count_loss(1, event.len() as u64);
            eprintln!("event writer: the event {event:?} does not start with {PREFIX:?}");
            return false;
        }
        if self.queued_events.load(Ordering::Relaxed) >= self.queue_events {
            self.count_loss(1, event.len() as u64);
            return false;
        }
        let fields = match to_raw_value(fields) {
            Ok(fields) => fields,
            Err(error) => {
                // A value that cannot be serialized is a defect of the caller: it is counted in place of the event.
                self.count_loss(1, event.len() as u64);
                eprintln!("event writer: the fields of {event} cannot be serialized: {error}");
                return false;
            }
        };
        self.enqueue(Pending {
            ts_us: now_us(),
            mono_ns: mono_ns(),
            level,
            event: event.to_string(),
            surface: subject.surface.map(str::to_string),
            session: subject.session.map(str::to_string),
            cids: subject.cids.to_vec(),
            fields,
            bytes: 0,
        })
    }

    fn enqueue(&self, mut pending: Pending) -> bool {
        let bytes = pending.fields.get().len()
            + pending.event.len()
            + pending.cids.iter().map(String::len).sum::<usize>();
        if self.queued_bytes.load(Ordering::Relaxed) + bytes > self.queue_bytes {
            self.count_loss(1, bytes as u64);
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
                self.count_loss(1, bytes as u64);
                false
            }
        }
    }

    /// Adds events and bytes that a producer of this process refused before they reached the writer; the writer records
    /// them as a `log.dropped`.
    pub fn count_loss(&self, events: u64, bytes: u64) {
        self.loss.events.fetch_add(events, Ordering::Relaxed);
        self.loss.bytes.fetch_add(bytes, Ordering::Relaxed);
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
        if let Err(error) = self.sender.try_send(Message::Stop) {
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
        "{{\"ts_us\":{},\"mono_ns\":{},\"seq\":{},\"level\":\"{}\",\"layer\":\"{LAYER}\",\"event\":{}",
        pending.ts_us,
        pending.mono_ns,
        seq,
        pending.level.name(),
        text(&pending.event),
    );
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
    line.push_str(&format!(",\"fields\":{}}}", pending.fields.get()));
    line
}

/// The file of the writer with its batch, its size and the sequence number of the last line.
struct Output {
    file: File,
    path: PathBuf,
    rotate_bytes: u64,
    generations: usize,
    /// The fields of `log.open`, written again at the start of every file.
    open_fields: String,
    batch: String,
    /// The bytes of the current file, written or waiting in the batch.
    size: u64,
    seq: u64,
}

impl Output {
    fn new(
        file: File,
        path: PathBuf,
        rotate_bytes: u64,
        generations: usize,
        open: &Pending,
    ) -> Output {
        Output {
            file,
            path,
            rotate_bytes,
            generations,
            open_fields: open.fields.get().to_string(),
            batch: String::new(),
            size: 0,
            seq: 0,
        }
    }

    /// Adds the line of an event with the next `seq`.
    fn append(&mut self, pending: &Pending) {
        self.seq += 1;
        let text = format!("{}\n", line(self.seq, pending));
        self.size += text.len() as u64;
        self.batch.push_str(&text);
    }

    /// Adds an event. A file that the line would take past the bound is closed first; the new file opens with
    /// `log.open` and the record of the rotation, so every file of the contract starts with `log.open` and `seq`
    /// continues.
    fn push(&mut self, pending: &Pending) {
        let next = line(self.seq + 1, pending).len() as u64 + 1;
        if self.size > 0 && self.size + next > self.rotate_bytes {
            self.flush();
            let full = self.size;
            if let Some(removed) = self.rotate() {
                self.size = 0;
                let open = Pending::of_writer(
                    Level::Info,
                    "log.open",
                    RawValue::from_string(self.open_fields.clone())
                        .expect("the fields of log.open are JSON"),
                );
                self.append(&open);
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
                self.append(&rotated);
            }
        }
        self.append(pending);
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
            }))
            .expect("a json value serializes"),
        );
        self.push(&pending);
    }

    /// Writes the batch. A failed write is reported on the standard error; the writer keeps running for the next batch.
    fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        if let Err(error) = self.file.write_all(self.batch.as_bytes()) {
            eprintln!("event writer: {error}");
        }
        self.batch.clear();
    }

    /// Moves the full file to generation 1 and the older generations one step up, removes the generation beyond the kept
    /// ones, and opens a new file. Returns None when a step failed (the failure is reported and the writer keeps the
    /// file it has), otherwise the name of the removed generation, if there was one.
    fn rotate(&mut self) -> Option<Option<String>> {
        let path = self.path.clone();
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
                eprintln!(
                    "event writer: rotation could not remove {}: {error}",
                    oldest.display()
                );
                return None;
            }
        };
        for n in (1..self.generations).rev() {
            match std::fs::rename(generation(n), generation(n + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    eprintln!(
                        "event writer: rotation could not move {}: {error}",
                        generation(n).display()
                    );
                    return None;
                }
            }
        }
        if let Err(error) = std::fs::rename(&path, generation(1)) {
            eprintln!(
                "event writer: rotation could not move {}: {error}",
                path.display()
            );
            return None;
        }
        // The new file is opened before the old one is released, so a failure leaves the writer with a file to write.
        let next = match OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) => {
                eprintln!(
                    "event writer: rotation could not open {}: {error}",
                    path.display()
                );
                return None;
            }
        };
        let full = std::mem::replace(&mut self.file, next);
        if let Err(error) = full.sync_all() {
            eprintln!("event writer: rotation could not sync the full file: {error}");
        }
        Some(removed)
    }
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
    let record_loss = |out: &mut Output| {
        let events = loss.events.swap(0, Ordering::Relaxed);
        let bytes = loss.bytes.swap(0, Ordering::Relaxed);
        out.dropped(events, bytes, "the queue was full");
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
                    eprintln!("event writer: the flush was answered after its deadline");
                }
                if matches!(following, Some(Message::Stop)) {
                    return;
                }
            }
            Message::Stop => {
                drain(&mut out);
                return;
            }
        }
    }
}

/// Reads the lines of an event file. A file whose first line is not `log.open` of the schema of this writer is rejected with
/// an error that names the file and what it found; no earlier schema is read.
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

/// The fields of an event of bytes: the length, the text (the bytes read as UTF-8, with the replacement character for
/// bytes that are not) and the same bytes in hexadecimal. An event holds every byte, because a defect of the input is
/// located from what was written and read.
pub fn bytes_fields(data: &[u8]) -> serde_json::Value {
    let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
    json!({
        "bytes": data.len(),
        "text": String::from_utf8_lossy(data),
        "hex": hex,
    })
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
            let directory = tempfile::tempdir().expect("create the folder of the events");
            let mut options = Options::service(directory.path().join("logs"), "test".to_string());
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
