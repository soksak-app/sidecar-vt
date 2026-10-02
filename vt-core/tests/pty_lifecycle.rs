use std::fs::OpenOptions;
use std::os::fd::AsRawFd;

use soksak_sidecar_vt_core::pty::PtyService;
use soksak_sidecar_vt_core::DaemonEvent;
use tokio::time::{timeout, Duration};

/// 실제 PTY 를 쓰는 test 를 하나씩 실행한다. test 마다 runtime 이 다르므로 await 를 넘어 잡는 async 잠금이다.
static LIFECYCLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn lifecycle_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    LIFECYCLE_LOCK.lock().await
}

fn native_pty_test_lock() -> std::fs::File {
    let path = std::env::temp_dir().join("soksak-vt-core-pty-tests.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .expect("PTY test lock file must open");
    nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)
        .expect("PTY test lock must acquire");
    file
}

#[tokio::test]
async fn real_sessions_are_independent_and_close_removes_session() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
    let (a, _) = service
        .open(
            "/bin/sh",
            &["-c".into(), "printf A".into()],
            None,
            80,
            24,
            tx_a,
        )
        .expect("PTY setup failed; this is an environmental test failure");
    let (b, _) = service
        .open(
            "/bin/sh",
            &["-c".into(), "printf B".into()],
            None,
            80,
            24,
            tx_b,
        )
        .expect("PTY setup failed; this is an environmental test failure");
    assert_ne!(a, b);
    let output_a = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonEvent::Output { data, .. }) = rx_a.recv().await {
                break data;
            }
        }
    })
    .await
    .expect("session A output timeout");
    let output_b = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonEvent::Output { data, .. }) = rx_b.recv().await {
                break data;
            }
        }
    })
    .await
    .expect("session B output timeout");
    assert!(String::from_utf8_lossy(&output_a).contains('A'));
    assert!(String::from_utf8_lossy(&output_b).contains('B'));
    service.close(&a).expect("explicit close failed");
    assert!(
        service.write(&a, b"x").is_err(),
        "closed session remained addressable"
    );
}

#[tokio::test]
async fn repeated_short_lived_sessions_close_without_process_group_races() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let started = std::time::Instant::now();
    let service = PtyService::new();
    for index in 0..20 {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (session, _) = service
            .open(
                "/bin/sh",
                &["-c".into(), format!("printf short-{index}")],
                None,
                80,
                24,
                tx,
            )
            .expect("short-lived PTY setup failed");
        service
            .close(&session)
            .expect("short-lived PTY close failed");
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "short-lived PTY setup loop exceeded its three-second bound: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn three_real_sessions_reconnect_with_same_pid_and_retained_output() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    let mut sessions = Vec::new();
    for label in ["A", "B", "C"] {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (session, attachment) = service
            .open(
                "/bin/sh",
                &["-c".into(), format!("printf {label}")],
                None,
                80,
                24,
                tx,
            )
            .expect("PTY setup failed; real PTY errors must fail this test");
        let pid = service.process_id(&session).expect("missing child pid");
        let output = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(DaemonEvent::Output { data, .. }) = rx.recv().await {
                    break data;
                }
            }
        })
        .await
        .expect("session output timeout");
        service
            .detach(&session, &attachment)
            .expect("detach failed");
        sessions.push((session, pid, output));
    }
    for (session, pid, output) in sessions {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _attachment = service
            .attach(&session, 0, tx)
            .expect("reconnect attach failed");
        assert_eq!(
            service
                .process_id(&session)
                .expect("reconnected pid missing"),
            pid
        );
        let replay = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(DaemonEvent::Output { data, .. }) = rx.recv().await {
                    break data;
                }
            }
        })
        .await
        .expect("replay timeout");
        assert_eq!(replay, output);
        service.close(&session).expect("close failed");
    }
}

// 마스터가 쓴 바이트 중 자식이 아직 읽지 않은 양의 계약. 읽지 않는 자식에서는 쓴 만큼 남는다.
// 마우스 보고를 쓰는 전체 화면 프로그램처럼 자식이 raw 모드를 켠 뒤에 쓴다. canonical 모드에서는 줄이
// 완성되기 전 바이트가 줄 조립 버퍼에 남아 큐에 반영되지 않는다.
#[tokio::test]
async fn pty_measurement_counts_master_written_bytes_a_non_reading_child_has_not_read() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open(
            "/bin/sh",
            &["-c".into(), "stty raw; printf READY; sleep 30".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; real PTY errors must fail this test");
    let mut seen = Vec::new();
    let ready = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonEvent::Output { data, .. }) = rx.recv().await {
                seen.extend_from_slice(&data);
                if seen.ends_with(b"READY") {
                    return;
                }
            }
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "the child never reported its raw mode; output so far: {seen:?}"
    );
    let first = b"pending-probe";
    let second = b"-second";
    service
        .write(&session, first)
        .expect("first production-path write failed");
    assert_eq!(
        service
            .pty_measurement(&session)
            .expect("PTY measurement is unavailable")
            .pending,
        first.len(),
        "a child that reads nothing must leave every written byte pending"
    );
    service
        .write(&session, second)
        .expect("second production-path write failed");
    assert_eq!(
        service
            .pty_measurement(&session)
            .expect("PTY measurement is unavailable")
            .pending,
        first.len() + second.len(),
        "pending input must accumulate across writes until the child reads"
    );
    service.close(&session).expect("close failed");
}

