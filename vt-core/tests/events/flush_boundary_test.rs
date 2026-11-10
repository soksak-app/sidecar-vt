use super::*;

#[test]
fn every_queued_flush_receives_its_own_acknowledgement() {
    let folder = tempfile::tempdir().expect("create writer folder");
    let path = folder.path().join("writer.jsonl");
    let open = Pending::of_writer(
        Level::Info,
        "log.open",
        to_raw_value(&json!({"schema": SCHEMA})).expect("encode open record"),
    );
    let output = Output::new(
        File::create(&path).expect("create writer file"),
        path.clone(),
        folder.path().join("writer.debug.jsonl"),
        1 << 20,
        2,
        &open,
        PolicyState::start(None),
    );
    let (sender, receiver) = sync_channel(4);
    let (first, first_reply) = sync_channel(1);
    let (second, second_reply) = sync_channel(1);
    // Queue both markers before the writer starts to reproduce the drain interleaving.
    sender
        .send(Message::Flush(first))
        .expect("queue first flush");
    sender
        .send(Message::Flush(second))
        .expect("queue second flush");
    sender.send(Message::Stop).expect("queue stop");
    let writer = std::thread::spawn(move || {
        write_loop(
            output,
            receiver,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Loss::new()),
            Duration::from_secs(1),
            open,
        );
    });
    let first_result = first_reply.recv_timeout(Duration::from_secs(2));
    let second_result = second_reply.recv_timeout(Duration::from_secs(2));
    writer.join().expect("join completed writer");
    first_result.expect("first flush receives its acknowledgement");
    second_result.expect("second flush receives its acknowledgement");
    assert_eq!(
        read_lines(&path).expect("read flushed records")[0]["event"],
        "log.open"
    );
}

// The wait of a flush ends by an event of the writer and by nothing else: its answer after the write, or its end, which
// drops the request unanswered. A writer that ends before it answers is an error of the flush, not a wait without end.
#[test]
fn a_flush_ends_with_the_end_of_a_writer_that_did_not_answer() {
    let (sender, receiver) = sync_channel(1);
    let writer = std::thread::spawn(move || match receiver.recv() {
        // The writer ends with the request, so the answer of the request is never sent.
        Ok(Message::Flush(done)) => drop(done),
        _ => panic!("the writer did not receive the flush request"),
    });
    assert_eq!(
        request_flush(&sender),
        Err("the event writer ended before it answered the flush".to_string())
    );
    writer.join().expect("join the writer");
}

#[test]
fn a_flush_returns_the_answer_of_its_writer() {
    let (sender, receiver) = sync_channel(1);
    let writer = std::thread::spawn(move || match receiver.recv() {
        Ok(Message::Flush(done)) => done.send(()).expect("answer the flush"),
        _ => panic!("the writer did not receive the flush request"),
    });
    assert_eq!(request_flush(&sender), Ok(()));
    writer.join().expect("join the writer");
}
