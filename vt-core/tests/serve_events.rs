// The events of the serve loop and of the PTY service (core docs/spec/logging.md): the frames, the
// requests with their whole bodies, and the writes to the PTY with the correlation identifiers of
// the request that caused them. The events are produced by the writer of this crate and read from
// its file.
#[path = "support/catalog_test.rs"]
mod catalog_test;
#[path = "support/events_test.rs"]
mod events_test;

use events_test::TestEvents;

// Frame producer: a serve with tracing enabled records the reason and the drawing time for each
// frame.
use soksak_sidecar_vt_core::policy::{Class, Policy};
use soksak_sidecar_vt_core::protocol::{
    Cursor, CursorShape, Engine, EngineEvent, Modes, Screen, SessionPort, ShellRequest,
    make_default_session_port_factory, serve,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[derive(Default)]
struct TraceEngine {
    // The number of screens that the surface built.
    screens: Arc<std::sync::atomic::AtomicUsize>,
    cols: u16,
    rows: u16,
    runtime_error: Option<String>,
    // The modes that the output of the program sets with `?1000h`, `?1000l` and `?1006h`.
    modes: Modes,
    // The screen places its cursor on the row after the last, which the screen hash refuses.
    unhashable: bool,
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

#[async_trait::async_trait]
impl Engine for TraceEngine {
    fn set_theme(&mut self, _theme: soksak_sidecar_vt_core::TerminalTheme) {}
    fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }
    fn set_cell_metrics(&mut self, _width: u16, _height: u16) -> Result<(), String> {
        Ok(())
    }
    fn feed(&mut self, bytes: &[u8]) {
        if contains(bytes, b"\x1b[?1000h") {
            self.modes.mouse_click = true;
        }
        if contains(bytes, b"\x1b[?1000l") {
            self.modes.mouse_click = false;
        }
        if contains(bytes, b"\x1b[?1006h") {
            self.modes.sgr_mouse = true;
        }
    }
    fn drain_events(&mut self) -> Vec<EngineEvent> {
        self.runtime_error
            .take()
            .map(EngineEvent::RuntimeError)
            .into_iter()
            .collect()
    }
    fn resolve_clipboard(&mut self, _request_id: u64, _text: &str) -> Result<(), String> {
        Ok(())
    }
    fn reject_clipboard(&mut self, _request_id: u64, _reason: &str) -> Result<(), String> {
        Ok(())
    }
    fn selection_start(&mut self, _col: u16, _row: u16) -> Result<(), String> {
        Ok(())
    }
    fn selection_update(&mut self, _col: u16, _row: u16) -> Result<(), String> {
        Ok(())
    }
    fn selection_end(&mut self) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn selection_clear(&mut self) -> bool {
        false
    }
    fn selection_text(&self) -> Option<String> {
        None
    }
    fn scroll_viewport(&mut self, _lines: i32) {}
    fn scroll_to_newest(&mut self) -> bool {
        false
    }
    fn cursor(&self) -> Cursor {
        Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: false,
            preedit: None,
        }
    }
    fn screen(&mut self) -> Screen {
        self.screens
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Screen {
            cols: self.cols,
            rows: self.rows,
            cursor: Cursor {
                col: 0,
                row: if self.unhashable { self.rows } else { 0 },
                shape: CursorShape::Block,
                visible: true,
                blinking: false,
                blink_visible: true,
                focused: false,
                preedit: None,
            },
            scrollback: Default::default(),
            background: "#1e1e1e".to_string(),
            lines: Vec::new(),
        }
    }
    fn modes(&self) -> Modes {
        self.modes.clone()
    }
    fn reset(&mut self) {}
}

struct TracePort;

#[async_trait::async_trait]
impl SessionPort for TracePort {
    async fn open(
        &self,
        _request: &ShellRequest,
        _cols: u16,
        _rows: u16,
    ) -> Result<String, String> {
        Ok("perf-session".to_string())
    }
    async fn write(&self, _session_id: &str, _data: &[u8]) -> Result<(), String> {
        Ok(())
    }
    async fn resize(&self, _session_id: &str, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }
    async fn detach(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn close(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn get_events(&self) -> mpsc::Receiver<soksak_sidecar_vt_core::protocol::DaemonEvent> {
        mpsc::channel(1).1
    }
}

/// Serves `input` on a connection with the engine and the port of this file and returns the lines
/// of the events after the connection ended and the writer put them on disk.
async fn serve_lines(events: &TestEvents, input: &[u8]) -> Vec<String> {
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let port = Arc::new(TracePort);
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    serve_with(events, engine_factory, factory, input).await
}

async fn serve_with(
    events: &TestEvents,
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    input: &[u8],
) -> Vec<String> {
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    let mut lines = BufReader::new(from_serve).lines();
    to_serve.write_all(input).await.unwrap();
    // The ending of the connection ends the serve loop after it handled every line that was sent.
    drop(to_serve);
    let mut replies = Vec::new();
    while let Some(line) =
        tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
            .await
            .expect("the serve loop did not end")
            .unwrap()
    {
        replies.push(line);
    }
    task.await.unwrap();
    replies
}

/// Serves `input` and keeps the connection open until the event `until` is in the file of `events`
/// (the output of a session reaches the surface task when the task takes it, and nothing tells the
/// test when). The wait has a limit that ends a stalled test; success is judged by the event.
async fn serve_until(
    events: &TestEvents,
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    input: &[u8],
    until: &str,
) {
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    to_serve.write_all(input).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while events.named(until).is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{until} was not stored"));
    drop(to_serve);
    let mut lines = BufReader::new(from_serve).lines();
    while lines.next_line().await.unwrap().is_some() {}
    task.await.unwrap();
}

#[tokio::test]
async fn a_served_surface_records_its_frames_with_reasons() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#,
    )
    .await;
    let frames = events.named("sidecar.vt.frame");
    assert!(!frames.is_empty(), "at least one frame is recorded");
    for frame in &frames {
        assert_eq!(frame["layer"], "sidecar");
        assert_eq!(frame["level"], "debug");
        assert_eq!(frame["surface"], "s1");
        assert!(
            frame["fields"]["reason"].is_string() && frame["fields"]["draw_us"].is_u64(),
            "every frame carries a reason and a draw time: {frame}"
        );
    }
    // Each frame names its session, the number of its image (consecutive from 1 for a surface), the
    // size of its screen and the screen hash (the contents of the frame record are checked by the
    // test of the replay records).
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame["session"], "perf-session", "{frame}");
        assert_eq!(frame["fields"]["image"], index as u64 + 1, "{frame}");
        assert!(frame["fields"].get("seq").is_none(), "{frame}");
        assert_eq!(
            frame["fields"]["hash"].as_str().unwrap().len(),
            64,
            "{frame}"
        );
    }
    assert!(
        frames
            .iter()
            .any(|f| f["fields"]["reason"] == "open" || f["fields"]["reason"] == "resize"),
        "the open and configure paths name themselves: {frames:?}"
    );
}

