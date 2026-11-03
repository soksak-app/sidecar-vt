//! The in-process PTY session that the terminal service uses.
//!
//! The service owns one PTY and one VT stream per session, and one surface consumes the output of
//! a session. The session end is handled by an explicit close.

use crate::events::{Events, Level, Subject};
use crate::platform::pty::{
    KillResult, kill_process_group, master_stream, pending_input, process_group_leader,
    wait_for_exit,
};
use crate::protocol::DaemonEvent;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use tokio::io::unix::AsyncFd;
use tokio::sync::{Notify, mpsc, oneshot};
use uuid::Uuid;

/// The shell path that the setting value of the request points to. `login` is the login shell of
/// the account, and any other value must be an absolute path to an executable file. An error is
/// returned without substituting another shell.
pub fn resolve_shell(request: &str) -> Result<String, String> {
    let shell = if request == "login" {
        crate::platform::darwin::account::login_shell()?
    } else {
        request.to_string()
    };
    if !shell.starts_with('/') {
        return Err(format!("terminal shell {shell:?} is not an absolute path"));
    }
    let path = std::path::Path::new(&shell);
    if !path.is_file() {
        return Err(format!("terminal shell {shell:?} is not a file"));
    }
    nix::unistd::access(path, nix::unistd::AccessFlags::X_OK)
        .map_err(|error| format!("terminal shell {shell:?} is not executable: {error}"))?;
    Ok(shell)
}

/// The directory the shell starts in. It must be the absolute path of an existing directory, and an
/// error is returned instead of substituting another directory.
pub fn resolve_directory(request: &str) -> Result<String, String> {
    if !request.starts_with('/') {
        return Err(format!(
            "terminal directory {request:?} is not an absolute path"
        ));
    }
    if !std::path::Path::new(request).is_dir() {
        return Err(format!("terminal directory {request:?} is not a directory"));
    }
    Ok(request.to_string())
}

/// The most bytes that one read of the session takes from the PTY.
const OUTPUT_CHUNK_BYTES: usize = 64 * 1024;
/// The chunks of output that the engine has not consumed which a session holds. A full channel
/// stops reads and nothing else, so the program waits in its own write and no output is dropped.
const OUTPUT_CHUNKS: usize = 16;

/// The bytes of queued input that a session holds. The bound passes a paste far larger than a person
/// pastes and bounds what a stuck session holds; no limit between the host and the service exists to
/// take it from.
const QUEUED_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// The channel through which a session delivers its output and its end to the one surface that
/// consumes it.
pub fn output_channel() -> (mpsc::Sender<DaemonEvent>, mpsc::Receiver<DaemonEvent>) {
    mpsc::channel(OUTPUT_CHUNKS)
}

/// The result of one write: `Ok` means that the master accepted every byte. A canonical tty with no
/// complete line can accept and discard bytes, so it does not state that the program read them.
pub type WriteCompletion = oneshot::Receiver<Result<(), String>>;

/// A completion that already holds its result, for a write that never reached the queue.
pub fn write_completed(result: Result<(), String>) -> WriteCompletion {
    let (sender, completion) = oneshot::channel();
    // default: the receiver is held right here, so this send cannot fail.
    let _ = sender.send(result);
    completion
}

/// Who caused a write. A person can stop and retry, so input of a person is refused at the bound of
/// the queue. A reply of the engine is never refused, because a refused reply leaves the program
/// waiting; the session stops reading output instead, so the program produces no more replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOrigin {
    Person,
    Engine,
}

/// One write in the queue of a session.
struct QueuedWrite {
    data: Arc<[u8]>,
    /// The bytes that the master has accepted.
    written: usize,
    done: oneshot::Sender<Result<(), String>>,
}

/// The PTY transfer state measured at one point in time. pending is the number of input bytes that
/// the master wrote and the child has not read yet, and written is the cumulative number of child
/// output bytes that the session reader has read so far. One measurement round trip returns both
/// values together, so a successful write, the child's receipt, and the child's output can be told
/// apart at the same point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyMeasurement {
    pub pending: usize,
    pub written: u64,
}

