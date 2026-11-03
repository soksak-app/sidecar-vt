#[path = "support/events_test.rs"]
mod events_test;
#[path = "support/session_port_test.rs"]
mod tracked_session_port;

use async_trait::async_trait;
#[cfg(feature = "fault-injection")]
use soksak_sidecar_vt_core::platform::darwin::frame::fault;
use soksak_sidecar_vt_core::platform::darwin::frame::{Frame, metrics};
/// Serve contract integration tests that use a fake daemon
use soksak_sidecar_vt_core::protocol::{
    Cell, Cursor, CursorShape, DaemonEvent, Engine, EngineEvent, Modes, OutputSubmissionObserver,
    OutputSubmissionStage, PersistentOwner, PersistentRegistry, Screen, SessionPort, ShellRequest,
    serve as serve_with_events, serve_with_registry, serve_with_submission_observer,
};
use soksak_sidecar_vt_core::pty::PtyMeasurement;
use soksak_sidecar_vt_core::{
    TerminalTheme,
    inline_image::{Dimension, InlineImageCommand},
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// The serve loop with a writer of events in a folder of its own, which lives as long as the loop
/// runs.
async fn serve<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let events = events_test::TestEvents::new();
    serve_with_events(
        engine_factory,
        reader,
        writer,
        session_port_factory,
        events.handle(),
    )
    .await
}

/// Reads the next line, skipping the screen line that comes with a presentation.
async fn next_line_except_frame_events<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> String {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for a sidecar line")
            .expect("failed to read a sidecar line")
            .expect("the sidecar closed its output");
        // A screen and the view of a frame describe the frame, not the reply of a request.
        if !line.contains(r#""event":"screen""#) && !line.contains(r#""event":"view""#) {
            return line;
        }
    }
}

struct MockEngine {
    cols: u16,
    rows: u16,
    feed_history: Vec<Vec<u8>>,
    custom_modes: Option<Modes>,
    themes: Vec<TerminalTheme>,
    pending_events: Vec<EngineEvent>,
    selection: Option<String>,
    selected_cells: Arc<Mutex<Vec<(u16, u16)>>>,
    viewport: Arc<Mutex<Vec<String>>>,
    cursor_at_end: bool,
    join_feed_chunks: bool,
}

impl MockEngine {
    fn new() -> Self {
        Self {
            cols: 80,
            rows: 24,
            feed_history: Vec::new(),
            custom_modes: None,
            themes: Vec::new(),
            pending_events: Vec::new(),
            selection: Some("selected".to_string()),
            selected_cells: Arc::new(Mutex::new(Vec::new())),
            viewport: Arc::new(Mutex::new(Vec::new())),
            cursor_at_end: false,
            join_feed_chunks: false,
        }
    }

    fn with_modes(modes: Modes) -> Self {
        Self {
            cols: 80,
            rows: 24,
            feed_history: Vec::new(),
            custom_modes: Some(modes),
            themes: Vec::new(),
            pending_events: Vec::new(),
            selection: Some("selected".to_string()),
            selected_cells: Arc::new(Mutex::new(Vec::new())),
            viewport: Arc::new(Mutex::new(Vec::new())),
            cursor_at_end: false,
            join_feed_chunks: false,
        }
    }

    fn with_cursor_at_end(mut self) -> Self {
        self.cursor_at_end = true;
        self
    }

    fn with_contiguous_feed(mut self) -> Self {
        self.join_feed_chunks = true;
        self
    }
}

impl Engine for MockEngine {
    fn set_theme(&mut self, theme: TerminalTheme) {
        self.themes.push(theme);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    fn set_cell_metrics(&mut self, width: u16, height: u16) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err("terminal cell metrics must be positive".to_string());
        }
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.feed_history.push(bytes.to_vec());
        if bytes == b"\x1b]1337;File=name=ZmlsZS5wbmc=;size=5;inline=1;width=2px:aGVsbG8=\x07" {
            self.pending_events.push(EngineEvent::InlineImage {
                anchor: Default::default(),
                command: InlineImageCommand::Display {
                    name: "file.png".to_string(),
                    data: b"hello".to_vec(),
                    width: Dimension::Pixels(2),
                    height: Dimension::Auto,
                    preserve_aspect_ratio: true,
                },
            });
        }
        if bytes == b"\x1b]1337;File=name=cmVkLnBuZw==;inline=1:RED\x07" {
            // A 2×1 red PNG that can be drawn.
            self.pending_events.push(EngineEvent::InlineImage {
                anchor: Default::default(),
                command: InlineImageCommand::Display {
                    name: "red.png".to_string(),
                    data: vec![
                        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2,
                        0, 0, 0, 1, 8, 6, 0, 0, 0, 244, 34, 127, 138, 0, 0, 0, 14, 73, 68, 65, 84,
                        120, 156, 99, 248, 207, 192, 240, 31, 132, 1, 17, 247, 3, 253, 227, 197,
                        245, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
                    ],
                    width: Dimension::Auto,
                    height: Dimension::Auto,
                    preserve_aspect_ratio: true,
                },
            });
        }
        if bytes == b"\x1b]7;file:///tmp/project\x07" {
            self.pending_events.push(EngineEvent::Directory {
                uri: "file:///tmp/project".to_string(),
                path: Some("/tmp/project".to_string()),
            });
        }
    }

    fn drain_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.pending_events)
    }

    fn resolve_clipboard(&mut self, request_id: u64, _text: &str) -> Result<(), String> {
        Err(format!("unknown clipboard request {request_id}"))
    }

    fn reject_clipboard(&mut self, request_id: u64, _reason: &str) -> Result<(), String> {
        let _ = request_id;
        Ok(())
    }

    fn selection_start(&mut self, col: u16, row: u16) -> Result<(), String> {
        self.selected_cells.lock().unwrap().push((col, row));
        Ok(())
    }
    fn selection_update(&mut self, col: u16, row: u16) -> Result<(), String> {
        self.selected_cells.lock().unwrap().push((col, row));
        Ok(())
    }
    fn selection_end(&mut self) -> Result<Option<String>, String> {
        Ok(self.selection.clone())
    }
    fn selection_clear(&mut self) -> bool {
        self.selection.take().is_some()
    }
    fn selection_text(&self) -> Option<String> {
        self.selection.clone()
    }
    fn scroll_viewport(&mut self, lines: i32) {
        self.viewport
            .lock()
            .unwrap()
            .push(format!("viewport {lines}"));
    }
    fn scroll_to_newest(&mut self) -> bool {
        self.viewport.lock().unwrap().push("newest".to_string());
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
        let mut lines = Vec::new();
        let contiguous_history = self.join_feed_chunks.then(|| self.feed_history.concat());
        let history = contiguous_history
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(&self.feed_history);
        for history_bytes in history {
            if let Ok(s) = std::str::from_utf8(history_bytes) {
                let mut row = Vec::new();
                for ch in s.chars() {
                    let width = if (ch as u32) > 127 { 2 } else { 1 };
                    let cell = Cell {
                        ch: Some(compact_str::CompactString::from(
                            &*ch.encode_utf8(&mut [0; 4]),
                        )),
                        width,
                        ..Default::default()
                    };
                    row.push(cell);
                    if self.cursor_at_end && width == 2 {
                        row.push(Cell {
                            width: 0,
                            ..Cell::default()
                        });
                    }
                }
                if !row.is_empty() {
                    row.truncate(usize::from(self.cols));
                    lines.push(row);
                }
            }
        }
        // A terminal shows the last `rows` lines; the screen message holds exactly `rows` x `cols`
        // cells.
        let hidden = lines.len().saturating_sub(usize::from(self.rows));
        lines.drain(..hidden);
        let (cursor_row, cursor_col) = if self.cursor_at_end {
            lines
                .last()
                .map(|line| {
                    let col = line
                        .iter()
                        .map(|cell| u16::from(cell.width))
                        .sum::<u16>()
                        .min(self.cols.saturating_sub(1));
                    (lines.len().saturating_sub(1) as u16, col)
                })
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        Screen {
            cols: self.cols,
            rows: self.rows,
            cursor: Cursor {
                col: cursor_col,
                row: cursor_row,
                shape: CursorShape::Block,
                visible: true,
                blinking: false,
                blink_visible: true,
                focused: false,
                preedit: None,
            },
            scrollback: Default::default(),
            background: "#1e1e1e".to_string(),
            lines,
        }
    }

    fn modes(&self) -> Modes {
        self.custom_modes.clone().unwrap_or_default()
    }

    fn reset(&mut self) {
        self.feed_history.clear();
    }
}

#[tokio::test]
async fn vendor_event_is_emitted_only_with_the_owning_surface_id() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = br#"{"surface":"owned-surface","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"owned-surface","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"owned-surface","body":{"operation":"input","actions":[{"type":"insert","text":"\u001b]7;file:///tmp/project\u0007","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let (mut to_serve, serve_in) = tokio::io::duplex(16 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "owned-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let task = tokio::spawn(serve(
        engine_factory,
        serve_in,
        serve_out,
        session_port_factory,
    ));
    to_serve.write_all(input).await.unwrap();
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    let mut found = false;
    for _ in 0..8 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for owned vendor event")
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value["body"]["event"] == "directory" {
            assert_eq!(value["surface"], "owned-surface");
            assert_eq!(value["body"]["uri"], "file:///tmp/project");
            assert_eq!(value["body"]["path"], "/tmp/project");
            found = true;
            break;
        }
    }
    assert!(found, "owned vendor event was not emitted");
    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[derive(Debug, Default, Clone)]
struct Calls {
    opens: Vec<(u16, u16)>,
    directories: Vec<Option<String>>,
    writes: Vec<(String, Vec<u8>)>,
    writes_completed: usize,
    echoes_enqueued: usize,
    timeline: Vec<TimelineEntry>,
    resizes: Vec<(String, u16, u16)>,
    detaches: Vec<String>,
    closes: Vec<String>,
    pendings: Vec<String>,
}

#[derive(Debug, Clone)]
struct TimelineEntry {
    kind: &'static str,
    frame_sequence: Option<u64>,
    glyphs: Option<usize>,
    preedit: Option<String>,
    writes_started: usize,
    writes_completed: usize,
    echoes_enqueued: usize,
}

struct FakeSessionPort {
    session_id: String,
    calls: Arc<Mutex<Calls>>,
    event_tx: mpsc::UnboundedSender<DaemonEvent>,
    event_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<DaemonEvent>>>>,
    ready_events: Arc<Mutex<Option<mpsc::Sender<DaemonEvent>>>>,
    deliver_echo_before_write_completes: bool,
    write_error: Option<String>,
    echo_input: std::sync::atomic::AtomicBool,
}

impl FakeSessionPort {
    fn new(session_id: String, calls: Arc<Mutex<Calls>>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            session_id,
            calls,
            event_tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(Some(event_rx))),
            ready_events: Arc::new(Mutex::new(None)),
            deliver_echo_before_write_completes: false,
            write_error: None,
            echo_input: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn with_ready_echo(mut self) -> Self {
        self.deliver_echo_before_write_completes = true;
        self
    }

    fn with_write_error(mut self, reason: impl Into<String>) -> Self {
        self.write_error = Some(reason.into());
        self
    }

    pub fn push_event(&self, event: DaemonEvent) {
        assert!(
            self.event_tx.send(event).is_ok(),
            "fake session event receiver closed"
        );
    }

    fn echo_input(&self) {
        self.echo_input
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl SessionPort for FakeSessionPort {
    async fn open(&self, request: &ShellRequest, cols: u16, rows: u16) -> Result<String, String> {
        let mut calls = self.calls.lock().unwrap();
        calls.opens.push((cols, rows));
        calls.directories.push(request.directory.clone());
        Ok(self.session_id.clone())
    }

    async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        let sequence = {
            let mut calls = self.calls.lock().unwrap();
            calls.writes.push((session_id.to_string(), data.to_vec()));
            let sequence = calls.writes.len() as i64;
            let writes_started = calls.writes.len();
            let writes_completed = calls.writes_completed;
            let echoes_enqueued = calls.echoes_enqueued;
            calls.timeline.push(TimelineEntry {
                kind: "write-start",
                frame_sequence: None,
                glyphs: None,
                preedit: None,
                writes_started,
                writes_completed,
                echoes_enqueued,
            });
            sequence
        };
        if data == b"\x1b]7;file:///tmp/project\x07" {
            let _ = self.event_tx.send(DaemonEvent::Output {
                session_id: self.session_id.clone(),
                data: data.to_vec(),
                sequence: 0,
                truncated: false,
            });
        }
        if self.echo_input.load(std::sync::atomic::Ordering::SeqCst) {
            let event = DaemonEvent::Output {
                session_id: self.session_id.clone(),
                data: data.to_vec(),
                sequence,
                truncated: false,
            };
            if self.deliver_echo_before_write_completes {
                let ready_events = self.ready_events.lock().unwrap().clone();
                if let Some(ready_events) = ready_events {
                    ready_events
                        .send(event)
                        .await
                        .expect("ready fake PTY echo is observed");
                } else {
                    self.event_tx
                        .send(event)
                        .expect("fake PTY echo is observed");
                }
            } else {
                self.event_tx
                    .send(event)
                    .expect("fake PTY echo is observed");
            }
            let mut calls = self.calls.lock().unwrap();
            calls.echoes_enqueued += 1;
            let writes_started = calls.writes.len();
            let writes_completed = calls.writes_completed;
            let echoes_enqueued = calls.echoes_enqueued;
            calls.timeline.push(TimelineEntry {
                kind: "echo-enqueued",
                frame_sequence: None,
                glyphs: None,
                preedit: None,
                writes_started,
                writes_completed,
                echoes_enqueued,
            });
        }
        let mut calls = self.calls.lock().unwrap();
        calls.writes_completed += 1;
        let writes_started = calls.writes.len();
        let writes_completed = calls.writes_completed;
        let echoes_enqueued = calls.echoes_enqueued;
        calls.timeline.push(TimelineEntry {
            kind: "write-complete",
            frame_sequence: None,
            glyphs: None,
            preedit: None,
            writes_started,
            writes_completed,
            echoes_enqueued,
        });
        self.write_error.clone().map_or(Ok(()), Err)
    }

    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .resizes
            .push((session_id.to_string(), cols, rows));
        Ok(())
    }

    async fn detach(&self, session_id: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .detaches
            .push(session_id.to_string());
        Ok(())
    }

    async fn close(&self, session_id: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .closes
            .push(session_id.to_string());
        Ok(())
    }

    async fn pty_measurement(&self, session_id: &str) -> Result<PtyMeasurement, String> {
        self.calls
            .lock()
            .unwrap()
            .pendings
            .push(session_id.to_string());
        Ok(PtyMeasurement {
            pending: 7,
            written: 123,
        })
    }

    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent> {
        let (tx, rx) = mpsc::channel(10);
        *self.ready_events.lock().unwrap() = Some(tx.clone());
        let mut rx_guard = self.event_rx.lock().await;
        if let Some(mut unbounded_rx) = rx_guard.take() {
            tokio::spawn(async move {
                while let Some(event) = unbounded_rx.recv().await {
                    let _ = tx.send(event).await;
                }
            });
        }
        rx
    }
}

#[tokio::test]
async fn test_a3_input_calls_write() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    // Verifies that write was called with the correct data
    let calls_lock = calls.lock().unwrap();
    assert!(!calls_lock.writes.is_empty(), "write not called");
    assert_eq!(
        calls_lock.writes[0].0, fake_session_id,
        "session_id mismatch"
    );
    assert_eq!(calls_lock.writes[0].1, b"hi", "write data mismatch");
}

// The correlation identifiers of a request reach the write to the session that the request caused:
// input bytes, keys, a paste and an alternate-screen scroll each pass them on, and a request
// without them passes none.
#[tokio::test]
async fn the_cids_of_a_request_reach_the_write_it_causes() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","cids":["b1.1"],"body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","cids":["b1.2","b1.3"],"body":{"operation":"input","actions":[{"type":"key","key":"Up","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","cids":["b1.4"],"body":{"operation":"paste","text":"one"}}
{"surface":"s1","cids":["b1.5"],"body":{"operation":"scroll","lines":1,"col":0,"row":0}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| {
        Box::new(MockEngine::with_modes(Modes {
            alt_screen: true,
            alternate_scroll: true,
            ..Modes::default()
        })) as Box<dyn Engine>
    });
    let port_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new("cids".to_string(), port_calls.clone()))
            as Arc<dyn SessionPort>
    });
    let events = events_test::TestEvents::new();
    let _ = serve_with_events(
        engine_factory,
        reader,
        &mut writer,
        session_port_factory,
        events.handle(),
    )
    .await;
    let calls = calls.lock().unwrap();
    // The surface records each write as the event `pty_write` with the identifiers of the request
    // that caused it.
    let written = events.named("sidecar.vt.pty_write");
    let ids: Vec<Vec<&str>> = written
        .iter()
        .map(|write| {
            write["cids"]
                .as_array()
                .map(|cids| cids.iter().map(|cid| cid.as_str().unwrap()).collect())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            vec!["b1.1"],
            vec!["b1.2", "b1.3"],
            vec!["b1.4"],
            vec!["b1.5"],
            vec![]
        ],
        "{:?} {}",
        calls.writes,
        String::from_utf8_lossy(&writer)
    );
}

// pty.pending answers with the unread input byte count of the open session and the cumulative child
// output byte count that the reader has read. Without a session, it returns an explicit error.
#[tokio::test]
async fn pty_pending_reports_the_session_transport_measurement() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"pty.pending"}}
{"surface":"s2","root":"/tmp","body":{"operation":"pty.pending"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let factory_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "pending-session".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    assert!(
        output
            .contains(r#""body":{"event":"pty.pending","pending":7,"written":123},"surface":"s1""#),
        "an open session did not report its pending input count and read output bytes: {output}"
    );
    assert!(
        output.contains(
            r#""body":{"error":"Session not open","event":"pty.pending"},"surface":"s2""#
        ),
        "a surface without a session must answer an explicit error marked as the pty.pending reply: {output}"
    );
    assert_eq!(
        calls.lock().unwrap().pendings,
        vec!["pending-session".to_string()],
        "pty.pending must query only the open session"
    );
}

#[tokio::test]
async fn an_open_without_a_shell_is_rejected_without_a_session() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let factory_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "no-shell".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    assert!(
        output.contains(r#""reason":"open requires a shell""#),
        "an open without a shell was not rejected: {output}"
    );
    assert!(
        calls.lock().unwrap().opens.is_empty(),
        "an open without a shell started a session"
    );
}

async fn serve_open(open: &str) -> (String, Calls) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = format!(
        "{open}\n{}\n",
        r#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}"#
    );
    let reader = std::io::Cursor::new(input.into_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let factory_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "directory".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let calls = calls.lock().unwrap().clone();
    (String::from_utf8(writer).unwrap(), calls)
}

#[tokio::test]
async fn an_open_passes_its_directory_to_the_session() {
    let (_, calls) = serve_open(
        r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","image":"view","shell":"login","directory":"/tmp"}}"#,
    )
    .await;
    assert_eq!(calls.directories, vec![Some("/tmp".to_string())]);
    let (_, calls) = serve_open(
        r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","image":"view","shell":"login"}}"#,
    )
    .await;
    assert_eq!(calls.directories, vec![None]);
}

#[tokio::test]
async fn an_open_with_a_relative_or_non_text_directory_is_rejected_without_a_session() {
    for directory in [r#""tmp""#, "7", "null"] {
        let (output, calls) = serve_open(&format!(
            r#"{{"surface":"s1","root":"/tmp","body":{{"operation":"open","image":"view","shell":"login","directory":{directory}}}}}"#
        ))
        .await;
        assert!(
            output.contains(r#""reason":"open directory must be an absolute path""#),
            "directory {directory} was not rejected: {output}"
        );
        assert!(
            calls.opens.is_empty(),
            "directory {directory} started a session"
        );
    }
}

#[tokio::test]
async fn native_input_ack_is_not_reported_as_an_unsolicited_event() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let factory_calls = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "input-ack".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"focus","focused":true}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"한","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);

    serve(engine_factory, reader, &mut writer, factory)
        .await
        .unwrap();

    let acknowledgements: Vec<serde_json::Value> = String::from_utf8(writer)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|value: &serde_json::Value| value["body"]["ack"] == true)
        .collect();
    assert_eq!(
        acknowledgements.len(),
        2,
        "focus and compose must each acknowledge once"
    );
    for acknowledgement in acknowledgements {
        assert!(
            acknowledgement["body"]["event"].is_null(),
            "native input ACK must not become an unsolicited event: {acknowledgement}"
        );
    }
}

#[tokio::test]
async fn each_native_preedit_update_presents_a_fresh_terminal_frame() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let factory_calls = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "preedit-frame".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"focus","focused":true}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"ㅎ","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":3}}}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"하","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":4}}}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"한","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":5}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);

    serve(engine_factory, reader, &mut writer, factory)
        .await
        .unwrap();

    let frames = String::from_utf8(writer)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["body"]["image"]["sequence"].is_number())
        .count();
    assert!(
        frames >= 5,
        "initial frame, focus frame, and all three successive preedit frames must be presented; got {frames}"
    );
}