#[tokio::test]
async fn a_served_request_records_its_whole_body_and_an_unparsed_line_as_it_arrived() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"\ud55c","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":false}}}
not json at all
"#,
    )
    .await;
    let requests = events.named("sidecar.vt.request");
    let compose = requests
        .iter()
        .find(|request| request["fields"]["operation"] == "input")
        .expect("the input request is recorded");
    assert_eq!(compose["level"], "info");
    assert_eq!(compose["fields"]["body"]["body"]["preedit"]["text"], "한");
    assert_eq!(
        compose["fields"]["body"]["body"]["preedit"]["selectedRange"]["location"],
        1
    );
    let unparsed = requests
        .iter()
        .find(|request| request["fields"]["operation"] == "unparsed")
        .expect("the unparsed line is recorded");
    assert_eq!(unparsed["fields"]["body"]["unparsed"], "not json at all");
}

// The host answers a frame with `consumed`: the time from sending the frame to the answer is the
// event `consumed_wait`.
#[tokio::test]
async fn the_answer_of_the_host_to_a_frame_is_the_event_consumed_wait() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#,
    )
    .await;
    let waits = events.named("sidecar.vt.consumed_wait");
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert_eq!(waits[0]["level"], "debug");
    assert_eq!(waits[0]["surface"], "s1");
    assert_eq!(waits[0]["fields"]["answer"], "consumed");
    assert_eq!(waits[0]["fields"]["name"], "view");
    assert_eq!(waits[0]["fields"]["generation"], 1);
    assert_eq!(waits[0]["fields"]["raster"], 1);
    assert_eq!(waits[0]["fields"]["sequence"], 1);
    assert!(waits[0]["fields"]["wait_us"].is_u64(), "{}", waits[0]);
    assert!(
        waits[0]["fields"]["queue_us"].is_u64(),
        "queue timing is missing: {}",
        waits[0]
    );
}

/// Serves a connection whose output pipe holds `output_capacity` bytes. The sender writes `line`
/// after `before_line`. The client reads the first byte of the replies, which the service has
/// written when the read returns, then waits `before_reading` and reads the rest; a write longer
/// than the pipe therefore waits for the client at least `before_reading`, however late the service
/// starts it. The waits are the condition that the events under test report.
async fn serve_slowly(
    events: &TestEvents,
    output_capacity: usize,
    before_line: std::time::Duration,
    line: &[u8],
    before_reading: std::time::Duration,
) {
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let port = Arc::new(TracePort);
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(output_capacity);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    tokio::time::sleep(before_line).await;
    to_serve.write_all(line).await.unwrap();
    drop(to_serve);
    let mut from_serve = from_serve;
    let mut first = [0u8; 1];
    from_serve
        .read_exact(&mut first)
        .await
        .expect("the service writes a reply");
    tokio::time::sleep(before_reading).await;
    let mut lines = BufReader::new(from_serve).lines();
    while lines.next_line().await.unwrap().is_some() {}
    task.await.unwrap();
}

const NOTHING: &[u8] =
    b"{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{\"operation\":\"nothing\"}}\n";

// A connection that sends nothing for one second and then a line is the event `read_wait`: the read
// side waited.
#[tokio::test]
async fn a_read_that_waited_a_second_is_the_event_read_wait() {
    let events = TestEvents::new();
    serve_slowly(
        &events,
        64 * 1024,
        std::time::Duration::from_millis(1200),
        NOTHING,
        std::time::Duration::ZERO,
    )
    .await;
    let waits = events.named("sidecar.vt.read_wait");
    assert_eq!(waits.len(), 1, "{waits:?}");
    assert_eq!(waits[0]["level"], "info");
    let fields = &waits[0]["fields"];
    assert!(
        fields["waited_us"].as_u64().unwrap() >= 1_000_000,
        "{fields}"
    );
    assert!(fields["pending_polls"].as_u64().unwrap() >= 1, "{fields}");
    assert!(
        fields["since_last_poll_us"].is_u64() && fields["bytes"].as_u64().unwrap() > 0,
        "{fields}"
    );
}

// A client that does not read blocks the write of a line: a write that takes 10 milliseconds or
// more is the event `output_blocked`.
#[tokio::test]
async fn a_write_to_a_client_that_does_not_read_is_the_event_output_blocked() {
    let events = TestEvents::new();
    // The pipe to the client holds 16 bytes, so the reply cannot be written before the client
    // reads.
    serve_slowly(
        &events,
        16,
        std::time::Duration::ZERO,
        NOTHING,
        std::time::Duration::from_millis(100),
    )
    .await;
    let blocked = events.named("sidecar.vt.output_blocked");
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert_eq!(blocked[0]["level"], "info");
    assert!(
        blocked[0]["fields"]["us"].as_u64().unwrap() >= 10_000,
        "{}",
        blocked[0]
    );
    assert!(
        blocked[0]["fields"]["bytes"].as_u64().unwrap() > 16,
        "{}",
        blocked[0]
    );
}

// A raster that holds no whole cell is a transient state of a drag: the grid stays and the service
// records that it did not size the grid to this raster.
#[tokio::test]
async fn a_raster_without_a_whole_cell_is_recorded_and_the_grid_stays() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":2,"height":2,"scale":1.0}}}}
"#,
    )
    .await;
    let records = events.named("sidecar.vt.record.info");
    let raster: Vec<_> = records
        .iter()
        .filter(|record| record["fields"]["where"] == "raster")
        .collect();
    assert!(!raster.is_empty(), "{records:?}");
    let text = raster[0]["fields"]["text"].as_str().unwrap();
    assert!(
        text.contains("surface s1") && text.contains("2x2"),
        "{text}"
    );
}

/// An engine that reports the inline image command it is scripted with when it is fed.
struct InlineImageEngine {
    inner: TraceEngine,
    command: Option<soksak_sidecar_vt_core::inline_image::InlineImageCommand>,
}

