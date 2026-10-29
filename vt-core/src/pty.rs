//! The in-process PTY session that the terminal service uses.
//!
//! The service owns one PTY and one VT stream per session. The attachment is intentionally not
//! represented as a PTY handle. Dropping an attachment only stops the delivery to that client, and
//! the session end is handled by an explicit close.

use crate::events::{Events, Level, Subject};
use crate::platform::pty::{
    KillResult, kill_process_group, pending_input, process_group_leader, wait_for_exit,
};
use crate::protocol::DaemonEvent;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
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

/// The chunks of output that a session keeps for a client that attaches later; the oldest chunk
/// leaves when it is exceeded.
const RETAINED_OUTPUT: usize = 10_000;

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
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    child: Mutex<SessionChild>,
    /// The reaper thread notifies after it records reaped of the child.
    child_reaped: Condvar,
    next_sequence: Mutex<i64>,
    written_output: Mutex<u64>,
    output: Mutex<VecDeque<(i64, Vec<u8>)>>,
    attachments: Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<DaemonEvent>>>,
    closed: Mutex<bool>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
    reaper: Mutex<Option<thread::JoinHandle<()>>>,
    /// The chunks of output that the ring keeps.
    retained_output: usize,
    /// The writer of the service that opened the session, which records each write, each read and
    /// the end of the child.
    log: Events,
}