#[tokio::test]
async fn one_input_request_applies_resulting_preedit_before_immediate_pty_echo() {
    let mut grid = ScreenGrid::default();
    let engine_factory =
        Arc::new(|| Box::new(MockEngine::new().with_cursor_at_end()) as Box<dyn Engine>);

    // The split-request control starts from six committed glyphs plus two marked glyphs.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("split-ime".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory.clone(), serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let initial = next_image_envelope(&mut lines).await;
    let mut sequence = initial["body"]["image"]["sequence"].as_u64().unwrap();
    acknowledge_image(&mut to_serve, &initial).await;
    let mut acknowledgements = 0;
    port.echo_input();
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"\u314f\u314f\u314f\u314f\u314f\u314f","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
        )
        .await
        .unwrap();
    let (next, glyphs, _, _, _, _, _) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    {
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.writes.len(),
            1,
            "the input transaction writes once: {calls:?}"
        );
        assert_eq!(
            calls.echoes_enqueued, 1,
            "the fake echo was queued during the write: {calls:?}"
        );
    }
    assert_eq!(
        glyphs, 6,
        "the six-character input document is the controlled baseline"
    );
    sequence = next;
    to_serve.write_all(format!("{}\n", r#"{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"ㅏㅏ","markedRange":{"location":0,"length":2},"selectedRange":{"location":2,"length":0},"replacementRange":null,"attributed":false}}}"#).as_bytes()).await.unwrap();
    let (next, glyphs, _, _, _, _, _) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        glyphs, 8,
        "the marked suffix adds two glyphs to the six-character document"
    );
    sequence = next;
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"\u314f\u314f","replacementRange":null,"attributed":false}],"preedit":{"text":"\u314f\u314f","markedRange":{"location":0,"length":2},"selectedRange":{"location":2,"length":0},"replacementRange":null,"attributed":false}}}
"#,
        )
        .await
        .unwrap();
    let (split_sequence, split_echo_glyphs, _, _, _, _, screen) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        split_echo_glyphs, 10,
        "separate byte delivery and immediate echo retain old preedit glyphs: {screen}"
    );
    to_serve.write_all(format!("{}\n", r#"{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}"#).as_bytes()).await.unwrap();
    let (_, cleared_glyphs, _, _, _, _, _) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        split_sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        cleared_glyphs, 8,
        "clearing the split request removes only the old marked suffix"
    );
    drop(to_serve);
    task.await.unwrap().unwrap();

    // A fresh surface starts from the same six-plus-two state for the combined request.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("combined-ime".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let observer_calls = calls.clone();
    // The observer sees each message at each submission stage, so the grid applies a message once and the later stages of the
    // same message take that result.
    let observer_grid = Mutex::new((ScreenGrid::default(), None::<(String, serde_json::Value)>));
    let observer: OutputSubmissionObserver = Arc::new(move |stage, message| {
        if serde_json::from_str::<serde_json::Value>(message).is_err() {
            return;
        }
        let value = {
            let mut state = observer_grid.lock().unwrap();
            let (grid, last) = &mut *state;
            match last {
                Some((line, value)) if line == message => value.clone(),
                _ => {
                    let value = grid.apply_line(message);
                    *last = Some((message.to_string(), value.clone()));
                    value
                }
            }
        };
        let body = &value["body"];
        let is_frame = body["event"] == "screen";
        let (frame_sequence, glyphs, preedit) = if is_frame {
            (
                body["frame"]["sequence"].as_u64(),
                Some(rendered_glyph_count(&value)),
                body["cursor"]["preedit"]["text"]
                    .as_str()
                    .map(str::to_owned),
            )
        } else {
            (None, None, None)
        };
        let kind = match (stage, is_frame, body["ack"] == true) {
            (OutputSubmissionStage::Attempt, true, _) => "frame-attempt",
            (OutputSubmissionStage::Queued, true, _) => "frame-queued",
            (OutputSubmissionStage::Failed, true, _) => "frame-failed",
            (OutputSubmissionStage::Attempt, false, true) => "ack-attempt",
            (OutputSubmissionStage::Queued, false, true) => "ack-queued",
            (OutputSubmissionStage::Failed, false, true) => "ack-failed",
            (OutputSubmissionStage::Attempt, false, false) => "output-attempt",
            (OutputSubmissionStage::Queued, false, false) => "output-queued",
            (OutputSubmissionStage::Failed, false, false) => "output-failed",
        };
        let mut calls = observer_calls.lock().unwrap();
        let writes_started = calls.writes.len();
        let writes_completed = calls.writes_completed;
        let echoes_enqueued = calls.echoes_enqueued;
        calls.timeline.push(TimelineEntry {
            kind,
            frame_sequence,
            glyphs,
            preedit,
            writes_started,
            writes_completed,
            echoes_enqueued,
        });
    });
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve_with_submission_observer(
        engine_factory,
        serve_in,
        serve_out,
        factory,
        events_test::TestEvents::new().handle(),
        observer,
    ));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s2","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s2","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let initial = next_image_envelope(&mut lines).await;
    let mut sequence = initial["body"]["image"]["sequence"].as_u64().unwrap();
    acknowledge_image(&mut to_serve, &initial).await;
    let mut acknowledgements = 0;
    port.echo_input();
    to_serve
        .write_all(
            br#"{"surface":"s2","body":{"operation":"input","actions":[{"type":"insert","text":"\u314f\u314f\u314f\u314f\u314f\u314f","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
        )
        .await
        .unwrap();
    let (next, glyphs, _, _, _, _, _) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        glyphs, 6,
        "the combined case begins from the same six committed glyphs"
    );
    sequence = next;
    to_serve.write_all(format!("{}\n", r#"{"surface":"s2","body":{"operation":"input","actions":[],"preedit":{"text":"ㅏㅏ","markedRange":{"location":0,"length":2},"selectedRange":{"location":2,"length":0},"replacementRange":null,"attributed":false}}}"#).as_bytes()).await.unwrap();
    let (next, glyphs, _, _, _, _, _) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        glyphs, 8,
        "the combined case has the same two-glyph marked suffix"
    );
    sequence = next;

    let sequence_before_combined = sequence;
    let timeline_start_before_combined = calls.lock().unwrap().timeline.len();
    to_serve.write_all(br#"{"surface":"s2","body":{"operation":"input","actions":[{"type":"insert","text":"\u314f\u314f","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":{"location":0,"length":0},"replacementRange":null,"attributed":false}}}
"#).await.unwrap();
    let mut frames = Vec::new();
    loop {
        let (frame_sequence, glyphs, ack_count, writes, completed, echoes, screen) =
            next_measured_frame_after(
                &mut lines,
                &mut to_serve,
                sequence,
                &mut acknowledgements,
                &calls,
                &mut grid,
            )
            .await;
        frames.push((
            frame_sequence,
            glyphs,
            ack_count,
            writes,
            completed,
            echoes,
            screen["body"]["cursor"]["preedit"]["text"].clone(),
        ));
        sequence = frame_sequence;
        if glyphs >= 8 && screen["body"]["cursor"]["preedit"]["text"] == "" {
            break;
        }
    }
    assert!(
        !frames.is_empty(),
        "combined input produced no measured image: {frames:?}"
    );
    assert!(
        frames.iter().all(|frame| frame.1 <= 8),
        "combined input must not render the retained preedit twice: split-request baseline={split_echo_glyphs}, frames={frames:?}"
    );
    assert_eq!(
        frames.last().unwrap().1,
        8,
        "the echoed commit is eight visible glyphs with no preedit overlay: {frames:?}"
    );
    let timeline = calls.lock().unwrap().timeline[timeline_start_before_combined..].to_vec();
    let combined_write_start = timeline
        .iter()
        .position(|event| event.kind == "write-start" && event.writes_started == 2)
        .expect("combined bytes write start is recorded");
    let combined_echo = timeline
        .iter()
        .position(|event| event.kind == "echo-enqueued" && event.echoes_enqueued == 2)
        .expect("the fake immediately enqueues its PTY echo");
    let combined_write_complete = timeline
        .iter()
        .position(|event| event.kind == "write-complete" && event.writes_completed == 2)
        .expect("combined bytes write completion is recorded");
    assert!(
        combined_write_start < combined_echo && combined_echo < combined_write_complete,
        "the fake echo is enqueued during the combined write, before its completion: {timeline:?}"
    );
    let resulting_frames = timeline
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            event.kind == "frame-attempt"
                && event
                    .frame_sequence
                    .is_some_and(|frame| frame > sequence_before_combined)
        })
        .collect::<Vec<_>>();
    assert!(
        !resulting_frames.is_empty(),
        "combined request submits no screen frame: {timeline:?}"
    );
    assert!(
        resulting_frames
            .iter()
            .all(|(index, _)| *index > combined_write_start),
        "a frame with the new compose snapshot was submitted before the combined bytes write started: split-request baseline={split_echo_glyphs}, timeline={timeline:?}"
    );
    assert!(
        resulting_frames
            .iter()
            .all(|(_, event)| event.preedit.as_deref() == Some("")),
        "each combined frame uses the resulting empty preedit snapshot: {timeline:?}"
    );
    assert!(
        resulting_frames
            .iter()
            .all(|(_, event)| event.writes_completed == 2),
        "each frame submission follows combined write completion: {timeline:?}"
    );
    assert!(
        resulting_frames
            .iter()
            .all(|(_, event)| event.glyphs.unwrap_or(usize::MAX) <= 8),
        "combined input must not render the retained preedit twice: {timeline:?}"
    );
    assert!(
        resulting_frames
            .iter()
            .any(|(_, event)| event.glyphs == Some(8)),
        "the frame after the immediate fake echo must show eight committed glyphs: {timeline:?}"
    );
    assert!(
        resulting_frames
            .iter()
            .all(|(index, event)| timeline[index + 1..]
                .iter()
                .any(|queued| queued.kind == "frame-queued"
                    && queued.frame_sequence == event.frame_sequence)),
        "every frame submission attempt must reach the output queue: {timeline:?}"
    );
    eprintln!("combined request submission timeline: {timeline:?}");

    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn appkit_cancel_operation_is_one_ordered_escape_and_unknown_selectors_are_atomic() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "escape-transaction".to_string(),
            port_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","cids":["initial-preedit"],"body":{"operation":"input","actions":[],"preedit":{"text":"ㅎ","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
{"surface":"s1","cids":["commit-escape"],"body":{"operation":"input","actions":[{"type":"insert","text":"한","replacementRange":null,"attributed":false},{"type":"command","selector":"cancelOperation:"}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
{"surface":"s1","cids":["cancel-only"],"body":{"operation":"input","actions":[{"type":"command","selector":"cancelOperation:"}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","cids":["key-escape"],"body":{"operation":"input","actions":[{"type":"key","key":"Escape","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","cids":["unknown-selector"],"body":{"operation":"input","actions":[{"type":"insert","text":"x","replacementRange":null,"attributed":false},{"type":"command","selector":"unsupportedNative:"}],"preedit":{"text":"changed","markedRange":{"location":0,"length":7},"selectedRange":{"location":7,"length":0},"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve(
            engine_factory,
            std::io::Cursor::new(input.as_bytes()),
            &mut writer,
            session_port_factory,
        ),
    )
    .await
    .expect("Escape input transactions complete within 2 s")
    .unwrap();

    let messages = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let reply = |cid: &str| {
        messages
            .iter()
            .find(|message| message["cids"] == serde_json::json!([cid]))
            .unwrap_or_else(|| panic!("missing reply for {cid}: {messages:?}"))
    };
    assert_eq!(reply("initial-preedit")["body"]["ack"], true);
    assert_eq!(reply("commit-escape")["body"]["ack"], true);
    assert_eq!(reply("cancel-only")["body"]["ack"], true);
    assert_eq!(reply("key-escape")["body"]["ack"], true);
    assert_eq!(reply("unknown-selector")["body"]["error"], "invalidParams");
    assert!(
        reply("unknown-selector")["body"]["reason"]
            .as_str()
            .unwrap()
            .contains("unsupported native command selector: unsupportedNative:")
    );
    assert_eq!(
        calls.lock().unwrap().writes,
        [
            (
                "escape-transaction".to_string(),
                "한\x1b".as_bytes().to_vec()
            ),
            ("escape-transaction".to_string(), b"\x1b".to_vec()),
            ("escape-transaction".to_string(), b"\x1b".to_vec()),
        ],
        "each accepted input transaction writes its ordered bytes exactly once"
    );
    let screens = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .collect::<Vec<_>>();
    assert_eq!(
        screens.len(),
        3,
        "each screen read is answered with one screen: {screens:?}"
    );
    assert_eq!(screens[0]["body"]["cursor"]["preedit"]["text"], "ㅎ");
    assert_eq!(screens[1]["body"]["cursor"]["preedit"]["text"], "");
    assert_eq!(
        screens[2]["body"]["cursor"]["preedit"]["text"], "",
        "the refused transaction leaves the preedit unchanged: {screens:?}"
    );
}

#[tokio::test]
async fn one_ordered_input_transaction_writes_actions_once_and_applies_final_preedit() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "atomic-input".to_string(),
            port_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","cids":["native-transaction"],"body":{"operation":"input","actions":[{"type":"insert","text":"x","replacementRange":null,"attributed":true},{"type":"key","key":"Up","shift":false,"alt":false,"ctrl":false},{"type":"command","selector":"deleteBackward:"},{"type":"focus","focused":true}],"preedit":{"text":"ㅏㅏㅏ","markedRange":{"location":0,"length":3},"selectedRange":{"location":3,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| {
        Box::new(MockEngine::with_modes(Modes {
            focus_in_out: true,
            ..Modes::default()
        })) as Box<dyn Engine>
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve(
            engine_factory,
            std::io::Cursor::new(input.as_bytes()),
            &mut writer,
            session_port_factory,
        ),
    )
    .await
    .expect("the ordered transaction is handled within 2 s")
    .unwrap();

    let messages = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let transaction_replies = messages
        .iter()
        .filter(|message| message["cids"] == serde_json::json!(["native-transaction"]))
        .collect::<Vec<_>>();
    let summary = transaction_replies
        .iter()
        .map(|message| {
            serde_json::json!({
                "ack": message["body"]["ack"],
                "error": message["body"]["error"],
                "reason": message["body"]["reason"]
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transaction_replies.len(),
        1,
        "one transaction gets one reply: {summary:?}"
    );
    assert_eq!(
        transaction_replies[0]["body"]["ack"], true,
        "the transaction must be accepted: {summary:?}"
    );
    assert_eq!(
        calls.lock().unwrap().writes,
        [("atomic-input".to_string(), b"x\x1b[A\x7f\x1b[I".to_vec())],
        "ordered write-producing actions are encoded into one PTY write"
    );
    let screens = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .collect::<Vec<_>>();
    assert_eq!(
        screens.len(),
        1,
        "only the screen read is answered with a screen: {screens:?}"
    );
    assert!(
        screens
            .iter()
            .all(|message| message["body"]["cursor"]["preedit"]["text"] == "ㅏㅏㅏ"),
        "the transaction applies its final preedit: {screens:?}"
    );
    assert!(
        screens[0]["body"]["cells"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cell| cell["grapheme"] != "x"),
        "without PTY echo, the inserted text remains preedit rather than fabricated committed output"
    );
}

#[tokio::test]
async fn ready_pty_echo_is_presented_with_the_transaction_preedit() {
    let mut grid = ScreenGrid::default();
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port =
        Arc::new(FakeSessionPort::new("ready-echo".to_string(), calls.clone()).with_ready_echo());
    port.echo_input();
    let session_port = port.clone();
    let session_port_factory = Arc::new(move || session_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| {
        Box::new(
            MockEngine::new()
                .with_cursor_at_end()
                .with_contiguous_feed(),
        ) as Box<dyn Engine>
    });
    let frame_records = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let observer_frames = frame_records.clone();
    let observer: OutputSubmissionObserver = Arc::new(move |stage, line| {
        if stage == OutputSubmissionStage::Queued
            && let Ok(message) = serde_json::from_str::<serde_json::Value>(line)
            && message["body"]["event"] == "screen"
        {
            observer_frames.lock().unwrap().push(message);
        }
    });
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve_with_submission_observer(
        engine_factory,
        serve_in,
        serve_out,
        session_port_factory,
        events_test::TestEvents::new().handle(),
        observer,
    ));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    let open = serde_json::json!({"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}});
    let configure = serde_json::json!({"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}});
    to_serve
        .write_all(format!("{}\n{}\n", open, configure).as_bytes())
        .await
        .unwrap();
    let initial = next_image_envelope(&mut lines).await;
    let mut sequence = initial["body"]["image"]["sequence"].as_u64().unwrap();
    acknowledge_image(&mut to_serve, &initial).await;
    let mut acknowledgements = 0;

    port.push_event(DaemonEvent::Output {
        session_id: "ready-echo".to_string(),
        data: "가가가가".as_bytes().to_vec(),
        sequence: 1,
        truncated: false,
    });
    let (next, _, _, _, _, _, baseline) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        baseline["body"]["cursor"]["caret"],
        serde_json::json!({"col":8,"row":0})
    );
    sequence = next;

    let preedit = serde_json::json!({"text":"ㅏㅏㅏ","markedRange":{"location":0,"length":3},"selectedRange":{"location":3,"length":0},"replacementRange":null,"attributed":true});
    let marked = serde_json::json!({"surface":"s1","body":{"operation":"input","actions":[],"preedit":preedit}});
    to_serve
        .write_all(format!("{marked}\n").as_bytes())
        .await
        .unwrap();
    let (next, _, _, _, _, _, marked_screen) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        marked_screen["body"]["cursor"]["caret"],
        serde_json::json!({"col":14,"row":0})
    );
    sequence = next;

    let frames_before_transaction = frame_records.lock().unwrap().len();
    let acknowledgements_before_transaction = acknowledgements;
    let transaction = serde_json::json!({
        "surface":"s1",
        "body":{"operation":"input","actions":[{"type":"insert","text":"ㅏ","replacementRange":null,"attributed":true}],"preedit":preedit}
    });
    to_serve
        .write_all(format!("{transaction}\n").as_bytes())
        .await
        .unwrap();
    let (_next, _, _, _, _, _, committed) = next_measured_frame_after(
        &mut lines,
        &mut to_serve,
        sequence,
        &mut acknowledgements,
        &calls,
        &mut grid,
    )
    .await;
    assert_eq!(
        committed["body"]["cursor"]["caret"],
        serde_json::json!({"col":16,"row":0})
    );
    assert_eq!(committed["body"]["cursor"]["preedit"]["text"], "ㅏㅏㅏ");
    // The acknowledgement of the transaction arrives before or after its frame.
    if acknowledgements == acknowledgements_before_transaction {
        let reply: serde_json::Value =
            serde_json::from_str(&next_line_except_frame_events(&mut lines).await).unwrap();
        assert_eq!(reply["body"]["ack"], true);
    }
    let transaction_frames = frame_records.lock().unwrap()[frames_before_transaction..].to_vec();
    assert_eq!(
        transaction_frames.len(),
        1,
        "no intermediate caret frame is published: {transaction_frames:?}"
    );
    assert_eq!(
        transaction_frames[0]["body"]["cursor"]["caret"],
        serde_json::json!({"col":16,"row":0})
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn native_non_character_keys_omit_text_and_char_requires_it() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "native-key-shapes".to_string(),
            port_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let preedit = serde_json::json!({
        "text":"ㅎ",
        "markedRange":{"location":0,"length":1},
        "selectedRange":{"location":1,"length":0},
        "replacementRange":null,
        "attributed":true
    });
    let requests = [
        serde_json::json!({"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}),
        serde_json::json!({"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}),
        serde_json::json!({
            "surface":"s1","cids":["valid-omitted-text"],
            "body":{"operation":"input","actions":[
                {"type":"key","key":"Enter","shift":false,"alt":false,"ctrl":false},
                {"type":"key","key":"Up","shift":false,"alt":false,"ctrl":false},
                {"type":"key","key":"Backspace","shift":false,"alt":false,"ctrl":false},
                {"type":"key","key":"Char","text":"x","shift":false,"alt":false,"ctrl":false}
            ],"preedit":preedit}
        }),
        serde_json::json!({
            "surface":"s1","cids":["char-missing-text"],
            "body":{"operation":"input","actions":[{"type":"key","key":"Char","shift":false,"alt":false,"ctrl":false}],"preedit":preedit}
        }),
        serde_json::json!({
            "surface":"s1","cids":["char-null-text"],
            "body":{"operation":"input","actions":[{"type":"key","key":"Char","text":null,"shift":false,"alt":false,"ctrl":false}],"preedit":preedit}
        }),
        serde_json::json!({
            "surface":"s1","cids":["nonchar-has-text"],
            "body":{"operation":"input","actions":[{"type":"key","key":"Up","text":"","shift":false,"alt":false,"ctrl":false}],"preedit":preedit}
        }),
        serde_json::json!({"surface":"s1","body":{"operation":"screen.read"}}),
    ];
    let input = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect::<String>();
    let mut writer = Vec::new();
    serve(
        Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>),
        std::io::Cursor::new(input.into_bytes()),
        &mut writer,
        session_port_factory,
    )
    .await
    .unwrap();

    let messages = String::from_utf8(writer)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    let reply_for = |cid: &str| {
        messages
            .iter()
            .find(|message| message["cids"] == serde_json::json!([cid]))
            .unwrap_or_else(|| panic!("missing correlated reply for {cid}: {messages:?}"))
    };
    assert_eq!(reply_for("valid-omitted-text")["body"]["ack"], true);
    for cid in ["char-missing-text", "char-null-text", "nonchar-has-text"] {
        assert_eq!(reply_for(cid)["body"]["error"], "invalidParams", "{cid}");
    }
    assert_eq!(
        calls.lock().unwrap().writes,
        [("native-key-shapes".to_string(), b"\r\x1b[A\x7fx".to_vec())]
    );
    let screen = messages
        .iter()
        .rev()
        .find(|message| message["body"]["event"] == "screen")
        .unwrap();
    assert_eq!(screen["body"]["cursor"]["preedit"]["text"], "ㅎ");
}

#[tokio::test]
async fn input_write_failure_reports_submitted_batch_and_unknown_progress() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(
        FakeSessionPort::new("failed-write".to_string(), calls.clone())
            .with_write_error("controlled PTY failure"),
    );
    let session_port = port.clone();
    let session_port_factory = Arc::new(move || session_port.clone() as Arc<dyn SessionPort>);
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","cids":["write-failure"],"body":{"operation":"input","actions":[{"type":"insert","text":"x","replacementRange":null,"attributed":true}],"preedit":{"text":"ㅎ","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
"#;
    let mut writer = Vec::new();
    serve(
        Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>),
        std::io::Cursor::new(input.as_bytes()),
        &mut writer,
        session_port_factory,
    )
    .await
    .unwrap();

    let replies = String::from_utf8(writer)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["cids"] == serde_json::json!(["write-failure"]))
        .collect::<Vec<_>>();
    assert_eq!(
        replies.len(),
        1,
        "the failed transaction receives one reply: {replies:?}"
    );
    let error = replies[0]["body"]["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("submitted action batch"),
        "the attempted write is explicit: {error}"
    );
    assert!(
        error.contains("progress is unknown"),
        "partial write progress is not guessed: {error}"
    );
    assert!(
        error.contains("controlled PTY failure"),
        "the underlying failure reason is preserved: {error}"
    );
    assert_eq!(
        calls.lock().unwrap().writes.len(),
        1,
        "the failed batch is not retried"
    );
}

#[tokio::test]
async fn an_invalid_late_input_action_rejects_the_transaction_before_any_side_effect() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "invalid-atomic-input".to_string(),
            port_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","cids":["compose-only"],"body":{"operation":"input","actions":[],"preedit":{"text":"ㅎ","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","cids":["invalid-late"],"body":{"operation":"input","actions":[{"type":"insert","text":"x","replacementRange":null,"attributed":true},{"type":"key","key":"Unsupported","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"changed","markedRange":{"location":0,"length":7},"selectedRange":{"location":7,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve(
            engine_factory,
            std::io::Cursor::new(input.as_bytes()),
            &mut writer,
            session_port_factory,
        ),
    )
    .await
    .expect("valid and invalid transactions complete within 2 s")
    .unwrap();

    let messages = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let valid_reply = messages
        .iter()
        .find(|message| message["cids"] == serde_json::json!(["compose-only"]));
    assert_eq!(
        valid_reply.unwrap()["body"]["ack"],
        true,
        "empty action list is a valid compose report"
    );
    let invalid_reply = messages
        .iter()
        .filter(|message| message["cids"] == serde_json::json!(["invalid-late"]))
        .collect::<Vec<_>>();
    assert_eq!(
        invalid_reply.len(),
        1,
        "the invalid transaction has one reply: {invalid_reply:?}"
    );
    assert_eq!(
        invalid_reply[0]["body"]["error"], "invalidParams",
        "the late unsupported key is rejected: {invalid_reply:?}"
    );
    assert!(
        calls.lock().unwrap().writes.is_empty(),
        "the earlier valid insert action was not written"
    );
    let screens = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .collect::<Vec<_>>();
    assert_eq!(
        screens.len(),
        1,
        "only the screen read is answered with a screen: {screens:?}"
    );
    assert!(
        screens
            .iter()
            .all(|message| message["body"]["cursor"]["preedit"]["text"] == "ㅎ"),
        "the invalid transaction leaves the previous preedit unchanged: {screens:?}"
    );
}

#[tokio::test]
async fn an_invalid_marked_range_is_rejected_before_preedit_changes_surface_state() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let factory_calls = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "invalid-marked-range".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"한","markedRange":{"location":1,"length":1},"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"focus","focused":true}],"preedit":{"text":"한","markedRange":{"location":0,"length":1},"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve(engine_factory, reader, &mut writer, factory),
    )
    .await
    .expect("invalid marked range and valid redraw complete within 2 s")
    .unwrap();

    let messages: Vec<serde_json::Value> = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("message JSON"))
        .collect();
    let summary: Vec<serde_json::Value> = messages
        .iter()
        .map(|message| {
            serde_json::json!({
                "event": message["body"]["event"],
                "error": message["body"]["error"],
                "reason": message["body"]["reason"],
                "imageSequence": message["body"]["image"]["sequence"],
                "ack": message["body"]["ack"]
            })
        })
        .collect();
    assert!(
        messages.iter().any(|message| {
            message["body"]["error"] == "invalidParams"
                && message["body"]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("markedRange"))
        }),
        "the out-of-text markedRange was not rejected at the input boundary: {summary:?}"
    );
    let frames = messages
        .iter()
        .filter(|message| message["body"]["image"]["sequence"].is_number())
        .count();
    assert_eq!(
        frames, 2,
        "the later valid focus redraw must present after rejecting markedRange: {summary:?}"
    );
}

#[tokio::test]
async fn malformed_compose_with_bytes_rejects_without_writing_or_replying_twice() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let calls_for_factory = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "malformed-compose-bytes".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"input","actions":[],"preedit":{"text":"ㅏ","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
{"surface":"s1","cids":["malformed-compose-bytes"],"body":{"operation":"input","actions":[{"type":"insert","text":"x","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":{"location":0,"length":1},"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve(
            engine_factory,
            std::io::Cursor::new(input.as_bytes()),
            &mut writer,
            factory,
        ),
    )
    .await
    .expect("malformed combined input returns within 2 s")
    .unwrap();
    let messages: Vec<serde_json::Value> = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("output is JSON"))
        .collect();
    let replies = messages
        .iter()
        .filter(|message| message["cids"] == serde_json::json!(["malformed-compose-bytes"]))
        .collect::<Vec<_>>();
    let screen_preedit = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .map(|message| message["body"]["cursor"]["preedit"]["text"].clone())
        .collect::<Vec<_>>();
    let image_sequences = messages
        .iter()
        .filter_map(|message| message["body"]["image"]["sequence"].as_u64())
        .collect::<Vec<_>>();
    let last_screen_frame = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .filter_map(|message| message["body"]["frame"]["sequence"].as_u64())
        .next_back();
    let writes = calls.lock().unwrap().writes.clone();
    let mut violations = Vec::new();
    if replies.len() != 1 || replies[0]["body"]["error"] != "invalidParams" {
        violations.push(format!("request replies={replies:?}"));
    }
    if !writes.is_empty() {
        violations.push(format!("session writes={writes:?}"));
    }
    if screen_preedit.last() != Some(&serde_json::json!("ㅏ")) {
        violations.push(format!("preedit snapshots={screen_preedit:?}"));
    }
    if image_sequences != [1, 2] || last_screen_frame != Some(2) {
        violations.push(format!(
            "image sequences={image_sequences:?}, last screen frame={last_screen_frame:?}"
        ));
    }
    assert!(
        violations.is_empty(),
        "invalid compose must reject the whole input without writing or changing preedit: {violations:?}; output={messages:?}"
    );
}

#[tokio::test]
async fn malformed_bytes_with_compose_do_not_change_preedit_or_reply_twice() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let calls_for_factory = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "malformed-bytes-compose".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","cids":["malformed-bytes-compose"],"body":{"operation":"input","actions":[{"type":"insert","text":7,"replacementRange":null,"attributed":false}],"preedit":{"text":"ㅏ","markedRange":{"location":0,"length":1},"selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        serve(
            engine_factory,
            std::io::Cursor::new(input.as_bytes()),
            &mut writer,
            factory,
        ),
    )
    .await
    .expect("malformed combined input and screen read return within 2 s")
    .unwrap();
    let messages: Vec<serde_json::Value> = String::from_utf8(writer)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("output is JSON"))
        .collect();
    let errors = messages
        .iter()
        .filter(|message| message["body"]["error"].is_string())
        .collect::<Vec<_>>();
    let screen_preedit = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .map(|message| message["body"]["cursor"]["preedit"]["text"].clone())
        .collect::<Vec<_>>();
    let image_sequences = messages
        .iter()
        .filter_map(|message| message["body"]["image"]["sequence"].as_u64())
        .collect::<Vec<_>>();
    let last_screen_frame = messages
        .iter()
        .filter(|message| message["body"]["event"] == "screen")
        .filter_map(|message| message["body"]["frame"]["sequence"].as_u64())
        .next_back();
    let writes = calls.lock().unwrap().writes.clone();
    let mut violations = Vec::new();
    if errors.len() != 1
        || !errors[0]["body"]["error"]
            .as_str()
            .is_some_and(|error| error == "invalidParams")
        || errors[0]["cids"] != serde_json::json!(["malformed-bytes-compose"])
    {
        violations.push(format!("errors={errors:?}"));
    }
    if !writes.is_empty() {
        violations.push(format!("session writes={writes:?}"));
    }
    if screen_preedit.iter().any(|preedit| preedit == "ㅏ") {
        violations.push(format!("preedit snapshots={screen_preedit:?}"));
    }
    if image_sequences != [1] || last_screen_frame != Some(1) {
        violations.push(format!(
            "image sequences={image_sequences:?}, last screen frame={last_screen_frame:?}"
        ));
    }
    assert!(
        violations.is_empty(),
        "malformed insert text must reject the whole input without compose mutation: {violations:?}; output={messages:?}"
    );
}