#[async_trait::async_trait]
impl Engine for InlineImageEngine {
    fn set_theme(&mut self, theme: soksak_sidecar_vt_core::TerminalTheme) {
        self.inner.set_theme(theme)
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.inner.resize(cols, rows)
    }
    fn set_cell_metrics(&mut self, width: u16, height: u16) -> Result<(), String> {
        self.inner.set_cell_metrics(width, height)
    }
    fn feed(&mut self, _bytes: &[u8]) {}
    fn drain_events(&mut self) -> Vec<EngineEvent> {
        self.command
            .take()
            .map(|command| {
                vec![EngineEvent::InlineImage {
                    command,
                    anchor: Default::default(),
                }]
            })
            .unwrap_or_default()
    }
    fn resolve_clipboard(&mut self, id: u64, text: &str) -> Result<(), String> {
        self.inner.resolve_clipboard(id, text)
    }
    fn reject_clipboard(&mut self, id: u64, reason: &str) -> Result<(), String> {
        self.inner.reject_clipboard(id, reason)
    }
    fn selection_start(&mut self, col: u16, row: u16) -> Result<(), String> {
        self.inner.selection_start(col, row)
    }
    fn selection_update(&mut self, col: u16, row: u16) -> Result<(), String> {
        self.inner.selection_update(col, row)
    }
    fn selection_end(&mut self) -> Result<Option<String>, String> {
        self.inner.selection_end()
    }
    fn selection_clear(&mut self) -> bool {
        self.inner.selection_clear()
    }
    fn selection_text(&self) -> Option<String> {
        self.inner.selection_text()
    }
    fn scroll_viewport(&mut self, lines: i32) {
        self.inner.scroll_viewport(lines)
    }
    fn scroll_to_newest(&mut self) -> bool {
        self.inner.scroll_to_newest()
    }
    fn cursor(&self) -> Cursor {
        self.inner.cursor()
    }
    fn screen(&mut self) -> Screen {
        self.inner.screen()
    }
    fn modes(&self) -> Modes {
        self.inner.modes()
    }
    fn reset(&mut self) {
        self.inner.reset()
    }
}

/// A port whose session reports `output` once, which the surface feeds to its engine.
struct OutputPort(
    std::sync::Mutex<Option<mpsc::Receiver<soksak_sidecar_vt_core::protocol::DaemonEvent>>>,
);

#[async_trait::async_trait]
impl SessionPort for OutputPort {
    async fn open(
        &self,
        _request: &ShellRequest,
        _cols: u16,
        _rows: u16,
    ) -> Result<String, String> {
        Ok("scripted-session".to_string())
    }
    async fn write(&self, _session_id: &str, _data: &[u8]) -> Result<(), String> {
        Ok(())
    }
    async fn resize(&self, _session_id: &str, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }
    async fn detach(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn close(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn get_events(&self) -> mpsc::Receiver<soksak_sidecar_vt_core::protocol::DaemonEvent> {
        self.0
            .lock()
            .unwrap()
            .take()
            .expect("the events are taken once")
    }
}

// An inline image command that cannot be applied is an error of the page (the event `error` of the
// surface) and an error record of the service, also for a surface that sends no surface events.
#[tokio::test]
async fn an_inline_image_command_that_cannot_be_applied_is_an_error_record() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| {
        Box::new(InlineImageEngine {
            inner: TraceEngine::default(),
            command: Some(
                soksak_sidecar_vt_core::inline_image::InlineImageCommand::MultipartPart(vec![1]),
            ),
        }) as Box<dyn Engine>
    });
    let (sender, receiver) = mpsc::channel(4096);
    let port = Arc::new(OutputPort(std::sync::Mutex::new(Some(receiver))));
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{\"operation\":\"open\",\"shell\":\"/bin/sh\"}}\n")
        .await
        .unwrap();
    // The surface task takes the output of its session when it chooses; the connection stays open
    // until the record exists. No notification reaches this test from the task, so it asks the file
    // again after a short pause.
    let records = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            // Output that arrives before the open is ignored, so the output is reported again until
            // the session takes it.
            sender
                .send(soksak_sidecar_vt_core::protocol::DaemonEvent::Output {
                    session_id: "scripted-session".to_string(),
                    data: b"x".to_vec(),
                    sequence: 0,
                    truncated: false,
                })
                .await
                .unwrap();
            let found = events.named("sidecar.vt.record.error");
            if !found.is_empty() {
                return found;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the inline image error was not recorded");
    drop(to_serve);
    let mut lines = BufReader::new(from_serve).lines();
    while lines.next_line().await.unwrap().is_some() {}
    task.await.unwrap();
    let inline: Vec<_> = records
        .iter()
        .filter(|record| record["fields"]["where"] == "inline image")
        .collect();
    assert_eq!(inline.len(), 1, "{records:?}");
    assert_eq!(
        inline[0]["fields"]["text"],
        "inline image multipart part has no active transfer"
    );
}

// The inputs of a session are replay inputs (core docs/spec/replay.md#records): the output of the
// program, the input to it and each resize carry the session, a `sequence` that counts the inputs
// of the session from 1 without a gap, and the complete bytes as `hex` without a text copy. A frame
// record carries the sequence of the last input that its screen includes, the size of the screen
// and the screen hash.
#[tokio::test]
async fn the_inputs_and_the_frames_of_a_session_are_replay_records() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(events.handle());
    serve_until(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.42"],"body":{"operation":"input","actions":[{"type":"insert","text":"\ud55ca\n","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
        "sidecar.vt.pty_read",
    )
    .await;
    let lines = events.lines();
    let inputs: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|line| {
            [
                "sidecar.vt.pty_read",
                "sidecar.vt.pty_write",
                "sidecar.vt.resize",
                "sidecar.vt.theme",
            ]
            .contains(&line["event"].as_str().unwrap())
        })
        .collect();
    let theme = inputs
        .iter()
        .find(|input| input["event"] == "sidecar.vt.theme")
        .expect("the theme in force is an input");
    assert_eq!(
        theme["fields"]["sequence"], 1,
        "the first input is the theme"
    );
    let session = inputs[0]["session"]
        .as_str()
        .expect("an input names its session")
        .to_string();
    let mut sequences: Vec<u64> = inputs
        .iter()
        .map(|input| {
            assert_eq!(input["session"], session.as_str(), "{input}");
            assert!(
                input["fields"].get("text").is_none(),
                "no derived copy: {input}"
            );
            input["fields"]["sequence"].as_u64().expect("a sequence")
        })
        .collect();
    sequences.sort_unstable();
    assert_eq!(
        sequences,
        (1..=sequences.len() as u64).collect::<Vec<_>>(),
        "the sequences of a session have no gap"
    );
    let resize = inputs
        .iter()
        .find(|input| input["event"] == "sidecar.vt.resize")
        .expect("the resize is an input");
    assert!(
        resize["fields"]["rows"].as_u64().unwrap() > 0
            && resize["fields"]["cols"].as_u64().unwrap() > 0
    );
    let write = inputs
        .iter()
        .find(|input| input["event"] == "sidecar.vt.pty_write")
        .expect("the write is an input");
    assert_eq!(write["fields"]["hex"], "ed959c610a");
    assert_eq!(write["cids"], serde_json::json!(["b1.42"]));
    let read = inputs
        .iter()
        .find(|input| input["event"] == "sidecar.vt.pty_read")
        .expect("the echo is an input");
    assert!(read["fields"]["hex"].as_str().unwrap().len() >= 2);
    let frames: Vec<_> = lines
        .iter()
        .filter(|line| line["event"] == "sidecar.vt.frame")
        .collect();
    assert!(!frames.is_empty());
    for frame in frames {
        assert_eq!(frame["session"], session.as_str(), "{frame}");
        let sequence = frame["fields"]["sequence"]
            .as_u64()
            .expect("the last input of the frame");
        assert!(sequence >= 1 && sequence <= inputs.len() as u64, "{frame}");
        assert!(
            frame["fields"]["rows"].as_u64().unwrap() > 0
                && frame["fields"]["cols"].as_u64().unwrap() > 0
        );
        let hash = frame["fields"]["hash"].as_str().expect("the screen hash");
        assert!(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{hash}"
        );
    }
}

