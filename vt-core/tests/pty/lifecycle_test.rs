use super::*;
use tokio::time::{Duration, timeout};

/// The deadline ends a stalled check; the expected event determines success.
const STALL: Duration = Duration::from_secs(60);

/// Reads the next daemon event and rejects a closed channel or an expired deadline.
async fn next_event(rx: &mut tokio::sync::mpsc::Receiver<DaemonEvent>) -> DaemonEvent {
    timeout(STALL, rx.recv())
        .await
        .expect("no daemon event arrived; the PTY test stalled")
        .expect("the daemon event channel closed before the expected event")
}

/// Reads the next output data.
async fn next_output(rx: &mut tokio::sync::mpsc::Receiver<DaemonEvent>) -> Vec<u8> {
    loop {
        if let DaemonEvent::Output { data, .. } = next_event(rx).await {
            return data;
        }
    }
}

/// The events of `name` in the file of `events`.
fn named(events: &crate::events::test_support::TestEvents, name: &str) -> Vec<serde_json::Value> {
    events
        .lines()
        .into_iter()
        .filter(|line| line["event"] == name)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_running_session_records_the_kill_of_its_process_group() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, _rx) = crate::pty::output_channel();
    let session_id = service
        .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    service.close(&session_id).unwrap();
    let kills = named(&events, "sidecar.vt.kill");
    let group = kills
        .iter()
        .find(|kill| kill["fields"]["target"] == "process group")
        // silent: a test; the closure panics with the stored events, which fails the test.
        .unwrap_or_else(|| panic!("the kill of the group is not recorded: {kills:?}"));
    assert_eq!(group["session"], session_id);
    assert_eq!(group["fields"]["signal"], "SIGKILL");
    assert_eq!(group["fields"]["result"], "sent");
    assert!(group["fields"]["id"].as_i64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_of_a_group_that_ended_is_recorded_as_esrch() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open("/bin/sh", &["-c".into(), "true".into()], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    // The exit event follows the reaping of the child, so the group no longer exists.
    while !matches!(next_event(&mut rx).await, DaemonEvent::Exit { .. }) {}
    service.close(&session_id).unwrap();
    let kills = named(&events, "sidecar.vt.kill");
    let group = kills
        .iter()
        .find(|kill| kill["fields"]["target"] == "process group")
        // silent: a test; the closure panics with the stored events, which fails the test.
        .unwrap_or_else(|| panic!("the kill of the group is not recorded: {kills:?}"));
    assert_eq!(group["fields"]["result"], "ESRCH");
}

/// Output that the engine has not consumed which one session holds: 16 chunks of 64 KiB.
const OUTPUT_BOUND: u64 = 16 * 64 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_that_nothing_consumes_stops_the_reads_at_the_bound() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, _rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "head -c 16000000 /dev/zero; sleep 60".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    // No event states that reads stopped, so the count is read until it stops changing or passes
    // the bound. The bound decides the result.
    let mut last = 0;
    let mut same = 0;
    while same < 50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let written = service.pty_measurement(&session_id).unwrap().written;
        assert!(
            written <= OUTPUT_BOUND,
            "the reader read {written} bytes while nothing consumed the output"
        );
        same = if written == last && written > 0 {
            same + 1
        } else {
            0
        };
        last = written;
    }
    service.close(&session_id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_returns_while_the_program_does_not_read() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, _rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "sleep 60".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let writing = service.clone();
    let id = session_id.clone();
    let write = tokio::task::spawn_blocking(move || {
        writing.write(&id, WriteOrigin::Person, b"line\n".repeat(800_000))
    });
    let waited = timeout(Duration::from_secs(10), write).await;
    let closed = service.close(&session_id);
    waited
        .expect("the write of 4 MiB waited for a program that does not read")
        .unwrap()
        .unwrap();
    closed.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writes_to_a_program_that_echoes_end_in_order_while_the_output_is_read() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let first = service
        .write(
            &session_id,
            crate::pty::WriteOrigin::Person,
            b"first\n".repeat(400_000),
        )
        .unwrap();
    let second = service
        .write(
            &session_id,
            crate::pty::WriteOrigin::Person,
            &b"second\n"[..],
        )
        .unwrap();
    let consumer = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    service.close(&session_id).unwrap();
    consumer.await.unwrap();
}