/// The child of the session and the result of the reaper thread. Only the reaper thread reaps the
/// child, and it reaps and records the result while holding this lock. Therefore, while reaped is
/// None, the pid that the holder of this lock sees refers to this child.
struct SessionChild {
    child: Box<dyn Child + Send>,
    /// The result of the reaper thread. None means the reaper thread has not finished yet.
    reaped: Option<Result<(), String>>,
}

struct Session {
    id: String,
    owner: String,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    process_group: Option<i32>,
    /// The queue of writes that the I/O task consumes. None once the writer is closed.
    input: Mutex<Option<mpsc::UnboundedSender<QueuedWrite>>>,
    /// The bytes of the queue that the master has not accepted.
    queued_input: AtomicUsize,
    input_bound: usize,
    child: Mutex<SessionChild>,
    /// The reaper thread notifies after it records reaped of the child.
    child_reaped: Condvar,
    written_output: Mutex<u64>,
    closed: Mutex<bool>,
    /// Tells the I/O task that close started.
    close: Notify,
    reaper: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The writer of the service that opened the session, which records each write, each read and
    /// the end of the child.
    log: Events,
}

impl Session {
    fn count_read(&self, size: usize) {
        *self.recover(&self.written_output, "written_output") += size as u64;
    }

    fn recovered<'a, T>(
        &self,
        field: &Mutex<T>,
        value: MutexGuard<'a, T>,
        name: &str,
    ) -> MutexGuard<'a, T> {
        self.log.emit_for(
            Level::Error,
            "sidecar.vt.record.error",
            Subject::session(&self.id, &[]),
            &serde_json::json!({
                "where": format!("pty {name}"),
                "text": "lock was poisoned; field value retained",
            }),
        );
        field.clear_poison();
        value
    }

    fn recover<'a, T>(&self, field: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
        field
            .lock()
            // default: recover the retained guard and report the field through the session writer.
            .unwrap_or_else(|error| self.recovered(field, error.into_inner(), name))
    }

    fn wait_reaped<'a>(&self, child: MutexGuard<'a, SessionChild>) -> MutexGuard<'a, SessionChild> {
        self.wait_reaped_observed(child, || {})
    }

    fn wait_reaped_observed<'a>(
        &self,
        mut child: MutexGuard<'a, SessionChild>,
        mut recovered: impl FnMut(),
    ) -> MutexGuard<'a, SessionChild> {
        while child.reaped.is_none() {
            child = self
                .child_reaped
                .wait_while(child, |child| child.reaped.is_none())
                // default: recover the same child guard and recheck completion after poisoning.
                .unwrap_or_else(|error| {
                    let child = self.recovered(&self.child, error.into_inner(), "child");
                    recovered();
                    child
                });
        }
        child
    }
}

#[derive(Clone)]
pub struct PtyService {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    input_bound: usize,
    /// The variables set in the environment of each child before the shell integration and the
    /// locale read it; `None` removes the variable that the child would inherit.
    environment: Vec<(String, Option<OsString>)>,
    log: Events,
}

/// Applies the terminal locale rules to the child environment (core docs/spec/terminal-runtime.md).
/// When the regional locale cannot be used, it sets `LC_CTYPE=UTF-8` and records that fact as an
/// info record event.
#[cfg(target_os = "macos")]
fn apply_shell_locale(command: &mut CommandBuilder, log: &Events) {
    let (language, region) = crate::platform::darwin::locale::codes();
    let locale = crate::locale::shell_locale(
        |name| command.get_env(name),
        language.as_deref(),
        region.as_deref(),
        crate::platform::darwin::locale::installed,
    );
    match locale {
        crate::locale::ShellLocale::Kept => {}
        crate::locale::ShellLocale::Lang(name) => command.env("LANG", name),
        crate::locale::ShellLocale::CtypeUtf8 { name } => {
            log.record_info("locale", format!("{name} is not installed; LC_CTYPE=UTF-8"));
            command.env("LC_CTYPE", "UTF-8");
        }
    }
}

