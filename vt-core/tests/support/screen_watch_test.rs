use super::*;

type WatchLines = tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>;

async fn fixture_with_port() -> (
    tokio::io::DuplexStream,
    WatchLines,
    tokio::task::JoinHandle<std::io::Result<()>>,
    Arc<FakeSessionPort>,
) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("watched".into(), calls));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine = Arc::new(|| {
        let mut engine = MockEngine::new();
        engine.cursor_at_end = true;
        engine.feed(b"WATCH-MARKER");
        Box::new(engine) as Box<dyn Engine>
    });
    let (mut input, reader) = tokio::io::duplex(1 << 20);
    let (writer, output) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine, reader, writer, factory));
    let mut lines = tokio::io::BufReader::new(output).lines();
    input.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":3,"raster":7,"width":805,"height":391,"scale":1.0}}}}
"#).await.unwrap();
    let image = next_image_envelope(&mut lines).await;
    assert_eq!(image["body"]["image"]["sequence"], 1);
    let screen = read_screen(&mut input, &mut lines).await;
    assert!(
        screen["body"]["render"].is_null(),
        "default draws must not collect metadata"
    );
    (input, lines, task, port)
}

async fn send(input: &mut tokio::io::DuplexStream, body: serde_json::Value) {
    input
        .write_all(format!("{}\n", serde_json::json!({"surface":"s1","body":body})).as_bytes())
        .await
        .unwrap();
}

async fn reply(lines: &mut WatchLines) -> serde_json::Value {
    serde_json::from_str(&next_line_except_frame_events(lines).await).unwrap()
}

// A semantic watch is independent of both image visibility and the ownership
// of an outstanding raster copy. No consumed message or redraw releases it.
#[tokio::test]
async fn screen_watch_notifies_engine_output_without_a_draw_and_stops_on_unwatch() {
    for drawing in [false, true] {
        let (mut input, mut lines, task, port) = fixture_with_port().await;
        if !drawing {
            send(&mut input, serde_json::json!({"image":{"drawing":{"name":"view","generation":3,"drawing":false}}})).await;
        }
        send(
            &mut input,
            serde_json::json!({"operation":"screen.watch","enabled":true}),
        )
        .await;
        let subscribed = reply(&mut lines).await;
        assert_eq!(
            subscribed["body"],
            serde_json::json!({"event":"screen.watching","enabled":true}),
            "semantic watch must register before its initial read"
        );
        port.push_event(DaemonEvent::Output {
            session_id: "watched".into(),
            data: b"WATCH-AFTER-OUTPUT".to_vec(),
        });
        let changed = reply(&mut lines).await;
        assert_eq!(
            changed["body"],
            serde_json::json!({"event":"screen.changed"}),
            "engine output must notify without a new raster (drawing={drawing})"
        );
        let screen = read_screen(&mut input, &mut lines).await;
        let cells = screen["body"]["cells"].as_array().unwrap();
        assert!(
            cells
                .iter()
                .filter_map(|cell| cell["grapheme"].as_str())
                .collect::<String>()
                .contains("WATCH-AFTER-OUTPUT"),
            "notification must follow engine.feed: {screen}"
        );
        send(
            &mut input,
            serde_json::json!({"operation":"screen.watch","enabled":false}),
        )
        .await;
        assert_eq!(
            reply(&mut lines).await["body"],
            serde_json::json!({"event":"screen.watching","enabled":false})
        );
        port.push_event(DaemonEvent::Output {
            session_id: "watched".into(),
            data: b"UNWATCHED".to_vec(),
        });
        // The same daemon channel orders this barrier after Output; unlike a
        // timeout it proves that the actor processed the unwatched output.
        port.push_event(DaemonEvent::InputHeld {
            session_id: "watched".into(),
            queued: 0,
            held: false,
        });
        assert_eq!(
            reply(&mut lines).await["body"]["event"],
            "input_held",
            "an unwatched output must send no screen.changed"
        );
        drop(input);
        task.await.unwrap().unwrap();
    }
}
