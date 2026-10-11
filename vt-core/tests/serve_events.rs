// The events of the serve loop and of the PTY service (core docs/spec/logging.md): the frames, the requests with their
// whole bodies, and the writes to the PTY with the correlation identifiers of the request that caused them. The events
// are produced by the writer of this crate and read from its file.
#[path = "support/catalog_test.rs"]
mod catalog_test;
#[path = "support/events_test.rs"]
mod events_test;

use events_test::TestEvents;

// 프레임 생산자: 트레이스가 켜진 serve 는 프레임마다 reason 과 그리기 시간을 남긴다.
use soksak_sidecar_vt_core::protocol::{
    make_default_session_port_factory, serve, Cursor, CursorShape, Engine, EngineEvent, Modes,
    Screen, SessionPort, ShellRequest,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[derive(Default)]
struct TraceEngine {
    cols: u16,
    rows: u16,
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
    fn feed(&mut self, _bytes: &[u8]) {}
    fn drain_events(&mut self) -> Vec<EngineEvent> {
        Vec::new()
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
        Screen {
            cols: self.cols,
            rows: self.rows,
            cursor: Cursor {
                col: 0,
                row: 0,
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
        Modes::default()
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
    async fn write(&self, _session_id: &str, _data: &[u8], _cids: &[String]) -> Result<(), String> {
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

/// Serves `input` on a connection with the engine and the port of this file and returns the lines of the events after the
/// connection ended and the writer put them on disk.
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
{"surface":"s1","body":{"operation":"input","compose":{"text":"\ud55c","selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":false}}}
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
    assert_eq!(compose["fields"]["body"]["body"]["compose"]["text"], "한");
    assert_eq!(
        compose["fields"]["body"]["body"]["compose"]["selectedRange"]["location"],
        1
    );
    let unparsed = requests
        .iter()
        .find(|request| request["fields"]["operation"] == "unparsed")
        .expect("the unparsed line is recorded");
    assert_eq!(unparsed["fields"]["body"]["unparsed"], "not json at all");
}

// A request line over the bound of an event is recorded by its first 4,096 bytes and its size, and the line is neither cloned
// nor serialized whole (core docs/spec/logging.md#bounds).
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
        assert!(
            replies.iter().any(|reply| reply.contains("Parse error")),
            "cids {cids} was accepted: {replies:?}"
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

// A failed request is stated as `error` in the body, the reply line carries the `cids` of the request beside `surface` and
// `body`, and the reply is the event `sidecar.vt.reply` in the result form of core (docs/spec/logging.md#result-form).
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

// The reply of a command that the surface task answers carries the `cids` of the request that sent the command, and the
// acknowledgement of an input is a reply with `ok` true.
#[tokio::test]
async fn the_replies_of_the_surface_task_carry_the_cids_of_their_requests() {
    let events = TestEvents::new();
    let replies = serve_lines(
        &events,
        br#"{"surface":"s0","root":"/tmp","cids":["b1.10"],"body":{"operation":"input","bytes":"AA=="}}
{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.11"],"body":{"operation":"input","bytes":"AA=="}}
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

// A line that no request asks for (the state, the screen, an image) carries no `cids`, even when the request that caused it
// had some.
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
    async fn write(&self, _session_id: &str, _data: &[u8], _cids: &[String]) -> Result<(), String> {
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

// The failure to open a headless session is answered on the surface that asked: a reply without `surface` is a message that
// the host cannot deliver.
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

// A terminal font family that is not installed is not an error of the request: the service records it.
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

/// The correlation identifiers of a request reach the event of the write to the PTY that the request caused, through a
/// real PTY service.
#[tokio::test]
async fn the_cids_of_an_input_request_reach_the_pty_write_event() {
    let events = TestEvents::new();
    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(events.handle());
    // "한a\n" in base64.
    serve_with(
        &events,
        engine_factory,
        factory,
        br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.42"],"body":{"operation":"input","bytes":"7ZWcYQo="}}
"#,
    )
    .await;
    let writes = events.named("sidecar.vt.pty_write");
    assert_eq!(writes.len(), 1, "{writes:?}");
    let write = &writes[0];
    assert_eq!(write["cids"], serde_json::json!(["b1.42"]));
    assert_eq!(write["level"], "info");
    assert!(write["session"].is_string(), "{write}");
    assert_eq!(write["fields"]["bytes"], 5);
    assert_eq!(write["fields"]["hex"], "ed959c610a");
    assert_eq!(write["fields"]["text"], "한a\n");
    assert_eq!(write["fields"]["ok"], true);
    assert!(write["fields"].get("cids").is_none());
}

// 요청 처리 기록의 작업 이름은 요청 종류를 밝힌다: 연결 operation, closed, body 의 operation, 또는 body 의 키와 그 하위 키.
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

// The events of a whole session through a real PTY satisfy the catalog of the service: each is declared, with its level and
// its required fields.
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
{"surface":"s1","cids":["b1.1"],"body":{"operation":"input","bytes":"7ZWcYQo="}}
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
