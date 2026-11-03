use std::fs::OpenOptions;
#[path = "support/events_test.rs"]
mod events_test;

use std::os::fd::AsRawFd;
use std::sync::Arc;

use soksak_sidecar_vt_core::pty::PtyService;
use soksak_sidecar_vt_core::{DaemonEvent, LocalSessionPort, SessionPort, ShellRequest};
use tokio::time::{Duration, timeout};

/// Runs the tests that use a real PTY one at a time. Each test has its own runtime, so this is an
/// async lock held across await.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_sessions_are_independent_and_close_removes_session() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx_a, mut rx_a) = soksak_sidecar_vt_core::pty::output_channel();
    let (tx_b, mut rx_b) = soksak_sidecar_vt_core::pty::output_channel();
    let a = service
        .open(
            "/bin/sh",
            &["-c".into(), "printf A".into()],
            None,
            80,
            24,
            tx_a,
        )
        .expect("PTY setup failed; this is an environmental test failure");
    let b = service
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
        service
            .write(
                &a,
                soksak_sidecar_vt_core::pty::WriteOrigin::Person,
                &b"x"[..]
            )
            .is_err(),
        "closed session remained addressable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_short_lived_sessions_close_without_process_group_races() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let started = std::time::Instant::now();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    for index in 0..20 {
        let (tx, _rx) = soksak_sidecar_vt_core::pty::output_channel();
        let session = service
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

// The contract for the bytes that the master wrote and the child has not read yet. A child that
// does not read leaves every written byte pending. Writes happen after the child enables raw mode,
// as a full-screen program that writes mouse reports does. In canonical mode, bytes before their
// line is complete stay in the line assembly buffer and are not reflected in the queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pty_measurement_counts_master_written_bytes_a_non_reading_child_has_not_read() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx, mut rx) = soksak_sidecar_vt_core::pty::output_channel();
    let session = service
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
        .write(
            &session,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            &first[..],
        )
        .expect("first production-path write failed")
        .await
        .expect("the session completes the write")
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
        .write(
            &session,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            &second[..],
        )
        .expect("second production-path write failed")
        .await
        .expect("the session completes the write")
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

// A child that reads empties the queue. The test fails if the count does not reach 0 within a
// finite time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pty_measurement_drains_to_zero_as_the_child_reads() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx, mut rx) = soksak_sidecar_vt_core::pty::output_channel();
    let session = service
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
        .write(
            &session,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            &b"drain-probe"[..],
        )
        .expect("production-path write failed")
        .await
        .expect("the session completes the write")
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

// A measurement of a missing session or a closed session is an error. It is reported as an explicit
// failure, not as 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pty_measurement_rejects_unknown_and_closed_sessions() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    assert!(
        service.pty_measurement("no-such-session").is_err(),
        "an unknown session must not report a measurement"
    );
    let (tx, _rx) = soksak_sidecar_vt_core::pty::output_channel();
    let session = service
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

// The contract for the cumulative bytes of child output that the reader has read. A silent child
// that prints only known bytes makes the counter reach exactly that number. This verifies the
// diagnostic question of whether child output followed a mouse-up. stty -opost disables output
// post-processing, so the bytes that the reader reads match the bytes that the child wrote exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn written_output_counts_exact_child_output_bytes() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx, mut rx) = soksak_sidecar_vt_core::pty::output_channel();
    let session = service
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

// A receiving fixture: the child reads back the exact bytes written through the production write
// path. It writes only after READY, so stty -echo is already applied, and stty -opost disables
// output post-processing (such as converting \n to \r\n), so the returned bytes are exactly what
// cat read, with no echo or conversion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_reads_exact_bytes_written_through_the_production_path() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let service = PtyService::new(events_test::TestEvents::new().handle());
    let (tx, mut rx) = soksak_sidecar_vt_core::pty::output_channel();
    let session = service
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
    // Writes the same SGR byte form as the drag reports that a ?1003 program receives. The newline
    // at the end of the line ends the read of cat.
    let bytes = b"\x1b[<0;5;2M\x1b[<32;6;2m\x1b[<35;7;2m\x1b[<0;7;2m\n";
    service
        .write(
            &session,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            &bytes[..],
        )
        .expect("production-path write failed")
        .await
        .expect("the session completes the write")
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

/// macOS answers a signal to a process group whose members are all zombies with EPERM. No target
/// remains in such a group to terminate, so close succeeds.
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
    // Waits for the exit without reaping, so that one zombie remains in the group.
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
    assert_eq!(
        result,
        Ok(soksak_sidecar_vt_core::platform::pty::KillResult::Ending)
    );
}