/// Queued lines that a program which does not read holds in the queue.
fn lines(bytes: usize) -> Vec<u8> {
    b"line\n".repeat(bytes / 5)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn input_of_a_person_over_the_bound_is_refused_with_an_explicit_error() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle())
        .with_input_bound(1024 * 1024);
    let (tx, _rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "sleep 60".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    service
        .write(&session_id, WriteOrigin::Person, lines(1024 * 1024))
        .expect("input within the bound is queued");
    let refused = service.write(&session_id, WriteOrigin::Person, lines(4096));
    let closed = service.close(&session_id);
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("input queue")),
        "input over the bound was not refused: {:?}",
        refused.map(|_| ())
    );
    closed.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_of_the_engine_over_the_bound_stops_reads_and_reports_it() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle())
        .with_input_bound(1024 * 1024);
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &[
                "-c".into(),
                "stty raw -echo; printf READY; sleep 1; cat > /dev/null".into(),
            ],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let mut seen = Vec::new();
    while !seen.ends_with(b"READY") {
        seen.extend(next_output(&mut rx).await);
    }
    let reply = service
        .write(&session_id, WriteOrigin::Engine, lines(2 * 1024 * 1024))
        .expect("a reply of the engine is never refused");
    let mut held = Vec::new();
    while held.len() < 2 {
        match next_event(&mut rx).await {
            DaemonEvent::InputHeld {
                queued,
                held: state,
                ..
            } => held.push((state, queued)),
            DaemonEvent::Output { .. } => {}
            event => panic!("the session ended before it reported the held input: {event:?}"),
        }
    }
    assert!(held[0].0 && held[0].1 > 1024 * 1024, "{held:?}");
    assert!(!held[1].0 && held[1].1 <= 1024 * 1024, "{held:?}");
    reply.await.unwrap().unwrap();
    service.close(&session_id).unwrap();
}