impl PtyService {
    /// The service whose sessions record every write to and read from the PTY, and their life, in
    /// `log`.
    pub fn new(log: Events) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            input_bound: QUEUED_INPUT_BYTES,
            environment: Vec::new(),
            log,
        }
    }

    /// The service whose sessions queue at most `bytes` bytes of input.
    pub fn with_input_bound(mut self, bytes: usize) -> Self {
        self.input_bound = bytes;
        self
    }

    /// The service whose children get `variables` in their environment: a `Some` value is set and a
    /// `None` removes the variable. The children inherit the environment of the process otherwise.
    pub fn with_environment(mut self, variables: Vec<(String, Option<OsString>)>) -> Self {
        self.environment = variables;
        self
    }

    fn apply_environment(&self, command: &mut CommandBuilder) {
        for (name, value) in &self.environment {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            }
        }
    }

    pub fn open(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        events: mpsc::Sender<DaemonEvent>,
    ) -> Result<String, String> {
        let mut command = CommandBuilder::new(program);
        self.apply_environment(&mut command);
        command.args(args);
        self.spawn_owned("", command, cwd, cols, rows, events)
    }

    /// Starts the shell path `shell`, which is already checked, as a login shell (argv[0] is `-`
    /// followed by the shell name, and SHELL is the shell path).
    pub fn open_shell(
        &self,
        owner: &str,
        shell: &str,
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        events: mpsc::Sender<DaemonEvent>,
    ) -> Result<String, String> {
        let mut command = CommandBuilder::new_default_prog();
        self.apply_environment(&mut command);
        command.env("SHELL", shell);
        crate::shell_integration::apply(shell, &mut command, &self.log)?;
        self.spawn_owned(owner, command, cwd, cols, rows, events)
    }

    fn spawn_owned(
        &self,
        owner: &str,
        mut command: CommandBuilder,
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        events: mpsc::Sender<DaemonEvent>,
    ) -> Result<String, String> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("open PTY: {error}"))?;

        if let Some(directory) = cwd {
            command.cwd(directory);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        #[cfg(target_os = "macos")]
        apply_shell_locale(&mut command, &self.log);

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("spawn PTY session: {error}"))?;
        let process_group = process_group_leader(pair.master.as_ref());
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| format!("start the PTY session task: {error}"))?;
        let stream = {
            let _reactor = runtime.enter();
            AsyncFd::new(master_stream(pair.master.as_ref())?)
                .map_err(|error| format!("register the PTY master: {error}"))?
        };
        let (input, input_queue) = mpsc::unbounded_channel();
        let (child_reaped, reaped) = oneshot::channel();

        let session_id = Uuid::new_v4().to_string();
        let session = Arc::new(Session {
            id: session_id.clone(),
            owner: owner.to_string(),
            master: Mutex::new(Some(pair.master)),
            process_group,
            input: Mutex::new(Some(input)),
            queued_input: AtomicUsize::new(0),
            input_bound: self.input_bound,
            child: Mutex::new(SessionChild {
                child,
                reaped: None,
            }),
            child_reaped: Condvar::new(),
            written_output: Mutex::new(0),
            closed: Mutex::new(false),
            close: Notify::new(),
            reaper: Mutex::new(None),
            log: self.log.clone(),
        });

        self.sessions().insert(session_id.clone(), session.clone());
        session.log.emit_for(
            Level::Info,
            "sidecar.vt.session_open",
            Subject::session(&session_id, &[]),
            &serde_json::json!({
                "owner": owner,
                "cwd": cwd,
                "cols": cols,
                "rows": rows,
                "pid": session.recover(&session.child, "child").child.process_id(),
            }),
        );
        runtime.spawn(session_io(SessionIo {
            session: session.clone(),
            stream,
            input_queue,
            output: events,
            reaped,
        }));
        let session_for_reaper = session.clone();
        let reaper_handle = crate::thread::start(&self.log, "pty-reaper", move || {
            reap_child(session_for_reaper, child_reaped)
        })?;
        *session.recover(&session.reaper, "reaper") = Some(reaper_handle);

        Ok(session_id)
    }

    /// Queues `data` for the session and returns without waiting for the program. The surface that
    /// sends it records the write as a replay input when the completion holds its result.
    pub fn write(
        &self,
        session_id: &str,
        origin: WriteOrigin,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<WriteCompletion, String> {
        let session = self.session(session_id)?;
        if *session.recover(&session.closed, "closed") {
            return Err("session is closed".into());
        }
        let data = data.into();
        let queued = session.queued_input.load(Ordering::Acquire);
        if origin == WriteOrigin::Person && queued + data.len() > session.input_bound {
            return Err(format!(
                "the session input queue holds {queued} bytes, and {} more exceed its bound of {} bytes",
                data.len(),
                session.input_bound
            ));
        }
        let (done, completion) = oneshot::channel();
        let input = session.recover(&session.input, "input");
        let input = input
            .as_ref()
            .ok_or_else(|| "session writer is closed".to_string())?;
        let size = data.len();
        session.queued_input.fetch_add(size, Ordering::AcqRel);
        input
            .send(QueuedWrite {
                data,
                written: 0,
                done,
            })
            .map_err(|_| {
                session.queued_input.fetch_sub(size, Ordering::AcqRel);
                "session writer is closed".to_string()
            })?;
        Ok(completion)
    }

    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let session = self.session(session_id)?;

        session
            .recover(&session.master, "master")
            .as_ref()
            .ok_or_else(|| "session master is closed".to_string())?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("resize PTY: {error}"))
    }

    /// Measures the PTY transfer state at one point in time. It is a diagnostic observation that
    /// distinguishes a successful write, the child's receipt, and the child's output. pending is
    /// the number of input bytes that the master wrote and the child has not read yet (FIONREAD),
    /// and written is the cumulative number of child output bytes that the reader has read so far.
    pub fn pty_measurement(&self, session_id: &str) -> Result<PtyMeasurement, String> {
        let session = self.session(session_id)?;
        if *session.recover(&session.closed, "closed") {
            return Err("session is closed".into());
        }
        let pending = {
            let guard = session.recover(&session.master, "master");
            let master = guard
                .as_ref()
                .ok_or_else(|| "session master is closed".to_string())?;
            pending_input(master.as_ref())?
        };
        let written = *session.recover(&session.written_output, "written_output");
        Ok(PtyMeasurement { pending, written })
    }

    pub fn close(&self, session_id: &str) -> Result<(), String> {
        let session = self.session(session_id)?;
        {
            let mut closed = session.recover(&session.closed, "closed");
            if *closed {
                return Ok(());
            }
            *closed = true;
        }
        session.close.notify_one();
        let kill_result = {
            let process_group_result = kill_process_group(session.process_group);
            if let (Ok(result), Some(group)) = (&process_group_result, session.process_group) {
                record_kill(&session, "process group", group, *result);
            }
            // A process with unread terminal output exits only after that output drains or the
            // master closes, so the master closes before the wait for the child.
            session.recover(&session.master, "master").take();
            // A signal to the process group may not reach the child, so SIGKILL is sent to the
            // child by pid, and this returns after the reaper thread reaps the child. The reaper
            // thread waits without holding the child lock and reaps while holding this lock, so a
            // signal sent while holding this lock, while reaped is None, does not reach another
            // process.
            let child = session.recover(&session.child, "child");
            let child_result = match &child.reaped {
                Some(result) => result.clone(),
                None => kill_child(child.child.as_ref()).and_then(|(pid, result)| {
                    record_kill(&session, "child", pid, result);
                    let child = session.wait_reaped(child);
                    child
                        .reaped
                        .clone()
                        .expect("wait_while returns after the reaper recorded its result")
                }),
            };
            match (process_group_result, child_result) {
                (Err(group_error), _) => Err(group_error),
                (_, Err(child_error)) => Err(child_error),
                (Ok(_), Ok(())) => Ok(()),
            }
        };
        session.recover(&session.input, "input").take();
        let reaper = session.recover(&session.reaper, "reaper").take();
        let log = session.log.clone();
        // The thread ends without a panic (the entrance records one), so the join fails only when
        // the entrance itself failed.
        let joined = crate::thread::start(&log, "pty-joiner", {
            let log = log.clone();
            move || {
                if let Some(reaper) = reaper
                    && reaper.join().is_err()
                {
                    log.record_error("pty close", "the reaper thread panicked");
                }
            }
        });
        if let Err(error) = joined {
            session.log.record_error("pty close", error);
        }
        self.sessions().remove(session_id);
        kill_result
    }

    pub fn close_owner(&self, owner: &str) -> Result<(), String> {
        let ids = self
            .sessions()
            .values()
            .filter(|session| session.owner == owner)
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        for session_id in ids {
            self.close(&session_id)?;
        }
        Ok(())
    }

    pub fn session_count(&self) -> usize {
        self.sessions().len()
    }

    pub fn process_id(&self, session_id: &str) -> Result<u32, String> {
        let session = self.session(session_id)?;
        let process_id = session.recover(&session.child, "child").child.process_id();
        process_id.ok_or_else(|| "PTY child has no process id".to_string())
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(|error| {
            let sessions = error.into_inner();
            self.log.record_error(
                "pty registry",
                "session registry lock was poisoned; sessions retained",
            );
            self.sessions.clear_poison();
            sessions
        })
    }

    fn session(&self, session_id: &str) -> Result<Arc<Session>, String> {
        self.sessions()
            .get(session_id)
            .cloned()
            .ok_or_else(|| format!("session {session_id} not found"))
    }
}

