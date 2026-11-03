use super::*;
use crate::events::test_support::TestEvents;

pub(super) fn poison<T>(field: &Mutex<T>) {
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = field.lock().expect("lock before poisoning");
            panic!("fixture PTY field poison");
        }))
        .is_err()
    );
    assert!(field.is_poisoned());
}

pub(super) async fn completed(
    events: &TestEvents,
) -> (
    crate::platform::pty::NativePtyTestLock,
    PtyService,
    Arc<Session>,
    tokio::sync::mpsc::UnboundedReceiver<DaemonEvent>,
) {
    let lock = crate::platform::pty::native_pty_test_lock();
    let service = PtyService::new(events.handle());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let id = service
        .open(
            "/bin/sh",
            &["-c".into(), "printf retained".into()],
            None,
            80,
            24,
            tx,
        )
        .expect("open retained-output fixture");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut exited = false;
        let mut bytes = Vec::new();
        while !exited || bytes != b"retained" {
            match rx.recv().await.expect("fixture event channel ended") {
                DaemonEvent::Output { data, .. } => bytes.extend(data),
                DaemonEvent::Exit { .. } => exited = true,
                event => panic!("unexpected fixture event: {event:?}"),
            }
        }
    })
    .await
    .expect("fixture output and exit did not arrive");
    let session = service.session(&id).expect("retained session");
    (lock, service, session, rx)
}

pub(super) fn reported_once(events: &TestEvents, session: &Session, field: &str) {
    let lines = events.lines();
    let errors: Vec<_> = lines
        .iter()
        .filter(|line| {
            line["event"] == "sidecar.vt.record.error"
                && line["fields"]["where"] == format!("pty {field}")
        })
        .collect();
    assert_eq!(errors.len(), 1, "one field recovery: {lines:?}");
    assert_eq!(
        lines.iter().filter(|line| line["level"] == "error").count(),
        1,
        "unexpected fixture error: {lines:?}"
    );
    assert_eq!(errors[0]["session"], session.id);
    assert_eq!(
        errors[0]["fields"]["text"],
        "lock was poisoned; field value retained"
    );
}