#[tokio::test]
async fn test_paste_writes_ordered_text_using_bracketed_mode() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-paste-session".to_string();
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"paste","text":"one\ntwo"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let engine_factory = Arc::new(|| {
        Box::new(MockEngine::with_modes(Modes {
            bracketed_paste: true,
            ..Modes::default()
        })) as Box<dyn Engine>
    });
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let calls_lock = calls.lock().unwrap();
    assert_eq!(calls_lock.writes[0].1, b"\x1b[200~one\ntwo\x1b[201~");
}

#[tokio::test]
async fn test_paste_rejects_non_text_payload() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"paste","kind":"png","data":"iVBORw=="}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    assert!(
        String::from_utf8(writer)
            .unwrap()
            .contains("paste requires text string")
    );
    assert!(calls.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn test_bracketed_paste_rejects_embedded_terminator_without_writing() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"operation":"paste","text":"before\u001b[201~after"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| {
        Box::new(MockEngine::with_modes(Modes {
            bracketed_paste: true,
            ..Modes::default()
        })) as Box<dyn Engine>
    });
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    assert!(output.contains("bracketed-paste terminator"));
    assert!(calls.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn test_clipboard_reject_is_an_explicit_protocol_event() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","body":{"operation":"clipboard.reject","requestId":7,"reason":"denied"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    assert!(output.contains("clipboard.rejected"));
    assert!(output.contains("\"requestId\":7"));
    assert!(output.contains("\"reason\":\"denied\""));
}

async fn serve_copy(selection: Option<&str>) -> String {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"copy"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let selection = selection.map(str::to_string);
    let engine_factory = Arc::new(move || {
        let mut engine = MockEngine::new();
        engine.selection = selection.clone();
        Box::new(engine) as Box<dyn Engine>
    });
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new("copy".to_string(), calls.clone())) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    String::from_utf8(writer).unwrap()
}

#[tokio::test]
async fn a_copy_request_sends_the_current_selection_text_or_reports_none() {
    let output = serve_copy(Some("selected")).await;
    assert!(
        output.contains(r#""event":"copy""#)
            && output.contains(r#""text":"selected""#)
            && output.contains(r#""userInitiated":true"#),
        "a copy request did not send the selection text: {output}"
    );
    let output = serve_copy(None).await;
    assert!(
        output.contains(r#""event":"copy""#) && output.contains(r#""copied":false"#),
        "a copy request without a selection did not report that nothing was copied: {output}"
    );
    assert!(!output.contains("Unknown operation: copy"), "{output}");
}

async fn serve_scroll(modes: Option<Modes>, requests: &str) -> (String, Vec<String>, Vec<Vec<u8>>) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = format!(
        "{}\n{}\n{}",
        r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}"#,
        r#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}"#,
        requests
    );
    let reader = std::io::Cursor::new(input.into_bytes());
    let mut writer = Vec::new();
    let viewport = Arc::new(Mutex::new(Vec::new()));
    let engine_viewport = viewport.clone();
    let engine_factory = Arc::new(move || {
        let mut engine = match modes.clone() {
            Some(modes) => MockEngine::with_modes(modes),
            None => MockEngine::new(),
        };
        engine.viewport = engine_viewport.clone();
        Box::new(engine) as Box<dyn Engine>
    });
    let port_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "scroll".to_string(),
            port_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let writes = calls
        .lock()
        .unwrap()
        .writes
        .iter()
        .map(|(_, bytes)| bytes.clone())
        .collect();
    let viewport = viewport.lock().unwrap().clone();
    (String::from_utf8(writer).unwrap(), viewport, writes)
}

#[tokio::test]
async fn a_scroll_moves_the_primary_viewport_and_input_returns_it_to_the_newest_output() {
    // This scroll happens after the host answers the first presentation. A scroll before the answer
    // appears as one screen after the answer arrives.
    let (output, viewport, writes) = serve_scroll(
        None,
        r#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"scroll","lines":3,"col":2,"row":1}}
{"surface":"s1","body":{"operation":"screen.read"}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#,
    )
    .await;
    assert_eq!(
        viewport,
        vec!["viewport 3".to_string(), "newest".to_string()],
        "{output}"
    );
    assert!(
        output.contains(r#""scrollback""#),
        "the screen read after a scroll carries scrollback: {output}"
    );
    assert_eq!(
        writes,
        vec![b"hi".to_vec()],
        "a primary-screen scroll writes nothing to the shell"
    );
}

#[tokio::test]
async fn a_scroll_with_mouse_reporting_writes_wheel_buttons_at_the_pointer_cell() {
    let modes = Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (output, viewport, writes) = serve_scroll(
        Some(modes),
        r#"{"surface":"s1","body":{"operation":"scroll","lines":2,"col":4,"row":6}}
{"surface":"s1","body":{"operation":"scroll","lines":-1,"col":4,"row":6}}
"#,
    )
    .await;
    assert!(
        viewport.iter().all(|call| !call.starts_with("viewport")),
        "{viewport:?}"
    );
    assert_eq!(
        writes,
        vec![
            b"\x1b[<64;5;7M\x1b[<64;5;7M".to_vec(),
            b"\x1b[<65;5;7M".to_vec()
        ],
        "{output}"
    );
}

#[tokio::test]
async fn a_scroll_on_the_alternate_screen_writes_cursor_keys() {
    let modes = Modes {
        alt_screen: true,
        alternate_scroll: true,
        ..Modes::default()
    };
    let (output, viewport, writes) = serve_scroll(
        Some(modes),
        r#"{"surface":"s1","body":{"operation":"scroll","lines":2,"col":0,"row":0}}
{"surface":"s1","body":{"operation":"scroll","lines":-1,"col":0,"row":0}}
"#,
    )
    .await;
    assert!(
        viewport.iter().all(|call| !call.starts_with("viewport")),
        "{viewport:?}"
    );
    assert_eq!(
        writes,
        vec![b"\x1b[A\x1b[A".to_vec(), b"\x1b[B".to_vec()],
        "{output}"
    );
}

#[tokio::test]
async fn a_viewport_request_moves_the_viewport_to_the_offset_without_writing() {
    let (output, viewport, writes) = serve_scroll(
        None,
        r#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"viewport","offset":7}}
{"surface":"s1","body":{"operation":"screen.read"}}
{"surface":"s1","body":{"operation":"viewport","offset":-1}}
{"surface":"s1","body":{"operation":"viewport","offset":2.5}}
"#,
    )
    .await;
    assert_eq!(viewport, vec!["viewport 7".to_string()], "{output}");
    assert!(
        output.contains(r#""scrollback""#),
        "the screen read after a viewport request carries scrollback: {output}"
    );
    assert_eq!(output.matches("invalidParams").count(), 2, "{output}");
    assert!(
        writes.is_empty(),
        "a viewport request writes nothing to the program"
    );
}

#[tokio::test]
async fn a_scroll_does_not_draw_over_a_raster_the_host_has_not_consumed() {
    // The host answers consumed after it copies the raster of the presentation request. Drawing
    // again before that overwrites the raster being copied.
    let (output, viewport, _) = serve_scroll(
        None,
        r#"{"surface":"s1","body":{"operation":"scroll","lines":3,"col":0,"row":0}}
{"surface":"s1","body":{"operation":"viewport","offset":1}}
"#,
    )
    .await;
    assert_eq!(viewport.len(), 2, "{viewport:?}");
    assert_eq!(
        output.matches(r#""kind":"iosurface-global""#).count(),
        1,
        "a raster was drawn and presented again before the host consumed the previous one: {output}"
    );
}

#[tokio::test]
async fn an_invalid_scroll_is_rejected() {
    let (output, viewport, writes) = serve_scroll(
        None,
        r#"{"surface":"s1","body":{"operation":"scroll","lines":0,"col":0,"row":0}}
{"surface":"s1","body":{"operation":"scroll","lines":1.5,"col":0,"row":0}}
"#,
    )
    .await;
    assert_eq!(output.matches("invalidParams").count(), 2, "{output}");
    assert!(viewport.is_empty() && writes.is_empty());
}

#[tokio::test]
async fn a_scroll_with_shift_moves_the_viewport_while_mouse_reporting_is_on() {
    // Shift skips the mouse reporting rule, so the wheel moves the primary viewport instead of
    // reaching the program.
    let modes = Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (output, viewport, writes) = serve_scroll(
        Some(modes),
        r#"{"surface":"s1","body":{"operation":"scroll","lines":2,"col":4,"row":6,"shift":true}}
{"surface":"s1","body":{"operation":"scroll","lines":-1,"col":4,"row":6,"shift":true}}
"#,
    )
    .await;
    assert_eq!(
        viewport,
        vec!["viewport 2".to_string(), "viewport -1".to_string()],
        "{output}"
    );
    assert!(
        writes.is_empty(),
        "a scroll with Shift was written to the program: {writes:?}"
    );
}

#[tokio::test]
async fn a_scroll_without_shift_under_mouse_reporting_is_written_to_the_program() {
    // An absent shift is a scroll without Shift (docs/spec/vt-core.md).
    let modes = Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (output, viewport, writes) = serve_scroll(
        Some(modes),
        r#"{"surface":"s1","body":{"operation":"scroll","lines":2,"col":4,"row":6}}
"#,
    )
    .await;
    assert!(viewport.is_empty(), "{output}");
    assert_eq!(
        writes,
        vec![b"\x1b[<64;5;7M\x1b[<64;5;7M".to_vec()],
        "{output}"
    );
}

#[tokio::test]
async fn a_scroll_with_a_non_boolean_shift_is_refused() {
    let modes = Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (output, viewport, writes) = serve_scroll(
        Some(modes),
        r#"{"surface":"s1","body":{"operation":"scroll","lines":2,"col":4,"row":6,"shift":1}}
{"surface":"s1","body":{"operation":"scroll","lines":2,"col":4,"row":6,"shift":null}}
"#,
    )
    .await;
    assert_eq!(output.matches("invalidParams").count(), 2, "{output}");
    assert!(viewport.is_empty() && writes.is_empty(), "{output}");
}

#[tokio::test]
async fn test_selection_release_emits_one_user_copy_event() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"selection.start","x":1.0,"y":1.0}}
{"surface":"s1","body":{"operation":"selection.update","x":25.0,"y":1.0}}
{"surface":"s1","body":{"operation":"selection.end"}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    assert!(
        output.contains("selection.copy"),
        "selection release must emit a copy event: {output}"
    );
    assert!(!output.contains("Unknown operation: selection.start"));
    assert!(!output.contains("Unknown operation: selection.update"));
    assert!(!output.contains("Unknown operation: selection.end"));
}

/// A point past the last complete row or column of the region selects up to the right edge of the
/// last row and the last column. The same holds for a point less than one device pixel past the
/// region; a point farther out is an error.
#[tokio::test]
async fn test_selection_in_the_region_padding_selects_to_the_last_edge() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    // 801 x 383 pixels is not a multiple of the cell size, so padding remains after the last
    // complete row and column.
    let input = r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":801,"height":383,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"selection.start","x":1.0,"y":1.0}}
{"surface":"s1","body":{"operation":"selection.update","x":800.5,"y":382.5}}
{"surface":"s1","body":{"operation":"selection.update","x":1.0,"y":383.5}}
{"surface":"s1","body":{"operation":"selection.update","x":1.0,"y":384.0}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let cells = Arc::new(Mutex::new(Vec::new()));
    let recorded = cells.clone();
    let engine_factory = Arc::new(move || {
        let mut engine = MockEngine::new();
        engine.selected_cells = recorded.clone();
        Box::new(engine) as Box<dyn Engine>
    });
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    let state = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|message| message["body"]["event"] == "state")
        .expect("a state event");
    let cols = state["body"]["cols"].as_u64().unwrap() as u16;
    let rows = state["body"]["rows"].as_u64().unwrap() as u16;
    let cell_width = state["body"]["cellWidth"].as_f64().unwrap();
    let cell_height = state["body"]["cellHeight"].as_f64().unwrap();
    assert!(
        f64::from(cols) * cell_width < 800.5 && f64::from(rows) * cell_height < 382.5,
        "the region must leave padding past the grid: {cols}x{rows} cells of {cell_width}x{cell_height}"
    );
    assert_eq!(
        *cells.lock().unwrap(),
        vec![(0, 0), (cols, rows - 1), (0, rows - 1)],
        "the padding point selects to the right edge of the last cell and the band point the last row: {output}"
    );
    let errors: Vec<&str> = output
        .lines()
        .filter(|line| line.contains("outside the terminal region"))
        .collect();
    assert_eq!(
        errors.len(),
        1,
        "only the point below the region is an error: {output}"
    );
}

/// A release over a blank cell reports the end of the gesture without an error or a copy.
#[tokio::test]
async fn test_blank_selection_release_reports_end_without_copy() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"selection.start","x":700.0,"y":300.0}}
{"surface":"s1","body":{"operation":"selection.update","x":760.0,"y":300.0}}
{"surface":"s1","body":{"operation":"selection.end"}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| {
        let mut engine = MockEngine::new();
        engine.selection = None;
        Box::new(engine) as Box<dyn Engine>
    });
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    let ends: Vec<serde_json::Value> = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["body"]["event"] == "selection.end")
        .collect();
    assert_eq!(ends.len(), 1, "one selection.end event: {output}");
    assert_eq!(ends[0]["body"]["copied"], serde_json::Value::Bool(false));
    assert!(
        !output.contains("selection.copy"),
        "nothing is copied: {output}"
    );
    assert!(
        !output.contains("\"error\""),
        "the release is not an error: {output}"
    );
}

/// A release that arrives while the event of the previous release waits for its raster is applied
/// after that event, not refused: two gestures in a row each report their end.
#[tokio::test]
async fn a_release_during_the_presentation_of_the_previous_release_waits_for_it() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"selection.start","x":700.0,"y":300.0}}
{"surface":"s1","body":{"operation":"selection.update","x":760.0,"y":300.0}}
{"surface":"s1","body":{"operation":"selection.end"}}
{"surface":"s1","body":{"operation":"selection.start","x":700.0,"y":300.0}}
{"surface":"s1","body":{"operation":"selection.update","x":760.0,"y":300.0}}
{"surface":"s1","body":{"operation":"selection.end"}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":3}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":4}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| {
        let mut engine = MockEngine::new();
        engine.selection = None;
        Box::new(engine) as Box<dyn Engine>
    });
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    assert!(
        !output.contains("selection presentation is still pending"),
        "the second release is not refused: {output}"
    );
    let ends = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["body"]["event"] == "selection.end")
        .count();
    assert_eq!(ends, 2, "each gesture reports its end: {output}");
}

/// Test A-7: the close operation ends the session (it calls close, not detach)
#[tokio::test]
async fn test_a7_close_op_ends_the_session() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"close"}}
{"surface":"s1","body":{"operation":"close"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let close_replies: Vec<serde_json::Value> = String::from_utf8(writer)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["body"] == serde_json::json!({}))
        .collect();
    assert_eq!(
        close_replies.len(),
        2,
        "repeated close must be acknowledged idempotently"
    );
    assert!(
        close_replies
            .iter()
            .all(|message| message["surface"] == "s1")
    );

    // Verifies that close was called and detach was not called
    let calls_lock = calls.lock().unwrap();
    assert_eq!(calls_lock.closes.len(), 1, "close not called exactly once");
    assert_eq!(
        calls_lock.closes[0], fake_session_id,
        "close session_id mismatch"
    );
    assert!(
        calls_lock.detaches.is_empty(),
        "detach should not be called for close op"
    );
}

/// Test A-8: the closed:true flag closes the terminal session
#[tokio::test]
async fn test_a8_closed_surface_closes_session() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","closed":true}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    // Verifies that close was called and detach was not called
    let calls_lock = calls.lock().unwrap();
    assert_eq!(calls_lock.closes.len(), 1, "close not called exactly once");
    assert_eq!(
        calls_lock.closes[0], fake_session_id,
        "close session_id mismatch"
    );
    assert!(
        calls_lock.detaches.is_empty(),
        "detach should not be called for closed:true flag"
    );
    // After the close finishes, the sidecar sends the closed answer defined in the core sidecar
    // specification.
    let output = String::from_utf8(writer).unwrap();
    let answer = serde_json::json!({"surface": "s1", "closed": true});
    assert!(
        output
            .lines()
            .any(|line| serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .as_ref()
                == Some(&answer)),
        "the close answer is missing: {output}"
    );
}

#[tokio::test]
async fn test_input_not_fed_to_engine() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"hello","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "test-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let result = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_stdin_eof_terminates_quickly() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "test-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    // The service reads the font first when it starts. This check measures only the exit time after
    // EOF, so it reads the font first in the same order. The first CoreText call of a new process
    // can take more than 2 seconds (V5-7).
    soksak_sidecar_vt_core::platform::darwin::frame::load_default_font()
        .expect("the system fixed-pitch font");
    let start = std::time::Instant::now();
    let result = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "stdin EOF took too long: {}ms",
        elapsed.as_millis()
    );
}

#[tokio::test]
async fn test_open_and_close_session() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","closed":true}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "test-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
}

#[tokio::test]
async fn test_repeated_image_open_resets_native_frame_without_closing_session() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"reconnect"}}
{"surface":"s1","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":2,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"b","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "test-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.opens.len(),
        1,
        "repeated image open must preserve the PTY session"
    );
    assert_eq!(
        calls.writes.len(),
        1,
        "input must still reach the preserved session"
    );
    let output = String::from_utf8(writer).unwrap();
    assert_eq!(
        output.matches("\"event\":\"session\"").count(),
        1,
        "the replacement page must receive the preserved session identity exactly once"
    );
    assert_eq!(
        output.matches("\"image\"").count(),
        2,
        "each image open must wait for a fresh configured raster: {output}"
    );
}