/// The echo of the queue is output that nothing reads while reads are held, so neither the program
/// nor the queue moves. Memory stays bounded, the event shows the state and close ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_ends_a_session_whose_held_reads_stop_the_echo_of_its_queue() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle())
        .with_input_bound(1024 * 1024);
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let reply = service
        .write(&session_id, WriteOrigin::Engine, lines(2 * 1024 * 1024))
        .expect("a reply of the engine is never refused");
    loop {
        match next_event(&mut rx).await {
            DaemonEvent::InputHeld { held: true, .. } => break,
            DaemonEvent::Output { .. } => {}
            event => panic!("the session ended before it held its reads: {event:?}"),
        }
    }
    service.close(&session_id).unwrap();
    let consumer = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    assert!(
        !matches!(reply.await, Ok(Ok(()))),
        "the queued reply reported success after close"
    );
    consumer.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_reaches_a_program_only_after_the_input_before_it() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, _rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "sleep 60".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let queued = service
        .write(&session_id, WriteOrigin::Person, lines(1024 * 1024))
        .unwrap();
    let mut interrupt = service
        .write(&session_id, WriteOrigin::Person, &b"\x03"[..])
        .unwrap();
    // The first write waits for a program that does not read, so the interrupt behind it waits.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let waiting = matches!(
        interrupt.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    );
    service.close(&session_id).unwrap();
    assert!(waiting, "the interrupt passed the input before it");
    drop(queued);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_ends_when_a_program_reads_it_without_any_output() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &[
                "-c".into(),
                "stty raw -echo; printf READY; cat > /dev/null".into(),
            ],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let mut seen = Vec::new();
    while !seen.ends_with(b"READY") {
        seen.extend(next_output(&mut rx).await);
    }
    let write = service
        .write(&session_id, WriteOrigin::Person, lines(2 * 1024 * 1024))
        .unwrap();
    let finished = timeout(Duration::from_secs(20), write).await;
    let written = service.pty_measurement(&session_id).unwrap();
    service.close(&session_id).unwrap();
    finished
        .unwrap_or_else(|_| panic!("the write did not end: {written:?}"))
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_that_the_consumer_reads_is_not_dropped() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "head -c 100000000 /dev/zero".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let mut read = 0;
    loop {
        match next_event(&mut rx).await {
            DaemonEvent::Output { data, .. } => read += data.len(),
            DaemonEvent::Exit { .. } => break,
            DaemonEvent::Error { error, .. } => panic!("the session reported {error}"),
            DaemonEvent::InputHeld { .. } => panic!("no input was queued"),
        }
    }
    assert_eq!(read, 100_000_000);
    assert_eq!(
        named(&events, "sidecar.vt.buffer_dropped"),
        Vec::<serde_json::Value>::new()
    );
    service.close(&session_id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_after_the_consumer_ended_is_counted_as_a_dropped_buffer() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, rx) = crate::pty::output_channel();
    let session_id = service
        .open("/bin/sh", &["-c".into(), "cat".into()], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    drop(rx);
    service
        .write(&session_id, crate::pty::WriteOrigin::Person, &b"gone\n"[..])
        .unwrap();
    // The reader thread stores the event when it sends the echo to the receiver that is gone. No
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
    assert_eq!(drop["fields"]["buffer"], "session events");
    assert_eq!(drop["fields"]["count"], 1);
    assert!(drop["fields"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        drop["fields"]["reason"],
        "the surface that consumes the session ended"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shell_without_integration_is_recorded_when_its_session_starts() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, _rx) = crate::pty::output_channel();
    let session_id = service
        .open_shell("owner", "/bin/sh", None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    service.close(&session_id).unwrap();
    let records = named(&events, "sidecar.vt.record.info");
    let record = records
        .iter()
        .find(|record| record["fields"]["where"] == "shell integration")
        // silent: a test; the closure panics with the stored events, which fails the test.
        .unwrap_or_else(|| panic!("the shell without integration is not recorded: {records:?}"));
    assert_eq!(
        record["fields"]["text"],
        "sh has no shell integration; the session starts without it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_sessions_have_independent_output() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx_a, mut rx_a) = crate::pty::output_channel();
    let (tx_b, mut rx_b) = crate::pty::output_channel();
    let session_a = service
        .open(
            "/bin/sh",
            &["-c".into(), "printf A".into()],
            None,
            80,
            24,
            tx_a,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let session_b = service
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_close_kills_the_child_and_drains_exit() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
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
    // The exit event remains available after close returns.
    while !matches!(next_event(&mut rx).await, DaemonEvent::Exit { .. }) {}
}

/// A child reports its reaper first access through started and stores its reaped exit status in status.
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

/// Registers a child that exits after seconds and starts its reaper. Returns after the reaper accesses the child.
/// The returned status records the reaped exit; without with_group, the session has no process group to signal.
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
        input: Mutex::new(None),
        queued_input: AtomicUsize::new(0),
        input_bound: QUEUED_INPUT_BYTES,
        child: Mutex::new(SessionChild {
            child: Box::new(ObservedChild {
                inner,
                started: Mutex::new(Some(started_sender)),
                status: Arc::clone(&status),
            }),
            reaped: None,
        }),
        child_reaped: Condvar::new(),
        written_output: Mutex::new(0),
        closed: Mutex::new(false),
        close: Notify::new(),
        reaper: Mutex::new(None),
        log: crate::events::test_support::TestEvents::new().handle(),
    });
    service
        .sessions
        .lock()
        .unwrap()
        .insert(session.id.clone(), Arc::clone(&session));
    let session_for_reaper = Arc::clone(&session);
    *session.reaper.lock().unwrap() = Some(
        crate::thread::start(&session.log, "pty-reaper", move || {
            reap_child(session_for_reaper, oneshot::channel().0)
        })
        .expect("start the reaper thread"),
    );
    started.recv().expect("the reaper thread did not start");
    status
}

// Close returns after the reaper reaps the child.
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

// Without a process group, close signals the child by PID and returns after reaping.
// The child exits independently after 20 seconds, exposing a close blocked by the reaper child lock.
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
