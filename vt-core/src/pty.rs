//! terminal service가 사용하는 in-process PTY session이다.
//!
//! service는 session마다 하나의 PTY와 하나의 VT stream을 소유한다.  attachment는
//! 의도적으로 PTY handle로 표현하지 않는다. attachment를 drop하면 그 client로의
//! 전달만 멈추고, session 종료는 명시적인 close가 담당한다.

use crate::events::{bytes_fields, Events, Level, Subject};
use crate::platform::pty::{
    kill_process_group, pending_input, process_group_leader, wait_for_exit, KillResult,
};
use crate::protocol::DaemonEvent;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
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

#[derive(Clone)]
pub struct PtyService {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    retained_output: usize,
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
            log,
        }
    }

    /// The service whose sessions keep `chunks` chunks of output in the ring.
    pub fn with_retained_output(mut self, chunks: usize) -> Self {
        self.retained_output = chunks;
        self
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
        command.env("SHELL", shell);
        crate::shell_integration::apply(shell, &mut command)?;
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
            .attachments
            .lock()
            .unwrap()
            .insert(attachment_id.clone(), events);

        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), session.clone());
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
        let reader_handle = thread::spawn(move || read_output(session, reader));
        // handle은 session이 게시된 뒤에 설치된다. close는 그 join을
        // request 경로에서 분리하므로 멈춘 PTY reader가 이후의 session open을
        // 막을 수 없다.
        let session = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .cloned()
            .unwrap();
        *session.reader.lock().unwrap() = Some(reader_handle);
        let session_for_reaper = session.clone();
        let reaper_handle = thread::spawn(move || reap_child(session_for_reaper));
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
            let output = session.output.lock().unwrap();
            let oldest = output
                .front()
                .map(|(sequence, _)| *sequence)
                // 기본값: 보관한 출력이 없으면 잘린 부분이 없다.
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
            .attachments
            .lock()
            .unwrap()
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
                    session.attachments.lock().unwrap().remove(&attachment_id);
                    "PTY attachment closed during replay".to_string()
                })?;
        }
        Ok(attachment_id)
    }

    pub fn detach(&self, session_id: &str, attachment_id: &str) -> Result<(), String> {
        let session = self.session(session_id)?;
        session.attachments.lock().unwrap().remove(attachment_id);
        Ok(())
    }

    /// Writes `data` to the session. `cids` are the correlation identifiers of the inputs that caused the write; the
    /// event of the write carries them.
    pub fn write(&self, session_id: &str, data: &[u8], cids: &[String]) -> Result<(), String> {
        let session = self.session(session_id)?;
        if *session.closed.lock().unwrap() {
            return Err("session is closed".into());
        }
        let result = session
            .writer
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| "session writer is closed".to_string())?
            .write_all(data)
            .map_err(|error| format!("write PTY: {error}"));
        let mut fields = bytes_fields(data);
        fields["ok"] = serde_json::json!(result.is_ok());
        if let Err(error) = &result {
            fields["error"] = serde_json::json!(error);
        }
        session.log.emit_for(
            Level::Info,
            "sidecar.vt.pty_write",
            Subject::session(session_id, cids),
            &fields,
        );
        result
    }

    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let session = self.session(session_id)?;
        let result = session
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
            .map_err(|error| format!("resize PTY: {error}"));
        result
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
        session.writer.lock().unwrap().take();
        session.master.lock().unwrap().take();
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
        let reader = session.reader.lock().unwrap().take();
        let reaper = session.reaper.lock().unwrap().take();
        let log = session.log.clone();
        thread::spawn(move || {
            if let Some(reader) = reader {
                if reader.join().is_err() {
                    log.record_error("pty close", "the reader thread panicked");
                }
            }
            if let Some(reaper) = reaper {
                if reaper.join().is_err() {
                    log.record_error("pty close", "the reaper thread panicked");
                }
            }
        });
        self.sessions.lock().unwrap().remove(session_id);
        kill_result
    }

    pub fn close_owner(&self, owner: &str) -> Result<(), String> {
        let ids = self
            .sessions
            .lock()
            .unwrap()
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
        self.sessions.lock().unwrap().len()
    }

    pub fn process_id(&self, session_id: &str) -> Result<u32, String> {
        let session = self.session(session_id)?;
        let process_id = session.child.lock().unwrap().child.process_id();
        process_id.ok_or_else(|| "PTY child has no process id".to_string())
    }

    fn session(&self, session_id: &str) -> Result<Arc<Session>, String> {
        self.sessions
            .lock()
            .unwrap()
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
                if session.log.enabled("io") {
                    let mut fields = bytes_fields(&data);
                    fields["sequence"] = serde_json::json!(sequence);
                    session.log.emit_for(
                        Level::Debug,
                        "sidecar.vt.pty_read",
                        Subject::session(&session.id, &[]),
                        &fields,
                    );
                }
                {
                    let mut output = session.output.lock().unwrap();
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
        .and_then(wait_for_exit);
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
    let mut attachments = session.attachments.lock().unwrap();
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
mod tests {
    use super::*;
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::time::{timeout, Duration};

    /// 멈춘 검사를 끝내는 상한. 성공은 기다린 event 로 판정하며 이 시간으로 판정하지 않는다. 부하가 큰 기계에서도
    /// PTY event 는 이 안에 온다.
    const STALL: Duration = Duration::from_secs(60);

    /// 다음 daemon event. 채널이 닫히거나 STALL 안에 event 가 없으면 실패한다.
    async fn next_event(rx: &mut UnboundedReceiver<DaemonEvent>) -> DaemonEvent {
        timeout(STALL, rx.recv())
            .await
            .expect("no daemon event arrived; the PTY test stalled")
            .expect("the daemon event channel closed before the expected event")
    }

    /// 다음 출력 data.
    async fn next_output(rx: &mut UnboundedReceiver<DaemonEvent>) -> Vec<u8> {
        loop {
            if let DaemonEvent::Output { data, .. } = next_event(rx).await {
                return data;
            }
        }
    }

    #[tokio::test]
    async fn the_events_record_every_write_to_and_read_from_the_pty_with_its_bytes() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let events = crate::events::test_support::TestEvents::new();
        let service = PtyService::new(events.handle());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        let cids = vec!["b1.5".to_string()];
        service
            .write(&session_id, "한a\n".as_bytes(), &cids)
            .unwrap();
        // The terminal echoes the line, which is the read that the events must record with the same text.
        let mut seen = String::new();
        while !seen.contains("한a") {
            seen.push_str(&String::from_utf8_lossy(&next_output(&mut rx).await));
        }
        service.close(&session_id).unwrap();
        let lines = events.lines();
        let named = |name: &str| -> Vec<&serde_json::Value> {
            lines.iter().filter(|line| line["event"] == name).collect()
        };
        let writes = named("sidecar.vt.pty_write");
        assert_eq!(writes.len(), 1, "{writes:?}");
        let written = writes[0];
        assert_eq!(written["session"], session_id);
        assert_eq!(written["cids"], serde_json::json!(["b1.5"]));
        assert_eq!(written["level"], "info");
        assert_eq!(written["fields"]["bytes"], 5);
        assert_eq!(written["fields"]["text"], "한a\n");
        assert_eq!(written["fields"]["hex"], "ed959c610a");
        assert_eq!(written["fields"]["ok"], true);
        let reads = named("sidecar.vt.pty_read");
        assert!(reads.iter().all(|read| read["level"] == "debug"));
        let read = reads
            .iter()
            .map(|record| record["fields"]["text"].as_str().unwrap().to_string())
            .collect::<String>();
        assert!(read.contains("한a"), "the read was not recorded: {read:?}");
        assert!(named("sidecar.vt.session_open")
            .iter()
            .any(|record| record["session"] == session_id));
    }

    #[tokio::test]
    async fn the_read_of_the_pty_is_not_recorded_when_the_policy_leaves_out_the_class_io() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let events =
            crate::events::test_support::TestEvents::with_policy(&["lifecycle", "input", "call"]);
        let service = PtyService::new(events.handle());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open(
                "/bin/sh",
                &["-c".into(), "printf policy".into()],
                None,
                80,
                24,
                tx,
            )
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        let mut seen = String::new();
        while !seen.contains("policy") {
            seen.push_str(&String::from_utf8_lossy(&next_output(&mut rx).await));
        }
        service.close(&session_id).unwrap();
        let lines = events.lines();
        assert_eq!(
            lines[0]["fields"]["policy"],
            serde_json::json!(["lifecycle", "input", "call"])
        );
        assert!(
            lines
                .iter()
                .all(|line| line["event"] != "sidecar.vt.pty_read"),
            "the class io is not stored: {lines:?}"
        );
        assert!(lines
            .iter()
            .any(|line| line["event"] == "sidecar.vt.session_open"));
    }

    /// The events of `name` in the file of `events`.
    fn named(
        events: &crate::events::test_support::TestEvents,
        name: &str,
    ) -> Vec<serde_json::Value> {
        events
            .lines()
            .into_iter()
            .filter(|line| line["event"] == name)
            .collect()
    }

    #[tokio::test]
    async fn closing_a_running_session_records_the_kill_of_its_process_group() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let events = crate::events::test_support::TestEvents::new();
        let service = PtyService::new(events.handle());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        service.close(&session_id).unwrap();
        let kills = named(&events, "sidecar.vt.kill");
        let group = kills
            .iter()
            .find(|kill| kill["fields"]["target"] == "process group")
            .unwrap_or_else(|| panic!("the kill of the group is not recorded: {kills:?}"));
        assert_eq!(group["session"], session_id);
        assert_eq!(group["fields"]["signal"], "SIGKILL");
        assert_eq!(group["fields"]["result"], "sent");
        assert!(group["fields"]["id"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn a_kill_of_a_group_that_ended_is_recorded_as_esrch() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let events = crate::events::test_support::TestEvents::new();
        let service = PtyService::new(events.handle());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open("/bin/sh", &["-c".into(), "true".into()], None, 80, 24, tx)
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        // The exit event follows the reaping of the child, so the group no longer exists.
        while !matches!(next_event(&mut rx).await, DaemonEvent::Exit { .. }) {}
        service.close(&session_id).unwrap();
        let kills = named(&events, "sidecar.vt.kill");
        let group = kills
            .iter()
            .find(|kill| kill["fields"]["target"] == "process group")
            .unwrap_or_else(|| panic!("the kill of the group is not recorded: {kills:?}"));
        assert_eq!(group["fields"]["result"], "ESRCH");
    }

    #[tokio::test]
    async fn output_that_leaves_the_ring_is_counted_as_a_dropped_buffer() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let events = crate::events::test_support::TestEvents::new();
        let service = PtyService::new(events.handle()).with_retained_output(2);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        for word in ["one", "two", "three", "four"] {
            service
                .write(&session_id, format!("{word}\n").as_bytes(), &[])
                .unwrap();
            let mut seen = String::new();
            // The terminal echoes the line and cat prints it again: the word is read twice.
            while seen.matches(word).count() < 2 {
                seen.push_str(&String::from_utf8_lossy(&next_output(&mut rx).await));
            }
        }
        service.close(&session_id).unwrap();
        let dropped = named(&events, "sidecar.vt.buffer_dropped");
        assert!(!dropped.is_empty(), "no chunk left the ring of 2 chunks");
        for drop in &dropped {
            assert_eq!(drop["level"], "warn");
            assert_eq!(drop["session"], session_id);
            assert_eq!(drop["fields"]["buffer"], "output ring");
            assert_eq!(drop["fields"]["count"], 1);
            assert!(drop["fields"]["bytes"].as_u64().unwrap() > 0);
            assert_eq!(drop["fields"]["reason"], "the ring holds 2 chunks");
        }
    }

    #[tokio::test]
    async fn an_attachment_that_nobody_reads_is_removed_and_counted_as_a_dropped_buffer() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let events = crate::events::test_support::TestEvents::new();
        let service = PtyService::new(events.handle());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        drop(rx);
        service.write(&session_id, b"gone\n", &[]).unwrap();
        // The reader thread stores the event when it broadcasts the echo to the attachment whose receiver is gone. No
        // notification reaches this test from that thread, so it asks the file again after a short pause.
        let dropped = timeout(STALL, async {
            loop {
                let found = named(&events, "sidecar.vt.buffer_dropped");
                if !found.is_empty() {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the dropped attachment was not recorded");
        service.close(&session_id).unwrap();
        let drop = &dropped[0];
        assert_eq!(drop["level"], "warn");
        assert_eq!(drop["session"], session_id);
        assert_eq!(drop["fields"]["buffer"], "broadcast");
        assert_eq!(drop["fields"]["count"], 1);
        assert!(drop["fields"]["bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            drop["fields"]["reason"],
            "the receiver of the attachment was closed"
        );
    }

    #[tokio::test]
    async fn independent_sessions_have_independent_output() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        let (session_a, _) = service
            .open(
                "/bin/sh",
                &["-c".into(), "printf A".into()],
                None,
                80,
                24,
                tx_a,
            )
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        let (session_b, _) = service
            .open(
                "/bin/sh",
                &["-c".into(), "printf B".into()],
                None,
                80,
                24,
                tx_b,
            )
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        assert_ne!(session_a, session_b);

        let output_a = next_output(&mut rx_a).await;
        let output_b = next_output(&mut rx_b).await;
        assert!(String::from_utf8_lossy(&output_a).contains('A'));
        assert!(String::from_utf8_lossy(&output_b).contains('B'));
    }

    #[tokio::test]
    async fn explicit_close_kills_the_child_and_drains_exit() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open(
                "/bin/sh",
                &["-c".into(), "sleep 30".into()],
                None,
                80,
                24,
                tx,
            )
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        let started = std::time::Instant::now();
        let close_service = service.clone();
        let close_session = session_id.clone();
        timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || close_service.close(&close_session)),
        )
        .await
        .expect("close exceeded its two-second operation limit")
        .expect("close worker panicked")
        .expect("explicit close failed");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "close operation exceeded its two-second limit: {:?}",
            started.elapsed()
        );
        // close 가 돌아온 뒤 exit event 가 남아 있어야 한다.
        while !matches!(next_event(&mut rx).await, DaemonEvent::Exit { .. }) {}
    }

    /// 회수 thread 의 시작과 회수 결과를 알리는 자식. 회수 thread 가 자식에 처음 닿으면(pid 조회나 wait) started 로
    /// 알리고, wait 가 회수한 종료 상태를 status 에 적는다.
    #[derive(Debug)]
    struct ObservedChild {
        inner: std::process::Child,
        started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        status: Arc<Mutex<Option<std::process::ExitStatus>>>,
    }

    impl ObservedChild {
        fn mark_started(&self) {
            if let Some(started) = self.started.lock().unwrap().take() {
                started
                    .send(())
                    .expect("the test waits for the reaper to start");
            }
        }
    }

    impl portable_pty::ChildKiller for ObservedChild {
        fn kill(&mut self) -> std::io::Result<()> {
            panic!("PtyService signals the session child by its process id")
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            panic!("PtyService does not clone the killer of a session child")
        }
    }

    impl Child for ObservedChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            panic!("only the reaper thread reaps the session child")
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            self.mark_started();
            let status = self.inner.wait()?;
            *self.status.lock().unwrap() = Some(status);
            Ok(status.into())
        }

        fn process_id(&self) -> Option<u32> {
            self.mark_started();
            Some(self.inner.id())
        }
    }

    /// 스스로 seconds 초 뒤에 끝나는 자식의 session 을 service 에 넣고 회수 thread 를 시작한다. 회수 thread 가 자식에
    /// 닿은 뒤에 돌아오며, 자식을 회수하면 적히는 종료 상태를 돌려준다. with_group 이 거짓이면 session 에 process group
    /// 이 없어 close 의 group 신호가 실패한다.
    fn observed_session(
        service: &PtyService,
        id: &str,
        seconds: u32,
        with_group: bool,
    ) -> Arc<Mutex<Option<std::process::ExitStatus>>> {
        use std::os::unix::process::CommandExt;
        let inner = std::process::Command::new("/bin/sleep")
            .arg(seconds.to_string())
            .process_group(0)
            .spawn()
            .expect("spawn the session child");
        let group = inner.id() as i32;
        let (started_sender, started) = std::sync::mpsc::channel();
        let status = Arc::new(Mutex::new(None));
        let session = Arc::new(Session {
            id: id.to_string(),
            owner: String::new(),
            master: Mutex::new(None),
            process_group: with_group.then_some(group),
            writer: Mutex::new(None),
            child: Mutex::new(SessionChild {
                child: Box::new(ObservedChild {
                    inner,
                    started: Mutex::new(Some(started_sender)),
                    status: Arc::clone(&status),
                }),
                reaped: None,
            }),
            child_reaped: Condvar::new(),
            next_sequence: Mutex::new(0),
            written_output: Mutex::new(0),
            output: Mutex::new(VecDeque::new()),
            attachments: Mutex::new(HashMap::new()),
            closed: Mutex::new(false),
            reader: Mutex::new(None),
            reaper: Mutex::new(None),
            retained_output: RETAINED_OUTPUT,
            log: crate::events::test_support::TestEvents::new().handle(),
        });
        service
            .sessions
            .lock()
            .unwrap()
            .insert(session.id.clone(), Arc::clone(&session));
        let session_for_reaper = Arc::clone(&session);
        *session.reaper.lock().unwrap() =
            Some(thread::spawn(move || reap_child(session_for_reaper)));
        started.recv().expect("the reaper thread did not start");
        status
    }

    // close 는 회수 thread 가 자식을 회수한 뒤에 돌아온다(S20, core F61).
    #[test]
    fn close_returns_after_the_reaper_reaps_the_child() {
        let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
        let status = observed_session(&service, "reaped", 30, true);
        let closed = service.close("reaped");
        let status_at_return = *status.lock().unwrap();
        closed.expect("close failed");
        assert!(
            status_at_return.is_some(),
            "close returned before the PTY child was reaped"
        );
    }

    // 회수 thread 가 자식을 기다리는 동안 process group 신호가 자식에 닿지 않아도(여기서는 group 이 없어 실패한다),
    // close 는 자식을 pid 로 끝내고 회수된 뒤에 돌아온다. 자식은 스스로 20 초 뒤에 끝나므로, close 가 회수 thread 의
    // child lock 에서 막히면 자식은 신호 없이 끝난다(S22).
    #[test]
    fn close_ends_a_child_that_the_group_signal_missed_while_the_reaper_waits() {
        use std::os::unix::process::ExitStatusExt;
        let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
        let status = observed_session(&service, "group-missed", 20, false);
        let closed = service.close("group-missed");
        let status_at_return = *status.lock().unwrap();
        assert_eq!(
            status_at_return.map(|status| status.signal()),
            Some(Some(libc::SIGKILL)),
            "close did not end the PTY child with SIGKILL before it returned: {status_at_return:?}"
        );
        assert_eq!(closed, Err("PTY process group is unavailable".to_string()));
    }

    #[tokio::test]
    async fn attach_replays_retained_output_with_stable_session_id() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (session_id, _) = service
            .open(
                "/bin/sh",
                &["-c".into(), "printf retained".into()],
                None,
                80,
                24,
                tx_a,
            )
            .expect("PTY setup failed; environmental PTY errors must fail this test");
        while !matches!(next_event(&mut rx_a).await, DaemonEvent::Exit { .. }) {}
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
        let attachment_id = service.attach(&session_id, 0, tx_b).unwrap();
        assert!(!attachment_id.is_empty());
        let event = next_event(&mut rx_b).await;
        match event {
            DaemonEvent::Output {
                session_id: replayed,
                data,
                sequence,
                ..
            } => {
                assert_eq!(replayed, session_id);
                assert_eq!(sequence, 0);
                assert!(String::from_utf8_lossy(&data).contains("retained"));
            }
            other => panic!("expected replayed output, got {other:?}"),
        }
    }
}