#[tokio::test]
async fn detaching_an_image_keeps_the_session_for_headless_open_and_later_image_attach() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("detach-session".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(
        (first["name"].as_str(), first["generation"].as_u64()),
        (Some("view"), Some(1)),
        "the initial frame does not belong to the configured owner: {first}"
    );
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"detach":{"request":"detach-1","name":"view","generation":1}}}}
"#).await.unwrap();
    let detached_line = next_line_except_frame_events(&mut lines).await;
    let detached: serde_json::Value = serde_json::from_str(&detached_line).unwrap();
    assert_eq!(detached["surface"], "s1", "{detached_line}");
    assert_eq!(
        detached["body"]["image"]["detached"],
        serde_json::json!({
            "request": "detach-1",
            "name": "view",
            "generation": 1,
            "ok": true,
            "result": "detached"
        }),
        "detach did not acknowledge its owner: {detached_line}"
    );

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":2,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"a","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"close"}}
"#).await.unwrap();
    let second = next_image(&mut lines).await;
    assert_eq!(
        (second["name"].as_str(), second["generation"].as_u64()),
        (Some("view"), Some(2)),
        "the later image did not attach to the preserved session: {second}"
    );
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.opens.len(),
        1,
        "detach replaced the PTY session: {calls:?}"
    );
    assert_eq!(
        calls.writes,
        vec![("detach-session".to_string(), b"a".to_vec())],
        "headless open or later image attach did not reuse the session: {calls:?}"
    );
}

#[tokio::test]
async fn an_image_response_with_wrong_frame_identity_does_not_release_a_newer_pending_draw() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new(
        "image-response-session".into(),
        calls.clone(),
    ));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(first["generation"], 1, "{first}");

    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"reconnect"}}
"#,
        )
        .await
        .unwrap();
    wait_for_reply(&mut lines, LettingGo::Reconnect).await;
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":2,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let current = next_image(&mut lines).await;
    assert_eq!(current["generation"], 2, "{current}");

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":2,"raster":2,"width":1600,"height":768,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":2,"sequence":1}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"a","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}},"cids":["stale-response-barrier"]}
"#).await.unwrap();
    let mut stale_frames = Vec::new();
    loop {
        let line = next_line_except_frame_events(&mut lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if message["body"].get("image").is_some() {
            stale_frames.push(message["body"]["image"].clone());
        }
        if message["cids"] == serde_json::json!(["stale-response-barrier"]) {
            break;
        }
    }

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":2,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"close"}}
"#).await.unwrap();
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();

    assert!(
        stale_frames
            .iter()
            .all(|image| image["generation"] != 2 || image["raster"] != 2),
        "a response with stale frame identity released the newer draw: {stale_frames:?}"
    );
}

#[tokio::test]
async fn detaching_a_requested_image_before_configure_allows_headless_open_and_replay() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new(
        "requested-image-session".into(),
        calls.clone(),
    ));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"detach":{"request":"requested-detach","name":"view","generation":1}}}}
"#).await.unwrap();
    let first = next_image_detach_reply(&mut lines, "requested-detach").await;
    assert_eq!(first["body"]["image"]["detached"]["ok"], true, "{first}");
    assert_eq!(
        first["body"]["image"]["detached"]["result"], "detached",
        "{first}"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"detach":{"request":"requested-detach","name":"view","generation":1}}}}
{"surface":"s1","body":{"image":{"detach":{"request":"requested-detach-again","name":"view","generation":1}}}}
"#).await.unwrap();
    let replay = next_image_detach_reply(&mut lines, "requested-detach").await;
    assert_eq!(
        replay["body"]["image"]["detached"]["result"], "detached",
        "{replay}"
    );
    let duplicate = next_image_detach_reply(&mut lines, "requested-detach-again").await;
    assert_eq!(
        duplicate["body"]["image"]["detached"]["result"], "alreadyDetached",
        "{duplicate}"
    );

    to_serve
        .write_all(
            br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"a","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}},"cids":["headless-input"]}
{"surface":"s1","body":{"operation":"close"}}
"#,
        )
        .await
        .unwrap();
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();

    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.opens.len(),
        1,
        "headless open did not create its PTY: {calls:?}"
    );
    assert_eq!(
        calls.writes,
        vec![("requested-image-session".to_string(), b"a".to_vec())],
        "headless input did not reach the opened session: {calls:?}"
    );
}

#[tokio::test]
async fn an_image_detach_replay_survives_reconnect_without_a_newer_owner() {
    let factory = Arc::new(|| {
        Arc::new(FakeSessionPort::new(
            "detach-reconnect-session".into(),
            Arc::new(Mutex::new(Calls::default())),
        )) as Arc<dyn SessionPort>
    });
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"detach":{"request":"retry-after-reconnect","name":"view","generation":1}}}}
{"surface":"s1","body":{"operation":"reconnect"}}
{"surface":"s1","body":{"image":{"detach":{"request":"retry-after-reconnect","name":"view","generation":1}}}}
{"surface":"s1","body":{"operation":"close"}}
"#).await.unwrap();

    let ack = next_image_detach_reply(&mut lines, "retry-after-reconnect").await;
    assert_eq!(ack["body"]["image"]["detached"]["ok"], true, "{ack}");
    let retry = next_image_detach_reply(&mut lines, "retry-after-reconnect").await;
    assert_eq!(
        retry["body"]["image"]["detached"]["ok"], true,
        "reconnect without a newer owner must preserve idempotent detach replay: {retry}"
    );
    assert_eq!(
        retry["body"]["image"]["detached"]["result"], "detached",
        "{retry}"
    );
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn an_image_detach_request_id_cannot_be_reused_with_another_tuple() {
    let factory = Arc::new(|| {
        Arc::new(FakeSessionPort::new(
            "request-id-surface".into(),
            Arc::new(Mutex::new(Calls::default())),
        )) as Arc<dyn SessionPort>
    });
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"first","body":{"image":{"detach":{"request":"invalid","name":"view","generation":0}}}}
"#).await.unwrap();
    // A malformed detach with a readable request answers that request.
    let invalid_line = next_line_except_frame_events(&mut lines).await;
    let invalid: serde_json::Value = serde_json::from_str(&invalid_line).unwrap();
    assert_eq!(
        invalid["body"]["image"]["detached"]["request"], "invalid",
        "{invalid_line}"
    );
    assert_eq!(
        invalid["body"]["image"]["detached"]["error"], "invalidParams",
        "{invalid_line}"
    );

    to_serve.write_all(br#"{"surface":"first","body":{"image":{"detach":{"request":"global-request","name":"view","generation":1}}}}
"#).await.unwrap();
    let first = next_image_detach_reply(&mut lines, "global-request").await;
    to_serve.write_all(br#"{"surface":"first","body":{"image":{"detach":{"request":"global-request","name":"other","generation":1}}}}
"#).await.unwrap();
    let second = next_image_detach_reply(&mut lines, "global-request").await;
    to_serve.write_all(br#"{"surface":"second","body":{"image":{"detach":{"request":"global-request","name":"view","generation":1}}}}
"#).await.unwrap();
    let third = next_image_detach_reply(&mut lines, "global-request").await;
    assert_eq!(first["surface"], "first", "{first}");
    assert_eq!(
        first["body"]["image"]["detached"]["result"], "alreadyAbsent",
        "{first}"
    );
    assert_eq!(second["surface"], "first", "{second}");
    assert_eq!(
        second["body"]["image"]["detached"]["error"], "requestMismatch",
        "{second}"
    );
    assert_eq!(third["surface"], "second", "{third}");
    assert_eq!(
        third["body"]["image"]["detached"]["error"], "requestMismatch",
        "{third}"
    );
    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn stale_future_and_mismatched_detach_requests_preserve_the_newer_attachment() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new(
        "generation-fence".into(),
        calls.clone(),
    ));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":2,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(first["generation"], 2, "{first}");
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":3,"raster":1,"width":900,"height":432,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"detach":{"request":"stale-two","name":"view","generation":2}}}}
"#).await.unwrap();
    let stale = next_image_detach_reply(&mut lines, "stale-two").await;
    assert_eq!(
        stale["body"]["image"]["detached"]["error"], "staleGeneration",
        "{stale}"
    );
    assert_eq!(
        stale["body"]["image"]["detached"]["currentGeneration"], 3,
        "{stale}"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":2,"raster":1,"sequence":1}}}}
"#).await.unwrap();
    let current = next_image(&mut lines).await;
    assert_eq!(
        current["generation"], 3,
        "a stale detach removed pending generation 3: {current}"
    );
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"detach":{"request":"stale-two","name":"view","generation":2}}}}
"#).await.unwrap();
    let stale_replay = next_image_detach_reply(&mut lines, "stale-two").await;
    assert_eq!(
        stale_replay["body"]["image"]["detached"]["error"], "staleGeneration",
        "{stale_replay}"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"detach":{"request":"future-four","name":"view","generation":4}}}}
{"surface":"s1","body":{"image":{"detach":{"request":"wrong-name","name":"other","generation":3}}}}
{"surface":"s1","body":{"image":{"detach":{"request":"detach-three","name":"view","generation":3}}}}
"#).await.unwrap();
    let future = next_image_detach_reply(&mut lines, "future-four").await;
    assert_eq!(
        future["body"]["image"]["detached"]["error"], "futureGeneration",
        "{future}"
    );
    let wrong_name = next_image_detach_reply(&mut lines, "wrong-name").await;
    assert_eq!(
        wrong_name["body"]["image"]["detached"]["error"], "imageNameMismatch",
        "{wrong_name}"
    );
    let detached = next_image_detach_reply(&mut lines, "detach-three").await;
    assert_eq!(
        detached["body"]["image"]["detached"]["result"], "detached",
        "{detached}"
    );

    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"close"}}
"#,
        )
        .await
        .unwrap();
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.opens.len(),
        1,
        "generation detach replaced the PTY: {calls:?}"
    );
}

#[tokio::test]
async fn test_resize() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":1600,"height":768,"scale":1.0}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "test-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
}

#[tokio::test]
async fn test_close_session() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","closed":true}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "test-session".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
}

#[tokio::test]
async fn test_wide_chars() {
    let mut engine = MockEngine::new();
    engine.feed("안".as_bytes());

    let screen = engine.screen();
    assert_eq!(screen.lines.len(), 1);
    assert_eq!(screen.lines[0][0].width, 2);
}

/// Test A-2: pushed output reaches the screen
#[tokio::test]
async fn test_a2_pushed_output_reaches_screen() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-a2".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    // Uses tokio::io::duplex for stdin and stdout
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Sends the open command
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // Waits for the state response (it must contain sessionId)
    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");
    assert_eq!(state_json["body"]["event"], "state", "expected state event");
    // Answers the first presentation as the host does. Output before the answer appears on the
    // screen after the answer arrives.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // Pushes an output event with "hi\r\n"
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"hi\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // Waits for a screen event (it must contain "hi")
    let mut found_hi = false;
    let mut grid = ScreenGrid::default();
    let mut observed = vec![state_line];
    let mut last_consumed = 1;
    loop {
        let screen_line =
            tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timeout waiting for screen; observed {observed:?}; calls {:?}",
                        calls.lock().unwrap()
                    )
                })
                .expect("failed to read screen line")
                .expect("screen line is empty");
        observed.push(screen_line.clone());

        let screen_json = grid.apply_line(&screen_line);

        // A cursor state change can also produce a frame. As the real host does, the next frame
        // must be consumed so that the PTY output that arrives after it advances to the screen.
        if let Some(image) = screen_json["body"].get("image") {
            let sequence = image["sequence"]
                .as_u64()
                .expect("image sequence is missing");
            if sequence > last_consumed {
                let consumed = serde_json::json!({"surface": "s1", "body": {"image": {"consumed": {
                    "name": "view", "generation": 1, "raster": 1, "sequence": sequence
                }}}});
                to_serve
                    .write_all(format!("{consumed}\n").as_bytes())
                    .await
                    .expect("failed to consume the next image");
                // The sidecar sends no screen with a frame; a screen read returns the cells.
                to_serve
                    .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"screen.read\"}}\n")
                    .await
                    .expect("failed to read the screen");
                last_consumed = sequence;
            }
        }

        if let Some(event) = screen_json.get("body").and_then(|b| b.get("event"))
            && event == "screen"
        {
            // The screen event was found, so this checks which line contains "hi"
            let body = &screen_json["body"];
            if let (Some(cells), Some(cols)) = (body["cells"].as_array(), body["cols"].as_u64()) {
                for row in cells.chunks(cols as usize) {
                    let line_text: String = row
                        .iter()
                        .filter_map(|cell| cell["grapheme"].as_str())
                        .collect();
                    if line_text.trim_end() == "hi" {
                        found_hi = true;
                        break;
                    }
                }
            }
            if found_hi {
                break;
            }
        }
    }

    assert!(found_hi, "Output 'hi' did not appear in a screen read");

    // Closes stdin to end serve
    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_inline_image_event_is_explicit_and_base64_encoded() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-inline-image".to_string();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(fake_session_id.clone(), calls.clone()));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve
        .write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#)
        .await
        .unwrap();
    let _state = lines.next_line().await.unwrap().unwrap();
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id,
        data: b"\x1b]1337;File=name=ZmlsZS5wbmc=;size=5;inline=1;width=2px:aGVsbG8=\x07".to_vec(),
        sequence: 0,
        truncated: false,
    });

    let mut found = false;
    for _ in 0..12 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for inline image event")
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        // Answers the presentation as the host does. An image event follows the presentation that
        // drew the image (or the drawing error).
        if let Some(sequence) = value["body"]["image"]["sequence"].as_i64() {
            to_serve.write_all(format!("{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"sequence\":{sequence}}}}}}}}}\n").as_bytes()).await.unwrap();
        }
        if value["body"]["event"] == "image.inline" {
            assert_eq!(value["body"]["command"], "display");
            assert_eq!(value["body"]["name"], "file.png");
            assert_eq!(value["body"]["data"], "aGVsbG8=");
            assert_eq!(value["body"]["width"], "2px");
            found = true;
            break;
        }
    }
    assert!(found, "inline image event was not emitted");
    to_serve
        .write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"image.inline.delete","name":"file.png"}}
"#)
        .await
        .unwrap();
    let mut deleted = false;
    for _ in 0..4 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for inline image deletion")
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value["body"]["event"] == "image.inline.deleted" {
            assert_eq!(value["body"]["name"], "file.png");
            deleted = true;
            break;
        }
    }
    assert!(deleted, "owned inline image was not deleted");
    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_inline_image_delete_is_explicit_for_unowned_names() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new("delete-session".into(), calls));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(16 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve
        .write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"image.inline.delete","name":"missing.png"}}
"#)
        .await
        .unwrap();
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for explicit delete error")
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["body"]["error"], "imageNotConfigured");
    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_a5_screen_read_returns_current_screen() {
    let input = r#"
{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;

    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let session_port: Arc<dyn SessionPort> = Arc::new(tracked_session_port::FakeSessionPort::new());
    let session_port_for_factory = session_port.clone();
    let factory = Arc::new(move || session_port_for_factory.clone());

    let result = serve(engine_factory, reader, &mut writer, factory).await;
    assert!(result.is_ok());

    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<&str> = output.lines().collect();

    // The open response, the input ack, and the screen.read screen response must be present. The
    // first line is the open response
    let open_json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(open_json["body"]["event"], "state");

    // Finds the screen event that contains "hi" (it comes from screen.read)
    let mut found_screen_read = false;
    for line in lines.iter().skip(1) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(event) = json.get("body").and_then(|b| b.get("event"))
            && event == "screen"
        {
            found_screen_read = true;
            break;
        }
    }
    assert!(
        found_screen_read,
        "screen.read should return a screen event"
    );
}

#[tokio::test]
async fn test_a6_unknown_op_returns_error() {
    let input = r#"
{"surface":"s1","body":{"operation":"unknown_operation"}}
"#;

    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let session_port: Arc<dyn SessionPort> = Arc::new(tracked_session_port::FakeSessionPort::new());
    let session_port_for_factory = session_port.clone();
    let factory = Arc::new(move || session_port_for_factory.clone());

    let result = serve(engine_factory, reader, &mut writer, factory).await;
    assert!(result.is_ok());

    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<&str> = output.lines().collect();
    assert!(!lines.is_empty());

    let json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert!(
        json["body"]["error"]
            .as_str()
            .unwrap()
            .contains("Unknown operation")
    );
}

#[tokio::test]
async fn test_theme_rejects_unknown_mode_without_fallback() {
    let input = r##"
{"surface":"s1","body":{"operation":"theme","mode":"light","background":"#e8f5ee","foreground":"#12684a","cursor":"#12684a","selection":"#d8dbe4"}}
{"surface":"s1","body":{"operation":"theme","mode":"sepia","background":"#e8f5ee","foreground":"#12684a","cursor":"#12684a","selection":"#d8dbe4"}}
{"surface":"s1","body":{"operation":"theme","mode":"dark"}}
{"surface":"s1","body":{"operation":"theme","mode":"dark","background":"red","foreground":"#12684a","cursor":"#12684a","selection":"#d8dbe4"}}
"##;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let session_port: Arc<dyn SessionPort> = Arc::new(tracked_session_port::FakeSessionPort::new());
    let session_port_for_factory = session_port.clone();
    let factory = Arc::new(move || session_port_for_factory.clone());

    serve(engine_factory, reader, &mut writer, factory)
        .await
        .expect("theme contract");
    let output = String::from_utf8(writer).expect("utf8 output");
    let outputs = output
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        outputs
            .iter()
            .any(|value| value["body"]["event"] == "theme")
    );
    assert!(outputs.iter().any(|value| {
        value["body"]["error"] == "invalidParams"
            && value["body"]["reason"] == "theme.mode must be dark or light"
    }));
    // A color that is missing or is not #rrggbb is rejected even when the mode is valid.
    assert!(outputs.iter().any(|value| {
        value["body"]["error"] == "invalidParams"
            && value["body"]["reason"] == "theme.background must be a #rrggbb color"
    }));
    assert_eq!(
        outputs
            .iter()
            .filter(|value| value["body"]["event"] == "theme")
            .map(|value| value["body"]["mode"].clone())
            .collect::<Vec<_>>(),
        vec![serde_json::json!("light")],
        "only the complete light theme is applied"
    );
}

#[tokio::test]
async fn test_cursor_policy_rejects_invalid_values_without_fallback() {
    let input = r#"
{"surface":"s1","body":{"operation":"cursor","shape":"beam","blink":"Always","interval":750,"idleTimeout":5000,"unfocused":"hollow"}}
{"surface":"s1","body":{"operation":"cursor","blink":"Sometimes"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let session_port: Arc<dyn SessionPort> = Arc::new(tracked_session_port::FakeSessionPort::new());
    let session_port_for_factory = session_port.clone();
    let factory = Arc::new(move || session_port_for_factory.clone());

    serve(engine_factory, reader, &mut writer, factory)
        .await
        .expect("cursor policy contract");
    let outputs = String::from_utf8(writer)
        .expect("utf8 output")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        outputs.iter().any(|value| {
            value["body"]["event"] == "cursor" && value["body"]["blink"] == "Always"
        })
    );
    assert!(outputs.iter().any(|value| {
        value["body"]["error"] == "invalidParams"
            && value["body"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("cursor.blink"))
    }));
}

/// Test K1: encodes a key without app_cursor mode
#[tokio::test]
async fn test_k1_keys_up_without_app_cursor() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"key","key":"Up","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let calls_lock = calls.lock().unwrap();
    assert_eq!(calls_lock.writes.len(), 1, "should have exactly one write");
    assert_eq!(
        calls_lock.writes[0].1, b"\x1b[A",
        "Up key without app_cursor should be ESC[A"
    );
}

/// Test K2: encodes a key with app_cursor mode
#[tokio::test]
async fn test_k2_keys_up_with_app_cursor() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"key","key":"Up","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let modes = Modes {
        app_cursor: true,
        ..Default::default()
    };
    let engine_factory =
        Arc::new(move || Box::new(MockEngine::with_modes(modes.clone())) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let calls_lock = calls.lock().unwrap();
    assert_eq!(calls_lock.writes.len(), 1, "should have exactly one write");
    assert_eq!(
        calls_lock.writes[0].1, b"\x1bOA",
        "Up key with app_cursor should be ESC O A"
    );
}

/// Test K3: char key encoding with ctrl and UTF-8
#[tokio::test]
async fn test_k3_char_key_encoding() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    // After the native physical key mapping, Ctrl+C is recorded as 0x03 and Ctrl+U as 0x15.
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"key","key":"Char","text":"c","shift":false,"alt":false,"ctrl":true}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"key","key":"Char","text":"u","shift":false,"alt":false,"ctrl":true}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(calls_lock.writes.len(), 2);
        assert_eq!(calls_lock.writes[0].1, vec![0x03], "ctrl+c should be 0x03");
        assert_eq!(calls_lock.writes[1].1, vec![0x15], "ctrl+u should be 0x15");

        // Test UTF-8 encoding (example character: U+D55C)
    }

    let calls2 = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id2 = "test-session-2".to_string();

    let input2 = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"key","key":"Char","text":"한","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader2 = std::io::Cursor::new(input2.as_bytes());
    let mut writer2 = Vec::new();

    let engine_factory2 = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory2 = calls2.clone();
    let session_id_for_factory2 = fake_session_id2.clone();
    let session_port_factory2 = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory2.clone(),
            calls_for_factory2.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(
        engine_factory2,
        reader2,
        &mut writer2,
        session_port_factory2,
    )
    .await;

    let calls_lock2 = calls2.lock().unwrap();
    assert_eq!(calls_lock2.writes.len(), 1);
    assert_eq!(
        calls_lock2.writes[0].1,
        "한".as_bytes(),
        "Char with UTF-8 should encode as UTF-8 bytes"
    );
}

/// Test K4: an unknown key returns an error and does not write
#[tokio::test]
async fn test_k4_unknown_key_returns_error() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","actions":[{"type":"key","key":"Nope","shift":false,"alt":false,"ctrl":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let calls_lock = calls.lock().unwrap();
    assert_eq!(
        calls_lock.writes.len(),
        0,
        "should not call write for unknown key"
    );

    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<&str> = output.lines().collect();

    let mut found_error = false;
    for line in lines {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
            && json["body"]["error"] == "invalidParams"
            && let Some(reason) = json["body"]["reason"].as_str()
            && reason.contains("unknown key: Nope")
        {
            found_error = true;
            break;
        }
    }
    assert!(found_error, "should return error for unknown key");
}