// The class `io` is stored when the policy stores it: with the class off neither the output of the
// program nor the bytes written to it are stored (both are declared with the class `io`), and the
// gate counts what it refused.
#[tokio::test]
async fn the_bytes_of_the_program_are_not_stored_when_the_policy_turns_the_class_io_off() {
    let events = TestEvents::with_policy(Policy::default().with(Class::Io, None));
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(events.handle());
    serve_with(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"\ud55ca\n","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
    )
    .await;
    assert!(events.named("sidecar.vt.pty_read").is_empty());
    assert!(events.named("sidecar.vt.pty_write").is_empty());
    let gated = events.named("log.gated");
    assert!(
        gated.iter().any(|line| line["fields"]["class"] == "io"
            && line["fields"]["level"] == "info"
            && line["fields"]["count"].as_u64().unwrap() >= 1),
        "{gated:?}"
    );
}

// The frame record is the only reader of the screen hash of a drawn frame, so a frame whose record
// the policy does not store is not hashed: a screen that the hash refuses records no error then.
#[tokio::test]
async fn a_frame_whose_record_is_not_stored_is_not_hashed() {
    let events = TestEvents::with_policy(Policy::default().with(Class::Frame, None));
    let engine_factory = Arc::new(|| {
        Box::new(TraceEngine {
            unhashable: true,
            ..TraceEngine::default()
        }) as Box<dyn Engine>
    });
    let port = Arc::new(TracePort);
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    serve_with(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#,
    )
    .await;
    let errors = events.named("sidecar.vt.record.error");
    assert!(
        errors.iter().all(|line| line["fields"]["where"] != "frame"),
        "a frame whose record is not stored was hashed: {errors:?}"
    );
    assert!(events.named("sidecar.vt.frame").is_empty());
}

// The `screen` event that answers `screen.read` carries the members of the screen hash, and its
// `sequence` is the sequence of the frame record that carries the hash of the same screen, so a
// page compares the two (core docs/spec/terminal-runtime.md).
#[tokio::test]
async fn a_screen_read_is_the_screen_of_the_frame_record_of_its_sequence() {
    let events = TestEvents::new();
    let replies = serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#,
    )
    .await;
    let screens: Vec<serde_json::Value> = replies
        .iter()
        .map(|reply| serde_json::from_str::<serde_json::Value>(reply).unwrap())
        .filter(|reply| reply["body"]["event"] == "screen")
        .map(|reply| reply["body"].clone())
        .collect();
    assert!(!screens.is_empty());
    let frames = events.named("sidecar.vt.frame");
    for screen in &screens {
        assert!(screen.get("lines").is_none(), "{screen}");
        let rows = screen["rows"].as_u64().unwrap();
        let cols = screen["cols"].as_u64().unwrap();
        assert_eq!(
            screen["cells"].as_array().unwrap().len() as u64,
            rows * cols
        );
        let frame = frames
            .iter()
            .find(|frame| frame["fields"]["sequence"] == screen["sequence"])
            .unwrap_or_else(|| panic!("no frame record has sequence {}", screen["sequence"]));
        assert_eq!(
            frame["fields"]["hash"],
            soksak_sidecar_vt_core::screen_hash::screen_hash_json(screen).unwrap(),
            "the hash of the message is the hash of the frame record"
        );
        // The segment is the boot of the event file that holds the frame record (core
        // docs/spec/replay.md#segments).
        assert_eq!(screen["segment"], "test", "{screen}");
    }
}

// A request line over the bound of an event is recorded by its first 4,096 bytes and its size, and
// the line is neither cloned nor serialized whole (core docs/spec/logging.md#bounds).
#[tokio::test]
async fn a_request_line_over_the_bound_is_recorded_by_its_head_and_its_size() {
    let events = TestEvents::new();
    let line = format!(
        "{{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{{\"operation\":\"nothing\",\"pad\":\"{}\"}}}}",
        "x".repeat(1_200_000)
    );
    serve_lines(&events, format!("{line}\n").as_bytes()).await;
    let request = &events.named("sidecar.vt.request")[0];
    assert_eq!(request["fields"]["operation"], "nothing");
    let body = &request["fields"]["body"];
    assert_eq!(body["size"], line.len());
    assert_eq!(body["head"].as_str().unwrap(), &line[..4096]);
    assert!(request.get("truncated").is_none());
}

#[tokio::test]
async fn a_request_records_the_correlation_identifiers_it_continues() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","cids":["b1.7","b1.8"],"body":{"operation":"open","shell":"/bin/sh","image":"view"}}
"#,
    )
    .await;
    let request = &events.named("sidecar.vt.request")[0];
    assert_eq!(request["cids"], serde_json::json!(["b1.7", "b1.8"]));
    assert!(
        request["fields"].get("cids").is_none(),
        "the identifiers belong to the line, not to the fields: {request}"
    );
}

