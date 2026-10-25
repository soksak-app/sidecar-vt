//! terminal service가 사용하는 in-process PTY session이다.
//!
//! service는 session마다 하나의 PTY와 하나의 VT stream을 소유한다.  attachment는
//! 의도적으로 PTY handle로 표현하지 않는다. attachment를 drop하면 그 client로의
//! 전달만 멈추고, session 종료는 명시적인 close가 담당한다.

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

/// 설정 값 request 가 가리키는 셸 경로. `login` 은 계정의 로그인 셸이고, 그 밖의 값은 실행할 수 있는 절대
/// 경로여야 한다. 다른 셸로 대신하지 않고 오류를 반환한다.
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

/// 셸이 시작할 디렉터리. 있는 디렉터리의 절대 경로여야 하며, 다른 디렉터리로 대신하지 않고 오류를 반환한다.
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

/// The chunks of output that a session keeps for a client that attaches later; the oldest chunk leaves when it is exceeded.
const RETAINED_OUTPUT: usize = 10_000;

/// 한 시점의 PTY 전송 상태 측정. pending 은 마스터가 쓰고 자식이 아직 읽지 않은 입력 바이트 수이고,
/// written 는 세션 reader 가 지금까지 읽은 자식 출력의 누적 바이트 수다. 한 번의 측정 왕복이 두 값을
/// 함께 돌려주므로 쓰기 성공, 자식 수신, 자식 출력을 같은 시점에 구분할 수 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyMeasurement {
    pub pending: usize,
    pub written: u64,
}

/// session 의 자식과 회수 thread 의 결과. 회수 thread 만 자식을 회수하고, 이 lock 을 쥔 채 회수하고 결과를 적는다.
/// 그러므로 reaped 가 None 인 동안 이 lock 을 쥔 쪽이 보는 pid 는 이 자식을 가리킨다.
struct SessionChild {
    child: Box<dyn Child + Send>,
    /// 회수 thread 의 결과. None 이면 회수 thread 가 아직 끝나지 않았다.
    reaped: Option<Result<(), String>>,
}

struct Session {
    id: String,
    owner: String,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    process_group: Option<i32>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    child: Mutex<SessionChild>,
    /// 회수 thread 가 child 의 reaped 를 적은 뒤에 알린다.
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
    /// The writer of the service that opened the session, which records each write, each read and the end of the child.
    log: Events,
}

impl Session {
    fn recover<'a, T>(&self, field: &'a Mutex<T>, name: &str) -> MutexGuard<'a, T> {
        field.lock().unwrap_or_else(|error| {
            let value = error.into_inner();
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
        })
    }
}

#[derive(Clone)]
pub struct PtyService {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    retained_output: usize,
    /// The variables set in the environment of each child before the shell integration and the locale read it; `None`
    /// removes the variable that the child would inherit.
    environment: Vec<(String, Option<OsString>)>,
    log: Events,
}