/// open does not run until the host provides the exact native raster configuration.
#[tokio::test]
async fn test_open_waits_for_host_raster_configuration() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-i4".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve
        .write_all(
            br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
"#,
        )
        .await
        .unwrap();

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line())
            .await
            .is_err(),
        "open produced output before the host configured a raster"
    );
    assert!(calls.lock().unwrap().opens.is_empty());

    to_serve
        .write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#)
        .await
        .unwrap();
    let state = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(json["body"]["event"], "state");
    assert_eq!(calls.lock().unwrap().opens.len(), 1);

    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_headless_open_creates_one_session_before_configuration() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("headless".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut input, serve_input) = tokio::io::duplex(1 << 20);
    let (serve_output, output) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(serve(engine_factory, serve_input, serve_output, factory));
    let mut lines = tokio::io::BufReader::new(output).lines();
    input
        .write_all(
            b"{\"surface\":\"hidden\",\"root\":\"/tmp\",\"body\":{\"operation\":\"open\",\"shell\":\"/bin/sh\"}}\n",
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line())
            .await
            .is_err()
    );
    assert_eq!(calls.lock().unwrap().opens, vec![(80, 24)]);
    input.write_all(b"{\"surface\":\"hidden\",\"body\":{\"image\":{\"configure\":{\"name\":\"view\",\"generation\":1,\"raster\":1,\"width\":800,\"height\":384,\"scale\":1.0}}}}\n").await.unwrap();
    let state = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&state).unwrap()["body"]["event"],
        "state"
    );
    assert_eq!(calls.lock().unwrap().opens.len(), 1);
    drop(input);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_legacy_op_field_is_rejected_without_fallback() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("legacy-op".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut input, serve_input) = tokio::io::duplex(1 << 20);
    let (serve_output, output) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(serve(engine_factory, serve_input, serve_output, factory));
    let mut lines = tokio::io::BufReader::new(output).lines();

    input
        .write_all(
            br#"{"surface":"legacy","root":"/tmp","body":{"op":"open"}}
"#,
        )
        .await
        .unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(json["body"]["error"], "unknown operation");
    assert!(
        calls.lock().unwrap().opens.is_empty(),
        "legacy op must not open a session"
    );

    drop(input);
    task.await.unwrap().unwrap();
}

/// Test I1: when open has an image field, sends an image envelope after the output
#[tokio::test]
async fn test_i1_image_envelope_on_output() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-i1".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens with an image field
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // Waits for the state response
    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");
    assert_eq!(state_json["body"]["event"], "state", "expected state event");

    // Pushes an output event
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"hi\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // Waits for the image envelope
    let image_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for image envelope")
        .expect("failed to read image line")
        .expect("image line is empty");

    let image_json: serde_json::Value =
        serde_json::from_str(&image_line).expect("failed to parse image JSON");

    // Verifies the image envelope structure
    let image_obj = image_json
        .get("body")
        .and_then(|b| b.get("image"))
        .expect("should have image envelope");

    assert_eq!(image_obj["name"], "view", "image name should be 'view'");
    assert_eq!(image_obj["sequence"], 1, "initial sequence should be 1");
    assert_eq!(image_obj["format"], "bgra8", "format should be bgra8");

    let token = image_obj.get("token").expect("should have token");
    assert_eq!(
        token["kind"], "iosurface-global",
        "token kind should be iosurface-global"
    );

    let nonce_b64 = token["nonce"].as_str().expect("nonce should be string");
    let nonce_bytes = base64_decode_test(nonce_b64).expect("nonce should be valid base64");
    assert_eq!(nonce_bytes.len(), 16, "nonce should be 16 bytes");

    assert!(
        image_obj["width"].as_u64().unwrap_or(0) > 0,
        "width should be > 0"
    );
    assert!(
        image_obj["height"].as_u64().unwrap_or(0) > 0,
        "height should be > 0"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test I2: no second image envelope exists until the host answers, including a stale response
#[tokio::test]
async fn test_i2_no_image_envelope_until_consumed() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-i2".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens with an image field
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    // Pushes the first output
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"test1\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // Waits for the first image envelope
    let image_line1 =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .expect("timeout on first image envelope")
            .expect("failed to read first image line")
            .expect("first image line is empty");

    // Pushes the second output without a release
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"test2\r\n".to_vec(),
        sequence: 1,
        truncated: false,
    });

    // Absence has no event source: observe the entire existing 300 ms interval, including every
    // screen line.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
    loop {
        match tokio::time::timeout_at(deadline, lines.next_line()).await {
            Err(_) => break,
            Ok(line) => {
                let line = line
                    .expect("read before image release")
                    .expect("connection ended before image release");
                let message: serde_json::Value =
                    serde_json::from_str(&line).expect("valid message before image release");
                assert!(
                    message["body"].get("image").is_none(),
                    "image arrived before release: {message}"
                );
            }
        }
    }

    // An error releases only the exact image in flight, including its generation and raster.
    let first: serde_json::Value =
        serde_json::from_str(&image_line1).expect("the first image envelope is valid JSON");
    let image = first["body"]["image"]
        .as_object()
        .expect("the first envelope contains an image");
    assert!(
        image.contains_key("token"),
        "the first envelope contains the native image token"
    );
    let answer = serde_json::json!({"surface": "s1", "body": {"image": {
        "error": "stale",
        "name": image["name"],
        "generation": image["generation"],
        "raster": image["raster"],
        "sequence": image["sequence"],
    }}});
    to_serve
        .write_all(format!("{answer}\n").as_bytes())
        .await
        .unwrap();

    let next_image = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("read after image release")
                .expect("connection ended before the second image");
            let message: serde_json::Value =
                serde_json::from_str(&line).expect("valid message after image release");
            if let Some(image) = message["body"].get("image") {
                break image.clone();
            }
        }
    })
    .await
    .expect("second image did not arrive after the matching answer");
    assert!(
        next_image.get("token").is_some(),
        "the second image has a native token"
    );
    assert_eq!(next_image["sequence"], 2);
    assert_eq!(next_image["generation"], image["generation"]);
    assert_eq!(next_image["raster"], image["raster"]);

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test I3: handles host image responses (consumed/error) and returns no error
#[tokio::test]
async fn test_i3_image_response_no_error() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-i3".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens with an image field
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    // Pushes the output
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"test\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // Waits for the image envelope
    let _image_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    // Test 1: sends a consumed response (no "error")
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // Must receive only a screen or image envelope, not an error response
    let mut found_error_response = false;
    for _ in 0..10 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(error) = json.get("body").and_then(|b| b.get("error"))
            && error
                .as_str()
                .map(|e| e.contains("unknown operation"))
                .unwrap_or(false)
        {
            found_error_response = true;
            break;
        }
    }
    assert!(
        !found_error_response,
        "should NOT return 'unknown operation' error for consumed response"
    );

    // Test 2: sends an error response
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"error":"forbidden","name":"view","generation":1,"raster":1,"sequence":1}}}
"#).await.unwrap();

    // Must not receive an error response for an unknown operation
    found_error_response = false;
    for _ in 0..10 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(error) = json.get("body").and_then(|b| b.get("error"))
            && error
                .as_str()
                .map(|e| e.contains("unknown operation"))
                .unwrap_or(false)
        {
            found_error_response = true;
            break;
        }
    }
    assert!(
        !found_error_response,
        "should NOT return 'unknown operation' error for error response"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: an open with an image field is a request, not a host response
#[tokio::test]
async fn test_open_with_image_is_a_request() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-open-image".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            session_id_for_factory.clone(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });

    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;

    let calls_lock = calls.lock().unwrap();
    assert_eq!(
        calls_lock.opens.len(),
        1,
        "should have called open exactly once - open with image was misclassified as host response!"
    );
    assert!(
        calls_lock.opens[0].0 > 0 && calls_lock.opens[0].1 > 0,
        "should have valid cols and rows"
    );

    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<&str> = output.lines().collect();
    assert!(!lines.is_empty(), "should have output");

    // Checks that a state response was received, not an error
    let first_json: serde_json::Value =
        serde_json::from_str(lines[0]).expect("failed to parse first output as JSON");
    assert_eq!(
        first_json["body"]["event"], "state",
        "expected state event after open with image"
    );
}

fn base64_decode_test(s: &str) -> Result<Vec<u8>, String> {
    use soksak_sidecar_vt_core::protocol::base64_decode;
    base64_decode(s)
}

/// A host raster configuration whose width is not a number is rejected.
#[tokio::test]
async fn test_configure_with_invalid_width_type() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-invalid-width".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":"800","height":384,"scale":1.0}}}}
"#).await.unwrap();

    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    assert!(
        error_json["body"]["error"] == "invalidParams",
        "should return invalidParams error"
    );
    assert_eq!(reason, "invalid image configure");

    // The key point is that the daemon must not have been called
    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.opens.len(),
            0,
            "daemon open should not be called for an invalid raster"
        );
    }
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: an input without bytes or keys is rejected
#[tokio::test]
async fn test_input_without_bytes_or_keys_rejected() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-input-empty".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens first
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    let _initial_image = next_image_envelope(&mut lines).await;

    // Sends an input with neither bytes nor keys
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input"}}
"#,
        )
        .await
        .unwrap();

    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    // Must receive an error for the missing fields
    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    let error_code = error_json["body"]["error"].as_str().unwrap_or("");
    assert!(
        error_code.contains("invalidParams")
            && reason.contains("actions")
            && reason.contains("preedit"),
        "should report that actions and preedit are required, got error={} reason={}",
        error_code,
        reason
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: an input whose bytes are not a string is rejected
#[tokio::test]
async fn test_legacy_input_bytes_field_is_rejected() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-bytes-type".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens first
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    let _initial_image = next_image_envelope(&mut lines).await;

    // The removed bytes field is rejected instead of being accepted as another wire form.
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input","bytes":123}}
"#,
        )
        .await
        .unwrap();

    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    // The current transaction fields are required.
    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    let error_code = error_json["body"]["error"].as_str().unwrap_or("");
    assert!(
        error_code.contains("invalidParams")
            && reason.contains("actions")
            && reason.contains("preedit"),
        "should reject the legacy bytes field, got error={} reason={}",
        error_code,
        reason
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: an input whose keys are not an array is rejected
#[tokio::test]
async fn test_legacy_input_keys_field_is_rejected() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-keys-type".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens first
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    let _initial_image = next_image_envelope(&mut lines).await;

    // The removed keys field is rejected instead of being accepted as another wire form.
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input","keys":{"key":"Up"}}}
"#,
        )
        .await
        .unwrap();

    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    // The current transaction fields are required.
    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    let error_code = error_json["body"]["error"].as_str().unwrap_or("");
    assert!(
        error_code.contains("invalidParams")
            && reason.contains("actions")
            && reason.contains("preedit"),
        "should reject the legacy keys field, got error={} reason={}",
        error_code,
        reason
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// A replacement raster without height is rejected.
#[tokio::test]
async fn test_replacement_raster_with_missing_height_is_rejected() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-resize-missing".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens first
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    let _initial_image = next_image_envelope(&mut lines).await;

    // Sends a new raster without height.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":1024,"scale":1.0}}}}
"#).await.unwrap();

    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    assert_eq!(
        error_json["body"]["error"], "invalidParams",
        "should return invalidParams"
    );
    assert_eq!(error_json["body"]["reason"], "invalid image configure");

    // Verifies that the daemon resize was not called
    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.resizes.len(),
            0,
            "daemon resize should not be called for an incomplete raster"
        );
    }
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: cellWidth and cellHeight are computed from the metrics, not fixed values
#[tokio::test]
async fn test_cell_dimensions_from_metrics() {
    use soksak_sidecar_vt_core::platform::metrics;

    // Tests with scale=2.0
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-metrics".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    let scale = 2.0f32;
    let width_px = 800u32;
    let height_px = 384u32;

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":2.0}}}}
"#).await.unwrap();

    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");

    // Gets the actual metrics from the platform
    let m = metrics(13.0, scale);

    // cellWidth and cellHeight must equal CSS pixels = device_pixels / scale
    let expected_cell_width = m.cell_width as f64 / scale as f64;
    let expected_cell_height = m.cell_height as f64 / scale as f64;

    let cell_width = state_json["body"]["cellWidth"]
        .as_f64()
        .expect("cellWidth should be present and numeric");
    let cell_height = state_json["body"]["cellHeight"]
        .as_f64()
        .expect("cellHeight should be present and numeric");

    // Must match the metrics exactly (within a floating-point tolerance)
    assert!(
        (cell_width - expected_cell_width).abs() < 0.1,
        "cellWidth at scale 2.0: expected {}, got {}",
        expected_cell_width,
        cell_width
    );
    assert!(
        (cell_height - expected_cell_height).abs() < 0.1,
        "cellHeight at scale 2.0: expected {}, got {}",
        expected_cell_height,
        cell_height
    );

    // Verifies the cols/rows computation: cols = width_px / cell_width (width_px and cell_width are
    // both device pixels, and scale is already reflected in the metrics)
    let cols = state_json["body"]["cols"]
        .as_u64()
        .expect("cols should be present") as u16;
    let rows = state_json["body"]["rows"]
        .as_u64()
        .expect("rows should be present") as u16;

    let expected_cols = (width_px as f32 / m.cell_width) as u16;
    let expected_rows = (height_px as f32 / m.cell_height) as u16;

    assert_eq!(
        cols, expected_cols,
        "cols should match metrics at scale 2.0: expected {}, got {}",
        expected_cols, cols
    );
    assert_eq!(
        rows, expected_rows,
        "rows should match metrics at scale 2.0: expected {}, got {}",
        expected_rows, rows
    );

    drop(to_serve);
    task.await.unwrap().unwrap();

    // Tests with scale=1.0 to verify that the computation also works at another scale
    let calls2 = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id2 = "test-session-metrics-scale1".to_string();

    let engine_factory2 = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory2 = calls2.clone();
    let session_id_for_factory2 = fake_session_id2.clone();

    let port2 = Arc::new(FakeSessionPort::new(
        session_id_for_factory2.clone(),
        calls_for_factory2.clone(),
    ));
    let port_for_factory2 = port2.clone();
    let factory2 = Arc::new(move || port_for_factory2.clone() as Arc<dyn SessionPort>);

    let (mut to_serve2, serve_in2) = tokio::io::duplex(1 << 20);
    let (serve_out2, from_serve2) = tokio::io::duplex(16 * 1024 * 1024);

    let task2 = tokio::spawn(serve(engine_factory2, serve_in2, serve_out2, factory2));
    let mut lines2 = tokio::io::BufReader::new(from_serve2).lines();

    let scale2 = 1.0f32;

    to_serve2.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let state_line2 = tokio::time::timeout(std::time::Duration::from_secs(2), lines2.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json2: serde_json::Value =
        serde_json::from_str(&state_line2).expect("failed to parse state JSON");

    let m2 = metrics(13.0, scale2);
    let expected_cell_width2 = m2.cell_width as f64 / scale2 as f64;
    let expected_cell_height2 = m2.cell_height as f64 / scale2 as f64;

    let cell_width2 = state_json2["body"]["cellWidth"]
        .as_f64()
        .expect("cellWidth should be present");
    let cell_height2 = state_json2["body"]["cellHeight"]
        .as_f64()
        .expect("cellHeight should be present");

    assert!(
        (cell_width2 - expected_cell_width2).abs() < 0.1,
        "cellWidth at scale 1.0: expected {}, got {}",
        expected_cell_width2,
        cell_width2
    );
    assert!(
        (cell_height2 - expected_cell_height2).abs() < 0.1,
        "cellHeight at scale 1.0: expected {}, got {}",
        expected_cell_height2,
        cell_height2
    );

    drop(to_serve2);
    task2.await.unwrap().unwrap();
}

/// A host raster with zero size is rejected, and the next valid raster opens the session.
#[tokio::test]
async fn test_zero_sized_configuration_is_rejected() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-zero".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":0,"height":0,"scale":1.0}}}}
"#).await.unwrap();

    // Must receive an error response
    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    assert_eq!(
        error_json["body"]["error"], "invalidParams",
        "expected invalidParams error"
    );
    assert_eq!(error_json["body"]["reason"], "invalid image configure");

    // Verifies that the daemon was not called
    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.opens.len(),
            0,
            "daemon open should not be called for invalid params"
        );
    }

    // The original open request stays pending until a valid host raster arrives.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");

    assert_eq!(
        state_json["body"]["event"], "state",
        "valid open should produce state event"
    );

    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.opens.len(),
            1,
            "daemon open should be called for valid params"
        );
    }
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// A replacement raster with zero size is rejected.
#[tokio::test]
async fn test_replacement_raster_with_zero_size_is_rejected() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-resize-zero".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Sends open with the normal size
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // Reads the state response
    let _state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _initial_image = next_image_envelope(&mut lines).await;

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":0,"height":0,"scale":1.0}}}}
"#).await.unwrap();

    // Reads the response; it must be an error
    let error_line = next_line_except_frame_events(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error response");

    assert_eq!(
        error_json["body"]["error"], "invalidParams",
        "zero raster should return invalidParams"
    );

    // Verifies that the daemon resize was not called
    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.resizes.len(),
            0,
            "daemon resize should not be called for an invalid raster"
        );
    }

    // Sends screen.read to verify that the task is still alive
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"screen.read"}}
"#,
        )
        .await
        .unwrap();

    let screen_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let screen_json: serde_json::Value =
        serde_json::from_str(&screen_line).expect("failed to parse screen response");

    assert_eq!(
        screen_json["body"]["event"], "screen",
        "task should still be alive and respond to screen.read"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: a raster smaller than one cell keeps the session closed without an error, and the next
/// valid raster opens the session. A drag through the collapsed range sends such a raster on every
/// frame; an error there would show a wrong surface error and leave the host waiting for the
/// presentation barrier of a frame that never arrives (V5-96-14-6-4-8).
#[tokio::test]
async fn test_too_small_raster_holds_the_open() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-small".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // This raster has a positive size, but it is too small to hold one terminal cell.
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":3,"height":3,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // A sub-cell raster must not answer with an error, so the first line returned is the state
    // event for the open of the valid raster.
    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");

    assert_eq!(
        state_json["body"]["event"], "state",
        "the sub-cell raster must be held silently and the valid raster must open the session"
    );

    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.opens.len(),
            1,
            "the valid raster opens exactly one session"
        );
    }
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: a sub-cell replacement raster keeps the current grid and still presents a frame, so the
/// host presentation barrier completes. The PTY is resized to the unchanged grid, so no SIGWINCH is
/// sent.
#[tokio::test]
async fn test_sub_cell_replacement_holds_the_grid() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-hold-grid".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");
    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");
    let (opened_cols, opened_rows) = (
        state_json["body"]["cols"]
            .as_u64()
            .expect("state carries cols"),
        state_json["body"]["rows"]
            .as_u64()
            .expect("state carries rows"),
    );
    let initial_image = next_image_envelope(&mut lines).await;
    acknowledge_image(&mut to_serve, &initial_image).await;

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":3,"height":3,"scale":1.0}}}}
"#).await.unwrap();

    // The sub-cell raster is presented: instead of an error, a state event (same grid) and an image
    // envelope arrive. The screen event of the frame comes with the image, so the screen line is
    // skipped while waiting for the state. The sub-cell raster is still presented: after the
    // transfer is acknowledged, the pending configuration is applied, a state event with the
    // unchanged grid is sent, and a frame for the 3x3 raster is drawn. Intermediate envelopes of
    // the previous raster are acknowledged as the host acknowledges them.
    let mut held_state = None;
    let mut held_image = None;
    for _ in 0..30 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for the held raster")
            .expect("failed to read a line")
            .expect("output ended");
        let json: serde_json::Value = serde_json::from_str(&line).expect("failed to parse line");
        assert!(
            json["body"]["error"].is_null(),
            "the sub-cell replacement must not answer with an error: {line}"
        );
        if json["body"]["event"] == "state" {
            held_state = Some(json.clone());
        }
        if let Some(image) = json["body"]["image"].as_object() {
            if image.get("raster").and_then(|v| v.as_u64()) == Some(2) {
                held_image = Some(json);
                break;
            }
            acknowledge_image(&mut to_serve, &json).await;
        }
    }
    let held_state = held_state.expect("no state event arrived for the held raster");
    let held_image = held_image.expect("no image envelope arrived for the held raster");
    assert_eq!(
        (
            held_state["body"]["cols"].as_u64(),
            held_state["body"]["rows"].as_u64()
        ),
        (Some(opened_cols), Some(opened_rows)),
        "the grid is unchanged while the raster is smaller than one cell"
    );
    assert_eq!(
        (
            held_image["body"]["image"]["width"].as_u64(),
            held_image["body"]["image"]["height"].as_u64()
        ),
        (Some(3), Some(3)),
        "the frame is drawn for the sub-cell raster, so the presentation barrier completes"
    );

    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.resizes.len(),
            1,
            "the sub-cell replacement resizes the PTY once, to the unchanged grid"
        );
        assert_eq!(
            (
                calls_lock.resizes[0].1 as u64,
                calls_lock.resizes[0].2 as u64
            ),
            (opened_cols, opened_rows),
            "the PTY keeps the held grid dimensions"
        );
    }
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: an engine that panics reports an error as a surface event
#[tokio::test]
async fn test_panicking_surface_reports_error() {
    struct PanicEngine {
        cols: u16,
        rows: u16,
        panic_on_resize: bool,
    }

    impl PanicEngine {
        fn new() -> Self {
            Self {
                cols: 80,
                rows: 24,
                panic_on_resize: false,
            }
        }

        fn with_panic() -> Self {
            Self {
                cols: 80,
                rows: 24,
                panic_on_resize: true,
            }
        }
    }

    impl Engine for PanicEngine {
        fn set_theme(&mut self, _theme: soksak_sidecar_vt_core::TerminalTheme) {}

        fn resize(&mut self, cols: u16, rows: u16) {
            if self.panic_on_resize {
                panic!(
                    "test panic from engine: requested resize to {}x{}",
                    cols, rows
                );
            }
            self.cols = cols;
            self.rows = rows;
        }

        fn set_cell_metrics(&mut self, width: u16, height: u16) -> Result<(), String> {
            if width == 0 || height == 0 {
                return Err("terminal cell metrics must be positive".to_string());
            }
            Ok(())
        }

        fn feed(&mut self, _bytes: &[u8]) {}

        fn drain_events(&mut self) -> Vec<EngineEvent> {
            Vec::new()
        }

        fn resolve_clipboard(&mut self, request_id: u64, _text: &str) -> Result<(), String> {
            Err(format!("unknown clipboard request {request_id}"))
        }

        fn reject_clipboard(&mut self, request_id: u64, _reason: &str) -> Result<(), String> {
            Err(format!("unknown clipboard request {request_id}"))
        }

        fn selection_start(&mut self, _col: u16, _row: u16) -> Result<(), String> {
            Ok(())
        }
        fn selection_update(&mut self, _col: u16, _row: u16) -> Result<(), String> {
            Ok(())
        }
        fn selection_end(&mut self) -> Result<Option<String>, String> {
            Ok(Some("selected".to_string()))
        }
        fn selection_clear(&mut self) -> bool {
            false
        }
        fn selection_text(&self) -> Option<String> {
            Some("selected".to_string())
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

    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-panic".to_string();

    let panic_engine_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let panic_engine_flag_for_factory = panic_engine_flag.clone();

    let engine_factory = Arc::new(move || {
        if panic_engine_flag_for_factory.load(std::sync::atomic::Ordering::Relaxed) {
            Box::new(PanicEngine::with_panic()) as Box<dyn Engine>
        } else {
            Box::new(PanicEngine::new()) as Box<dyn Engine>
        }
    });

    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Enables a panic in the engine
    panic_engine_flag.store(true, std::sync::atomic::Ordering::Relaxed);

    // Sends open; the resize must trigger the panic
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // Waits for the error event
    let mut found_error = false;
    for _ in 0..10 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line()).await
        {
            let json: serde_json::Value =
                serde_json::from_str(&line).expect("failed to parse JSON");

            if let Some(event) = json.get("body").and_then(|b| b.get("event"))
                && event == "error"
            {
                found_error = true;
                if let Some(reason) = json.get("body").and_then(|b| b.get("reason")) {
                    assert!(
                        reason.as_str().unwrap().contains("surface task ended"),
                        "error reason should mention surface task ended"
                    );
                }
                break;
            }
        }
    }

    assert!(
        found_error,
        "should receive error event when surface task panics"
    );

    drop(to_serve);
    let shutdown = task.await.unwrap();
    assert!(
        shutdown.is_err(),
        "surface shutdown failure must remain observable"
    );
}