/// What the I/O task of a session owns. The session keeps the master for resize and for the
/// measurement, and the task owns a non-blocking duplicate of it.
struct SessionIo {
    session: Arc<Session>,
    stream: AsyncFd<File>,
    input_queue: mpsc::UnboundedReceiver<QueuedWrite>,
    output: mpsc::Sender<DaemonEvent>,
    reaped: oneshot::Receiver<Result<(), String>>,
}

/// Reads and writes one session. It reads only while it holds a permit of the output channel, so a
/// consumer that falls behind stops reads and nothing else, and it writes the queued input in order
/// whenever the master accepts bytes. It ends at the end of the child or at close.
async fn session_io(mut io: SessionIo) {
    let mut queue = VecDeque::<QueuedWrite>::new();
    let mut permit: Option<mpsc::OwnedPermit<DaemonEvent>> = None;
    let mut reading = true;
    let mut consumer_ended = false;
    let mut input_open = true;
    let mut held = false;
    let mut held_report = None;
    // A write to a master whose slave no process holds fails with EIO. That happens before the
    // reaper reports the end of the child, so the queue waits for that report or for close.
    let mut hung_up = false;
    let mut buffer = vec![0; OUTPUT_CHUNK_BYTES];
    let session = io.session.clone();
    let ended = loop {
        let queued = session.queued_input.load(Ordering::Acquire);
        if (queued > session.input_bound) != held {
            held = !held;
            held_report = Some(queued);
            session.log.emit_for(
                Level::Warn,
                "sidecar.vt.input_held",
                Subject::session(&session.id, &[]),
                &serde_json::json!({"queued": queued, "bound": session.input_bound, "held": held}),
            );
        }
        if consumer_ended {
            held_report = None;
        }
        if held_report.is_some()
            && let Some(reserved) = permit.take()
        {
            reserved.send(DaemonEvent::InputHeld {
                session_id: session.id.clone(),
                queued: held_report.take().expect("this branch runs with a report"),
                held,
            });
        }
        tokio::select! {
            reserved = io.output.clone().reserve_owned(), if permit.is_none() && !consumer_ended && ((reading && !held) || held_report.is_some()) => {
                match reserved {
                    Ok(reserved) => permit = Some(reserved),
                    Err(_) => consumer_ended = true,
                }
            }
            ready = io.stream.readable(), if reading && !held && (permit.is_some() || consumer_ended) => {
                let mut ready = match ready {
                    Ok(ready) => ready,
                    Err(error) => {
                        read_failed(&session, &mut permit, &error);
                        reading = false;
                        continue;
                    }
                };
                match ready.try_io(|stream| stream.get_ref().read(&mut buffer)) {
                    Err(_would_block) => {}
                    Ok(Ok(0)) => {
                        session.log.emit_for(
                            Level::Info,
                            "sidecar.vt.pty_eof",
                            Subject::session(&session.id, &[]),
                            &serde_json::json!({}),
                        );
                        reading = false;
                    }
                    Ok(Ok(size)) => {
                        session.count_read(size);
                        match permit.take() {
                            Some(reserved) => {
                                reserved.send(DaemonEvent::Output {
                                    session_id: session.id.clone(),
                                    data: buffer[..size].to_vec(),
                                });
                            }
                            None => record_dropped_output(&session, size),
                        }
                    }
                    Ok(Err(error)) => {
                        read_failed(&session, &mut permit, &error);
                        reading = false;
                    }
                }
            }
            ready = io.stream.writable(), if !queue.is_empty() && !hung_up => {
                let mut ready = match ready {
                    Ok(ready) => ready,
                    Err(error) => {
                        for write in queue.drain(..) {
                            complete_write(&session, write, Err(format!("write PTY: {error}")));
                        }
                        continue;
                    }
                };
                let write = queue.front_mut().expect("this branch runs only with queued input");
                let (data, written) = (write.data.clone(), write.written);
                match ready.try_io(|stream| stream.get_ref().write(&data[written..])) {
                    Err(_would_block) => {}
                    Ok(Ok(size)) => {
                        write.written += size;
                        session.queued_input.fetch_sub(size, Ordering::AcqRel);
                        if write.written == write.data.len() {
                            let write = queue.pop_front().expect("the front write exists");
                            complete_write(&session, write, Ok(()));
                        }
                    }
                    Ok(Err(error)) if error.kind() == ErrorKind::Interrupted => {}
                    Ok(Err(error)) if error.raw_os_error() == Some(nix::errno::Errno::EIO as i32) => {
                        hung_up = true;
                    }
                    Ok(Err(error)) => {
                        let write = queue.pop_front().expect("the front write exists");
                        complete_write(&session, write, Err(format!("write PTY: {error}")));
                    }
                }
            }
            write = io.input_queue.recv(), if input_open => match write {
                Some(write) => queue.push_back(write),
                None => input_open = false,
            },
            reaped = &mut io.reaped => break Some(reaped),
            _ = session.close.notified() => break None,
        }
    };
    session.recover(&session.input, "input").take();
    drop(permit);
    let SessionIo {
        output,
        stream,
        mut input_queue,
        reaped,
        ..
    } = io;
    input_queue.close();
    while let Ok(write) = input_queue.try_recv() {
        queue.push_back(write);
    }
    // The report of the reaper and a close can both be ready, and a close that was requested decides.
    let closing = *session.recover(&session.closed, "closed");
    let reason = if closing {
        "session closed"
    } else {
        "child exited"
    };
    for write in queue.drain(..) {
        complete_write(&session, write, Err(reason.to_string()));
    }
    // The end of the child leaves its last output in the terminal.
    let last_output = if !closing {
        read_remaining_output(&session, stream.get_ref(), &mut buffer)
    } else {
        0
    };
    // A process with unread terminal output exits only after that output drains or the master
    // closes, and the reads may have stopped, so the descriptors close before the wait for the child.
    session.recover(&session.master, "master").take();
    drop(stream);
    if last_output > 0 {
        buffer.truncate(last_output);
        send_last_output(
            &session,
            &output,
            DaemonEvent::Output {
                session_id: session.id.clone(),
                data: buffer,
            },
            last_output,
        )
        .await;
    }
    let wait_failed = match ended {
        Some(reaped) => reaped,
        None => reaped.await,
    };
    if let Ok(Err(error)) = wait_failed {
        send_end(
            &session,
            &output,
            DaemonEvent::Error {
                session_id: session.id.clone(),
                error,
            },
        )
        .await;
    }
    send_end(
        &session,
        &output,
        DaemonEvent::Exit {
            session_id: session.id.clone(),
        },
    )
    .await;
}