/// 터미널 locale 규칙을 자식 환경에 적용한다(core docs/spec/terminal-runtime.md). 지역 locale 을 쓸 수 없으면
/// `LC_CTYPE=UTF-8` 을 정하고 그 사실을 info 기록 event 로 남긴다.
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
    /// The service whose sessions record every write to and read from the PTY, and their life, in `log`.
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

    /// The service whose children get `variables` in their environment: a `Some` value is set and a `None` removes the
    /// variable. The children inherit the environment of the process otherwise.
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

    /// 이미 확인한 셸 경로 shell 을 로그인 셸로 시작한다(argv[0] 은 `-` 와 셸 이름, SHELL 은 셸 경로).
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
                "pid": session.child.lock().unwrap().child.process_id(),
            }),
        );
        let reader_handle = crate::thread::start(&self.log, "pty-reader", move || {
            read_output(session, reader)
        })?;
        // handle은 session이 게시된 뒤에 설치된다. close는 그 join을
        // request 경로에서 분리하므로 멈춘 PTY reader가 이후의 session open을
        // 막을 수 없다.
        let session = self.sessions().get(&session_id).cloned().unwrap();
        *session.reader.lock().unwrap() = Some(reader_handle);
        let session_for_reaper = session.clone();
        let reaper_handle = crate::thread::start(&self.log, "pty-reaper", move || {
            reap_child(session_for_reaper)
        })?;
        *session.reaper.lock().unwrap() = Some(reaper_handle);

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
        if *session.closed.lock().unwrap() {
            return Err("session is closed".into());
        }

        session
            .writer
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "session writer is closed".to_string())?
            .write_all(data)
            .map_err(|error| format!("write PTY: {error}"))
    }

    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let session = self.session(session_id)?;

        session
            .master
            .lock()
            .unwrap()
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

    /// 한 시점의 PTY 전송 상태 측정. 쓰기 성공, 자식 수신, 자식 출력을 구분하는 진단 관측이다.
    /// pending 은 마스터가 쓰고 자식이 아직 읽지 않은 입력 바이트 수(FIONREAD)이고, written 는
    /// reader 가 지금까지 읽은 자식 출력의 누적 바이트 수다.
    pub fn pty_measurement(&self, session_id: &str) -> Result<PtyMeasurement, String> {
        let session = self.session(session_id)?;
        if *session.closed.lock().unwrap() {
            return Err("session is closed".into());
        }
        let pending = {
            let guard = session.master.lock().unwrap();
            let master = guard
                .as_ref()
                .ok_or_else(|| "session master is closed".to_string())?;
            pending_input(master.as_ref())?
        };
        let written = *session.written_output.lock().unwrap();
        Ok(PtyMeasurement { pending, written })
    }

    pub fn close(&self, session_id: &str) -> Result<(), String> {
        let session = self.session(session_id)?;
        {
            let mut closed = session.closed.lock().unwrap();
            if *closed {
                return Ok(());
            }
            *closed = true;
        }
        // The child ends before the writer is taken: a write that waits for a child that does not read holds the writer
        // and ends only when the child ends.
        let kill_result = {
            let process_group_result = kill_process_group(session.process_group);
            if let (Ok(result), Some(group)) = (&process_group_result, session.process_group) {
                record_kill(&session, "process group", group, *result);
            }
            // process group 신호가 자식에 닿지 않았을 수 있으므로 자식에 pid 로 SIGKILL 을 보내고, 회수 thread 가 자식을
            // 회수한 뒤에 돌아온다. 회수 thread 는 child lock 을 쥐지 않고 기다리고 이 lock 을 쥔 채 회수하므로,
            // reaped 가 None 인 동안 이 lock 을 쥐고 보내는 신호는 다른 프로세스에 가지 않는다.
            let child = session.child.lock().unwrap();
            let child_result = match &child.reaped {
                Some(result) => result.clone(),
                None => kill_child(child.child.as_ref()).and_then(|(pid, result)| {
                    record_kill(&session, "child", pid, result);
                    let child = session
                        .child_reaped
                        .wait_while(child, |child| child.reaped.is_none())
                        .unwrap();
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
        session.writer.lock().unwrap().take();
        session.master.lock().unwrap().take();
        let reader = session.reader.lock().unwrap().take();
        let reaper = session.reaper.lock().unwrap().take();
        let log = session.log.clone();
        // The threads end without a panic (the entrance records one), so the join fails only when the entrance itself failed.
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
        let process_id = session.child.lock().unwrap().child.process_id();
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
                *session.written_output.lock().unwrap() += size as u64;
                let sequence = {
                    let mut next = session.next_sequence.lock().unwrap();
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

/// session 의 자식에 SIGKILL 을 보낸다. 부르는 쪽은 child lock 을 쥐고 회수 thread 가 자식을 아직 회수하지 않았음을
/// 확인했다. 돌려주는 것은 신호가 닿은 결과와 자식의 pid 이다.
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
        // ESRCH: 자식이 이미 끝나 회수를 기다린다. 회수 thread 가 회수한다.
        Err(nix::errno::Errno::ESRCH) => Ok((pid, KillResult::NoSuchProcess)),
        Err(error) => Err(format!("kill PTY child {pid}: {error}")),
    }
}

/// session 의 자식을 회수한다. 자식이 끝날 때까지 child lock 을 쥐지 않고 기다리므로, 그동안 close 는 lock 을 얻어
/// 자식에 신호를 보낸다. 끝난 자식은 회수될 때까지 좀비로 남아 그 pid 가 다시 쓰이지 않는다. 회수와 그 결과는 같은
/// lock 안에서 적고 child_reaped 로 알린다.
fn reap_child(session: Arc<Session>) {
    let process_id = session.child.lock().unwrap().child.process_id();
    let exited = process_id
        .ok_or_else(|| "PTY child has no process id".to_string())
        .and_then(|pid| wait_for_child_exit(&session.id, pid));
    let result = {
        let mut child = session.child.lock().unwrap();
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
        // The record is stored whether or not a view is attached; a broadcast reaches only the attached views.
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

/// Waits for the child of the session to exit. A build with the fault-injection feature fails the wait of a session that a test
/// named with `fault::fail_wait_of`; the failure is taken after the child exited, because the reaper starts its wait at the
/// start of the session.
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
pub mod fault {
    use std::sync::Mutex;

    /// The identifiers of the sessions whose next wait fails.
    static FAILING_WAITS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// Makes the next wait of the child of the session `session_id` fail.
    pub fn fail_wait_of(session_id: &str) {
        FAILING_WAITS.lock().unwrap().push(session_id.to_string());
    }

    /// Whether the next wait of the session fails; the failure is taken once.
    pub(crate) fn take_wait_failure(session_id: &str) -> bool {
        let mut failing = FAILING_WAITS.lock().unwrap();
        match failing.iter().position(|id| id == session_id) {
            Some(index) => {
                failing.remove(index);
                true
            }
            None => false,
        }
    }
}

/// Records the SIGKILL that closing a session sent to its process group or its child, and what it reached.
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