/// Reads output lines until an image envelope arrives. Screen event lines are skipped.
async fn next_image_envelope(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
) -> serde_json::Value {
    for _ in 0..20 {
        let line = tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .expect("timeout waiting for an image envelope")
            .expect("failed to read an output line")
            .expect("output ended");
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
            && json["body"]["image"].is_object()
        {
            return json;
        }
    }
    panic!("no image envelope arrived");
}

// Answers the image envelope with consumed, as the host does. The sidecar does not apply the next
// raster configuration until it is answered.
async fn acknowledge_image(to_serve: &mut tokio::io::DuplexStream, envelope: &serde_json::Value) {
    let image = &envelope["body"]["image"];
    let consumed = serde_json::json!({"surface": envelope["surface"], "body": {"image": {"consumed": {
        "name": image["name"], "generation": image["generation"],
        "raster": image["raster"], "sequence": image["sequence"]}}}});
    to_serve
        .write_all(format!("{consumed}\n").as_bytes())
        .await
        .unwrap();
}

// Waits for the image envelope of the given raster. An image drawn with a previous raster, such as
// one from a cursor tick, is answered and skipped.
async fn next_image_of_raster(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
    to_serve: &mut tokio::io::DuplexStream,
    raster: u64,
) -> serde_json::Value {
    for _ in 0..20 {
        let envelope = next_image_envelope(lines).await;
        if envelope["body"]["image"]["raster"].as_u64() == Some(raster) {
            return envelope;
        }
        acknowledge_image(to_serve, &envelope).await;
    }
    panic!("no image envelope for raster {raster} arrived");
}

/// A replacement raster is not used until the previous transfer is acknowledged.
#[tokio::test]
async fn test_replacement_raster_waits_for_prior_transfer() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-resize-image".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // Opens with an 800x384 image
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let image1 = next_image_envelope(&mut lines).await;
    let image1_obj = image1["body"]["image"]
        .as_object()
        .expect("first image envelope");
    assert_eq!(
        image1_obj["width"].as_u64(),
        Some(800),
        "first image width should be 800"
    );
    assert_eq!(
        image1_obj["height"].as_u64(),
        Some(384),
        "first image height should be 384"
    );

    // The host acknowledges the immutable snapshot before it replaces the raster.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":1600,"height":768,"scale":1.0}}}}
"#).await.unwrap();

    let image2 = next_image_of_raster(&mut lines, &mut to_serve, 2).await;
    let image2_obj = image2["body"]["image"]
        .as_object()
        .expect("image envelope after resize");
    assert_eq!(
        image2_obj["width"].as_u64(),
        Some(1600),
        "image width after resize should be 1600, got {}",
        image2_obj["width"]
    );
    assert_eq!(
        image2_obj["height"].as_u64(),
        Some(768),
        "image height after resize should be 768, got {}",
        image2_obj["height"]
    );
    assert_eq!(image2_obj["raster"].as_u64(), Some(2));
    assert_eq!(
        image2_obj["sequence"].as_u64(),
        Some(1),
        "a new raster starts a new sequence"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: after a release cycle, output while an image is unresolved is not presented again
#[tokio::test]
async fn test_no_image_envelope_while_outstanding_after_release() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-outstanding".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // First image
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"one\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });
    let first = next_image_envelope(&mut lines).await;
    assert_eq!(
        first["body"]["image"]["sequence"].as_u64(),
        Some(1),
        "first image envelope sequence"
    );

    // Releases sequence 1
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // Second image after the release (the frame was empty again)
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"two\r\n".to_vec(),
        sequence: 1,
        truncated: false,
    });
    let second = next_image_envelope(&mut lines).await;
    assert_eq!(
        second["body"]["image"]["sequence"].as_u64(),
        Some(2),
        "second image envelope sequence"
    );

    // Output while sequence 2 is unresolved must not be presented again before that release
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"three\r\n".to_vec(),
        sequence: 2,
        truncated: false,
    });

    let result =
        tokio::time::timeout(std::time::Duration::from_millis(300), lines.next_line()).await;
    if let Ok(Ok(Some(line))) = result {
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(
            json["body"]["image"].is_null(),
            "must not present an image while the previous one is outstanding, got: {}",
            line
        );
    }

    // Releasing sequence 2 presents the content that arrived in the meantime
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
"#).await.unwrap();

    let mut caught_up = false;
    for _ in 0..20 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
            && json["body"]["image"]["sequence"].as_u64() == Some(3)
        {
            caught_up = true;
            break;
        }
    }
    assert!(
        caught_up,
        "release of sequence 2 should present the caught-up image"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: an error response re-enables drawing on the next output
#[tokio::test]
async fn test_image_error_response_reenables_drawing() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-image-error".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"one\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });
    let first = next_image_envelope(&mut lines).await;
    assert_eq!(
        first["body"]["image"]["sequence"].as_u64(),
        Some(1),
        "first image envelope sequence"
    );

    // The host rejects the image
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"error":"scale","name":"view","generation":1,"raster":1,"sequence":1}}}
"#).await.unwrap();

    // The next output must be drawn again
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"two\r\n".to_vec(),
        sequence: 1,
        truncated: false,
    });

    let mut drew_again = false;
    for _ in 0..20 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
            && json["body"]["image"]["sequence"].as_u64() == Some(2)
        {
            drew_again = true;
            break;
        }
    }
    assert!(
        drew_again,
        "output after an image error must present a new image envelope"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// The state event after a replacement raster carries the full session fields.
#[tokio::test]
async fn test_replacement_raster_state_has_cell_dimensions() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session-resize-state".to_string();

    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let calls_for_factory = calls.clone();
    let session_id_for_factory = fake_session_id.clone();

    let port = Arc::new(FakeSessionPort::new(
        session_id_for_factory.clone(),
        calls_for_factory.clone(),
    ));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _open_state = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _initial_image = next_image_envelope(&mut lines).await;
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":1600,"height":768,"scale":1.0}}}}
"#).await.unwrap();

    // Reads up to the state event of the resize. An image made by a cursor tick can arrive first,
    // and each image must be answered with consumed, as the host does, before the pending raster 2
    // configuration is applied.
    let mut state_json: Option<serde_json::Value> = None;
    for _ in 0..10 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for resize response")
            .unwrap()
            .unwrap();

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
            if json["body"]["event"].as_str() == Some("state") {
                state_json = Some(json);
                break;
            }
            if json["body"]["image"].is_object() {
                acknowledge_image(&mut to_serve, &json).await;
            }
        }
    }

    let state = state_json.expect("resize should answer with a state event");
    assert!(
        state["body"]["sessionId"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "resize state event should carry sessionId"
    );
    let cell_width = state["body"]["cellWidth"]
        .as_f64()
        .expect("resize state event should carry cellWidth");
    let cell_height = state["body"]["cellHeight"]
        .as_f64()
        .expect("resize state event should carry cellHeight");
    assert!(
        cell_width > 0.0,
        "cellWidth should be positive, got {}",
        cell_width
    );
    assert!(
        cell_height > 0.0,
        "cellHeight should be positive, got {}",
        cell_height
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_font_applies_the_first_installed_family_of_a_list() {
    let input = r#"
{"surface":"s1","body":{"operation":"font","family":"No Such Terminal Font Family;Menlo","size":13}}
{"surface":"s1","body":{"operation":"font","family":"Courier","size":13}}
{"surface":"s1","body":{"operation":"font","family":"No Such Terminal Font Family","size":13}}
{"surface":"s1","body":{"operation":"font","family":" ; ","size":13}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let session_port: Arc<dyn SessionPort> = Arc::new(tracked_session_port::FakeSessionPort::new());
    let session_port_for_factory = session_port.clone();
    let factory = Arc::new(move || session_port_for_factory.clone());

    serve(engine_factory, reader, &mut writer, factory)
        .await
        .expect("font contract");
    let outputs = String::from_utf8(writer)
        .expect("utf8 output")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let applied = outputs
        .iter()
        .filter(|value| value["body"]["event"] == "font")
        .map(|value| value["body"]["family"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(applied.len(), 3);
    assert_eq!(applied[..2], ["Menlo".to_string(), "Courier".to_string()]);
    let system = outputs
        .iter()
        .filter(|value| value["body"]["event"] == "font")
        .map(|value| value["body"]["system"].as_bool().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        system,
        vec![false, false, true],
        "only a list without installed families uses the system font"
    );
    let skipped = outputs
        .iter()
        .filter(|value| value["body"]["event"] == "font")
        .map(|value| value["body"]["skipped"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        skipped,
        vec![
            serde_json::json!(["No Such Terminal Font Family"]),
            serde_json::json!([]),
            serde_json::json!(["No Such Terminal Font Family"]),
        ],
        "each answer names the families it skipped"
    );
    let errors = outputs
        .iter()
        .filter(|value| value["body"]["error"] == "invalidParams")
        .map(|value| value["body"]["reason"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        errors,
        vec!["font.family must name at least one family".to_string()]
    );
}

/// Reads lines until a state event or an error response arrives.
async fn next_state(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
    expect: &str,
) -> serde_json::Value {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect(expect)
            .unwrap()
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&line).unwrap();
        if json["body"]["event"] == "state" || json["body"]["error"].is_string() {
            return json;
        }
    }
}

/// The font size sets the cell size (docs/spec/text-size.md). A size twice as large makes the cell
/// height twice as large, and an invalid size is an error.
#[tokio::test]
async fn test_font_size_sets_the_cell_size() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(
        "test-session-font-size".to_string(),
        calls.clone(),
    ));
    let factory = Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let initial = next_state(&mut lines, "the open state").await;
    let base = initial["body"]["cellHeight"].as_f64().unwrap();
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"font","family":"Menlo","size":26}}
"#,
        )
        .await
        .unwrap();
    let larger = next_state(&mut lines, "the state after a font size change").await;
    let height = larger["body"]["cellHeight"].as_f64().unwrap();
    assert!(
        (height / base - 2.0).abs() < 0.1,
        "a font twice as large doubles the cell height: {base} -> {height}"
    );
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"font","family":"Menlo","size":0}}
{"surface":"s1","body":{"operation":"font","family":"Menlo"}}
"#,
        )
        .await
        .unwrap();
    let invalid = next_state(&mut lines, "the invalid size error").await;
    assert_eq!(invalid["body"]["error"], "invalidParams");
    assert!(
        invalid["body"]["reason"]
            .as_str()
            .unwrap()
            .contains("font.size"),
        "{invalid}"
    );
    let missing = next_state(&mut lines, "the missing size error").await;
    assert!(
        missing["body"]["reason"]
            .as_str()
            .unwrap()
            .contains("font.size"),
        "{missing}"
    );
    drop(to_serve);
    let _ = task.await;
}

/// Joins the cell characters of one line of a screen event. In JSON each character is a cell
/// object, so a string search cannot find it.
/// The full screen that a page holds after each screen message: a full form replaces it and a change replaces the rows it
/// carries (core docs/spec/persistent-services.md#screen-message).
#[derive(Default)]
struct ScreenGrid {
    number: Option<u64>,
    cells: Vec<serde_json::Value>,
}

impl ScreenGrid {
    /// Parses a line and returns it; a screen message is returned with every cell of the screen in `cells`.
    fn apply_line(&mut self, line: &str) -> serde_json::Value {
        let mut message: serde_json::Value = serde_json::from_str(line).expect("sidecar line JSON");
        let body = &mut message["body"];
        if body["event"] != "screen" {
            return message;
        }
        if body["base"] == true {
            self.cells = body["cells"].as_array().expect("full form cells").clone();
        } else {
            assert_eq!(
                body["previous"].as_u64(),
                self.number,
                "a change follows the held screen: {body}"
            );
            let cols = body["cols"].as_u64().unwrap() as usize;
            for change in body["changed"].as_array().expect("change form rows") {
                let row = change["row"].as_u64().unwrap() as usize;
                for (col, cell) in change["cells"].as_array().unwrap().iter().enumerate() {
                    self.cells[row * cols + col] = cell.clone();
                }
            }
            body["cells"] = serde_json::Value::Array(self.cells.clone());
        }
        self.number = body["number"].as_u64();
        message
    }
}

fn screen_text(grid: &mut ScreenGrid, line: &str) -> String {
    let value = grid.apply_line(line);
    value["body"]["cells"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|cell| cell["grapheme"].as_str())
        .collect()
}

#[tokio::test]
async fn output_while_the_host_holds_a_raster_is_drawn_once_after_the_answer() {
    // Output that arrives while the host holds the previous raster is not drawn over it. After the
    // answer, it draws once, and a screen read shows the latest output.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let session_id = "held-raster".to_string();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(session_id.clone(), calls.clone()));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1024 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    // Reads up to the first presentation of the open. That raster is still in the host.
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for the first presentation")
            .unwrap()
            .unwrap();
        if line.contains(r#""kind":"iosurface-global""#) {
            break;
        }
    }
    let mut grid = ScreenGrid::default();
    for index in 0..50 {
        port.push_event(DaemonEvent::Output {
            session_id: session_id.clone(),
            data: format!("line{index}\r\n").into_bytes(),
            sequence: index,
            truncated: false,
        });
    }
    // The output channel and the request channel have no ordering, so consumed is sent only after
    // the read answer shows the last output. Every screen event in between must be an answer to a
    // read request.
    let mut reads = 0;
    let mut screens_before = 0;
    let mut presentations_before = 0;
    'fed: loop {
        assert!(reads < 200, "200 screen reads did not show the last output");
        to_serve
            .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"screen.read\"}}\n")
            .await
            .unwrap();
        reads += 1;
        loop {
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
                .await
                .expect("timeout waiting for a screen read")
                .unwrap()
                .unwrap();
            if line.contains(r#""kind":"iosurface-global""#) {
                presentations_before += 1;
            }
            if line.contains(r#""event":"screen""#) {
                screens_before += 1;
                if screen_text(&mut grid, &line).contains("line49") {
                    break 'fed;
                }
                break;
            }
        }
    }
    assert_eq!(
        presentations_before, 0,
        "output was drawn over a raster the host still held"
    );
    assert_eq!(
        screens_before, reads,
        "output during a held raster sent screen events besides the read answers"
    );
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();
    let mut screens = Vec::new();
    let mut presentations = 0;
    while let Ok(Some(line)) =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .map(|line| line.unwrap())
    {
        if line.contains(r#""kind":"iosurface-global""#) {
            presentations += 1;
        }
        if line.contains(r#""event":"screen""#) {
            screens.push(line);
        }
    }
    let read = read_screen(&mut to_serve, &mut lines).await;
    assert!(
        screen_text(&mut grid, &read.to_string()).contains("line49"),
        "the screen after the presentation is not the latest output: {read}"
    );
    drop(to_serve);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
    assert_eq!(
        presentations, 1,
        "the held raster must be followed by exactly one presentation"
    );
    assert!(
        screens.is_empty(),
        "the presentation sent screen events: {screens:?}"
    );
}

#[tokio::test]
async fn the_page_screen_event_carries_only_text_width_links_and_set_attributes() {
    // The page uses only characters, widths, links, and selection (inverse). The raster draws the
    // colors. Sending the default fields for each cell makes one 122×18 screen full of characters
    // about 240 KB, which crosses the host and the page on every scroll frame.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let session_id = "compact".to_string();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(session_id.clone(), calls.clone()));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1024 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    port.push_event(DaemonEvent::Output {
        session_id: session_id.clone(),
        data: b"text".to_vec(),
        sequence: 0,
        truncated: false,
    });
    let mut screen = None;
    let mut grid = ScreenGrid::default();
    for _ in 0..200 {
        to_serve
            .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"screen.read\"}}\n")
            .await
            .unwrap();
        let line = loop {
            let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
                .await
                .expect("timeout waiting for a screen read")
                .unwrap()
                .unwrap();
            if line.contains(r#""event":"screen""#) {
                break line;
            }
        };
        if screen_text(&mut grid, &line).contains("text") {
            screen = Some(line);
            break;
        }
    }
    drop(to_serve);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
    let line = screen.expect("200 screen reads did not show the output");
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    // Each cell has the members of the screen hash and may have a link; the message has no member
    // of the earlier cell form.
    for cell in value["body"]["cells"].as_array().unwrap() {
        for key in cell.as_object().unwrap().keys() {
            assert!(
                matches!(
                    key.as_str(),
                    "grapheme" | "width" | "fg" | "bg" | "attributes" | "link"
                ),
                "the page cell carries {key} in {line}"
            );
        }
        for key in ["grapheme", "width", "fg", "bg", "attributes"] {
            assert!(
                cell.get(key).is_some(),
                "the page cell lacks {key} in {line}"
            );
        }
    }
}

#[tokio::test]
async fn a_focus_change_presents_its_frame_before_its_acknowledgement() {
    // Focus changes the drawn shape of the cursor. The frame that draws it is presented before the
    // input is acknowledged, and a screen read after it shows the focused cursor.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new("focus".to_string(), calls.clone()));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1024 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    macro_rules! next {
        () => {
            tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
                .await
                .expect("timeout waiting for sidecar output")
                .unwrap()
                .unwrap()
        };
    }
    // Answers every presentation with consumed, as the host does, and reads until no presentation
    // precedes the screen read answer, so that the sidecar becomes idle.
    macro_rules! consume {
        ($sequence:expr) => {
            to_serve.write_all(format!(
                "{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"sequence\":{}}}}}}}}}\n",
                $sequence
            ).as_bytes()).await.unwrap();
        };
    }
    let mut rounds = 0;
    loop {
        rounds += 1;
        assert!(rounds < 20, "the sidecar kept presenting");
        to_serve
            .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"screen.read\"}}\n")
            .await
            .unwrap();
        let mut presented = false;
        loop {
            let line = next!();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            if let Some(sequence) = value["body"]["image"]["sequence"].as_i64() {
                consume!(sequence);
                presented = true;
                continue;
            }
            if value["body"]["event"] == "screen" {
                break;
            }
        }
        if !presented {
            break;
        }
    }
    to_serve.write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"input\",\"actions\":[{\"type\":\"focus\",\"focused\":true}],\"preedit\":{\"text\":\"\",\"markedRange\":null,\"selectedRange\":null,\"replacementRange\":null,\"attributed\":false}}}\n").await.unwrap();
    let mut seen = Vec::new();
    let mut presented = false;
    loop {
        let line = next!();
        seen.push(line.chars().take(160).collect::<String>());
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value["body"]["image"]["sequence"].is_i64() {
            presented = true;
            continue;
        }
        if value["body"]["ack"] == true {
            assert!(
                presented,
                "the focus change was acknowledged before its frame: {seen:#?}"
            );
            break;
        }
    }
    let screen = read_screen(&mut to_serve, &mut lines).await;
    assert_eq!(screen["body"]["cursor"]["focused"], true, "{screen}");
    task.abort();
}

fn mouse(phase: &str, x: f64, pressed: bool, shift: bool) -> String {
    let input_id = uuid::Uuid::new_v4().to_string();
    format!(
        "{{\"surface\":\"s1\",\"body\":{{\"operation\":\"mouse\",\"inputId\":\"{input_id}\",\"phase\":\"{phase}\",\"x\":{x},\"y\":0.5,\"pressed\":{pressed},\"shift\":{shift},\"alt\":false,\"ctrl\":false}}}}\n"
    )
}

fn selection(operation: &str, x: f64) -> String {
    if operation == "selection.end" {
        return "{\"surface\":\"s1\",\"body\":{\"operation\":\"selection.end\"}}\n".to_string();
    }
    format!(
        "{{\"surface\":\"s1\",\"body\":{{\"operation\":\"{operation}\",\"x\":{x},\"y\":0.5}}}}\n"
    )
}

/// The page sends press, move, selection operations, and release in that order.
fn drag_gesture(shift: bool) -> String {
    [
        mouse("down", 0.5, true, shift),
        mouse("move", 200.5, true, shift),
        selection("selection.start", 0.5),
        selection("selection.update", 200.5),
        selection("selection.end", 0.0),
        mouse("up", 200.5, false, shift),
    ]
    .concat()
}

fn written(writes: &[Vec<u8>]) -> Vec<String> {
    writes
        .iter()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .collect()
}

