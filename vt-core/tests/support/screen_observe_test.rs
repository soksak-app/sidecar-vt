use super::*;

type ObservationLines = tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>;

async fn fixture() -> (
    tokio::io::DuplexStream,
    ObservationLines,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (input, lines, task, _) = fixture_with_port().await;
    (input, lines, task)
}

async fn fixture_with_port() -> (
    tokio::io::DuplexStream,
    ObservationLines,
    tokio::task::JoinHandle<std::io::Result<()>>,
    Arc<FakeSessionPort>,
) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("observed".into(), calls));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine = Arc::new(|| {
        let mut engine = MockEngine::new();
        engine.cursor_at_end = true;
        engine.feed(b"OBSERVE-MARKER");
        Box::new(engine) as Box<dyn Engine>
    });
    let (mut input, reader) = tokio::io::duplex(1 << 20);
    let (writer, output) = tokio::io::duplex(16 * 1024 * 1024);
    // No startup trace directory: observation must itself opt in for one draw.
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

#[cfg(feature = "diagnostics")]
#[tokio::test]
async fn screen_observe_natural_daemon_exit_closes_the_pending_request_once_before_exit() {
    let (mut input, mut lines, task, port) = fixture_with_port().await;
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":55}),
    )
    .await;
    // Establish actor order without releasing the initial raster's copy.
    let _ = read_screen(&mut input, &mut lines).await;
    port.push_event(DaemonEvent::Exit {
        session_id: "observed".into(),
    });
    let result = reply(&mut lines).await;
    assert_eq!(
        result["body"]["requestId"], 55,
        "natural daemon exit must settle the pending request before the ordinary exit: {result}"
    );
    assert_eq!(result["body"]["error"], "observationClosed");
    let exit = reply(&mut lines).await;
    assert_eq!(exit["body"]["event"], "exit");
    drop(input);
    let mut remaining = String::new();
    use tokio::io::AsyncReadExt;
    lines
        .into_inner()
        .read_to_string(&mut remaining)
        .await
        .unwrap();
    assert!(
        !remaining.contains("\"requestId\":55"),
        "natural exit completed observation twice: {remaining}"
    );
    // S20: this fixture has no persistent registry. Its surface actor ended
    // naturally before input EOF, so serve must report that disconnected
    // actor rather than claiming it acknowledged ConnectionEnded.
    let shutdown = task
        .await
        .unwrap()
        .expect_err("S20 requires the ended actor to remain observable at non-persistent EOF");
    assert_eq!(shutdown.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(
        shutdown.to_string(),
        "surface actor ended before serve shutdown: /tmp\0s1"
    );
}

async fn send(input: &mut tokio::io::DuplexStream, body: serde_json::Value) {
    input
        .write_all(format!("{}\n", serde_json::json!({"surface":"s1","body":body})).as_bytes())
        .await
        .unwrap();
}

#[cfg(feature = "diagnostics")]
async fn consumed(input: &mut tokio::io::DuplexStream, sequence: u32) {
    send(input, serde_json::json!({"image":{"consumed":{"name":"view","generation":3,"raster":7,"sequence":sequence}}})).await;
}

async fn reply(lines: &mut ObservationLines) -> serde_json::Value {
    serde_json::from_str(&next_line_except_frame_events(lines).await).unwrap()
}