#[test]
fn process_group_members_report_running_and_ended_processes() {
    use soksak_sidecar_vt_core::platform::darwin::process_group::{Member, members};
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
    // Waits for the exit without reaping. The ended process is reported as a zombie.
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

/// When a shell ends with unread PTY output, the kernel closes the slave and waits for the output
/// to drain. During that wait the shell is exiting, not a zombie, and macOS answers signals to its
/// group with EPERM.
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
    // Keeps the slave only in the child and closes it in this process.
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "printf unread-output"])
        .stdout(unsafe { std::process::Stdio::from_raw_fd(slave) })
        .process_group(0)
        .spawn()
        .expect("the shell must start");
    let group = child.id() as i32;
    // No event reports the stage of an exit, so the test reads the answer of the kernel. The kernel
    // reports PROC_FLAG_INEXIT before it refuses signals, and it accepts a signal in between.
    let exiting = loop {
        let refused = unsafe { libc::kill(-group, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if refused {
            break members(group).expect("the group must be readable");
        }
        std::thread::yield_now();
    };
    assert!(
        exiting
            .iter()
            .any(|member| member.exiting && !member.zombie),
        "the group refused signals without an exit in progress: {exiting:?}"
    );
    let result = soksak_sidecar_vt_core::platform::pty::kill_process_group(Some(group));
    drop(master);
    child.wait().expect("the shell must be reaped");
    assert_eq!(
        result,
        Ok(soksak_sidecar_vt_core::platform::pty::KillResult::Ending),
        "members while exiting: {exiting:?}"
    );
}

/// A session whose shell is a script that sleeps and never reads its input. The write of 1 MiB
/// fills the terminal buffer and waits for the child, which ends after three seconds.
#[tokio::test(flavor = "current_thread")]
async fn a_write_waiting_for_the_child_does_not_block_the_runtime() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let dir = tempfile::tempdir().expect("temporary directory must be created");
    let script = dir.path().join("sleeper");
    std::fs::write(&script, "#!/bin/sh\nsleep 3\n").expect("script must be written");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("script must be executable");
    let port = Arc::new(LocalSessionPort::new_with_owner(
        Arc::new(PtyService::new(events_test::TestEvents::new().handle())),
        String::new(),
    ));
    let shell = script.to_str().expect("script path is UTF-8").to_string();
    let session = port
        .open(
            &ShellRequest {
                shell,
                directory: None,
            },
            80,
            24,
        )
        .await
        .expect("PTY setup failed; this is an environmental test failure");
    let writer = {
        let port = Arc::clone(&port);
        let session = session.clone();
        // Complete lines fill the canonical queue of the line discipline, which blocks the master
        // write once the child does not read it.
        let data = b"x\n".repeat(1 << 19);
        tokio::spawn(async move {
            port.write(
                &session,
                soksak_sidecar_vt_core::pty::WriteOrigin::Person,
                Arc::from(data),
            )
            .await
        })
    };
    let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel();
    let ticker = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            if tick_tx.send(()).is_err() {
                break;
            }
        }
    });
    let mut ticks = 0usize;
    let _ = timeout(Duration::from_secs(1), async {
        while ticks < 5 {
            if tick_rx.recv().await.is_none() {
                break;
            }
            ticks += 1;
        }
    })
    .await;
    ticker.abort();
    port.close(&session)
        .await
        .expect("closing the session must succeed");
    let written = writer
        .await
        .expect("write task must finish")
        .expect("the session queues the write")
        .await;
    assert!(
        !matches!(written, Ok(Ok(()))),
        "a 1 MiB write to a child that never reads must fail when the child ends: {written:?}"
    );
    assert!(
        ticks >= 5,
        "the runtime advanced {ticks} ticks of 50 ms within 1 s while a 1 MiB write waited for its child, expected at least 5"
    );
}

/// The close of a session ends its child first, so a write that waits for a child that never reads
/// ends with the child and does not hold the close. The child sleeps 30 s, far longer than the
/// close may take.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_ends_a_write_that_waits_for_the_child() {
    let _test_lock = lifecycle_test_lock().await;
    let _native_test_lock = native_pty_test_lock();
    let dir = tempfile::tempdir().expect("temporary directory must be created");
    let script = dir.path().join("sleeper");
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").expect("script must be written");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("script must be executable");
    let port = Arc::new(LocalSessionPort::new_with_owner(
        Arc::new(PtyService::new(events_test::TestEvents::new().handle())),
        String::new(),
    ));
    let shell = script.to_str().expect("script path is UTF-8").to_string();
    let session = port
        .open(
            &ShellRequest {
                shell,
                directory: None,
            },
            80,
            24,
        )
        .await
        .expect("PTY setup failed; this is an environmental test failure");
    let writer = {
        let port = Arc::clone(&port);
        let session = session.clone();
        let data = b"x\n".repeat(1 << 19);
        tokio::spawn(async move {
            port.write(
                &session,
                soksak_sidecar_vt_core::pty::WriteOrigin::Person,
                Arc::from(data),
            )
            .await
        })
    };
    // The write fills the canonical queue of the line discipline within this time and then waits
    // for the child.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    let closed = timeout(Duration::from_secs(40), port.close(&session))
        .await
        .expect("the close did not end within 40 s");
    let elapsed = started.elapsed();
    closed.expect("closing the session must succeed");
    let written = writer
        .await
        .expect("write task must finish")
        .expect("the session queues the write")
        .await;
    assert!(
        !matches!(written, Ok(Ok(()))),
        "a write to a child that the close ended must fail: {written:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the close of a session with a waiting write took {} ms; the child sleeps 30 s",
        elapsed.as_millis()
    );
}