#[tokio::test]
async fn a_drag_is_reported_as_press_motion_and_release_when_the_program_tracks_button_motion() {
    let modes = Modes {
        mouse_drag: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (output, _, writes) = serve_scroll(Some(modes), &drag_gesture(false)).await;
    let writes = written(&writes);
    assert_eq!(writes.len(), 3, "{writes:?}");
    assert_eq!(writes[0], "\x1b[<0;1;1M");
    assert!(
        output.contains(r#""copied":false"#),
        "reported drag must not select text: {output}"
    );
}

#[tokio::test]
async fn a_shift_drag_selects_instead_of_reporting() {
    let modes = Modes {
        mouse_drag: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (_, _, writes) = serve_scroll(Some(modes), &drag_gesture(true)).await;
    assert!(writes.is_empty(), "{:?}", written(&writes));
}

#[tokio::test]
async fn a_click_program_receives_press_and_release_but_no_motion() {
    let modes = Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let (_, _, writes) = serve_scroll(Some(modes), &drag_gesture(false)).await;
    let writes = written(&writes);
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[0], "\x1b[<0;1;1M");
    assert!(
        writes[1].starts_with("\x1b[<0;") && writes[1].ends_with(";1m"),
        "{writes:?}"
    );
}

#[tokio::test]
async fn a_reported_click_clears_a_previous_text_selection() {
    let modes = Modes {
        mouse_click: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let requests = [
        mouse("down", 0.5, true, false),
        selection("selection.start", 0.5),
        selection("selection.end", 0.0),
        mouse("up", 0.5, false, false),
        "{\"surface\":\"s1\",\"body\":{\"operation\":\"copy\"}}\n".to_string(),
    ]
    .concat();
    let (output, _, writes) = serve_scroll(Some(modes), &requests).await;
    assert_eq!(written(&writes), vec!["\x1b[<0;1;1M", "\x1b[<0;1;1m"]);
    assert!(
        output.contains("\"copied\":false,\"event\":\"copy\""),
        "{output}"
    );
}

#[tokio::test]
async fn pointer_input_is_not_reported_without_a_mouse_mode() {
    let (_, _, writes) = serve_scroll(None, &drag_gesture(false)).await;
    assert!(writes.is_empty(), "{:?}", written(&writes));
}

#[tokio::test]
async fn any_motion_tracking_reports_moves_without_a_button_once_per_cell() {
    let modes = Modes {
        mouse_motion: true,
        sgr_mouse: true,
        ..Modes::default()
    };
    let requests = [
        mouse("move", 0.5, false, false),
        mouse("move", 1.0, false, false),
        mouse("move", 200.5, false, false),
    ]
    .concat();
    let (_, _, writes) = serve_scroll(Some(modes), &requests).await;
    let writes = written(&writes);
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[0], "\x1b[<35;1;1M");
    assert!(
        writes[1].starts_with("\x1b[<35;") && writes[1] != writes[0],
        "{writes:?}"
    );
}

#[tokio::test]
async fn an_invalid_mouse_operation_is_rejected() {
    let (output, _, writes) = serve_scroll(None,
        "{\"surface\":\"s1\",\"body\":{\"operation\":\"mouse\",\"phase\":\"hover\",\"x\":1,\"y\":1,\"pressed\":false,\"shift\":false,\"alt\":false,\"ctrl\":false}}\n").await;
    assert!(output.contains("mouse requires phase"), "{output}");
    assert!(writes.is_empty());
}

#[tokio::test]
async fn focus_changes_are_reported_to_a_program_that_enables_focus_reports() {
    let focus = |focused: bool| {
        format!(
            "{{\"surface\":\"s1\",\"body\":{{\"operation\":\"input\",\"actions\":[{{\"type\":\"focus\",\"focused\":{focused}}}],\"preedit\":{{\"text\":\"\",\"markedRange\":null,\"selectedRange\":null,\"replacementRange\":null,\"attributed\":false}}}}}}\n"
        )
    };
    let requests = [focus(true), focus(true), focus(false)].concat();
    let modes = Modes {
        focus_in_out: true,
        ..Modes::default()
    };
    let (_, _, writes) = serve_scroll(Some(modes), &requests).await;
    assert_eq!(
        written(&writes),
        vec!["\x1b[I".to_string(), "\x1b[O".to_string()],
        "a focus report must follow each change and only a change"
    );
    let (_, _, writes) = serve_scroll(None, &requests).await;
    assert!(
        writes.is_empty(),
        "focus changes must not be written without ?1004: {:?}",
        written(&writes)
    );
}

#[tokio::test]
async fn keypad_keys_follow_the_application_keypad_mode() {
    let keys = "{\"surface\":\"s1\",\"body\":{\"operation\":\"input\",\"actions\":[{\"type\":\"key\",\"key\":\"Keypad5\",\"shift\":false,\"alt\":false,\"ctrl\":false},{\"type\":\"key\",\"key\":\"KeypadEnter\",\"shift\":false,\"alt\":false,\"ctrl\":false}],\"preedit\":{\"text\":\"\",\"markedRange\":null,\"selectedRange\":null,\"replacementRange\":null,\"attributed\":false}}}\n";
    let (_, _, writes) = serve_scroll(
        Some(Modes {
            app_keypad: true,
            ..Modes::default()
        }),
        keys,
    )
    .await;
    assert_eq!(written(&writes), vec!["\x1bOu\x1bOM".to_string()]);
    let (_, _, writes) = serve_scroll(None, keys).await;
    assert_eq!(written(&writes), vec!["5\r".to_string()]);
}

#[tokio::test]
async fn an_inline_image_event_follows_the_presentation_that_draws_the_image() {
    // The page image state changes together with the raster that drew the image. If the screen
    // arrives first, the page takes the screen without the image as the current image state.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let session = "inline-order".to_string();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(session.clone(), calls));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    // Receives the presentation of the open but does not answer consumed yet. Image output arrives
    // meanwhile.
    let mut first = None;
    while first.is_none() {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        first = value["body"]["image"]["sequence"].as_i64();
    }
    port.push_event(DaemonEvent::Output {
        session_id: session,
        data: b"\x1b]1337;File=name=cmVkLnBuZw==;inline=1:RED\x07".to_vec(),
        sequence: 0,
        truncated: false,
    });
    let consumed = |sequence: i64| {
        format!(
            "{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"sequence\":{sequence}}}}}}}}}\n"
        )
    };
    let mut seen = Vec::new();
    let mut presented_after_output = false;
    let mut released = false;
    loop {
        let line =
            match tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
                .await
            {
                Ok(line) => line.unwrap().unwrap(),
                Err(_) if !released => {
                    // Waits long enough for the image output to be processed, then consumes the
                    // first presentation.
                    to_serve
                        .write_all(consumed(first.unwrap()).as_bytes())
                        .await
                        .unwrap();
                    released = true;
                    continue;
                }
                Err(_) => panic!("no inline image event arrived: {seen:#?}"),
            };
        seen.push(line.chars().take(120).collect::<String>());
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if let Some(sequence) = value["body"]["image"]["sequence"].as_i64() {
            presented_after_output = true;
            to_serve
                .write_all(consumed(sequence).as_bytes())
                .await
                .unwrap();
        }
        if value["body"]["event"] == "image.inline" {
            assert!(
                presented_after_output,
                "the inline image event came before the presentation that draws it: {seen:#?}"
            );
            break;
        }
    }
    task.abort();
}

/// A selection point goes to the nearest cell edge, so when the pointer passes the midpoint of a
/// cell, that cell is selected.
#[tokio::test]
async fn test_selection_points_go_to_the_nearest_cell_edge() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let cell = soksak_sidecar_vt_core::platform::metrics(13.0, 1.0).cell_width as f64;
    let input = format!(
        r#"{{"surface":"s1","body":{{"operation":"open","shell":"/bin/sh"}}}}
{{"surface":"s1","body":{{"image":{{"configure":{{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}}}}}
{{"surface":"s1","body":{{"operation":"selection.start","x":{},"y":1.0}}}}
{{"surface":"s1","body":{{"operation":"selection.update","x":{},"y":1.0}}}}
{{"surface":"s1","body":{{"operation":"selection.update","x":{},"y":1.0}}}}
"#,
        cell * 0.4,
        cell * 3.4,
        cell * 3.6
    );
    let reader = std::io::Cursor::new(input.into_bytes());
    let mut writer = Vec::new();
    let cells = Arc::new(Mutex::new(Vec::new()));
    let recorded = cells.clone();
    let engine_factory = Arc::new(move || {
        let mut engine = MockEngine::new();
        engine.selected_cells = recorded.clone();
        Box::new(engine) as Box<dyn Engine>
    });
    let calls_for_factory = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "unused".to_string(),
            calls_for_factory.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    assert_eq!(
        *cells.lock().unwrap(),
        vec![(0, 0), (3, 0), (4, 0)],
        "a point before a cell's midpoint stays at its left edge and a point past it goes to its right edge"
    );
}

#[tokio::test]
async fn mouse_results_echo_the_requested_input_identity() {
    let requests = ["down", "move", "up"]
        .iter()
        .enumerate()
        .map(|(index, phase)| {
            serde_json::json!({"surface":"s1","body": {
                "operation":"mouse", "inputId":format!("gesture-{index}"), "phase":phase,
                "x":0.5 + index as f64 * 20.0, "y":0.5, "pressed":*phase != "up",
                "shift":false,"alt":false,"ctrl":false
            }})
            .to_string()
                + "\n"
        })
        .collect::<String>();
    let (output, _, _) = serve_scroll(
        Some(Modes {
            mouse_motion: true,
            sgr_mouse: true,
            ..Modes::default()
        }),
        &requests,
    )
    .await;
    let ids = output
        .lines()
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            (value["body"]["event"] == "mouse").then(|| value["body"]["inputId"].clone())
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["gesture-0", "gesture-1", "gesture-2"]);
}

#[tokio::test]
async fn mouse_without_an_input_identity_is_rejected() {
    let request = serde_json::json!({"surface":"s1","body": {
        "operation":"mouse", "phase":"down", "x":0.5, "y":0.5,
        "pressed":true,"shift":false,"alt":false,"ctrl":false
    }})
    .to_string()
        + "\n";
    let (output, _, writes) = serve_scroll(
        Some(Modes {
            mouse_motion: true,
            ..Modes::default()
        }),
        &request,
    )
    .await;
    assert!(output.contains("inputId"), "{output}");
    assert!(writes.is_empty());
}

/// The host aligns the edges of the native region to device pixels, so the region can be less than
/// one device pixel smaller than the view. A point in that band is not an error. A point farther
/// out reports an error in a result that carries the inputId of that mouse operation.
#[tokio::test]
async fn a_mouse_point_in_the_snapping_band_is_accepted_and_a_failure_keeps_its_input_identity() {
    let request = |id: &str, y: f64| {
        serde_json::json!({"surface":"s1","body": {
            "operation":"mouse", "inputId":id, "phase":"move", "x":1.0, "y":y, "pressed":false,
            "shift":false,"alt":false,"ctrl":false
        }})
        .to_string()
            + "\n"
    };
    let requests = request("band", 384.5) + &request("outside", 385.0) + &request("after", 1.0);
    let (output, _, _) = serve_scroll(None, &requests).await;
    let results = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["body"]["event"] == "mouse")
        .map(|value| {
            (
                value["body"]["inputId"].clone(),
                value["body"]["error"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results,
        vec![
            (serde_json::json!("band"), serde_json::Value::Null),
            (
                serde_json::json!("outside"),
                serde_json::json!(
                    "invalidParams: selection coordinates are outside the terminal region: 1,385"
                )
            ),
            (serde_json::json!("after"), serde_json::Value::Null),
        ],
        "{output}"
    );
    assert!(
        !output
            .lines()
            .any(|line| line.contains(r#""error":"invalidParams""#)),
        "a mouse failure is a mouse result, not a bare error: {output}"
    );
}

#[link(name = "IOSurface", kind = "framework")]
unsafe extern "C" {
    // Finds a global IOSurface by its ID, as the host finds a transfer image.
    fn IOSurfaceLookup(id: u32) -> *const std::ffi::c_void;
    fn IOSurfaceSetValue(
        buffer: *const std::ffi::c_void,
        key: *const std::ffi::c_void,
        value: *const std::ffi::c_void,
    );
    fn IOSurfaceCopyValue(
        buffer: *const std::ffi::c_void,
        key: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const std::ffi::c_void);
    fn CFEqual(left: *const std::ffi::c_void, right: *const std::ffi::c_void) -> u8;
    fn CFStringCreateWithCString(
        allocator: *const std::ffi::c_void,
        text: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const std::ffi::c_void;
}

/// Whether the transfer image with that ID still exists. The found reference is released at once so
/// that it does not extend the lifetime. The state of the transfer image that a check marked.
/// IOSurface ids are global and reused, so a surface that a lookup finds by the id of a released
/// image can be another surface; the mark tells them apart.
#[derive(Debug, PartialEq)]
enum TransferImage {
    /// The id names the marked image.
    Marked,
    /// The id names another surface, which a later allocation gave the released id.
    Other,
    /// No surface has the id.
    Absent,
}

/// The key of the value by which a check marks the transfer image it watches.
const TRANSFER_MARK: &std::ffi::CStr = c"soksak.test.transfer";

/// A CFString of text. The caller releases it.
fn cf_string(text: &std::ffi::CStr) -> *const std::ffi::c_void {
    // kCFStringEncodingUTF8.
    unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), 0x0800_0100) }
}

/// Marks the transfer image id with mark. The value does not keep the surface alive.
fn mark_transfer_image(id: u32, mark: &std::ffi::CStr) {
    let surface = unsafe { IOSurfaceLookup(id) };
    assert!(
        !surface.is_null(),
        "transfer image {id} does not exist when it is marked"
    );
    let (key, value) = (cf_string(TRANSFER_MARK), cf_string(mark));
    unsafe {
        IOSurfaceSetValue(surface, key, value);
        CFRelease(value);
        CFRelease(key);
        CFRelease(surface);
    }
}

/// The state of the transfer image id that mark_transfer_image marked with mark.
fn transfer_image(id: u32, mark: &std::ffi::CStr) -> TransferImage {
    let surface = unsafe { IOSurfaceLookup(id) };
    if surface.is_null() {
        return TransferImage::Absent;
    }
    let (key, expected) = (cf_string(TRANSFER_MARK), cf_string(mark));
    let value = unsafe { IOSurfaceCopyValue(surface, key) };
    let marked = !value.is_null() && unsafe { CFEqual(value, expected) } != 0;
    unsafe {
        if !value.is_null() {
            CFRelease(value);
        }
        CFRelease(expected);
        CFRelease(key);
        CFRelease(surface);
    }
    if marked {
        TransferImage::Marked
    } else {
        TransferImage::Other
    }
}

/// A request that makes the surface release its image state before the answer to the transfer
/// image.
#[derive(Clone, Copy, Debug)]
enum LettingGo {
    Close,
    Reconnect,
}

/// The action that releases a remaining transfer image.
#[derive(Clone, Copy, Debug)]
enum Release {
    Answer,
    ConnectionEnd,
}

/// Waits for the next answer of that surface. The answer to close is an empty body, and the answer
/// to reconnect is a session event.
async fn wait_for_reply<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    letting_go: LettingGo,
) {
    loop {
        let line = next_line_except_frame_events(lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(message["body"].get("error").is_none(), "{line}");
        let replied = match letting_go {
            LettingGo::Close => message["body"] == serde_json::json!({}),
            LettingGo::Reconnect => message["body"]["event"] == "session",
        };
        if message["surface"] == "s1" && replied {
            return;
        }
    }
}

/// Opens a surface, receives its first presentation request, and makes the surface release its
/// image state without answering. That transfer image must remain until the host answers it or the
/// connection that carried it ends. The close answer that follows the answer shows that the input
/// loop has processed it.
async fn check_unanswered_transfer(persistent: bool, letting_go: LettingGo, release: Release) {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("transfer".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = if persistent {
        let owner = PersistentOwner {
            close: Arc::new(|| Ok(())),
            registry: PersistentRegistry::new(),
            owner: "transfer-owner".to_string(),
            log: events_test::TestEvents::new().handle(),
        };
        tokio::spawn(serve_with_registry(
            engine_factory,
            serve_in,
            serve_out,
            factory,
            owner,
        ))
    } else {
        tokio::spawn(serve(engine_factory, serve_in, serve_out, factory))
    };
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    let case = format!("persistent {persistent}, {letting_go:?}, {release:?}");
    // Each check marks its image with a value of its own, because checks run in parallel and reuse
    // released ids.
    let mark = std::ffi::CString::new(format!("{} {case}", std::process::id())).unwrap();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let id = loop {
        let line = next_line_except_frame_events(&mut lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if let Some(image) = message["body"].get("image") {
            assert_eq!(image["sequence"], 1, "{line}");
            let id = image["token"]["id"].as_u64().expect("the token has no id");
            break u32::try_from(id).expect("the token id is not a u32");
        }
    };
    mark_transfer_image(id, &mark);

    let request: &[u8] = match letting_go {
        LettingGo::Close => b"{\"surface\":\"s1\",\"body\":{\"operation\":\"close\"}}\n",
        LettingGo::Reconnect => b"{\"surface\":\"s1\",\"body\":{\"operation\":\"reconnect\"}}\n",
    };
    to_serve.write_all(request).await.unwrap();
    wait_for_reply(&mut lines, letting_go).await;
    assert_eq!(
        transfer_image(id, &mark),
        TransferImage::Marked,
        "{case}: the service released transfer image {id} before the host answered its frame"
    );

    match release {
        Release::Answer => {
            to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"close"}}
"#).await.unwrap();
            wait_for_reply(&mut lines, LettingGo::Close).await;
            assert_ne!(
                transfer_image(id, &mark),
                TransferImage::Marked,
                "{case}: the service kept transfer image {id} after the host answered its frame"
            );
            drop(to_serve);
            task.await.unwrap().unwrap();
            let calls = calls.lock().unwrap();
            assert_eq!(
                (calls.opens.len(), calls.closes.len()),
                (1, 1),
                "{case}: the answer opened or closed a session: {calls:?}"
            );
        }
        Release::ConnectionEnd => {
            drop(to_serve);
            task.await.unwrap().unwrap();
            assert_ne!(
                transfer_image(id, &mark),
                TransferImage::Marked,
                "{case}: the service kept transfer image {id} after the connection that carried it ended"
            );
        }
    }
}

/// The transfer image of a closed surface also remains until the host answers that presentation.
/// When the answer arrives, the image is released, and the answer does not create the surface
/// again.
#[tokio::test]
async fn a_closed_surface_keeps_its_unanswered_transfer_image_until_the_host_answers() {
    check_unanswered_transfer(false, LettingGo::Close, Release::Answer).await;
}

/// When the connection ends without an answer, no further answer can arrive, so the transfer image
/// of the closed surface is released.
#[tokio::test]
async fn a_closed_surface_releases_its_unanswered_transfer_image_when_the_connection_ends() {
    check_unanswered_transfer(false, LettingGo::Close, Release::ConnectionEnd).await;
}

/// reconnect discards the native image state, but a transfer image that waits for an answer remains
/// until the answer arrives.
#[tokio::test]
async fn a_reconnected_surface_keeps_its_unanswered_transfer_image_until_the_host_answers() {
    check_unanswered_transfer(false, LettingGo::Reconnect, Release::Answer).await;
}

#[tokio::test]
async fn a_reconnected_surface_releases_its_unanswered_transfer_image_when_the_connection_ends() {
    check_unanswered_transfer(false, LettingGo::Reconnect, Release::ConnectionEnd).await;
}

/// The close of a persistent connection closes the surface through the registry. Its transfer image
/// follows the same rule.
#[tokio::test]
async fn a_persistent_closed_surface_keeps_its_unanswered_transfer_image_until_the_host_answers() {
    check_unanswered_transfer(true, LettingGo::Close, Release::Answer).await;
}

#[tokio::test]
async fn a_persistent_closed_surface_releases_its_unanswered_transfer_image_when_the_connection_ends()
 {
    check_unanswered_transfer(true, LettingGo::Close, Release::ConnectionEnd).await;
}

#[tokio::test]
async fn a_detached_surface_keeps_its_unanswered_transfer_until_the_exact_frame_answer() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new(
        "detach-transfer".into(),
        calls.clone(),
    ));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    let mark = std::ffi::CString::new(format!("{} detached transfer", std::process::id())).unwrap();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let frame = next_image(&mut lines).await;
    let id = u32::try_from(
        frame["token"]["id"]
            .as_u64()
            .expect("frame has a transfer token"),
    )
    .expect("transfer token fits the native identifier");
    mark_transfer_image(id, &mark);

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"detach":{"request":"detach-transfer","name":"view","generation":1}}}}
"#).await.unwrap();
    let ack = next_image_detach_reply(&mut lines, "detach-transfer").await;
    assert_eq!(ack["body"]["image"]["detached"]["ok"], true, "{ack}");
    assert_eq!(
        transfer_image(id, &mark),
        TransferImage::Marked,
        "detach released transfer {id} before the host answered the frame"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"close"}}
"#).await.unwrap();
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();
    assert_ne!(
        transfer_image(id, &mark),
        TransferImage::Marked,
        "the exact frame answer did not release transfer {id}"
    );
    let calls = calls.lock().unwrap();
    assert_eq!((calls.opens.len(), calls.closes.len()), (1, 1), "{calls:?}");
}

/// Waits for the next presentation request and returns its image.
async fn next_image<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> serde_json::Value {
    loop {
        let line = next_line_except_frame_events(lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(message["body"].get("error").is_none(), "{line}");
        if let Some(image) = message["body"].get("image") {
            return image.clone();
        }
    }
}

async fn next_image_detach_reply<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    request: &str,
) -> serde_json::Value {
    loop {
        let line = next_line_except_frame_events(lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if message["body"]["image"]["detached"]["request"] == request {
            return message;
        }
    }
}

/// An empty input transaction whose reply carries the `attached` barrier, so the answer shows that
/// the connection has processed its open.
const ATTACH_BARRIER: &[u8] = br#"{"surface":"s1","cids":["attached"],"body":{"operation":"input","actions":[],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;

/// Waits for the reply that carries the `cids` barrier.
async fn wait_for_cids_reply<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    cid: &str,
) {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for the barrier reply")
            .expect("failed to read a sidecar line")
            .expect("the sidecar closed its output");
        if line.contains(&format!(r#""cids":["{cid}"]"#)) {
            return;
        }
    }
}