/// Hands a write its result and removes its remaining bytes from the count of the queue. A surface
/// that ended no longer holds the receiver, and the write has its record through the error of that
/// surface.
fn complete_write(session: &Session, write: QueuedWrite, result: Result<(), String>) {
    session
        .queued_input
        .fetch_sub(write.data.len() - write.written, Ordering::AcqRel);
    // default: a surface that ended has no use for the result of its write.
    let _ = write.done.send(result);
}

/// Reads the output that remains in the terminal after the end of the child, until a read returns 0
/// or would block or the buffer is full, and returns its size. Bytes that a job of the session
/// writes later are not read: that write fails with EIO or the hangup ends the job.
fn read_remaining_output(session: &Session, stream: &File, buffer: &mut [u8]) -> usize {
    let mut stream = stream;
    let mut read = 0;
    while read < buffer.len() {
        match stream.read(&mut buffer[read..]) {
            Ok(0) => break,
            Ok(size) => {
                session.count_read(size);
                read += size;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            // Reading the master after the last slave closed can fail with EIO, which is its end.
            Err(error) if error.raw_os_error() == Some(nix::errno::Errno::EIO as i32) => break,
            Err(error) => {
                session.log.emit_for(
                    Level::Info,
                    "sidecar.vt.pty_read_error",
                    Subject::session(&session.id, &[]),
                    &serde_json::json!({"error": error.to_string()}),
                );
                break;
            }
        }
    }
    read
}

/// Sends the last output of a child. A close that comes while the consumer holds no free slot ends
/// the wait, and the bytes that were read and not sent are recorded.
async fn send_last_output(
    session: &Session,
    output: &mpsc::Sender<DaemonEvent>,
    event: DaemonEvent,
    bytes: usize,
) {
    tokio::select! {
        biased;
        sent = output.send(event) => {
            if sent.is_err() {
                record_dropped_output(session, bytes);
            }
        }
        _ = session.close.notified() => record_dropped_output(session, bytes),
    }
}

/// Records a read that failed and reports it to the consumer through the permit that the task holds.
fn read_failed(
    session: &Session,
    permit: &mut Option<mpsc::OwnedPermit<DaemonEvent>>,
    error: &std::io::Error,
) {
    session.log.emit_for(
        Level::Info,
        "sidecar.vt.pty_read_error",
        Subject::session(&session.id, &[]),
        &serde_json::json!({"error": error.to_string()}),
    );
    if let Some(reserved) = permit.take() {
        reserved.send(DaemonEvent::Error {
            session_id: session.id.clone(),
            error: format!("read PTY: {error}"),
        });
    }
}

/// Records output that was read after the surface that consumes the session ended.
fn record_dropped_output(session: &Session, bytes: usize) {
    session.log.emit_for(
        Level::Warn,
        "sidecar.vt.buffer_dropped",
        Subject::session(&session.id, &[]),
        &serde_json::json!({
            "buffer": "session events",
            "count": 1,
            "bytes": bytes,
            "reason": "the surface that consumes the session ended",
        }),
    );
}

/// Sends an event of the end of a session. It waits until the consumer takes it or ends, and the
/// task holds nothing of the PTY by then.
async fn send_end(session: &Session, output: &mpsc::Sender<DaemonEvent>, event: DaemonEvent) {
    if output.send(event).await.is_err() {
        record_dropped_output(session, 0);
    }
}

/// Sends SIGKILL to the child of the session. The caller holds the child lock and has confirmed
/// that the reaper thread has not reaped the child yet. It returns the result of the signal's
/// delivery and the pid of the child.
fn kill_child(child: &(dyn Child + Send)) -> Result<(i32, KillResult), String> {
    let pid = child
        .process_id()
        .ok_or_else(|| "PTY child has no process id".to_string())?;
    let pid = i32::try_from(pid).map_err(|_| format!("PTY child process id {pid} is invalid"))?;
    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) => Ok((pid, KillResult::Sent)),
        // ESRCH: the child has already ended and waits to be reaped. The reaper thread reaps it.
        Err(nix::errno::Errno::ESRCH) => Ok((pid, KillResult::NoSuchProcess)),
        Err(error) => Err(format!("kill PTY child {pid}: {error}")),
    }
}

