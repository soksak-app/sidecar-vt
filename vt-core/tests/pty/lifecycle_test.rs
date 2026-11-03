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
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "trap '' HUP; echo ready; sleep 30".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let mut seen = Vec::new();
    while !seen.windows(5).any(|part| part == b"ready") {
        seen.extend(next_output(&mut rx).await);
    }
    service.close(&session_id).unwrap();
    let kills = named(&events, "sidecar.vt.kill");
    let group = kills
        .iter()
        .find(|kill| kill["fields"]["target"] == "process group")
        // silent: a test; the closure panics with the stored events, which fails the test.
        .unwrap_or_else(|| panic!("the kill of the group is not recorded: {kills:?}"));
    assert_eq!(group["session"], session_id);
    assert_eq!(group["fields"]["signal"], "SIGKILL");
    assert_eq!(group["fields"]["result"], "sent", "{kills:?}");
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

/// Writers that race for the last room of the queue accept no more than the bound holds. The program
/// does not read, so the master takes about 1 KiB and the queue holds the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_input_of_a_person_never_passes_the_bound() {
    const CHUNK: usize = 256 * 1024;
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    for round in 0..20 {
        let service = std::sync::Arc::new(
            PtyService::new(crate::events::test_support::TestEvents::new().handle())
                .with_input_bound(1024 * 1024),
        );
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
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let chunk: std::sync::Arc<[u8]> = lines(CHUNK).into();
        let writers = (0..8)
            .map(|_| {
                let service = service.clone();
                let session_id = session_id.clone();
                let barrier = barrier.clone();
                let chunk = chunk.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut accepted = Vec::new();
                    while let Ok(completion) =
                        service.write(&session_id, WriteOrigin::Person, chunk.clone())
                    {
                        accepted.push(completion);
                    }
                    accepted
                })
            })
            .collect::<Vec<_>>();
        let accepted = writers
            .into_iter()
            .flat_map(|writer| writer.join().unwrap())
            .count();
        service.close(&session_id).unwrap();
        assert!(
            accepted <= 4,
            "round {round}: {accepted} chunks of {CHUNK} bytes passed a bound of 1 MiB"
        );
    }
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
    assert_eq!(reply.await.unwrap(), Err("session closed".to_string()));
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