#[tokio::test]
async fn cids_that_are_not_a_list_of_strings_are_rejected_with_an_error() {
    let events = TestEvents::new();
    for cids in ["7", "[1]", "\"b1.7\"", "[\"b1.7\",null]"] {
        let line = format!(
            "{{\"surface\":\"s1\",\"root\":\"/tmp\",\"cids\":{cids},\"body\":{{\"operation\":\"open\",\"shell\":\"/bin/sh\"}}}}\n"
        );
        let replies = serve_lines(&events, line.as_bytes()).await;
        // The answer goes to the surface of the request and names the cids. It stated the parse
        // error before, which named no surface, so the answer did not reach the surface that sent
        // the request.
        assert!(
            replies
                .iter()
                .any(|reply| reply.contains(r#""surface":"s1""#)
                    && reply.contains("cids must be a list of strings")),
            "cids {cids} was not answered on its surface: {replies:?}"
        );
    }
}

/// The lines of `replies` that parse as JSON.
fn parsed(replies: &[String]) -> Vec<serde_json::Value> {
    replies
        .iter()
        .map(|reply| serde_json::from_str(reply).expect("a reply line is JSON"))
        .collect()
}

// A failed request is stated as `error` in the body, the reply line carries the `cids` of the
// request beside `surface` and `body`, and the reply is the event `sidecar.vt.reply` in the result
// form of core (docs/spec/logging.md#result-form).
#[tokio::test]
async fn an_error_reply_carries_the_cids_of_its_request_and_is_the_event_reply() {
    let events = TestEvents::new();
    let replies = serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","cids":["b1.9"],"body":{"operation":"nothing"}}
"#,
    )
    .await;
    let replies = parsed(&replies);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["surface"], "s1");
    assert_eq!(replies[0]["cids"], serde_json::json!(["b1.9"]));
    assert_eq!(replies[0]["body"]["error"], "Unknown operation: nothing");
    let recorded = events.named("sidecar.vt.reply");
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    let reply = &recorded[0];
    assert_eq!(reply["level"], "warn");
    assert_eq!(reply["surface"], "s1");
    assert_eq!(reply["cids"], serde_json::json!(["b1.9"]));
    assert_eq!(reply["fields"]["operation"], "nothing");
    assert_eq!(reply["fields"]["ok"], false);
    assert_eq!(
        reply["fields"]["error"],
        "-32603: Unknown operation: nothing"
    );
    assert!(reply["fields"]["micros"].is_u64(), "{reply}");
}

// The reply of a command that the surface task answers carries the `cids` of the request that sent
// the command, and the acknowledgement of an input is a reply with `ok` true.
#[tokio::test]
async fn the_replies_of_the_surface_task_carry_the_cids_of_their_requests() {
    let events = TestEvents::new();
    let replies = serve_lines(
        &events,
        br#"{"surface":"s0","root":"/tmp","cids":["b1.10"],"body":{"operation":"input","actions":[{"type":"insert","text":"\u0000","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.11"],"body":{"operation":"input","actions":[{"type":"insert","text":"\u0000","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
    )
    .await;
    let replies = parsed(&replies);
    let not_open = replies
        .iter()
        .find(|reply| reply["surface"] == "s0")
        .expect("the input of a surface without a session is answered");
    assert_eq!(not_open["body"]["error"], "Session not open");
    assert_eq!(not_open["cids"], serde_json::json!(["b1.10"]));
    let ack = replies
        .iter()
        .find(|reply| reply["body"]["ack"] == true)
        .expect("the input is acknowledged");
    assert_eq!(ack["cids"], serde_json::json!(["b1.11"]));
    let recorded = events.named("sidecar.vt.reply");
    let failed = recorded
        .iter()
        .find(|reply| reply["cids"] == serde_json::json!(["b1.10"]))
        .expect("the failure is a reply event");
    assert_eq!(failed["fields"]["error"], "-32603: Session not open");
    assert_eq!(failed["fields"]["operation"], "input");
    let acknowledged = recorded
        .iter()
        .find(|reply| reply["cids"] == serde_json::json!(["b1.11"]))
        .expect("the acknowledgement is a reply event");
    assert_eq!(acknowledged["level"], "info");
    assert_eq!(acknowledged["fields"]["ok"], true);
    assert!(acknowledged["fields"].get("error").is_none());
}

// A line that no request asks for (the state, the screen, an image) carries no `cids`, even when
// the request that caused it had some.
#[tokio::test]
async fn an_event_that_a_request_does_not_ask_for_carries_no_cids() {
    let events = TestEvents::new();
    let replies = serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","cids":["b1.12"],"body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#,
    )
    .await;
    let replies = parsed(&replies);
    let events_sent: Vec<_> = replies
        .iter()
        .filter(|reply| reply["body"]["event"].is_string() || reply["body"]["image"].is_object())
        .collect();
    assert!(!events_sent.is_empty(), "{replies:?}");
    for line in events_sent {
        assert!(line.get("cids").is_none(), "{line}");
    }
}

/// A port whose session cannot be opened.
struct FailingOpenPort;

#[async_trait::async_trait]
impl SessionPort for FailingOpenPort {
    async fn open(
        &self,
        _request: &ShellRequest,
        _cols: u16,
        _rows: u16,
    ) -> Result<String, String> {
        Err("no shell".to_string())
    }
    async fn write(&self, _session_id: &str, _data: &[u8]) -> Result<(), String> {
        Ok(())
    }
    async fn resize(&self, _session_id: &str, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }
    async fn detach(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn close(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn get_events(&self) -> mpsc::Receiver<soksak_sidecar_vt_core::protocol::DaemonEvent> {
        mpsc::channel(1).1
    }
}

// The failure to open a headless session is answered on the surface that asked: a reply without
// `surface` is a message that the host cannot deliver.
#[tokio::test]
async fn the_failure_to_open_a_headless_session_names_the_surface() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let port = Arc::new(FailingOpenPort);
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let replies = serve_with(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","cids":["b1.13"],"body":{"operation":"open","shell":"/bin/sh"}}
"#,
    )
    .await;
    let replies = parsed(&replies);
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["surface"], "s1");
    assert_eq!(replies[0]["cids"], serde_json::json!(["b1.13"]));
    assert_eq!(
        replies[0]["body"]["error"],
        "Failed to open headless session: no shell"
    );
}

// A terminal font family that is not installed is not an error of the request: the service records
// it.
#[tokio::test]
async fn a_font_family_that_is_not_installed_is_recorded_as_an_info_record() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"font","family":"No Such Terminal Font Family","size":13}}
"#,
    )
    .await;
    let records = events.named("sidecar.vt.record.info");
    let texts: Vec<_> = records
        .iter()
        .map(|record| {
            (
                record["fields"]["where"].as_str().unwrap(),
                record["fields"]["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            (
                "font",
                "terminal font family is not installed: No Such Terminal Font Family"
            ),
            (
                "font",
                "no listed terminal font family is installed; using the system fixed-pitch font"
            ),
        ]
    );
}

/// The correlation identifiers of a request reach the event of the write to the PTY that the
/// request caused, through a real PTY service.
#[tokio::test]
async fn the_cids_of_an_input_request_reach_the_pty_write_event() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(events.handle());
    // "\u{d55c}a\n" encoded in base64.
    serve_with(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.42"],"body":{"operation":"input","actions":[{"type":"insert","text":"\ud55ca\n","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
    )
    .await;
    let writes = events.named("sidecar.vt.pty_write");
    assert_eq!(writes.len(), 1, "{writes:?}");
    let write = &writes[0];
    assert_eq!(write["cids"], serde_json::json!(["b1.42"]));
    assert_eq!(write["level"], "info");
    assert!(write["session"].is_string(), "{write}");
    assert_eq!(write["fields"]["hex"], "ed959c610a");
    assert!(write["fields"].get("text").is_none());
    assert_eq!(write["fields"]["ok"], true);
    assert!(write["fields"].get("cids").is_none());
}