/// Reaps the child of the session. It waits without holding the child lock until the child ends, so
/// during that time close can take the lock and send a signal to the child. An ended child stays a
/// zombie until it is reaped, so its pid is not reused. The reaping and its result are recorded
/// under the same lock and notified with child_reaped.
fn reap_child(session: Arc<Session>, child_reaped: oneshot::Sender<Result<(), String>>) {
    let process_id = session.recover(&session.child, "child").child.process_id();
    let exited = process_id
        .ok_or_else(|| "PTY child has no process id".to_string())
        .and_then(|pid| wait_for_child_exit(&session.id, pid));
    let result = {
        let mut child = session.recover(&session.child, "child");
        let result = exited.and_then(|()| {
            child
                .child
                .wait()
                .map(|status| {
                    session.log.emit_for(
                        Level::Info,
                        "sidecar.vt.session_exit",
                        Subject::session(&session.id, &[]),
                        &serde_json::json!({"status": format!("{status:?}")}),
                    );
                })
                .map_err(|error| format!("wait PTY: {error}"))
        });
        child.reaped = Some(result.clone());
        result
    };
    session.child_reaped.notify_all();
    if let Err(error) = &result {
        // The record is stored whether or not a surface still consumes the events.
        session.log.emit_for(
            Level::Error,
            "sidecar.vt.wait_failed",
            Subject::session(&session.id, &[]),
            &serde_json::json!({"error": error}),
        );
    }
    // default: the I/O task ends only after it received this result, except when the runtime that
    // ran it stopped, and then nothing consumes the session.
    let _ = child_reaped.send(result);
}

