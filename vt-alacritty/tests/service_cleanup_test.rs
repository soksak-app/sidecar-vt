//! 영속 service 가 끝날 때와 다음 service 가 시작할 때 전용 socket 디렉터리를 남기지 않는지 검사한다.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::time::Duration;

/// 멈춘 검사를 끝내는 상한이다. 성공은 준비 줄과 끝난 process 로 판정하며 이 시간으로 판정하지 않는다.
const STALL: Duration = Duration::from_secs(60);

/// 검사가 띄운 service. 검사가 실패해도 끝나면 그 process 를 끝내고 회수한다.
struct Service(Child);

impl Drop for Service {
    fn drop(&mut self) {
        // 끝난 process 는 회수되어 있다. 아니면 종료를 요청해 service 가 자기 디렉터리를 정리하게 하고, STALL 안에
        // 끝나지 않으면 강제로 끝낸다.
        if matches!(self.0.try_wait(), Ok(Some(_)) | Err(_)) {
            return;
        }
        // SAFETY: 이 검사가 만든 자식 process 에 종료를 요청한다.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        if reaped_within(self.0.id(), STALL).is_none() {
            self.0.kill().expect("kill the service that did not end");
            self.0.wait().expect("reap the service");
        }
    }
}

/// 자식 process pid 를 limit 안에 회수하면 waitpid 의 결과를, 아니면 None 을 돌려준다.
fn reaped_within(pid: u32, limit: Duration) -> Option<i32> {
    let (sent, received) = channel();
    std::thread::spawn(move || {
        let mut status = 0;
        // SAFETY: 이 검사가 만든 자식 process 를 회수한다.
        let waited = unsafe { libc::waitpid(pid as i32, &mut status, 0) };
        // 받는 쪽이 상한으로 끝났으면 회수 결과는 쓰이지 않는다.
        let _ = sent.send(waited);
    });
    received.recv_timeout(limit).ok()
}

/// 검사의 service 디렉터리. 검사가 실패해도 끝나면 지운다.
struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove the service directory");
    }
}

/// service 를 시작하고 준비 줄의 socket 경로를 돌려준다.
fn start(service_dir: &Path) -> (Service, PathBuf) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--service-dir")
        .arg(service_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the service");
    let stdout = child.stdout.take().expect("service stdout");
    let (sent, received) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        sent.send(read).expect("the test waits for the ready line");
    });
    let line = received
        .recv_timeout(STALL)
        .expect("the service printed no ready line")
        .expect("read the ready line");
    let endpoint: serde_json::Value = serde_json::from_str(&line).expect("ready endpoint");
    let socket = PathBuf::from(endpoint["socket"].as_str().expect("socket path"));
    (Service(child), socket)
}

/// 끝나기를 STALL 까지 기다린다.
fn wait(service: &mut Service) {
    let pid = service.0.id();
    let waited = reaped_within(pid, STALL).expect("the service did not end");
    assert_eq!(waited, pid as i32, "waitpid failed");
}

/// 검사 service 디렉터리의 경로.
fn service_directory_name() -> PathBuf {
    std::env::temp_dir().join(format!("vt-cleanup-{}-{}", std::process::id(), uuid()))
}

fn service_dir() -> Directory {
    let directory = service_directory_name();
    std::fs::create_dir_all(&directory).expect("create the service directory");
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("secure the service directory");
    Directory(directory)
}

/// 이 process 안에서 겹치지 않는 이름 조각. 시계는 마이크로초 단위라 같은 순간에 만든 이름이 같아지므로 증가 번호를
/// 붙인다.
fn uuid() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{time}-{next}")
}

#[test]
fn a_terminated_service_removes_its_socket_directory() {
    let directory = service_dir();
    let (mut service, socket) = start(&directory.0);
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    assert!(
        socket_dir.exists(),
        "the service did not create {}",
        socket_dir.display()
    );
    // SAFETY: 이 검사가 만든 자식 process 에 종료를 요청한다.
    assert_eq!(
        unsafe { libc::kill(service.0.id() as i32, libc::SIGTERM) },
        0,
        "SIGTERM failed"
    );
    wait(&mut service);
    let left = socket_dir.exists();
    if left {
        std::fs::remove_dir_all(&socket_dir).expect("remove the socket directory the service left");
    }
    assert!(
        !left,
        "the terminated service left {}",
        socket_dir.display()
    );
    assert!(
        !directory.0.join("endpoint.json").exists(),
        "the terminated service left its endpoint"
    );
}

#[test]
fn a_new_service_removes_the_socket_directory_of_a_killed_one() {
    let directory = service_dir();
    let (mut service, socket) = start(&directory.0);
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    // 강제 종료는 정리할 기회를 주지 않는다.
    service.0.kill().expect("kill the first service");
    wait(&mut service);
    assert!(
        socket_dir.exists(),
        "the killed service removed {}",
        socket_dir.display()
    );
    let (_next, _) = start(&directory.0);
    let left = socket_dir.exists();
    if left {
        std::fs::remove_dir_all(&socket_dir).expect("remove the socket directory the service left");
    }
    assert!(!left, "the next service left {}", socket_dir.display());
}

// 같은 process 에서 병렬로 도는 검사들의 service 디렉터리는 서로 달라야 한다. 이름이 같으면 한 검사의 service 가
// 다른 검사의 잠금이나 endpoint 를 보고 실패한다(S15).
#[test]
fn service_directories_are_unique_within_the_test_process() {
    // 검사들처럼 여러 thread 가 같은 순간에 이름을 만든다.
    let start = std::sync::Arc::new(std::sync::Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                (0..20)
                    .map(|_| service_directory_name())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let names: Vec<PathBuf> = threads
        .into_iter()
        .flat_map(|thread| thread.join().expect("name thread"))
        .collect();
    let unique: std::collections::HashSet<&PathBuf> = names.iter().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "service directory names repeated"
    );
}