// The operation name in a request-processing record identifies the request kind: the connection
// operation, closed, the body operation, or the body key and its sub-key.
#[test]
fn request_operation_names_the_kind_of_request() {
    use soksak_sidecar_vt_core::protocol::request_operation;
    let name = |value: serde_json::Value| request_operation(&value);
    assert_eq!(
        name(serde_json::json!({"operation": "retain", "request": "1"})),
        "retain"
    );
    assert_eq!(
        name(serde_json::json!({"surface": "s", "closed": true})),
        "closed"
    );
    assert_eq!(
        name(
            serde_json::json!({"surface": "s", "body": {"operation": "open", "shell": "/bin/zsh"}})
        ),
        "open"
    );
    assert_eq!(
        name(serde_json::json!({"surface": "s", "body": {"image": {"configure": {}}}})),
        "image.configure"
    );
    assert_eq!(
        name(serde_json::json!({"surface": "s", "body": {"bytes": "AA=="}})),
        "bytes"
    );
    assert_eq!(name(serde_json::json!({"surface": "s"})), "unknown");
}

// The events of a whole session through a real PTY satisfy the catalog of the service: each is
// declared, with its level and its required fields.
#[tokio::test]
async fn the_events_of_a_session_satisfy_the_catalog() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(events.handle());
    serve_with(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.1"],"body":{"operation":"input","actions":[{"type":"insert","text":"\ud55ca\n","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"font","family":"No Such Terminal Font Family","size":13}}
not json at all
"#,
    )
    .await;
    let lines = events.lines();
    let names: std::collections::BTreeSet<&str> = lines
        .iter()
        .map(|line| line["event"].as_str().unwrap())
        .collect();
    for expected in [
        "sidecar.vt.frame",
        "sidecar.vt.pty_write",
        "sidecar.vt.record.info",
        "sidecar.vt.request",
        "sidecar.vt.session_open",
    ] {
        assert!(
            names.contains(expected),
            "{expected} was not written: {names:?}"
        );
    }
    assert_eq!(catalog_test::findings(&lines), Vec::<String>::new());
}

// Each `open` and `image.configure` records the drawing mode of the surface that it leaves (core
// checklist item F192): a surface opened with `image` draws, and a surface opened without `image`
// is headless.
#[tokio::test]
async fn each_open_and_configure_records_the_drawing_mode_of_the_surface() {
    let events = TestEvents::new();
    serve_lines(
        &events,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s2","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
"#,
    )
    .await;
    let modes: Vec<(String, String, String)> = events
        .named("sidecar.vt.surface_mode")
        .iter()
        .map(|event| {
            (
                event["surface"].as_str().unwrap_or("").to_string(),
                event["fields"]["cause"].as_str().unwrap_or("").to_string(),
                event["fields"]["mode"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let expected = [
        ("s1", "open", "image"),
        ("s1", "configure", "image"),
        ("s2", "open", "headless"),
    ];
    assert_eq!(
        modes,
        expected
            .map(|(s, c, m)| (s.to_string(), c.to_string(), m.to_string()))
            .to_vec()
    );
}

/// Waits until `count` events named `name` that `matches` accepts are in the file of `events`. The
/// surface task records an event when it handles the request or the output, and nothing tells the
/// test when, so it reads the file again after a short pause; the wait has a limit that ends a
/// stalled test, and the result is judged by the events.
async fn wait_events(
    events: &TestEvents,
    name: &str,
    count: usize,
    matches: impl Fn(&serde_json::Value) -> bool,
) -> Vec<serde_json::Value> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let found: Vec<_> = events
                .named(name)
                .into_iter()
                .filter(|event| matches(event))
                .collect();
            if found.len() >= count {
                return found;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{count} events {name} were not stored: {:?}",
            events.named(name)
        )
    })
}

// A surface that has an image keeps drawing the output of its session after an `open` without
// `image` for it, which the background session of core sent for a terminal tab that another tab of
// its card hid while its surface stayed mounted (core checklist item F192): the `open` is refused
// with an error that names the surface, and the next output is drawn.
#[tokio::test]
async fn an_open_without_image_for_a_surface_with_an_image_is_refused_and_its_output_is_drawn() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let (sender, receiver) = mpsc::channel(16);
    let port = Arc::new(OutputPort(std::sync::Mutex::new(Some(receiver))));
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(from_serve).lines();
        let mut replies = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            replies.push(line);
        }
        replies
    });
    // An empty input is answered by the surface task after it handled every earlier request of the
    // surface, so its reply marks the end of the step before it.
    let empty_input = b"{\"surface\":\"s1\",\"body\":{\"operation\":\"input\",\"actions\":[],\"preedit\":{\"text\":\"\",\"markedRange\":null,\"selectedRange\":null,\"replacementRange\":null,\"attributed\":false}}}\n";
    let input_replies = |events: &serde_json::Value| events["fields"]["operation"] == "input";
    // The open with the image, the configure that opens the session and draws its first frame, and
    // the answer to that frame.
    to_serve
        .write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#)
        .await
        .unwrap();
    to_serve.write_all(empty_input).await.unwrap();
    wait_events(&events, "sidecar.vt.reply", 1, input_replies).await;
    // The open without image that the background session of core sends for the same surface.
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{\"operation\":\"open\",\"shell\":\"/bin/sh\"}}\n")
        .await
        .unwrap();
    to_serve.write_all(empty_input).await.unwrap();
    wait_events(&events, "sidecar.vt.reply", 2, input_replies).await;
    // The output of the program after the open, and an empty input whose reply marks the end of the
    // handling of the output.
    sender
        .send(soksak_sidecar_vt_core::protocol::DaemonEvent::Output {
            session_id: "scripted-session".to_string(),
            data: b"prompt".to_vec(),
            sequence: 0,
            truncated: false,
        })
        .await
        .unwrap();
    let read = wait_events(&events, "sidecar.vt.pty_read", 1, |_| true).await;
    let output_sequence = read[0]["fields"]["sequence"]
        .as_u64()
        .expect("the output has a sequence");
    to_serve.write_all(empty_input).await.unwrap();
    wait_events(&events, "sidecar.vt.reply", 3, input_replies).await;
    let drawn: Vec<_> = events
        .named("sidecar.vt.frame")
        .into_iter()
        .filter(|frame| {
            frame["fields"]["sequence"]
                .as_u64()
                .is_some_and(|sequence| sequence >= output_sequence)
        })
        .collect();
    drop(to_serve);
    let replies = parsed(&reader.await.unwrap());
    task.await.unwrap();
    assert_eq!(
        drawn.len(),
        1,
        "the output of sequence {output_sequence} drew no frame: {:?}",
        events.named("sidecar.vt.frame")
    );
    let refusals: Vec<_> = replies
        .iter()
        .filter(|reply| reply["surface"] == "s1" && reply["body"]["error"] == "imageAttached")
        .collect();
    assert_eq!(refusals.len(), 1, "{replies:?}");
    assert!(
        refusals[0]["body"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("s1")),
        "{}",
        refusals[0]
    );
}

