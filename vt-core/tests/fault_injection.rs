// The failures that the code reports only through a test-only seam (the fault-injection feature). Each test fails one step
// on purpose and states that failure as the error it causes.
#[path = "support/events_test.rs"]
mod events_test;

use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use soksak_sidecar_vt_core::events::{EventLog, Level, Options};
use soksak_sidecar_vt_core::pty::{PtyService, fault};

/// The PTY tests of this binary run one at a time, because a process group of one test must not be seen by another.
fn native_pty_test_lock() -> std::fs::File {
    let path = std::env::temp_dir().join("soksak-vt-core-pty-tests.lock");
    let file = std::fs::OpenOptions::new()
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

// A child whose wait fails, with no view attached, is recorded: the record is the only report of the failure.
#[test]
fn a_failed_child_wait_without_a_view_is_recorded() {
    let _lock = native_pty_test_lock();
    let events = events_test::TestEvents::new();
    let service = PtyService::new(events.handle());
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    let (session, attachment) = service
        .open("/bin/cat", &[], None, 80, 24, sender)
        .expect("open the session");
    service
        .detach(&session, &attachment)
        .expect("detach the only view");
    fault::fail_wait_of(&session);
    let _ = service.close(&session);
    let lines = events.lines();
    let record = lines
        .iter()
        .find(|line| line["event"] == "sidecar.vt.wait_failed")
        .unwrap_or_else(|| panic!("no record of the failed wait: {lines:?}"));
    assert_eq!(record["level"], "error");
    assert!(
        record["fields"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("fault-injection")),
        "the record does not state the failure: {record}"
    );
}

// A policy watch whose lock is poisoned is still stopped when the log is dropped.
#[test]
fn a_poisoned_policy_watch_lock_is_recovered_and_its_stop_runs() {
    let events = events_test::TestEvents::new();
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    events
        .handle()
        .poison_policy_watch_for_test(move || flag.store(true, Ordering::SeqCst));
    drop(events);
    assert!(
        stopped.load(Ordering::SeqCst),
        "the policy watch was not stopped after its lock was poisoned"
    );
}

// A poisoned writer lock does not stop the drop of the log from joining the writer, and the last event of the log is stored
// when the drop returns.
#[test]
fn a_poisoned_writer_lock_is_recovered_and_the_writer_is_joined() {
    let directory = tempfile::tempdir().expect("create the folder of the events");
    let mut options = Options::service(directory.path().join("logs"), "test".to_string());
    options.batch_interval = std::time::Duration::from_millis(20);
    let log = Arc::new(EventLog::start(options).expect("start the writer"));
    let path = log.path().to_path_buf();
    let joins = log.poison_writer_for_test();
    assert!(log.emit(
        Level::Error,
        "sidecar.vt.test",
        &serde_json::json!({"index": 1})
    ));
    drop(log);
    assert_eq!(
        joins.load(Ordering::SeqCst),
        1,
        "drop did not join the writer"
    );
    let text = std::fs::read_to_string(&path).expect("read the event file");
    assert!(
        text.contains(r#""index":1"#),
        "the last event is not stored: {text}"
    );
}