/// A detach refused as a stale attachment answers its request. A second connection of the same owner
/// attaches the surface, which replaces the epoch of the first connection, so the detach of the first
/// connection is refused. The refusal carries the request, name and generation of the detach.
#[tokio::test]
async fn a_detach_refused_as_a_stale_attachment_answers_its_request() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("stale-epoch".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let registry = PersistentRegistry::new();
    let owner = || PersistentOwner {
        close: Arc::new(|| Ok(())),
        registry: registry.clone(),
        owner: "stale-owner".to_string(),
        log: events_test::TestEvents::new().handle(),
    };

    let (mut first_in, first_serve_in) = tokio::io::duplex(1 << 20);
    let (first_serve_out, first_from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let first_task = tokio::spawn(serve_with_registry(
        engine_factory.clone(),
        first_serve_in,
        first_serve_out,
        factory.clone(),
        owner(),
    ));
    let mut first_lines = tokio::io::BufReader::new(first_from_serve).lines();
    first_in
        .write_all(
            br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
"#,
        )
        .await
        .unwrap();
    first_in.write_all(ATTACH_BARRIER).await.unwrap();
    wait_for_cids_reply(&mut first_lines, "attached").await;

    let (mut second_in, second_serve_in) = tokio::io::duplex(1 << 20);
    let (second_serve_out, second_from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let second_task = tokio::spawn(serve_with_registry(
        engine_factory,
        second_serve_in,
        second_serve_out,
        factory,
        owner(),
    ));
    let mut second_lines = tokio::io::BufReader::new(second_from_serve).lines();
    second_in
        .write_all(
            br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
"#,
        )
        .await
        .unwrap();
    second_in.write_all(ATTACH_BARRIER).await.unwrap();
    wait_for_cids_reply(&mut second_lines, "attached").await;

    first_in
        .write_all(br#"{"surface":"s1","body":{"image":{"detach":{"request":"stale-detach","name":"view","generation":1}}}}
"#)
        .await
        .unwrap();
    // The output of a surface goes to the connection that is attached to it, so the refusal of the
    // first connection is read from the second connection.
    let line = next_line_except_frame_events(&mut second_lines).await;
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        reply["body"]["image"]["detached"]["request"], "stale-detach",
        "the stale attachment refusal does not answer the detach request: {line}"
    );
    assert_eq!(reply["body"]["image"]["detached"]["name"], "view", "{line}");
    assert_eq!(
        reply["body"]["image"]["detached"]["generation"], 1,
        "{line}"
    );
    assert_eq!(reply["body"]["image"]["detached"]["ok"], false, "{line}");
    assert_eq!(
        reply["body"]["image"]["detached"]["error"], "stale attachment",
        "{line}"
    );

    // The surface is closed through the attached connection, so both connections end.
    second_in
        .write_all(
            br#"{"surface":"s1","body":{"operation":"close"}}
"#,
        )
        .await
        .unwrap();
    // The answer to close is an empty body.
    loop {
        let line = next_line_except_frame_events(&mut second_lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if message["body"] == serde_json::json!({}) {
            break;
        }
    }
    drop(first_in);
    drop(second_in);
    tokio::time::timeout(std::time::Duration::from_secs(5), first_task)
        .await
        .expect("the first connection did not end within 5 s")
        .unwrap()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), second_task)
        .await
        .expect("the second connection did not end within 5 s")
        .unwrap()
        .unwrap();
}

/// A malformed detach whose request is a nonempty string answers its request with the name and
/// generation as received and an invalidParams error. The detach has generation 0, which is refused.
#[tokio::test]
async fn a_malformed_detach_with_a_readable_request_answers_its_request() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new(
        "malformed-detach".into(),
        calls.clone(),
    ));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh"}}
{"surface":"s1","body":{"image":{"detach":{"request":"malformed-detach","name":"view","generation":0}}}}
"#).await.unwrap();
    let line = next_line_except_frame_events(&mut lines).await;
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        reply["body"]["image"]["detached"]["request"], "malformed-detach",
        "the malformed detach does not answer its request: {line}"
    );
    assert_eq!(reply["surface"], "s1", "{line}");
    assert_eq!(reply["body"]["image"]["detached"]["name"], "view", "{line}");
    assert_eq!(
        reply["body"]["image"]["detached"]["generation"], 0,
        "{line}"
    );
    assert_eq!(reply["body"]["image"]["detached"]["ok"], false, "{line}");
    assert_eq!(
        reply["body"]["image"]["detached"]["error"], "invalidParams",
        "{line}"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// A transfer image left by reconnect and a presentation request of a new raster can have the same
/// name and sequence. The host answer carries the generation and the raster. The answer of the new
/// raster releases the drawing wait of that surface and does not release the remaining transfer
/// image. The remaining image is released by the answer of its own raster.
#[tokio::test]
async fn the_answer_of_a_new_raster_is_not_taken_for_a_retained_transfer_of_the_same_sequence() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("raster".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    let mark = std::ffi::CString::new(format!("{} retained raster", std::process::id())).unwrap();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(
        (first["raster"].as_u64(), first["sequence"].as_u64()),
        (Some(1), Some(1)),
        "{first}"
    );
    let retained = u32::try_from(first["token"]["id"].as_u64().expect("the token has no id"))
        .expect("the token id is not a u32");
    mark_transfer_image(retained, &mark);

    to_serve
        .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"reconnect\"}}\n")
        .await
        .unwrap();
    wait_for_reply(&mut lines, LettingGo::Reconnect).await;
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let second = next_image(&mut lines).await;
    assert_eq!(
        (second["raster"].as_u64(), second["sequence"].as_u64()),
        (Some(2), Some(1)),
        "{second}"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":2,"sequence":1}}}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":3,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    // An answer that does not reach the surface leaves it waiting, and the configure of raster 3
    // draws no frame.
    let third = next_image(&mut lines).await;
    assert_eq!(third["raster"].as_u64(), Some(3), "{third}");
    assert_eq!(
        transfer_image(retained, &mark),
        TransferImage::Marked,
        "the answer of raster 2 released the retained transfer image of raster 1"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"close"}}
"#).await.unwrap();
    wait_for_reply(&mut lines, LettingGo::Close).await;
    assert_ne!(
        transfer_image(retained, &mark),
        TransferImage::Marked,
        "the answer of raster 1 did not release its retained transfer image"
    );
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Sends `screen.read` for the surface s1 and returns its answer.
async fn read_screen<R: tokio::io::AsyncBufRead + Unpin>(
    to_serve: &mut tokio::io::DuplexStream,
    lines: &mut tokio::io::Lines<R>,
) -> serde_json::Value {
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"screen.read\"}}\n")
        .await
        .unwrap();
    next_screen_line(lines).await
}

/// Waits for the next `screen` line, which is the answer to `screen.read`.
async fn next_screen_line<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> serde_json::Value {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for a screen line")
            .expect("failed to read a sidecar line")
            .expect("the sidecar closed its output");
        if line.contains(r#""event":"screen""#) {
            return serde_json::from_str(&line).unwrap();
        }
    }
}

async fn next_measured_frame_after(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
    to_serve: &mut tokio::io::DuplexStream,
    after: u64,
    acknowledgements: &mut usize,
    calls: &Arc<Mutex<Calls>>,
    grid: &mut ScreenGrid,
) -> (u64, usize, usize, usize, usize, usize, serde_json::Value) {
    let mut screens = std::collections::HashMap::new();
    let mut images = std::collections::HashMap::new();
    let mut recent_messages = Vec::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "measured frame arrives within 2 s; pending calls: {:?}; recent output: {:?}",
                    calls.lock().unwrap(),
                    recent_messages
                )
            })
            .expect("output read succeeds")
            .expect("sidecar output remains open");
        let message = grid.apply_line(&line);
        recent_messages.push(message.clone());
        if recent_messages.len() > 8 {
            recent_messages.remove(0);
        }
        if message["body"]["ack"] == true {
            *acknowledgements += 1;
        }
        if message["body"]["event"] == "screen" {
            let sequence = message["body"]["frame"]["sequence"].as_u64().unwrap_or(0);
            if sequence > after {
                screens.insert(sequence, message.clone());
            }
        }
        if let Some(image) = message["body"].get("image") {
            let sequence = image["sequence"].as_u64().unwrap_or(0);
            if sequence > after {
                let counts = {
                    let calls = calls.lock().unwrap();
                    (
                        *acknowledgements,
                        calls.writes.len(),
                        calls.writes_completed,
                        calls.echoes_enqueued,
                    )
                };
                images.insert(sequence, counts);
                // The sidecar sends no screen with a frame. A screen read before the answer names
                // the frame that shows the screen.
                let read = serde_json::json!({"surface": message["surface"], "body": {"operation": "screen.read"}});
                to_serve
                    .write_all(format!("{read}\n").as_bytes())
                    .await
                    .unwrap();
                acknowledge_image(to_serve, &message).await;
            }
        }
        if let Some(sequence) = screens
            .keys()
            .copied()
            .find(|sequence| images.contains_key(sequence))
        {
            let screen = screens.remove(&sequence).unwrap();
            let (ack_count, writes, completed, echoes) = images[&sequence];
            return (
                sequence,
                rendered_glyph_count(&screen),
                ack_count,
                writes,
                completed,
                echoes,
                screen,
            );
        }
    }
}

fn rendered_glyph_count(message: &serde_json::Value) -> usize {
    let body = &message["body"];
    let cols = body["cols"].as_u64().unwrap() as u16;
    let rows = body["rows"].as_u64().unwrap() as u16;
    let cells = body["cells"].as_array().unwrap();
    let lines = cells
        .chunks(usize::from(cols))
        .map(|line| {
            line.iter()
                .map(|cell| Cell {
                    ch: cell["grapheme"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .map(Into::into),
                    width: cell["width"].as_u64().unwrap() as u8,
                    ..Cell::default()
                })
                .collect()
        })
        .collect();
    let screen = Screen {
        cols,
        rows,
        cursor: serde_json::from_value(body["cursor"].clone()).unwrap(),
        scrollback: Default::default(),
        background: body["background"].as_str().unwrap().to_string(),
        lines,
    };
    let metrics = metrics(13.0, 1.0);
    let width = (metrics.cell_width * f32::from(cols)).ceil() as u32;
    let height = (metrics.cell_height * f32::from(rows)).ceil() as u32;
    let frame = Frame::new(width, height).unwrap();
    frame.draw(&screen, &metrics).unwrap();
    let background = frame.read_pixel(0, 0).unwrap();
    let has_ink = |row: u16, col: u16, width_cells: u16| {
        let x0 = (f32::from(col) * metrics.cell_width).floor() as u32;
        let x1 = (f32::from(col + width_cells) * metrics.cell_width).ceil() as u32;
        let y0 = (f32::from(row) * metrics.cell_height).floor() as u32;
        let y1 = (f32::from(row + 1) * metrics.cell_height).ceil() as u32;
        (x0..x1).any(|x| {
            (y0..y1).any(|y| {
                frame
                    .read_pixel(x, y)
                    .is_some_and(|pixel| pixel != background)
            })
        })
    };
    let mut count = 0;
    for row in 0..rows {
        let mut col = 0;
        while col < cols {
            if row == screen.cursor.row
                && col == screen.cursor.col
                && let Some(preedit) = screen.cursor.preedit.as_ref()
            {
                for _ in preedit.text.chars() {
                    if has_ink(row, col, 2) {
                        count += 1;
                    }
                    col = col.saturating_add(2);
                }
            }
            let cell = &screen.lines[usize::from(row)][usize::from(col.min(cols - 1))];
            if cell.ch.as_ref().is_some_and(|text| !text.is_empty()) {
                if has_ink(row, col, u16::from(cell.width.max(1))) {
                    count += 1;
                }
                col = col.saturating_add(u16::from(cell.width.max(1)));
            } else {
                col += 1;
            }
        }
    }
    count
}

#[cfg(feature = "diagnostics")]
#[tokio::test]
async fn a_screen_read_publishes_render_metadata_for_its_drawn_frame() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new(
        "render-frame-session".into(),
        calls.clone(),
    ));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let trace_dir = tempfile::tempdir().expect("trace folder");
    let events = events_test::TestEvents::with_trace_dir(trace_dir.path());
    let task = tokio::spawn(serve_with_events(
        engine_factory,
        serve_in,
        serve_out,
        factory,
        events.handle(),
    ));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":3,"raster":7,"width":805,"height":391,"scale":1.0}}}}
"#).await.unwrap();
    let image_envelope = next_image_envelope(&mut lines).await;
    let image = &image_envelope["body"]["image"];
    let screen = read_screen(&mut to_serve, &mut lines).await;
    let image_identity = serde_json::json!({
        "generation": image["generation"],
        "raster": image["raster"],
        "sequence": image["sequence"]
    });
    assert_eq!(
        image_identity,
        serde_json::json!({ "generation": 3, "raster": 7, "sequence": 1 })
    );
    assert_eq!(screen["body"]["frame"], image_identity);
    assert_eq!(screen["body"]["render"]["frame"], image_identity);
    assert_eq!(screen["body"]["render"]["pixel"]["width"], 805);
    assert_eq!(screen["body"]["render"]["pixel"]["height"], 391);

    to_serve.write_all(br#"{"surface":"s1","body":{"operation":"font","family":"Menlo","size":14},"cids":["pending-render-input"]}
"#).await.unwrap();
    loop {
        let line = next_line_except_frame_events(&mut lines).await;
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        if message["cids"] == serde_json::json!(["pending-render-input"]) {
            break;
        }
    }
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"screen.read"}}
"#,
        )
        .await
        .unwrap();
    let waiting = next_screen_line(&mut lines).await;
    assert_eq!(
        waiting["body"]["frame"],
        serde_json::json!({ "generation": 3, "raster": 7, "sequence": 2 })
    );
    assert!(
        waiting["body"]["render"].is_null(),
        "the pending screen was paired with metadata from the previous draw: {waiting}"
    );

    acknowledge_image(&mut to_serve, &image_envelope).await;
    let second_envelope = next_image_envelope(&mut lines).await;
    let second_screen = read_screen(&mut to_serve, &mut lines).await;
    let second_image = &second_envelope["body"]["image"];
    let second_identity = serde_json::json!({
        "generation": second_image["generation"],
        "raster": second_image["raster"],
        "sequence": second_image["sequence"]
    });
    assert_eq!(second_screen["body"]["frame"], second_identity);
    assert_eq!(second_screen["body"]["render"]["frame"], second_identity);
    acknowledge_image(&mut to_serve, &second_envelope).await;
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"close"}}
"#,
        )
        .await
        .unwrap();
    wait_for_reply(&mut lines, LettingGo::Close).await;
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// The answer to `screen.read` names the presented frame that shows that screen as `frame`. After
/// the answered frame, the named frame is that frame.
#[tokio::test]
async fn a_screen_read_names_the_answered_frame_that_shows_the_screen() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("raster".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(
        (first["raster"].as_u64(), first["sequence"].as_u64()),
        (Some(1), Some(1)),
        "{first}"
    );

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#).await.unwrap();
    let read = next_screen_line(&mut lines).await;
    assert_eq!(
        read["body"]["frame"],
        serde_json::json!({"generation": 1, "raster": 1, "sequence": 1}),
        "{read}"
    );
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// A screen that changes while the frame is unanswered is shown by the next frame, so `screen.read`
/// names that frame.
#[tokio::test]
async fn a_screen_read_names_the_next_frame_while_a_change_waits_for_the_answer() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("raster".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(
        (first["raster"].as_u64(), first["sequence"].as_u64()),
        (Some(1), Some(1)),
        "{first}"
    );

    // The first frame stays unanswered. A font change resizes the engine and marks the screen as
    // changed, and the reply to `screen.read` follows it in the same order as the requests.
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"font","family":"Menlo","size":14}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#,
        )
        .await
        .unwrap();
    let read = next_screen_line(&mut lines).await;
    assert_eq!(
        read["body"]["frame"],
        serde_json::json!({"generation": 1, "raster": 1, "sequence": 2}),
        "{read}"
    );
    assert!(
        read["body"]["cursor"]["caret"].is_null(),
        "an unsent future frame must not publish a native caret: {read}"
    );
    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// A controlled draw failure leaves the changed screen out of any frame. The screen names the next
/// frame, and a later change draws it.
#[cfg(feature = "fault-injection")]
#[tokio::test(flavor = "current_thread")]
async fn a_failed_draw_keeps_the_screen_unsent_so_the_next_frame_names_it() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let port = Arc::new(FakeSessionPort::new("raster".into(), calls.clone()));
    let factory_port = port.clone();
    let factory = Arc::new(move || factory_port.clone() as Arc<dyn SessionPort>);
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    let first = next_image(&mut lines).await;
    assert_eq!(
        (first["raster"].as_u64(), first["sequence"].as_u64()),
        (Some(1), Some(1)),
        "{first}"
    );
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // The accepted composition is valid; the test seam fails only the next raster draw.
    fault::fail_next_draw();
    let compose = serde_json::json!({
        "surface": "s1",
        "body": {"operation": "input", "actions": [], "preedit": {
            "text": "a",
            "markedRange": {"location": 0, "length": 1},
            "selectedRange": {"location": 1, "length": 0},
            "replacementRange": null,
            "attributed": true
        }}
    });
    to_serve
        .write_all(format!("{compose}\n").as_bytes())
        .await
        .unwrap();
    let failure = next_line_except_frame_events(&mut lines).await;
    assert!(
        failure.contains("frame draw failed by fault-injection"),
        "{failure}"
    );

    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"screen.read"}}
"#,
        )
        .await
        .unwrap();
    let read = next_screen_line(&mut lines).await;
    assert_eq!(
        read["body"]["frame"],
        serde_json::json!({"generation": 1, "raster": 1, "sequence": 2}),
        "{read}"
    );

    // The one-shot draw fault is consumed; a different valid composition draws the frame the screen
    // named.
    let short = serde_json::json!({
        "surface": "s1",
        "body": {"operation": "input", "actions": [], "preedit": {
            "text": "b",
            "markedRange": {"location": 0, "length": 1},
            "selectedRange": {"location": 1, "length": 0},
            "replacementRange": null,
            "attributed": true
        }}
    });
    to_serve
        .write_all(format!("{short}\n").as_bytes())
        .await
        .unwrap();
    let drawn = next_image(&mut lines).await;
    assert_eq!(
        (drawn["raster"].as_u64(), drawn["sequence"].as_u64()),
        (Some(1), Some(2)),
        "{drawn}"
    );
    drop(to_serve);
    task.await.unwrap().unwrap();
}

// An envelope whose `cids` is not a list of strings is answered on its surface with an error that
// names the cids.
#[tokio::test]
async fn an_envelope_with_invalid_cids_is_answered_on_its_surface() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = r#"{"surface":"s1","cids":[1],"body":{"operation":"input","actions":[{"type":"insert","text":"hi","replacementRange":null,"attributed":false}],"preedit":{"text":"","markedRange":null,"selectedRange":null,"replacementRange":null,"attributed":false}}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let factory_calls = calls.clone();
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "invalid-cids".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    let output = String::from_utf8(writer).unwrap();
    let reply: serde_json::Value = serde_json::from_str(output.lines().next().unwrap_or(""))
        .unwrap_or_else(|error| panic!("no reply line: {error}: {output}"));
    assert_eq!(
        reply["surface"], "s1",
        "the reply is not on the surface: {output}"
    );
    assert_eq!(reply["body"]["error"], "invalidParams", "{output}");
    assert!(
        reply["body"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("cids")),
        "the reason does not name the cids: {output}"
    );
}

/// Serves an open followed by the requests and returns the output.
async fn serve_requests(requests: &str) -> String {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let input = format!(
        "{}\n{}",
        r#"{"surface":"s1","body":{"operation":"open","shell":"/bin/sh"}}"#, requests
    );
    let reader = std::io::Cursor::new(input.into_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let session_port_factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new("drawing".to_string(), calls.clone())) as Arc<dyn SessionPort>
    });
    let _ = serve(engine_factory, reader, &mut writer, session_port_factory).await;
    String::from_utf8(writer).unwrap()
}

const DRAWING_CONFIGURE: &str = r#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}"#;

#[tokio::test]
async fn a_region_that_is_not_drawing_gets_no_raster_and_no_screen() {
    let output = serve_requests(&format!(
        "{}\n{}\n",
        r#"{"surface":"s1","body":{"image":{"drawing":{"name":"view","generation":1,"drawing":false}}}}"#,
        DRAWING_CONFIGURE
    ))
    .await;
    assert_eq!(
        output.matches(r#""kind":"iosurface-global""#).count(),
        0,
        "a raster was sent for a region that is not drawing: {output}"
    );
    assert!(
        !output.contains(r#""event":"screen""#),
        "a screen message was sent for a region that is not drawing: {output}"
    );
    assert!(
        !output.contains(r#""error""#),
        "the drawing message was answered with an error: {output}"
    );
}

#[tokio::test]
async fn a_region_that_draws_again_gets_one_raster_and_no_screen() {
    let output = serve_requests(&format!(
        "{}\n{}\n{}\n",
        r#"{"surface":"s1","body":{"image":{"drawing":{"name":"view","generation":1,"drawing":false}}}}"#,
        DRAWING_CONFIGURE,
        r#"{"surface":"s1","body":{"image":{"drawing":{"name":"view","generation":1,"drawing":true}}}}"#,
    ))
    .await;
    assert_eq!(
        output.matches(r#""kind":"iosurface-global""#).count(),
        1,
        "a region that draws again did not get exactly one raster: {output}"
    );
    assert_eq!(
        output.matches(r#""event":"screen""#).count(),
        0,
        "a frame sends no screen message: {output}"
    );
}

#[tokio::test]
async fn a_drawing_message_of_an_earlier_generation_is_ignored() {
    // The region of generation 2 draws its configure, keeps drawing after a late state of
    // generation 1, and draws the viewport change after the host consumed the first raster.
    let output = serve_requests(&format!(
        "{}\n{}\n{}\n{}\n",
        DRAWING_CONFIGURE.replace(r#""generation":1"#, r#""generation":2"#),
        r#"{"surface":"s1","body":{"image":{"drawing":{"name":"view","generation":1,"drawing":false}}}}"#,
        r#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":2,"raster":1,"sequence":1}}}}"#,
        r#"{"surface":"s1","body":{"operation":"viewport","offset":1}}"#,
    ))
    .await;
    assert_eq!(
        output.matches(r#""kind":"iosurface-global""#).count(),
        2,
        "a drawing state of generation 1 stopped the region of generation 2: {output}"
    );
}

#[tokio::test]
async fn an_invalid_drawing_message_is_answered_with_invalid_params() {
    let output = serve_requests(
        "{\"surface\":\"s1\",\"body\":{\"image\":{\"drawing\":{\"name\":\"view\",\"generation\":1,\"drawing\":\"no\"}}}}\n",
    )
    .await;
    assert!(
        output.contains(r#""reason":"invalid image drawing""#),
        "an invalid drawing message was not answered with invalidParams: {output}"
    );
}

// The sidecar sends no screen message for a frame and answers `screen.read` with the full screen
// (the second screen form of core docs/spec/persistent-services.md): the raster shows the screen,
// and the page reads the cells only when it needs them.
#[tokio::test]
async fn a_frame_sends_its_view_when_the_cursor_or_the_scrollback_changes() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let factory_calls = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "view".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r##"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"theme","mode":"light","background":"#e8f5ee","foreground":"#12684a","cursor":"#12684a","selection":"#d8dbe4"}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
{"surface":"s1","body":{"operation":"theme","mode":"dark","background":"#101418","foreground":"#e8f5ee","cursor":"#e8f5ee","selection":"#2a3038"}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":3}}}}
"##;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    serve(engine_factory, reader, &mut writer, factory)
        .await
        .unwrap();
    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<serde_json::Value> = output
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let frames: Vec<u64> = lines
        .iter()
        .filter_map(|value| value["body"]["image"]["sequence"].as_u64())
        .collect();
    let views: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|value| value["body"]["event"] == "view")
        .map(|value| &value["body"])
        .collect();
    assert_eq!(frames, vec![1, 2, 3], "each theme draws a frame: {output}");
    // The second theme keeps the cursor and the scrollback of the second frame, so the third frame
    // sends no view.
    let sequences: Vec<&serde_json::Value> = views
        .iter()
        .map(|view| &view["frame"]["sequence"])
        .collect();
    assert_eq!(sequences, vec![1, 2], "{output}");
    assert_ne!(
        (&views[0]["cursor"], &views[0]["scrollback"]),
        (&views[1]["cursor"], &views[1]["scrollback"]),
        "{output}"
    );
    let view = views[0];
    assert_eq!(view["frame"]["generation"], 1, "{view}");
    assert!(view["cursor"]["caret"]["row"].is_u64(), "{view}");
    assert!(view["cursor"]["caret"]["col"].is_u64(), "{view}");
    assert!(view["cursor"]["visible"].is_boolean(), "{view}");
    assert!(view["scrollback"]["history"].is_u64(), "{view}");
    assert!(view["scrollback"]["offset"].is_u64(), "{view}");
}

#[tokio::test]
async fn a_frame_sends_no_screen_message_and_a_screen_read_answers_the_full_screen() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let factory_calls = calls.clone();
    let factory = Arc::new(move || {
        Arc::new(FakeSessionPort::new(
            "read".to_string(),
            factory_calls.clone(),
        )) as Arc<dyn SessionPort>
    });
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"screen.read"}}
"#;
    let reader = std::io::Cursor::new(input.as_bytes());
    let mut writer = Vec::new();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    serve(engine_factory, reader, &mut writer, factory)
        .await
        .unwrap();
    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<serde_json::Value> = output
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let frames = lines
        .iter()
        .filter(|value| value["body"]["image"]["sequence"].is_number())
        .count();
    let screens: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|value| value["body"]["event"] == "screen")
        .collect();
    assert_eq!(frames, 1, "{output}");
    assert_eq!(
        screens.len(),
        1,
        "only the reply of screen.read is a screen message: {output}"
    );
    assert!(
        screens[0]["body"]["cells"].is_array(),
        "the reply is the full form: {}",
        screens[0]
    );
}