#[tokio::test]
async fn an_engine_runtime_error_is_logged_instead_of_a_sequence_rejection() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| {
        Box::new(TraceEngine {
            runtime_error: Some("fixture engine queue recovery".to_string()),
            ..TraceEngine::default()
        }) as Box<dyn Engine>
    });
    let factory = Arc::new(|| Arc::new(TracePort) as Arc<dyn SessionPort>);
    let replies = serve_with(&events, engine_factory, factory,
        b"{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{\"operation\":\"open\",\"shell\":\"/bin/sh\"}}\n{\"surface\":\"s1\",\"body\":{\"operation\":\"clipboard.resolve\",\"requestId\":1,\"text\":\"x\"}}\n").await;
    let errors = events.named("sidecar.vt.record.error");
    assert_eq!(
        errors.len(),
        1,
        "the runtime failure must reach the structured error log: {errors:?}"
    );
    assert_eq!(errors[0]["fields"]["where"], "engine");
    assert_eq!(errors[0]["fields"]["text"], "fixture engine queue recovery");
    assert!(
        !replies
            .iter()
            .any(|line| line.contains("sequence.rejected")),
        "an internal engine failure is not a program sequence rejection"
    );
}

fn daemon_output(data: &[u8]) -> soksak_sidecar_vt_core::protocol::DaemonEvent {
    soksak_sidecar_vt_core::protocol::DaemonEvent::Output {
        session_id: "scripted-session".to_string(),
        data: data.to_vec(),
        sequence: 0,
        truncated: false,
    }
}

// A change of the mouse reporting modes is one event with the modes in force, and each wheel
// operation is one event that names its route and the modes it was routed by (core checklist
// item of the terminal wheel records, terminal P38).
#[tokio::test]
async fn each_mouse_mode_change_and_each_wheel_route_is_recorded_with_the_modes_in_force() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let (sender, receiver) = mpsc::channel(16);
    let port = Arc::new(OutputPort(std::sync::Mutex::new(Some(receiver))));
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(from_serve).lines();
        while lines.next_line().await.unwrap().is_some() {}
    });
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{\"operation\":\"open\",\"shell\":\"/bin/sh\"}}\n")
        .await
        .unwrap();
    // The program enables mouse click reporting with SGR encoding. The output is ignored until the
    // session exists, so it is sent again until the change is recorded.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            sender
                .send(daemon_output(b"\x1b[?1000h\x1b[?1006h"))
                .await
                .unwrap();
            if !events.named("sidecar.vt.mouse_modes").is_empty() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the mouse mode change was not recorded");
    let enabled = wait_events(&events, "sidecar.vt.mouse_modes", 1, |_| true).await;
    assert_eq!(enabled[0]["fields"]["mouse_click"], true, "{enabled:?}");
    assert_eq!(enabled[0]["fields"]["sgr_mouse"], true, "{enabled:?}");
    assert_eq!(enabled[0]["fields"]["mouse_drag"], false, "{enabled:?}");
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"scroll\",\"lines\":2,\"col\":4,\"row\":6}}\n")
        .await
        .unwrap();
    let routed = wait_events(&events, "sidecar.vt.wheel", 1, |_| true).await;
    assert_eq!(routed[0]["fields"]["route"], "program", "{routed:?}");
    assert_eq!(routed[0]["fields"]["lines"], 2, "{routed:?}");
    assert_eq!(routed[0]["fields"]["col"], 4, "{routed:?}");
    assert_eq!(routed[0]["fields"]["row"], 6, "{routed:?}");
    assert_eq!(routed[0]["fields"]["mouse_click"], true, "{routed:?}");
    // The program disables mouse click reporting; the next wheel operation scrolls the viewport.
    sender.send(daemon_output(b"\x1b[?1000l")).await.unwrap();
    let disabled = wait_events(&events, "sidecar.vt.mouse_modes", 2, |_| true).await;
    assert_eq!(disabled[1]["fields"]["mouse_click"], false, "{disabled:?}");
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"scroll\",\"lines\":-1,\"col\":0,\"row\":0}}\n")
        .await
        .unwrap();
    let viewport = wait_events(&events, "sidecar.vt.wheel", 2, |_| true).await;
    assert_eq!(viewport[1]["fields"]["route"], "viewport", "{viewport:?}");
    assert_eq!(viewport[1]["fields"]["mouse_click"], false, "{viewport:?}");
    drop(to_serve);
    reader.await.unwrap();
    task.await.unwrap();
}

/// A port whose session opens only after the test releases it: each open waits until the test
/// closes the semaphore and then fails.
struct HeldOpenPort(Arc<tokio::sync::Semaphore>);

#[async_trait::async_trait]
impl SessionPort for HeldOpenPort {
    async fn open(
        &self,
        _request: &ShellRequest,
        _cols: u16,
        _rows: u16,
    ) -> Result<String, String> {
        match self.0.acquire().await {
            Ok(_) => Err("the held open received a permit".to_string()),
            Err(_) => Err("released".to_string()),
        }
    }
    async fn write(&self, _session_id: &str, _data: &[u8]) -> Result<(), String> {
        Ok(())
    }
    async fn resize(&self, _session_id: &str, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }
    async fn detach(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn close(&self, _session_id: &str) -> Result<(), String> {
        Ok(())
    }
    async fn get_events(&self) -> mpsc::Receiver<soksak_sidecar_vt_core::protocol::DaemonEvent> {
        mpsc::channel(1).1
    }
}

// A surface actor that does not end a request holds the next requests of its surface in its queue,
// and the input loop waits for room in that queue. Each request still in progress 10 seconds after
// it arrived is the error event `request_waiting` with the stage it waits in.
#[tokio::test(start_paused = true)]
async fn a_request_in_progress_after_the_bound_is_the_event_request_waiting_with_its_stage() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let port = Arc::new(HeldOpenPort(Arc::clone(&release)));
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    // The first open holds the actor, the next ten fill its queue, and the last one waits for room.
    let open = br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
"#;
    for _ in 0..12 {
        to_serve.write_all(open).await.unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while events.named("sidecar.vt.request_waiting").len() < 12 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "12 requests in progress were not each recorded: {:?}",
            events.named("sidecar.vt.request_waiting")
        )
    });
    release.close();
    drop(to_serve);
    let mut lines = BufReader::new(from_serve).lines();
    while lines.next_line().await.unwrap().is_some() {}
    task.await.unwrap();

    let waiting = events.named("sidecar.vt.request_waiting");
    let stages: Vec<&str> = waiting
        .iter()
        .map(|event| event["fields"]["stage"].as_str().unwrap())
        .collect();
    let mut expected = vec!["actor"];
    expected.extend(["actor queue"; 10]);
    expected.push("surface queue");
    assert_eq!(stages, expected, "{waiting:?}");
    for event in &waiting {
        assert_eq!(event["level"], "error", "{event}");
        assert_eq!(event["surface"], "s1", "{event}");
        assert_eq!(event["fields"]["operation"], "open", "{event}");
        assert!(event["fields"]["ms"].as_u64().unwrap() >= 10_000, "{event}");
    }
}