// 읽는 자식은 큐를 비운다. 유한 시간 안에 0 이 되지 않으면 실패한다.
#[tokio::test]
async fn pty_measurement_drains_to_zero_as_the_child_reads() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open(
            "/bin/sh",
            &[
                "-c".into(),
                "stty raw; printf READY; cat > /dev/null".into(),
            ],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; real PTY errors must fail this test");
    let mut seen = Vec::new();
    let ready = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonEvent::Output { data, .. }) = rx.recv().await {
                seen.extend_from_slice(&data);
                if seen.ends_with(b"READY") {
                    return;
                }
            }
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "the child never reported its raw mode; output so far: {seen:?}"
    );
    service
        .write(&session, b"drain-probe")
        .expect("production-path write failed");
    let started = std::time::Instant::now();
    loop {
        let pending = service
            .pty_measurement(&session)
            .expect("PTY measurement is unavailable")
            .pending;
        if pending == 0 {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "a reading child did not drain {pending} pending bytes within two seconds"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    service.close(&session).expect("close failed");
}

// 없는 세션과 닫은 세션의 측정은 오류이다. 0 이 아니라 명시적 실패로 구분한다.
#[tokio::test]
async fn pty_measurement_rejects_unknown_and_closed_sessions() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    assert!(
        service.pty_measurement("no-such-session").is_err(),
        "an unknown session must not report a measurement"
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open(
            "/bin/sh",
            &["-c".into(), "sleep 30".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; real PTY errors must fail this test");
    service.close(&session).expect("close failed");
    assert!(
        service.pty_measurement(&session).is_err(),
        "a closed session must not report a measurement"
    );
}

// reader 가 읽은 자식 출력의 누적 바이트 계약. 정확히 알려진 바이트만 출력하고 조용히 있는 자식에서
// 카운터는 그 수에 정확히 도달한다. mouse-up 뒤 자식 출력이 있었는지 묻는 진단 질문의 소유 검증이다.
// stty -opost 로 출력 후처리를 끊어 reader 가 읽은 바이트가 자식이 쓴 바이트와 정확히 일치한다.
#[tokio::test]
async fn written_output_counts_exact_child_output_bytes() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open(
            "/bin/sh",
            &["-c".into(), "stty -opost; printf MEASURED; sleep 30".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; real PTY errors must fail this test");
    let mut seen = Vec::new();
    let ready = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonEvent::Output { data, .. }) = rx.recv().await {
                seen.extend_from_slice(&data);
                if seen.ends_with(b"MEASURED") {
                    return;
                }
            }
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "the child never finished its measured output; output so far: {seen:?}"
    );
    let expected = b"MEASURED".len() as u64;
    let started = std::time::Instant::now();
    let written = loop {
        let measurement = service
            .pty_measurement(&session)
            .expect("PTY measurement is unavailable");
        assert_eq!(
            measurement.pending, 0,
            "nothing was written to this child, so no input byte may be pending"
        );
        if measurement.written >= expected {
            break measurement.written;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the reader did not count {expected} output bytes within two seconds; counted {} so far",
            measurement.written
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(
        written, expected,
        "a child that emits exactly {expected} bytes must move exactly that many bytes through the reader"
    );
    service.close(&session).expect("close failed");
}

// 생산 write 경로로 쓴 정확한 바이트를 자식이 읽어 되돌리는 수신 fixture. READY 뒤에만 쓰므로
// stty -echo 가 이미 적용됐고, stty -opost 로 출력 후처리(\n 을 \r\n 으로 바꾸는 등)를 끊어
// 돌아온 바이트는 echo 나 가공이 아니라 cat 이 읽은 그대로다.
#[tokio::test]
async fn child_reads_exact_bytes_written_through_the_production_path() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (session, _) = service
        .open(
            "/bin/sh",
            &["-c".into(), "stty -echo -opost; printf READY; cat".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; real PTY errors must fail this test");
    let mut seen = Vec::new();
    let ready = timeout(Duration::from_secs(2), async {
        loop {
            if let Some(DaemonEvent::Output { data, .. }) = rx.recv().await {
                seen.extend_from_slice(&data);
                if seen.ends_with(b"READY") {
                    return;
                }
            }
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "the receiving child never reported READY; output so far: {seen:?}"
    );
    // ?1003 프로그램이 받는 드래그 보고와 같은 SGR 바이트 형태를 쓴다. 행 끝 newline 이 cat 의 읽기를 끝낸다.
    let bytes = b"\x1b[<0;5;2M\x1b[<32;6;2m\x1b[<35;7;2m\x1b[<0;7;2m\n";
    service
        .write(&session, bytes)
        .expect("production-path write failed");
    let receipt = timeout(Duration::from_secs(2), async {
        loop {
            if seen.len() >= b"READY".len() + bytes.len() {
                return seen[b"READY".len()..].to_vec();
            }
            match rx.recv().await {
                Some(DaemonEvent::Output { data, .. }) => seen.extend_from_slice(&data),
                other => panic!("the session ended before the child returned the bytes: {other:?}"),
            }
        }
    })
    .await
    .expect("the child did not return the written bytes within two seconds");
    assert_eq!(
        receipt,
        bytes.to_vec(),
        "bytes read by the child differ from the production write path"
    );
    service.close(&session).expect("close failed");
}

/// macOS는 member가 모두 zombie인 process group에 대한 signal에 EPERM으로 응답한다.
/// 그런 group에는 종료할 대상이 남아 있지 않으므로 close는 성공한다.
#[test]
fn a_process_group_of_only_zombies_is_already_terminated() {
    use std::os::unix::process::CommandExt;
    let _test_lock = LIFECYCLE_LOCK.blocking_lock();
    let _native_test_lock = native_pty_test_lock();
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .expect("the shell must start");
    let group = child.id() as i32;
    // 종료를 기다리되 회수하지 않아 그룹에 좀비 하나만 남긴다.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            group as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(
        waited,
        0,
        "waitid failed: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(-group),
            nix::sys::signal::Signal::SIGKILL
        ),
        Err(nix::errno::Errno::EPERM),
        "the operating system answers a zombie-only group with EPERM"
    );
    let result = soksak_sidecar_vt_core::platform::pty::kill_process_group(Some(group));
    child.wait().expect("the shell must be reaped");
    assert_eq!(result, Ok(()));
}

#[test]
fn process_group_members_report_running_and_ended_processes() {
    use soksak_sidecar_vt_core::platform::darwin::process_group::{members, Member};
    use std::os::unix::process::CommandExt;
    let _test_lock = LIFECYCLE_LOCK.blocking_lock();
    let _native_test_lock = native_pty_test_lock();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .process_group(0)
        .spawn()
        .expect("sleep must start");
    let group = child.id() as i32;
    assert_eq!(
        members(group),
        Ok(vec![Member {
            pid: group,
            zombie: false,
            exiting: false
        }])
    );
    child.kill().expect("sleep must end");
    // 종료를 기다리되 회수하지 않는다. 끝난 프로세스는 좀비로 보고된다.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            group as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(
        waited,
        0,
        "waitid failed: {}",
        std::io::Error::last_os_error()
    );
    let ended = members(group).expect("the group must be readable");
    assert!(
        ended.len() == 1 && ended[0].pid == group && ended[0].zombie && ended[0].ended(),
        "the ended process was not reported as a zombie: {ended:?}"
    );
    child.wait().expect("sleep must be reaped");
    assert_eq!(members(group), Ok(vec![]));
}

/// 셸이 읽히지 않은 PTY 출력을 남기고 끝나면 커널은 슬레이브를 닫으며 출력이 비워지기를 기다린다.
/// 그동안 셸은 좀비가 아니라 종료 중이고, macOS 는 그 그룹의 신호에 EPERM 으로 답한다.
#[test]
fn a_process_group_whose_members_are_exiting_is_already_terminated() {
    use soksak_sidecar_vt_core::platform::darwin::process_group::members;
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    let _test_lock = LIFECYCLE_LOCK.blocking_lock();
    let _native_test_lock = native_pty_test_lock();
    let (mut master, mut slave) = (0, 0);
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        opened,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    let master = unsafe { std::os::fd::OwnedFd::from_raw_fd(master) };
    // 슬레이브는 자식에게만 남기고 이 프로세스에서는 닫는다.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf unread-output"])
        .stdout(unsafe { std::process::Stdio::from_raw_fd(slave) })
        .process_group(0)
        .spawn()
        .expect("the shell must start");
    let group = child.id() as i32;
    let started = std::time::Instant::now();
    let exiting = loop {
        let now = members(group).expect("the group must be readable");
        if now.iter().any(|member| member.exiting && !member.zombie) {
            break now;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the shell never reported an exit in progress: {now:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let result = soksak_sidecar_vt_core::platform::pty::kill_process_group(Some(group));
    drop(master);
    child.wait().expect("the shell must be reaped");
    assert_eq!(result, Ok(()), "members while exiting: {exiting:?}");
}
