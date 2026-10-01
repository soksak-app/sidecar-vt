//! terminal service가 사용하는 in-process PTY session이다.
//!
//! service는 session마다 하나의 PTY와 하나의 VT stream을 소유한다.  attachment는
//! 의도적으로 PTY handle로 표현하지 않는다. attachment를 drop하면 그 client로의
//! 전달만 멈추고, session 종료는 명시적인 close가 담당한다.

use crate::platform::pty::{kill_process_group, pending_input, process_group_leader};
use crate::protocol::DaemonEvent;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
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

const RETAINED_OUTPUT: usize = 10_000;

/// 한 시점의 PTY 전송 상태 측정. pending 은 마스터가 쓰고 자식이 아직 읽지 않은 입력 바이트 수이고,
/// written 는 세션 reader 가 지금까지 읽은 자식 출력의 누적 바이트 수다. 한 번의 측정 왕복이 두 값을
/// 함께 돌려주므로 쓰기 성공, 자식 수신, 자식 출력을 같은 시점에 구분할 수 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyMeasurement {
    pub pending: usize,
    pub written: u64,
}

struct Session {
    id: String,
    owner: String,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    process_group: Option<i32>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    child: Mutex<Box<dyn Child + Send>>,
    next_sequence: Mutex<i64>,
    written_output: Mutex<u64>,
    output: Mutex<VecDeque<(i64, Vec<u8>)>>,
    attachments: Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<DaemonEvent>>>,
    closed: Mutex<bool>,
    reader: Mutex<Option<thread::JoinHandle<()>>>,
    reaper: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct PtyService {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
}

impl PtyService {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
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
        self.open_owned("", program, args, cwd, cols, rows, events)
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

    pub fn open_owned(
        &self,
        owner: &str,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
        events: tokio::sync::mpsc::UnboundedSender<DaemonEvent>,
    ) -> Result<(String, String), String> {
        let mut command = CommandBuilder::new(program);
        command.args(args);
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
            child: Mutex::new(child),
            next_sequence: Mutex::new(0),
            written_output: Mutex::new(0),
            output: Mutex::new(VecDeque::with_capacity(RETAINED_OUTPUT)),
            attachments: Mutex::new(HashMap::new()),
            closed: Mutex::new(false),
            reader: Mutex::new(None),
            reaper: Mutex::new(None),
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

    pub fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
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
            let mut child = session.child.lock().unwrap();
            let child_result = match child.try_wait() {
                Ok(Some(_status)) => Ok(()),
                Ok(None) => match child.kill() {
                    Ok(()) => Ok(()),
                    Err(kill_error) => match child.try_wait() {
                        Ok(Some(_status)) => Ok(()),
                        Ok(None) => Err(format!("close PTY: {kill_error}")),
                        Err(wait_error) => {
                            Err(format!("close PTY: {kill_error}; wait PTY: {wait_error}"))
                        }
                    },
                },
                Err(wait_error) => Err(format!("check PTY child: {wait_error}")),
            };
            match (process_group_result, child_result) {
                (Err(group_error), _) => Err(group_error),
                (_, Err(child_error)) => Err(child_error),
                (Ok(()), Ok(())) => Ok(()),
            }
        };
        let reader = session.reader.lock().unwrap().take();
        let reaper = session.reaper.lock().unwrap().take();
        thread::spawn(move || {
            if let Some(reader) = reader {
                if reader.join().is_err() {
                    eprintln!("PTY reader thread panicked during close");
                }
            }
            if let Some(reaper) = reaper {
                if reaper.join().is_err() {
                    eprintln!("PTY reaper thread panicked during close");
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
        let process_id = session.child.lock().unwrap().process_id();
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
            Ok(0) => break,
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
                    let mut output = session.output.lock().unwrap();
                    output.push_back((sequence, data.clone()));
                    while output.len() > RETAINED_OUTPUT {
                        output.pop_front();
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

fn reap_child(session: Arc<Session>) {
    if let Err(error) = session.child.lock().unwrap().wait() {
        // 이 thread가 child lock을 얻기 전에 명시적인 close가 try_wait로 child를
        // 회수했을 수 있다. 그것은 의도된 close 결과이며,
        // child 회수 실패가 아니다.
        if !*session.closed.lock().unwrap() {
            broadcast(
                &session,
                DaemonEvent::Error {
                    session_id: session.id.clone(),
                    error: format!("wait PTY: {error}"),
                },
            );
        }
    }
    broadcast(
        &session,
        DaemonEvent::Exit {
            session_id: session.id.clone(),
        },
    );
}

fn broadcast(session: &Session, event: DaemonEvent) {
    session
        .attachments
        .lock()
        .unwrap()
        .retain(|_, sender| sender.send(event.clone()).is_ok());
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
    async fn independent_sessions_have_independent_output() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let service = PtyService::new();
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
        let service = PtyService::new();
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

    #[tokio::test]
    async fn attach_replays_retained_output_with_stable_session_id() {
        let _test_lock = crate::platform::pty::native_pty_test_lock();
        let service = PtyService::new();
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