/// A surface whose host answers each frame with `consumed`, as the copy mode of core does. It records
/// the time of each frame that the surface sends.
struct AnsweredSurface {
    events: TestEvents,
    outputs: mpsc::Sender<soksak_sidecar_vt_core::protocol::DaemonEvent>,
    requests: mpsc::Sender<Vec<u8>>,
    frames: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
    screens: Arc<std::sync::atomic::AtomicUsize>,
}

async fn serve_answered() -> AnsweredSurface {
    let events = TestEvents::new();
    let screens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let built = Arc::clone(&screens);
    let engine_factory = Arc::new(move || {
        Box::new(TraceEngine {
            screens: Arc::clone(&built),
            ..TraceEngine::default()
        }) as Box<dyn Engine>
    });
    let (outputs, receiver) = mpsc::channel(64);
    let port = Arc::new(OutputPort(std::sync::Mutex::new(Some(receiver))));
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = events.handle();
    tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    let (requests, mut pending) = mpsc::channel::<Vec<u8>>(64);
    tokio::spawn(async move {
        while let Some(request) = pending.recv().await {
            to_serve.write_all(&request).await.unwrap();
        }
    });
    let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
    let answers = requests.clone();
    let times = Arc::clone(&frames);
    tokio::spawn(async move {
        let mut lines = BufReader::new(from_serve).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            let Some(sequence) = value["body"]["image"]["sequence"].as_u64() else {
                continue;
            };
            times.lock().unwrap().push(tokio::time::Instant::now());
            let answer = format!(
                "{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"sequence\":{sequence}}}}}}}}}\n"
            );
            if answers.send(answer.into_bytes()).await.is_err() {
                return;
            }
        }
    });
    requests
        .send(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#.to_vec())
        .await
        .unwrap();
    let surface = AnsweredSurface {
        events,
        outputs,
        requests,
        frames,
        screens,
    };
    surface.wait_frames(1).await;
    surface
}

impl AnsweredSurface {
    async fn wait_frames(&self, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while self.frames.lock().unwrap().len() < count {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{count} frames were not sent: {:?}",
                self.frames.lock().unwrap()
            )
        });
    }

    /// Sends `count` outputs and waits until a frame draws the last of them.
    async fn output(&self, count: usize) {
        let before = self.events.named("sidecar.vt.pty_read").len();
        for _ in 0..count {
            self.outputs.send(daemon_output(b"x")).await.unwrap();
        }
        let read = wait_events(&self.events, "sidecar.vt.pty_read", before + count, |_| {
            true
        })
        .await;
        let last = read[before + count - 1]["fields"]["sequence"]
            .as_u64()
            .unwrap();
        wait_events(&self.events, "sidecar.vt.frame", 1, |frame| {
            frame["fields"]["sequence"]
                .as_u64()
                .is_some_and(|sequence| sequence >= last)
        })
        .await;
    }
}

// The frame clock draws output on one clock of 33 ms: no two frames of output without input are
// closer than 33 ms, and the last output is drawn. The time of the test runs paused, so the clock
// of the runtime alone measures the interval.
#[tokio::test(start_paused = true)]
async fn the_frame_clock_draws_output_without_input_at_most_once_in_33_ms() {
    let surface = serve_answered().await;
    surface.output(20).await;
    let frames = surface.frames.lock().unwrap().clone();
    assert!(frames.len() >= 2, "the output drew no frame: {frames:?}");
    for pair in frames.windows(2) {
        assert!(
            pair[1] - pair[0] >= std::time::Duration::from_millis(33),
            "two frames are {:?} apart: {frames:?}",
            pair[1] - pair[0]
        );
    }
}

// Output that the frame clock holds builds no screen, because a screen copies the whole grid: the
// surface builds one screen for each frame that it draws, not one for each chunk of output.
#[tokio::test(start_paused = true)]
async fn the_frame_clock_builds_no_screen_for_output_that_it_holds() {
    let surface = serve_answered().await;
    let screens = surface.screens.load(std::sync::atomic::Ordering::SeqCst);
    let frames = surface.events.named("sidecar.vt.frame").len();
    surface.output(20).await;
    let built = surface.screens.load(std::sync::atomic::Ordering::SeqCst) - screens;
    let drawn = surface.events.named("sidecar.vt.frame").len() - frames;
    assert_eq!(
        built, drawn,
        "20 chunks of output built {built} screens for {drawn} frames"
    );
}

// The output that follows an input within 200 ms is drawn at once, without the frame clock: a
// second output right after the frame of the first one is drawn without waiting 33 ms.
#[tokio::test(start_paused = true)]
async fn the_frame_clock_draws_output_at_once_within_200_ms_after_an_input() {
    let surface = serve_answered().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let drawn = surface.frames.lock().unwrap().len();
    surface
        .requests
        .send(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"paste\",\"text\":\"a\"}}\n".to_vec())
        .await
        .unwrap();
    wait_events(&surface.events, "sidecar.vt.reply", 1, |event| {
        event["fields"]["operation"] == "paste"
    })
    .await;
    surface.output(1).await;
    surface.output(1).await;
    let frames = surface.frames.lock().unwrap().clone();
    assert!(
        frames.len() >= drawn + 2,
        "the outputs drew no two frames: {frames:?}"
    );
    let gap = frames[frames.len() - 1] - frames[frames.len() - 2];
    assert!(
        gap < std::time::Duration::from_millis(33),
        "the second output after an input waited {gap:?} for the frame clock: {:?}",
        surface
            .events
            .named("sidecar.vt.frame")
            .iter()
            .map(|frame| (
                frame["fields"]["reason"].clone(),
                frame["fields"]["sequence"].clone()
            ))
            .collect::<Vec<_>>()
    );
}