/// Reads the events of a session until its end and returns the bytes of its output.
async fn output_until_exit(rx: &mut tokio::sync::mpsc::Receiver<DaemonEvent>) -> usize {
    let mut read = 0;
    loop {
        match next_event(rx).await {
            DaemonEvent::Output { data, .. } => read += data.len(),
            DaemonEvent::Exit { .. } => return read,
            event => panic!("unexpected event before the end of the session: {event:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_chunk_of_a_child_that_ended_precedes_its_exit() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "head -c 4194304 /dev/zero; exit 0".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    // Nothing consumes the output for a while, so the child ends with the reads stopped.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(output_until_exit(&mut rx).await, 4_194_304);
    service.close(&session_id).unwrap();
}

// A job that left the session of the child holds the slave after the child ended. Its next write
// fails, so nothing keeps writing to a terminal that the service no longer reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_end_of_the_child_fails_the_writes_of_a_job_of_another_session() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let folder = tempfile::tempdir().unwrap();
    let marker = folder.path().join("hangup");
    nix::unistd::mkfifo(&marker, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let (hung_up, mut hangup) = tokio::sync::oneshot::channel();
    let reader = marker.clone();
    // Opening a named pipe blocks until the job opens it, and the thread ends with the test process.
    std::thread::spawn(move || {
        let _ = hung_up.send(std::fs::read_to_string(reader));
    });
    let ready = folder.path().join("ready");
    nix::unistd::mkfifo(&ready, nix::sys::stat::Mode::S_IRWXU).unwrap();
    // The job leaves the session of the child, tells the child that it did, and writes to the slave
    // until a write fails. The child ends after that message, so the job cannot end with it.
    let job = format!(
        "use POSIX; POSIX::setsid(); $SIG{{HUP}}=q(IGNORE); $|=1; \
         open(R, q(>), q({ready})); print R qq(ready\\n); close R; \
         select(undef,undef,undef,0.05) while print STDOUT q(x); \
         open(M, q(>), q({marker})); print M q(ended); close M",
        ready = ready.display(),
        marker = marker.display(),
    );
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &[
                "-c".into(),
                format!(
                    "/usr/bin/perl -e '{job}' & read line < '{}'",
                    ready.display()
                ),
            ],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    output_until_exit(&mut rx).await;
    let ended = tokio::time::timeout(Duration::from_secs(10), &mut hangup).await;
    assert_eq!(
        ended
            .expect("the job still writes to the slave")
            .unwrap()
            .unwrap(),
        "ended"
    );
    service.close(&session_id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_releases_the_reserved_permit_of_the_output_channel() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let probe = tx.clone();
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
    service.close(&session_id).unwrap();
    output_until_exit(&mut rx).await;
    assert_eq!(probe.capacity(), probe.max_capacity());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn last_output_goes_to_a_free_slot_and_a_close_records_what_it_does_not_send() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (session_tx, mut session_rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "sleep 60".into()],
            None,
            80,
            24,
            session_tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let session = service.session(&session_id).unwrap();
    // The I/O task waits for the close signal until it ends, so the signal below reaches this call.
    service.close(&session_id).unwrap();
    output_until_exit(&mut session_rx).await;
    let chunk = || DaemonEvent::Output {
        session_id: session_id.clone(),
        data: b"last".to_vec(),
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    crate::pty::send_last_output(&session, &tx, chunk(), 4).await;
    assert!(matches!(
        next_event(&mut rx).await,
        DaemonEvent::Output { .. }
    ));
    assert!(named(&events, "sidecar.vt.buffer_dropped").is_empty());

    tx.send(chunk()).await.unwrap();
    session.close.notify_one();
    crate::pty::send_last_output(&session, &tx, chunk(), 4).await;
    let dropped = named(&events, "sidecar.vt.buffer_dropped");
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert_eq!(dropped[0]["fields"]["bytes"], 4);
}

// The reaper reports the end while close runs, and the report and the close can both be ready.
// A close that was requested decides the reason whichever of them the task takes first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_requested_when_the_child_is_reaped_decides_and_exit_follows_once() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
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
    let session = service.session(&session_id).unwrap();
    *session.recover(&session.closed, "closed") = true;
    let pid = session
        .recover(&session.child, "child")
        .child
        .process_id()
        .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    output_until_exit(&mut rx).await;
    assert_eq!(queued.await.unwrap(), Err("session closed".to_string()));
    assert!(rx.recv().await.is_none(), "an event followed the Exit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_close_returns_ok_while_the_first_runs_and_names_a_removed_session_after_it() {
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
    let session = service.session(&session_id).unwrap();
    *session.recover(&session.closed, "closed") = true;
    assert_eq!(service.close(&session_id), Ok(()));
    *session.recover(&session.closed, "closed") = false;
    service.close(&session_id).unwrap();
    assert_eq!(
        service.close(&session_id),
        Err(format!("session {session_id} not found"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_writes_nothing_to_the_program() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let folder = tempfile::tempdir().unwrap();
    let received = folder.path().join("received");
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let script = format!(
        "stty -echo; echo ready; while read line; do echo \"$line\" >> '{}'; echo stored; done",
        received.display()
    );
    let session_id = service
        .open("/bin/sh", &["-c".into(), script], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let mut seen = Vec::new();
    while !seen.windows(5).any(|part| part == b"ready") {
        seen.extend(next_output(&mut rx).await);
    }
    service
        .write(&session_id, WriteOrigin::Person, &b"typed\n"[..])
        .unwrap();
    while !seen.windows(6).any(|part| part == b"stored") {
        seen.extend(next_output(&mut rx).await);
    }
    service.close(&session_id).unwrap();
    assert_eq!(std::fs::read_to_string(&received).unwrap(), "typed\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_end_of_the_child_fails_queued_input_and_closes_the_master() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "sleep 1".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let queued = service
        .write(&session_id, WriteOrigin::Person, lines(1024 * 1024))
        .unwrap();
    output_until_exit(&mut rx).await;
    assert_eq!(queued.await.unwrap(), Err("child exited".to_string()));
    assert_eq!(
        service
            .write(&session_id, WriteOrigin::Person, &b"x"[..])
            .map(|_| ())
            .unwrap_err(),
        "session writer is closed"
    );
    assert_eq!(
        service.resize(&session_id, 100, 30).unwrap_err(),
        "session master is closed"
    );
    assert_eq!(
        service.pty_measurement(&session_id).unwrap_err(),
        "session master is closed"
    );
    assert_eq!(service.session_count(), 1, "child exit removed the entry");
    assert!(
        named(&events, "sidecar.vt.kill").is_empty(),
        "child exit killed"
    );
    service.close(&session_id).unwrap();
    assert_eq!(service.session_count(), 0);
    assert!(!named(&events, "sidecar.vt.kill").is_empty());
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

/// A process that a test started in a session. It ends with the test when the behavior under test
/// left it running.
struct Job(nix::unistd::Pid);

impl Job {
    fn alive(&self) -> bool {
        nix::sys::signal::kill(self.0, None).is_ok()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // default: a job that the test ended has no process left to signal.
        let _ = nix::sys::signal::kill(self.0, nix::sys::signal::Signal::SIGKILL);
    }
}

/// Starts an interactive shell without its startup files and a job with the operator `background`,
/// and returns the session and the process of the job when the job reports after its exec. A signal
/// that reaches a job of zsh before its exec can be lost with its signal queue. The report is
/// computed by the job, so the echo of the command does not contain it.
async fn shell_with_job(
    service: &PtyService,
    rx: &mut tokio::sync::mpsc::Receiver<DaemonEvent>,
    tx: tokio::sync::mpsc::Sender<DaemonEvent>,
    background: &str,
) -> (String, Job) {
    let session_id = service
        .open("/bin/zsh", &["-f".into(), "-i".into()], None, 80, 24, tx)
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    service
        .write(
            &session_id,
            WriteOrigin::Person,
            format!("/bin/sh -c 'echo job-$((1+1)); exec sleep 1000' {background} echo job=$!\n")
                .into_bytes(),
        )
        .unwrap();
    let mut seen = String::new();
    while !seen.contains("job-2") || !seen.contains("job=") {
        seen.push_str(&String::from_utf8_lossy(&next_output(rx).await));
    }
    let pid = seen
        .split("job=")
        .filter_map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<i32>().ok()
        })
        .last()
        .expect("the shell reported no job");
    (session_id, Job(nix::unistd::Pid::from_raw(pid)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_session_ends_the_jobs_of_its_shell() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let (session_id, job) = shell_with_job(&service, &mut rx, tx, "&").await;
    service.close(&session_id).unwrap();
    // The end of a process of another parent produces no event for this test.
    timeout(Duration::from_secs(10), async {
        while job.alive() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let state = std::process::Command::new("/bin/ps")
            .args(["-o", "pid,ppid,pgid,stat,command", "-p", &job.0.to_string()])
            .output()
            .unwrap();
        panic!(
            "the job of the shell outlived the close of its session: {}",
            String::from_utf8_lossy(&state.stdout)
        )
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_session_leaves_a_disowned_job_running() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(crate::events::test_support::TestEvents::new().handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let (session_id, job) = shell_with_job(&service, &mut rx, tx, "&!").await;
    service.close(&session_id).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(job.alive(), "the close ended a disowned job");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_program_that_ignores_the_hangup_of_a_close_ends_by_the_kill() {
    let _test_lock = crate::platform::pty::native_pty_test_lock();
    let events = crate::events::test_support::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (tx, mut rx) = crate::pty::output_channel();
    let session_id = service
        .open(
            "/bin/sh",
            &["-c".into(), "trap '' HUP; echo ready; sleep 30".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("PTY setup failed; environmental PTY errors must fail this test");
    let mut seen = Vec::new();
    while !seen.windows(5).any(|part| part == b"ready") {
        seen.extend(next_output(&mut rx).await);
    }
    service.close(&session_id).unwrap();
    output_until_exit(&mut rx).await;
    let signals: Vec<_> = named(&events, "sidecar.vt.kill")
        .iter()
        .map(|line| {
            (
                line["fields"]["target"].as_str().unwrap().to_string(),
                line["fields"]["signal"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(signals[0], ("child".to_string(), "SIGHUP".to_string()));
    assert!(
        signals.contains(&("process group".to_string(), "SIGKILL".to_string())),
        "{signals:?}"
    );
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
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg(seconds.to_string()).process_group(0);
    // The child ignores the hangup of close, so these tests observe the SIGKILL that follows it.
    // SAFETY: signal is async-signal-safe, and the closure runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    let inner = command.spawn().expect("spawn the session child");
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