impl Session {
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
    retained_output: usize,
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
            retained_output: RETAINED_OUTPUT,
            environment: Vec::new(),
            log,
        }
    }

    /// The service whose sessions keep `chunks` chunks of output in the ring.
    pub fn with_retained_output(mut self, chunks: usize) -> Self {
        self.retained_output = chunks;
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
        events: tokio::sync::mpsc::UnboundedSender<DaemonEvent>,
    ) -> Result<(String, String), String> {
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
        events: tokio::sync::mpsc::UnboundedSender<DaemonEvent>,
    ) -> Result<(String, String), String> {
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
        events: tokio::sync::mpsc::UnboundedSender<DaemonEvent>,
    ) -> Result<(String, String), String> {
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
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("clone PTY reader: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("take PTY writer: {error}"))?;

        let session_id = Uuid::new_v4().to_string();
        let attachment_id = Uuid::new_v4().to_string();
        let session = Arc::new(Session {
            id: session_id.clone(),
            owner: owner.to_string(),
            master: Mutex::new(Some(pair.master)),
            process_group,
            writer: Mutex::new(Some(writer)),
            child: Mutex::new(SessionChild {
                child,
                reaped: None,
            }),
            child_reaped: Condvar::new(),
            next_sequence: Mutex::new(0),
            written_output: Mutex::new(0),
            output: Mutex::new(VecDeque::with_capacity(self.retained_output)),
            attachments: Mutex::new(HashMap::new()),
            closed: Mutex::new(false),
            reader: Mutex::new(None),
            reaper: Mutex::new(None),
            retained_output: self.retained_output,
            log: self.log.clone(),
        });
        session
            .recover(&session.attachments, "attachments")
            .insert(attachment_id.clone(), events);

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
        let reader_handle = crate::thread::start(&self.log, "pty-reader", move || {
            read_output(session, reader)
        })?;
        // The handle is installed after the session is published. Close separates its join from the
        // request path, so a stalled PTY reader cannot block a later session open.
        let session = self.sessions().get(&session_id).cloned().unwrap();
        *session.recover(&session.reader, "reader") = Some(reader_handle);
        let session_for_reaper = session.clone();
        let reaper_handle = crate::thread::start(&self.log, "pty-reaper", move || {
            reap_child(session_for_reaper)
        })?;
        *session.recover(&session.reaper, "reaper") = Some(reaper_handle);

        Ok((session_id, attachment_id))
    }

    pub fn attach(
        &self,
        session_id: &str,
        from: i64,
        events: tokio::sync::mpsc::UnboundedSender<DaemonEvent>,
    ) -> Result<String, String> {
        let session = self.session(session_id)?;
        let attachment_id = Uuid::new_v4().to_string();
        let replay = {
            let output = session.recover(&session.output, "output");
            let oldest = output
                .front()
                .map(|(sequence, _)| *sequence)
                // default: without retained output nothing was cut off.
                .unwrap_or(from);
            let truncated = from < oldest;
            output
                .iter()
                .filter(|(sequence, _)| *sequence >= from.max(oldest))
                .map(|(sequence, data)| {
                    (
                        *sequence,
                        data.clone(),
                        truncated && *sequence == from.max(oldest),
                    )
                })
                .collect::<Vec<_>>()
        };
        session
            .recover(&session.attachments, "attachments")
            .insert(attachment_id.clone(), events.clone());
        for (sequence, data, truncated) in replay {
            events
                .send(DaemonEvent::Output {
                    session_id: session_id.to_string(),
                    data,
                    sequence,
                    truncated,
                })
                .map_err(|_| {
                    session
                        .recover(&session.attachments, "attachments")
                        .remove(&attachment_id);
                    "PTY attachment closed during replay".to_string()
                })?;
        }
        Ok(attachment_id)
    }

    pub fn detach(&self, session_id: &str, attachment_id: &str) -> Result<(), String> {
        let session = self.session(session_id)?;
        session
            .recover(&session.attachments, "attachments")
            .remove(attachment_id);
        Ok(())
    }

    /// Writes `data` to the session. The surface that sends it records the write as a replay input.
    pub fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        let session = self.session(session_id)?;
        if *session.recover(&session.closed, "closed") {
            return Err("session is closed".into());
        }

        session
            .recover(&session.writer, "writer")
            .as_mut()
            .ok_or_else(|| "session writer is closed".to_string())?
            .write_all(data)
            .map_err(|error| format!("write PTY: {error}"))
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
        // The child ends before the writer is taken: a write that waits for a child that does not
        // read holds the writer and ends only when the child ends.
        let kill_result = {
            let process_group_result = kill_process_group(session.process_group);
            if let (Ok(result), Some(group)) = (&process_group_result, session.process_group) {
                record_kill(&session, "process group", group, *result);
            }
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
        session.recover(&session.writer, "writer").take();
        session.recover(&session.master, "master").take();
        let reader = session.recover(&session.reader, "reader").take();
        let reaper = session.recover(&session.reaper, "reaper").take();
        let log = session.log.clone();
        // The threads end without a panic (the entrance records one), so the join fails only when
        // the entrance itself failed.
        let joined = crate::thread::start(&log, "pty-joiner", {
            let log = log.clone();
            move || {
                if let Some(reader) = reader
                    && reader.join().is_err()
                {
                    log.record_error("pty close", "the reader thread panicked");
                }
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

fn read_output(session: Arc<Session>, mut reader: Box<dyn Read + Send>) {
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                session.log.emit_for(
                    Level::Info,
                    "sidecar.vt.pty_eof",
                    Subject::session(&session.id, &[]),
                    &serde_json::json!({}),
                );
                break;
            }
            Ok(size) => {
                let data = buffer[..size].to_vec();
                *session.recover(&session.written_output, "written_output") += size as u64;
                let sequence = {
                    let mut next = session.recover(&session.next_sequence, "next_sequence");
                    let sequence = *next;
                    *next += 1;
                    sequence
                };
                {
                    let mut output = session.recover(&session.output, "output");
                    output.push_back((sequence, data.clone()));
                    while output.len() > session.retained_output {
                        if let Some((_, left)) = output.pop_front() {
                            session.log.emit_for(
                                Level::Warn,
                                "sidecar.vt.buffer_dropped",
                                Subject::session(&session.id, &[]),
                                &serde_json::json!({
                                    "buffer": "output ring",
                                    "count": 1,
                                    "bytes": left.len(),
                                    "reason": format!("the ring holds {} chunks", session.retained_output),
                                }),
                            );
                        }
                    }
                }
                broadcast(
                    &session,
                    DaemonEvent::Output {
                        session_id: session.id.clone(),
                        data,
                        sequence,
                        truncated: false,
                    },
                );
            }
            Err(error) => {
                session.log.emit_for(
                    Level::Info,
                    "sidecar.vt.pty_read_error",
                    Subject::session(&session.id, &[]),
                    &serde_json::json!({"error": error.to_string()}),
                );
                broadcast(
                    &session,
                    DaemonEvent::Error {
                        session_id: session.id.clone(),
                        error: format!("read PTY: {error}"),
                    },
                );
                break;
            }
        }
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
fn reap_child(session: Arc<Session>) {
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
    if let Err(error) = result {
        // The record is stored whether or not a view is attached; a broadcast reaches only the
        // attached views.
        session.log.emit_for(
            Level::Error,
            "sidecar.vt.wait_failed",
            Subject::session(&session.id, &[]),
            &serde_json::json!({"error": error}),
        );
        broadcast(
            &session,
            DaemonEvent::Error {
                session_id: session.id.clone(),
                error,
            },
        );
    }
    broadcast(
        &session,
        DaemonEvent::Exit {
            session_id: session.id.clone(),
        },
    );
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

fn broadcast(session: &Session, event: DaemonEvent) {
    let mut attachments = session.recover(&session.attachments, "attachments");
    let before = attachments.len();
    attachments.retain(|_, sender| sender.send(event.clone()).is_ok());
    let removed = before - attachments.len();
    if removed > 0 {
        let bytes = match &event {
            DaemonEvent::Output { data, .. } => data.len(),
            _ => 0,
        };
        session.log.emit_for(
            Level::Warn,
            "sidecar.vt.buffer_dropped",
            Subject::session(&session.id, &[]),
            &serde_json::json!({
                "buffer": "broadcast",
                "count": removed,
                "bytes": bytes * removed,
                "reason": "the receiver of the attachment was closed",
            }),
        );
    }
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
