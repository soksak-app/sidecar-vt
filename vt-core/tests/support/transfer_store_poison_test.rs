use super::*;
use crate::events::test_support::TestEvents;

fn poison(store: &Arc<TransferStore>) {
    let held = store.clone();
    assert!(
        std::panic::catch_unwind(move || {
            let _guard = held.images.lock().expect("lock before poisoning");
            panic!("fixture transfer store poison");
        })
        .is_err()
    );
    assert!(store.images.is_poisoned());
}

fn transfer() -> RetainedTransfer {
    RetainedTransfer {
        surface: "s1".to_string(),
        name: "view".to_string(),
        generation: 1,
        raster: 1,
        sequence: 1,
        _frame: crate::platform::Frame::new(2, 2).expect("allocate retained frame"),
    }
}

fn answer(raster: u64) -> ImageAnswer<'static> {
    ImageAnswer {
        name: "view",
        generation: 1,
        raster,
        sequence: 1,
    }
}

fn reported_once(events: &TestEvents) {
    let lines = events.lines();
    let errors: Vec<_> = lines
        .iter()
        .filter(|line| {
            line["event"] == "sidecar.vt.record.error"
                && line["fields"]["where"] == "image transfer store"
        })
        .collect();
    assert_eq!(errors.len(), 1, "one recovery record: {lines:?}");
    assert_eq!(
        errors[0]["fields"]["text"],
        "transfer store lock was poisoned; transfer state retained"
    );
}

#[test]
fn a_poisoned_store_retains_frames_until_the_exact_answer() {
    let events = TestEvents::new();
    let store = TransferStore::new(events.handle());
    assert_eq!(store.log.path(), events.handle().path());
    store.retain(transfer());
    poison(&store);
    assert!(!store.answer("s1", answer(2)));
    assert_eq!(store.images().as_ref().expect("active store").len(), 1);
    assert!(store.answer("s1", answer(1)));
    assert!(store.images().as_ref().expect("active store").is_empty());
    reported_once(&events);
}

#[test]
fn a_poisoned_ended_store_is_not_reopened_by_late_operations() {
    let events = TestEvents::new();
    let store = TransferStore::new(events.handle());
    store.end();
    poison(&store);
    store.retain(transfer());
    assert!(!store.answer("s1", answer(1)));
    store.end();
    assert!(store.images().is_none());
    reported_once(&events);
}

#[test]
fn ending_a_poisoned_store_releases_its_retained_list() {
    let events = TestEvents::new();
    let store = TransferStore::new(events.handle());
    store.retain(transfer());
    poison(&store);
    store.end();
    store.end();
    assert!(store.images().is_none());
    reported_once(&events);
}
