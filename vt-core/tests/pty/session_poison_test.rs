use super::output_poison_test::{completed, poison, reported_once};
use super::*;
use crate::events::test_support::TestEvents;

#[tokio::test]
async fn poisoned_closed_writer_stays_closed() {
    let events = TestEvents::new();
    let (_lock, service, session, _rx) = completed(&events).await;
    session.writer.lock().expect("close fixture writer").take();
    poison(&session.writer);
    assert_eq!(
        service
            .write(&session.id, b"x")
            .expect_err("reject closed writer"),
        "session writer is closed"
    );
    reported_once(&events, &session, "writer");
}

#[tokio::test]
async fn poisoned_closed_master_stays_closed() {
    let events = TestEvents::new();
    let (_lock, service, session, _rx) = completed(&events).await;
    session.master.lock().expect("close fixture master").take();
    poison(&session.master);
    assert_eq!(
        service
            .resize(&session.id, 90, 30)
            .expect_err("reject closed master"),
        "session master is closed"
    );
    assert_eq!(
        service
            .pty_measurement(&session.id)
            .expect_err("reject closed master measurement"),
        "session master is closed"
    );
    reported_once(&events, &session, "master");
}

#[tokio::test]
async fn poisoned_closed_flag_rejects_input_and_measurement() {
    let events = TestEvents::new();
    let (_lock, service, session, _rx) = completed(&events).await;
    *session.closed.lock().expect("close fixture session") = true;
    poison(&session.closed);
    assert_eq!(
        service
            .write(&session.id, b"x")
            .expect_err("reject closed session"),
        "session is closed"
    );
    assert_eq!(
        service
            .pty_measurement(&session.id)
            .expect_err("reject closed session measurement"),
        "session is closed"
    );
    reported_once(&events, &session, "closed");
}

macro_rules! close_field {
    ($test:ident, $field:ident) => {
        #[tokio::test]
        async fn $test() {
            let events = TestEvents::new();
            let (_lock, service, session, _rx) = completed(&events).await;
            poison(&session.$field);
            service
                .close(&session.id)
                .expect("close poisoned completed session");
            assert_eq!(service.session_count(), 0);
            assert!(session.reader.lock().expect("reader handle").is_none());
            assert!(session.reaper.lock().expect("reaper handle").is_none());
            assert!(
                session
                    .child
                    .lock()
                    .expect("child result")
                    .reaped
                    .as_ref()
                    .is_some_and(Result::is_ok)
            );
            reported_once(&events, &session, stringify!($field));
        }
    };
}
close_field!(poisoned_reader_handle_is_taken_by_close, reader);
close_field!(poisoned_reaper_handle_is_taken_by_close, reaper);
close_field!(poisoned_child_result_is_retained_by_close, child);

async fn counter(field: &str) {
    let events = TestEvents::new();
    let (_lock, _service, session, mut rx) = completed(&events).await;
    let sequence = *session.next_sequence.lock().expect("initial sequence");
    let written = *session.written_output.lock().expect("initial output count");
    match field {
        "next_sequence" => poison(&session.next_sequence),
        "written_output" => poison(&session.written_output),
        other => panic!("unknown fixture counter {other}"),
    }
    read_output(
        session.clone(),
        Box::new(std::io::Cursor::new(b"after".to_vec())),
    );
    assert!(matches!(rx.recv().await.expect("continued output"),
        DaemonEvent::Output { data, sequence: next, .. } if data == b"after" && next == sequence));
    assert_eq!(
        *session.next_sequence.lock().expect("continued sequence"),
        sequence + 1
    );
    assert_eq!(
        *session.written_output.lock().expect("continued byte count"),
        written + 5
    );
    reported_once(&events, &session, field);
}

#[tokio::test]
async fn poisoned_sequence_continues_from_its_retained_value() {
    counter("next_sequence").await;
}
#[tokio::test]
async fn poisoned_written_count_continues_from_its_retained_value() {
    counter("written_output").await;
}

#[tokio::test]
async fn poisoned_condition_wake_retains_the_recorded_child_result() {
    let events = TestEvents::new();
    let (_lock, _service, session, _rx) = completed(&events).await;
    let mut child = session.child.lock().expect("child before wait");
    let recorded = child.reaped.take().expect("completed child result");
    let expected = recorded.clone();
    let reaper = session.clone();
    let worker = std::thread::spawn(move || {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut child = reaper.child.lock().expect("child in controlled reaper");
                child.reaped = Some(recorded);
                panic!("fixture reaper panic after recording exit");
            }))
            .is_err()
        );
        reaper.child_reaped.notify_all();
    });
    let child = session.wait_reaped(child);
    assert_eq!(child.reaped.as_ref(), Some(&expected));
    drop(child);
    worker.join().expect("controlled reaper finished");
    reported_once(&events, &session, "child");
}

#[tokio::test]
async fn poisoned_condition_wake_rechecks_completion_before_returning() {
    let events = TestEvents::new();
    let (_lock, _service, session, _rx) = completed(&events).await;
    let mut child = session.child.lock().expect("child before wait");
    let recorded = child.reaped.take().expect("completed child result");
    let expected = recorded.clone();
    let (release, released) = std::sync::mpsc::channel();
    let reaper = session.clone();
    let worker = std::thread::spawn(move || {
        poison(&reaper.child);
        reaper.child_reaped.notify_all();
        // Failure of the waiter drops release, so the fixture thread still terminates.
        if released.recv().is_err() {
            return;
        }
        let mut child = reaper.child.lock().expect("recovered child lock");
        child.reaped = Some(recorded);
        drop(child);
        reaper.child_reaped.notify_all();
    });
    let child = session.wait_reaped_observed(child, move || {
        release
            .send(())
            .expect("release the controlled reaper after recovery");
    });
    assert_eq!(child.reaped.as_ref(), Some(&expected));
    drop(child);
    worker.join().expect("controlled reaper finished");
    reported_once(&events, &session, "child");
}
