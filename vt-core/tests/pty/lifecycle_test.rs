use super::*;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Duration, timeout};

/// The deadline ends a stalled check; the expected event determines success.
const STALL: Duration = Duration::from_secs(60);

/// Reads the next daemon event and rejects a closed channel or an expired deadline.
async fn next_event(rx: &mut UnboundedReceiver<DaemonEvent>) -> DaemonEvent {
    timeout(STALL, rx.recv())
        .await
        .expect("no daemon event arrived; the PTY test stalled")
        .expect("the daemon event channel closed before the expected event")
}

/// Reads the next output data.
async fn next_output(rx: &mut UnboundedReceiver<DaemonEvent>) -> Vec<u8> {
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
        // silent: a test; the closure panics with the stored events, which fails the test.
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
        // silent: a test; the closure panics with the stored events, which fails the test.
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
            .write(&session_id, format!("{word}\n").as_bytes())
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
    service.write(&session_id, b"gone\n").unwrap();
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
async fn a_shell_without_integration_is_recorded_when_its_session_starts() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (session_id, _) = service
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
    *session.reaper.lock().unwrap() = Some(
        crate::thread::start(&session.log, "pty-reaper", move || {
            reap_child(session_for_reaper)
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
