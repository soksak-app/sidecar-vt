use super::*;

fn poison(queue: &Arc<EventQueue>) {
    let poisoned = queue.clone();
    let outcome = std::panic::catch_unwind(move || {
        let _guard = poisoned.events.lock().expect("lock before poisoning");
        panic!("fixture engine event queue poison");
    });
    assert!(outcome.is_err(), "the fixture must poison the event queue");
    assert!(queue.events.is_poisoned());
}

#[test]
fn poisoned_queue_draining_preserves_existing_events() {
    let mut engine = AlacrittyEngine::new();
    engine
        .events
        .events
        .lock()
        .expect("seed queue")
        .push_back(QueuedEvent::Neutral(EngineEvent::Title(
            "before".to_string(),
        )));
    poison(&engine.events);
    assert!(matches!(engine.drain_events().as_slice(),
        [EngineEvent::Title(title), EngineEvent::RuntimeError(reason)]
            if title == "before" && reason.contains("event queue lock was poisoned")));
    assert!(engine.drain_events().is_empty());
}

#[test]
fn poisoned_queue_accepts_alacritty_callbacks_in_order() {
    let mut engine = AlacrittyEngine::new();
    poison(&engine.events);
    let sink = EventSink(engine.events.clone());
    sink.send_event(Event::Title("first".to_string()));
    sink.send_event(Event::Title("second".to_string()));
    assert!(matches!(engine.drain_events().as_slice(),
        [EngineEvent::RuntimeError(reason), EngineEvent::Title(first), EngineEvent::Title(second)]
            if reason.contains("event queue lock was poisoned") && first == "first" && second == "second"));
}

#[test]
fn poisoned_queue_accepts_neutral_errors() {
    let mut engine = AlacrittyEngine::new();
    poison(&engine.events);
    engine.feed(b"\x1b[#p");
    assert!(matches!(engine.drain_events().as_slice(),
        [EngineEvent::RuntimeError(poison), EngineEvent::Error(reason)]
            if poison.contains("event queue lock was poisoned") && reason == "unsupported CSI palette report #p"));
}
