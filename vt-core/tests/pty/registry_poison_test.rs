use super::*;
use crate::events::test_support::TestEvents;

fn poison(service: &PtyService) {
    let sessions = service.sessions.clone();
    assert!(
        std::panic::catch_unwind(move || {
            let _guard = sessions.lock().expect("lock before poisoning");
            panic!("fixture PTY registry poison");
        })
        .is_err()
    );
    assert!(service.sessions.is_poisoned());
}

fn assert_reported_once(events: &TestEvents) {
    let lines = events.lines();
    let errors: Vec<_> = lines
        .iter()
        .filter(|line| {
            line["event"] == "sidecar.vt.record.error" && line["fields"]["where"] == "pty registry"
        })
        .collect();
    assert_eq!(errors.len(), 1, "one recovery record: {lines:?}");
    assert_eq!(
        errors[0]["fields"]["text"],
        "session registry lock was poisoned; sessions retained"
    );
}

#[test]
fn a_poisoned_registry_retains_shared_counts_and_reports_once() {
    let events = TestEvents::new();
    let service = PtyService::new(events.handle());
    let other = service.clone();
    poison(&service);
    assert_eq!(service.session_count(), 0);
    assert_eq!(other.session_count(), 0);
    assert!(Arc::ptr_eq(&service.sessions, &other.sessions));
    assert!(!service.sessions.is_poisoned());
    assert_reported_once(&events);
}

#[test]
fn a_poisoned_registry_preserves_unknown_session_errors() {
    let events = TestEvents::new();
    let service = PtyService::new(events.handle());
    poison(&service);
    assert_eq!(
        service
            .process_id("missing")
            .expect_err("reject missing session"),
        "session missing not found"
    );
    assert_eq!(service.session_count(), 0);
    assert_reported_once(&events);
}

#[test]
fn a_poisoned_registry_allows_owner_cleanup() {
    let events = TestEvents::new();
    let service = PtyService::new(events.handle());
    poison(&service);
    service.close_owner("owner").expect("close empty owner");
    service
        .close_owner("owner")
        .expect("repeat empty owner close");
    assert_reported_once(&events);
}
