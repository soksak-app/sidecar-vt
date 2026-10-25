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
    let (id, _) = service
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

async fn retained(rx: &mut tokio::sync::mpsc::UnboundedReceiver<DaemonEvent>, id: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        let mut next = 0;
        while bytes.len() < b"retained".len() {
            match rx.recv().await.expect("retained output channel ended") {
                DaemonEvent::Output {
                    session_id,
                    data,
                    sequence,
                    truncated,
                } => {
                    assert_eq!(session_id, id);
                    assert_eq!(sequence, next);
                    assert!(!truncated);
                    bytes.extend(data);
                    next += 1;
                }
                event => panic!("unexpected replay event: {event:?}"),
            }
        }
        assert_eq!(bytes, b"retained");
    })
    .await
    .expect("retained output did not arrive");
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

#[tokio::test]
async fn poisoned_output_replays_the_retained_bytes() {
    let events = TestEvents::new();
    let (_lock, service, session, _rx) = completed(&events).await;
    poison(&session.output);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let attachment = service
        .attach(&session.id, 0, tx)
        .expect("attach after poisoning");
    retained(&mut rx, &session.id).await;
    service
        .detach(&session.id, &attachment)
        .expect("detach replay consumer");
    reported_once(&events, &session, "output");
}

#[tokio::test]
async fn poisoned_attachments_allow_detach_and_reattach() {
    let events = TestEvents::new();
    let (_lock, service, session, _rx) = completed(&events).await;
    let (tx, _new_rx) = tokio::sync::mpsc::unbounded_channel();
    let attachment = service
        .attach(&session.id, 0, tx)
        .expect("initial attachment");
    poison(&session.attachments);
    service
        .detach(&session.id, &attachment)
        .expect("detach after poisoning");
    service
        .detach(&session.id, &attachment)
        .expect("repeat detach");
    assert!(
        !session
            .attachments
            .lock()
            .expect("recovered attachments")
            .contains_key(&attachment)
    );
    let (tx, mut replay) = tokio::sync::mpsc::unbounded_channel();
    let next = service
        .attach(&session.id, 0, tx)
        .expect("reattach after poisoning");
    assert_ne!(next, attachment);
    retained(&mut replay, &session.id).await;
    service
        .detach(&session.id, &next)
        .expect("detach replacement attachment");
    reported_once(&events, &session, "attachments");
}

#[tokio::test]
async fn poisoned_attachments_preserve_existing_broadcast_receivers() {
    let events = TestEvents::new();
    let (_lock, _service, session, mut rx) = completed(&events).await;
    poison(&session.attachments);
    broadcast(
        &session,
        DaemonEvent::Output {
            session_id: session.id.clone(),
            data: b"after".to_vec(),
            sequence: 1,
            truncated: false,
        },
    );
    assert!(
        matches!(rx.recv().await.expect("broadcast after poisoning"),
        DaemonEvent::Output { data, sequence: 1, .. } if data == b"after")
    );
    reported_once(&events, &session, "attachments");
}