/// Waits for the child of the session to exit. A build with the fault-injection feature fails the
/// wait of a session that a test named with `fault::fail_wait_of`; the failure is taken after the
/// child exited, because the reaper starts its wait at the start of the session.
fn wait_for_child_exit(session_id: &str, pid: u32) -> Result<(), String> {
    wait_for_exit(pid)?;
    injected_wait_failure(session_id, pid)
}

#[cfg(feature = "fault-injection")]
fn injected_wait_failure(session_id: &str, pid: u32) -> Result<(), String> {
    if fault::take_wait_failure(session_id) {
        return Err(format!(
            "wait for PTY child {pid} to exit: failed by the fault-injection feature"
        ));
    }
    Ok(())
}

#[cfg(not(feature = "fault-injection"))]
fn injected_wait_failure(_session_id: &str, _pid: u32) -> Result<(), String> {
    Ok(())
}

/// Test-only faults of the session reaper, compiled only with the fault-injection feature.
#[cfg(feature = "fault-injection")]
#[path = "../tests/pty/wait_fault_test.rs"]
pub mod fault;

/// Records the SIGKILL that closing a session sent to its process group or its child, and what it
/// reached.
fn record_kill(session: &Session, target: &str, id: i32, result: KillResult) {
    session.log.emit_for(
        Level::Info,
        "sidecar.vt.kill",
        Subject::session(&session.id, &[]),
        &serde_json::json!({
            "target": target,
            "id": id,
            "signal": "SIGKILL",
            "result": result.name(),
        }),
    );
}

#[cfg(test)]
#[path = "../tests/pty/lifecycle_test.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/pty/registry_poison_test.rs"]
mod registry_poison_test;

#[cfg(test)]
#[path = "../tests/pty/output_poison_test.rs"]
mod output_poison_test;

#[cfg(test)]
#[path = "../tests/pty/session_poison_test.rs"]
mod session_poison_test;