#[cfg(feature = "diagnostics")]
#[tokio::test]
async fn screen_observe_waits_for_consumed_then_publishes_one_atomic_actual_draw() {
    let (mut input, mut lines, task) = fixture().await;
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":41}),
    )
    .await;
    send(&mut input, serde_json::json!({"operation":"screen.read"})).await;
    // A normal screen reply is an actor-order barrier, not an observation.
    // No image may overwrite sequence 1 while its copy remains pending.
    loop {
        let message = next_reply(&mut lines).await;
        assert!(
            message["body"]["image"].is_null(),
            "observation overwrote a pending raster: {message}"
        );
        assert_ne!(
            message["body"]["event"], "screen.observed",
            "observation answered before a real draw"
        );
        assert!(
            message["body"]["error"].is_null(),
            "valid observation rejected: {message}"
        );
        if message["body"]["event"] == "screen" {
            break;
        }
    }
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":42}),
    )
    .await;
    let busy = reply(&mut lines).await;
    assert_eq!(busy["body"]["requestId"], 42);
    assert_eq!(busy["body"]["error"], "observationBusy");
    consumed(&mut input, 1).await;
    let image = next_image_envelope(&mut lines).await;
    let observed = reply(&mut lines).await;
    let body = &observed["body"];
    assert_eq!(body["event"], "screen.observed");
    assert_eq!(body["requestId"], 41);
    let frame = serde_json::json!({"generation":3,"raster":7,"sequence":2});
    assert_eq!(image["body"]["image"]["sequence"], 2);
    assert_eq!(body["frame"], frame);
    assert_eq!(body["render"]["frame"], frame);
    assert_eq!(body["render"]["pixel"]["width"], 805);
    assert_eq!(body["render"]["pixel"]["height"], 391);
    assert_eq!(
        body["cells"].as_array().unwrap().len(),
        (body["rows"].as_u64().unwrap() * body["cols"].as_u64().unwrap()) as usize
    );
    assert!(body["cursor"].is_object());
    assert!(body["scrollback"].is_object());
    assert!(body["background"].is_string());
    let glyphs = body["render"]["glyphs"].as_array().unwrap();
    assert!(
        !glyphs.is_empty(),
        "atomic snapshot must describe the marker's actual rendered glyphs"
    );
    for glyph in glyphs {
        let index = glyph["row"].as_u64().unwrap() * body["cols"].as_u64().unwrap()
            + glyph["col"].as_u64().unwrap();
        assert_eq!(
            body["cells"][index as usize]["grapheme"], glyph["grapheme"],
            "snapshot glyph and semantic cells differ"
        );
    }
    consumed(&mut input, 2).await;
    send(&mut input, serde_json::json!({"operation":"input","actions":[],"preedit":{"text":"B","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}})).await;
    let next = next_image_envelope(&mut lines).await;
    assert_eq!(next["body"]["image"]["sequence"], 3);
    let plain = read_screen(&mut input, &mut lines).await;
    assert!(
        plain["body"]["render"].is_null(),
        "one-shot observation enabled metadata on a later ordinary draw"
    );
    drop(input);
    task.await.unwrap().unwrap();
}

#[cfg(feature = "diagnostics")]
#[tokio::test]
async fn screen_observe_non_drawing_region_is_explicitly_unavailable() {
    let (mut input, mut lines, task) = fixture().await;
    send(
        &mut input,
        serde_json::json!({"image":{"drawing":{"name":"view","generation":3,"drawing":false}}}),
    )
    .await;
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":45}),
    )
    .await;
    let result = reply(&mut lines).await;
    assert_eq!(result["body"]["requestId"], 45);
    assert_eq!(result["body"]["error"], "observationUnavailable");
    drop(input);
    task.await.unwrap().unwrap();
}

#[cfg(feature = "diagnostics")]
#[tokio::test]
async fn screen_observe_close_finishes_the_pending_request_once_with_its_id() {
    let (mut input, mut lines, task) = fixture().await;
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":51}),
    )
    .await;
    send(&mut input, serde_json::json!({"operation":"close"})).await;
    let result = reply(&mut lines).await;
    assert_eq!(result["body"]["requestId"], 51);
    assert_eq!(result["body"]["error"], "observationClosed");
    drop(input);
    let mut remaining = String::new();
    use tokio::io::AsyncReadExt;
    lines
        .into_inner()
        .read_to_string(&mut remaining)
        .await
        .unwrap();
    assert!(
        !remaining.contains("\"requestId\":51"),
        "observation completed more than once: {remaining}"
    );
    task.await.unwrap().unwrap();
}

#[cfg(all(feature = "diagnostics", feature = "fault-injection"))]
#[tokio::test(flavor = "current_thread")]
async fn screen_observe_draw_failure_is_correlated_and_clears_the_one_shot() {
    let (mut input, mut lines, task) = fixture().await;
    consumed(&mut input, 1).await;
    // screen.read is the FIFO barrier proving consumed was processed before
    // the native draw fault is armed.
    let _ = read_screen(&mut input, &mut lines).await;
    fault::fail_next_draw();
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":61}),
    )
    .await;
    let result = reply(&mut lines).await;
    assert_eq!(result["body"]["requestId"], 61);
    assert_eq!(result["body"]["error"], "observationFailed");
    assert!(
        result["body"]["reason"]
            .as_str()
            .unwrap()
            .contains("fault-injection")
    );
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":62}),
    )
    .await;
    let _ = next_image_envelope(&mut lines).await;
    let recovered = reply(&mut lines).await;
    assert_eq!(recovered["body"]["requestId"], 62);
    assert_eq!(recovered["body"]["event"], "screen.observed");
    drop(input);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn screen_observe_without_an_image_returns_an_explicit_correlated_error() {
    let output = serve_requests(
        "{\"surface\":\"s1\",\"body\":{\"operation\":\"screen.observe\",\"requestId\":71}}\n",
    )
    .await;
    let replies: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let result = replies
        .iter()
        .find(|reply| reply["body"]["requestId"] == 71)
        .expect("missing correlated observation error");
    assert_eq!(result["body"]["error"], "observationUnavailable");
}

#[tokio::test]
async fn screen_observe_rejects_missing_zero_and_unsafe_request_ids() {
    for id in [
        serde_json::Value::Null,
        serde_json::json!(0),
        serde_json::json!(9007199254740992_u64),
    ] {
        let output = serve_requests(&format!(
            "{}\n",
            serde_json::json!({"surface":"s1","body":{"operation":"screen.observe","requestId":id}})
        ))
        .await;
        assert!(
            output.contains("\"error\":\"invalidParams\""),
            "invalid observation id accepted or misclassified: {output}"
        );
    }
}

#[cfg(not(feature = "diagnostics"))]
#[tokio::test]
async fn screen_observe_without_diagnostics_is_explicitly_unavailable() {
    let (mut input, mut lines, task) = fixture().await;
    send(
        &mut input,
        serde_json::json!({"operation":"screen.observe","requestId":81}),
    )
    .await;
    let result = reply(&mut lines).await;
    assert_eq!(result["body"]["requestId"], 81);
    assert_eq!(result["body"]["error"], "observationUnavailable");
    drop(input);
    task.await.unwrap().unwrap();
}
