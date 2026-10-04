#[path = "support/session_port_test.rs"]
mod tracked_session_port;

use async_trait::async_trait;
/// fake daemon을 사용하는 serve contract 통합 테스트
use soksak_sidecar_vt_core::protocol::{
    serve, Cell, Cursor, CursorShape, DaemonEvent, Engine, EngineEvent, Modes, Screen, SessionPort,
    ShellRequest,
};
use soksak_sidecar_vt_core::pty::PtyMeasurement;
use soksak_sidecar_vt_core::{
    inline_image::{Dimension, InlineImageCommand},
    TerminalTheme,
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// 표시와 함께 오는 화면 줄을 건너뛰고 다음 줄을 읽는다.
async fn next_line_except_screen<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> String {
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
            .await
            .expect("timeout waiting for a sidecar line")
            .expect("failed to read a sidecar line")
            .expect("the sidecar closed its output");
        if !line.contains(r#""event":"screen""#) {
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
        }
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
            // 그릴 수 있는 2×1 빨간 PNG.
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
        for history_bytes in &self.feed_history {
            if let Ok(s) = std::str::from_utf8(history_bytes) {
                let mut row = Vec::new();
                for ch in s.chars() {
                    let width = if (ch as u32) > 127 { 2 } else { 1 };
                    let cell = Cell {
                        ch: Some(ch.to_string()),
                        width,
                        ..Default::default()
                    };
                    row.push(cell);
                }
                if !row.is_empty() {
                    lines.push(row);
                }
            }
        }
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
{"surface":"owned-surface","body":{"operation":"input","bytes":"G103O2ZpbGU6Ly8vdG1wL3Byb2plY3QH"}}
"#;
    let (mut to_serve, serve_in) = tokio::io::duplex(16 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024);
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
    resizes: Vec<(String, u16, u16)>,
    detaches: Vec<String>,
    closes: Vec<String>,
    pendings: Vec<String>,
}

struct FakeSessionPort {
    session_id: String,
    calls: Arc<Mutex<Calls>>,
    event_tx: mpsc::UnboundedSender<DaemonEvent>,
    event_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<DaemonEvent>>>>,
}

impl FakeSessionPort {
    fn new(session_id: String, calls: Arc<Mutex<Calls>>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            session_id,
            calls,
            event_tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(Some(event_rx))),
        }
    }

    pub fn push_event(&self, event: DaemonEvent) {
        assert!(
            self.event_tx.send(event).is_ok(),
            "fake session event receiver closed"
        );
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
        self.calls
            .lock()
            .unwrap()
            .writes
            .push((session_id.to_string(), data.to_vec()));
        if data == b"\x1b]7;file:///tmp/project\x07" {
            let _ = self.event_tx.send(DaemonEvent::Output {
                session_id: self.session_id.clone(),
                data: data.to_vec(),
                sequence: 0,
                truncated: false,
            });
        }
        Ok(())
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
{"surface":"s1","body":{"operation":"input","bytes":"aGk="}}
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

    // write가 올바른 data로 호출되었는지 검증한다
    let calls_lock = calls.lock().unwrap();
    assert!(!calls_lock.writes.is_empty(), "write not called");
    assert_eq!(
        calls_lock.writes[0].0, fake_session_id,
        "session_id mismatch"
    );
    assert_eq!(calls_lock.writes[0].1, b"hi", "write data mismatch");
}

// pty.pending 은 열린 세션의 읽지 않은 입력 바이트 수와 reader 가 읽은 자식 출력 누적 바이트 수를
// 함께 응답하고 세션이 없으면 명시적 오류를 낸다.
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
        output.contains(r#""body":{"error":"Session not open","event":"pty.pending"},"surface":"s2""#),
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
{"surface":"s1","body":{"operation":"input","focus":{"focused":true}}}
{"surface":"s1","body":{"operation":"input","compose":{"text":"한","selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
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
{"surface":"s1","body":{"operation":"input","focus":{"focused":true}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
{"surface":"s1","body":{"operation":"input","compose":{"text":"ㅎ","selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":3}}}}
{"surface":"s1","body":{"operation":"input","compose":{"text":"하","selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":4}}}}
{"surface":"s1","body":{"operation":"input","compose":{"text":"한","selectedRange":{"location":1,"length":0},"replacementRange":null,"attributed":true}}}
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
    assert!(frames >= 5,
        "initial frame, focus frame, and all three successive preedit frames must be presented; got {frames}");
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
    assert!(String::from_utf8(writer)
        .unwrap()
        .contains("paste requires text string"));
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
    // 호스트가 첫 표시에 답한 뒤의 스크롤이다. 답하기 전의 스크롤은 답을 받은 뒤 한 화면으로 보인다.
    let (output, viewport, writes) = serve_scroll(
        None,
        r#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
{"surface":"s1","body":{"operation":"scroll","lines":3,"col":2,"row":1}}
{"surface":"s1","body":{"operation":"input","bytes":"aGk="}}
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
        "the screen event after a scroll carries scrollback: {output}"
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
{"surface":"s1","body":{"operation":"viewport","offset":-1}}
{"surface":"s1","body":{"operation":"viewport","offset":2.5}}
"#,
    )
    .await;
    assert_eq!(viewport, vec!["viewport 7".to_string()], "{output}");
    assert!(
        output.contains(r#""scrollback""#),
        "the viewport request is answered with a screen event: {output}"
    );
    assert_eq!(output.matches("invalidParams").count(), 2, "{output}");
    assert!(
        writes.is_empty(),
        "a viewport request writes nothing to the program"
    );
}

#[tokio::test]
async fn a_scroll_does_not_draw_over_a_raster_the_host_has_not_consumed() {
    // 호스트는 표시 요청의 래스터를 복사한 뒤 consumed 로 답한다. 그 전에 다시 그리면 복사 중인 래스터를 덮는다.
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

/// region에서 마지막 완전한 row 또는 column을 지난 point는 마지막 row와 마지막 column의 오른쪽 경계까지
/// 선택한다. region 을 한 device pixel 미만 지난 point 도 그렇고, 그보다 바깥의 point는 error이다.
#[tokio::test]
async fn test_selection_in_the_region_padding_selects_to_the_last_edge() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    // 801 x 383 픽셀은 칸 크기의 배수가 아니므로 마지막 완전한 행과 열 뒤에 여백이 남는다.
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

/// 빈 cell 위의 release는 error나 copy 없이 gesture 끝을 보고한다.
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

/// Test A-7: close op는 session을 끝낸다(detach가 아니라 close를 호출한다)
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
    assert!(close_replies
        .iter()
        .all(|message| message["surface"] == "s1"));

    // close가 호출되었고 detach는 호출되지 않았는지 검증한다
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

/// Test A-8: closed:true flag는 terminal session을 닫는다
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

    // close가 호출되었고 detach는 호출되지 않았는지 검증한다
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
    // 닫기를 마치면 core 사이드카 명세의 closed 답을 보낸다.
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
{"surface":"s1","body":{"operation":"input","bytes":"aGVsbG8="}}
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

    // 서비스는 시작할 때 글꼴을 먼저 읽는다. 이 검사는 EOF 뒤의 종료 시간만 재므로 같은 순서로 글꼴을 먼저 읽는다.
    // 새 프로세스의 첫 CoreText 호출은 2초 넘게 걸릴 수 있다(V5-7).
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
{"surface":"s1","body":{"operation":"input","bytes":"Yg=="}}
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
{"surface":"s1","body":{"operation":"input","bytes":"aGk="}}
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

/// Test A-2: push된 output이 screen에 도달한다
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

    // stdin/stdout에 tokio::io::duplex를 사용한다
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // open command를 보낸다
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // state response를 기다린다(sessionId를 포함해야 한다)
    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");
    assert_eq!(state_json["body"]["event"], "state", "expected state event");
    // 호스트처럼 첫 표시에 답한다. 답하기 전의 출력은 답을 받은 뒤의 화면으로 보인다.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // "hi\r\n"으로 output event를 push한다
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"hi\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // screen event를 기다린다("hi"를 포함해야 한다)
    let mut found_hi = false;
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

        let screen_json: serde_json::Value =
            serde_json::from_str(&screen_line).expect("failed to parse screen JSON");

        // 커서 상태 변경도 프레임을 만들 수 있다. 실제 호스트처럼 다음 프레임을 소비해야
        // 그 뒤에 도착한 PTY 출력이 화면으로 진행된다.
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
                last_consumed = sequence;
            }
        }

        if let Some(event) = screen_json.get("body").and_then(|b| b.get("event")) {
            if event == "screen" {
                // screen event를 찾았으므로 어떤 line이 "hi"를 포함하는지 확인한다
                if let Some(lines_arr) = screen_json.get("body").and_then(|b| b.get("lines")) {
                    if let Some(lines_vec) = lines_arr.as_array() {
                        for line in lines_vec {
                            if let Some(cells) = line.as_array() {
                                let mut line_text = String::new();
                                for cell in cells {
                                    if let Some(ch) = cell.get("ch").and_then(|c| c.as_str()) {
                                        line_text.push_str(ch);
                                    }
                                }
                                let trimmed = line_text.trim_end();
                                if trimmed == "hi" {
                                    found_hi = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if found_hi {
                    break;
                }
            }
        }
    }

    assert!(found_hi, "Output 'hi' did not appear in screen event");

    // stdin을 닫아 serve를 종료한다
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
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
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
        // 호스트처럼 표시에 답한다. 그림 이벤트는 그 그림을 그린 표시(또는 그리기 오류) 뒤에 온다.
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
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024);
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
{"surface":"s1","body":{"operation":"input","bytes":"aGk="}}
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

    // open response, input ack, screen.read screen response가 있어야 한다
    // 첫 line은 open response이다
    let open_json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(open_json["body"]["event"], "state");

    // "hi"를 포함한 screen event를 찾는다(screen.read에서 온다)
    let mut found_screen_read = false;
    for line in lines.iter().skip(1) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(event) = json.get("body").and_then(|b| b.get("event")) {
                if event == "screen" {
                    found_screen_read = true;
                    break;
                }
            }
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
    assert!(json["body"]["error"]
        .as_str()
        .unwrap()
        .contains("Unknown operation"));
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
    assert!(outputs
        .iter()
        .any(|value| value["body"]["event"] == "theme"));
    assert!(outputs.iter().any(|value| {
        value["body"]["error"] == "invalidParams"
            && value["body"]["reason"] == "theme.mode must be dark or light"
    }));
    // 색이 빠지거나 #rrggbb 가 아니면 모드가 맞아도 거부한다.
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
    assert!(outputs
        .iter()
        .any(|value| { value["body"]["event"] == "cursor" && value["body"]["blink"] == "Always" }));
    assert!(outputs.iter().any(|value| {
        value["body"]["error"] == "invalidParams"
            && value["body"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("cursor.blink"))
    }));
}

/// Test K1: app_cursor mode 없이 key를 encode한다
#[tokio::test]
async fn test_k1_keys_up_without_app_cursor() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","keys":[{"key":"Up"}]}}
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

/// Test K2: app_cursor mode로 key를 encode한다
#[tokio::test]
async fn test_k2_keys_up_with_app_cursor() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","keys":[{"key":"Up"}]}}
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

/// Test K3: ctrl과 UTF-8을 포함한 char key encoding
#[tokio::test]
async fn test_k3_char_key_encoding() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    // native 물리 키 매핑 뒤 Ctrl+C는 0x03, Ctrl+U는 0x15를 기록한다.
    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","keys":[{"key":"Char","text":"c","ctrl":true}]}}
{"surface":"s1","body":{"operation":"input","keys":[{"key":"Char","text":"u","ctrl":true}]}}
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

        // Test UTF-8 encoding (한)
    }

    let calls2 = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id2 = "test-session-2".to_string();

    let input2 = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","keys":[{"key":"Char","text":"한"}]}}
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

/// Test K4: 알 수 없는 key는 error를 반환하고 write하지 않는다
#[tokio::test]
async fn test_k4_unknown_key_returns_error() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let fake_session_id = "test-session".to_string();

    let input = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"operation":"input","keys":[{"key":"Nope"}]}}
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
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(error) = json.get("body").and_then(|b| b.get("error")) {
                if error.as_str().unwrap().contains("unknown key: Nope") {
                    found_error = true;
                    break;
                }
            }
        }
    }
    assert!(found_error, "should return error for unknown key");
}

/// open은 host가 정확한 native raster configuration을 제공할 때까지 동작하지 않는다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

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
    let (mut input, serve_input) = tokio::io::duplex(64 * 1024);
    let (serve_output, output) = tokio::io::duplex(64 * 1024);
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
    let (mut input, serve_input) = tokio::io::duplex(64 * 1024);
    let (serve_output, output) = tokio::io::duplex(64 * 1024);
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

/// Test I1: open에 image field가 있으면 output 뒤에 image envelope를 보낸다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // image field를 포함하여 open한다
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // state response를 기다린다
    let state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for state")
        .expect("failed to read state line")
        .expect("state line is empty");

    let state_json: serde_json::Value =
        serde_json::from_str(&state_line).expect("failed to parse state JSON");
    assert_eq!(state_json["body"]["event"], "state", "expected state event");

    // output event를 push한다
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"hi\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // image envelope를 기다린다
    let image_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for image envelope")
        .expect("failed to read image line")
        .expect("image line is empty");

    let image_json: serde_json::Value =
        serde_json::from_str(&image_line).expect("failed to parse image JSON");

    // image envelope 구조를 검증한다
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

/// Test I2: host가 응답할 때까지 두 번째 image envelope가 없다. stale response도 포함한다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // image field를 포함하여 open한다
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    // 첫 output을 push한다
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"test1\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // 첫 image envelope를 기다린다
    let _image_line1 =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .expect("timeout on first image envelope")
            .expect("failed to read first image line")
            .expect("first image line is empty");

    // release 없이 두 번째 output을 push한다
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"test2\r\n".to_vec(),
        sequence: 1,
        truncated: false,
    });

    // 대신 screen event를 기다린다(300ms 안에 image envelope를 받지 않아야 한다)
    let result =
        tokio::time::timeout(std::time::Duration::from_millis(300), lines.next_line()).await;

    // image envelope가 아니라 timeout 또는 screen event가 와야 한다
    if let Ok(Ok(Some(line))) = result {
        let json: serde_json::Value = serde_json::from_str(&line).expect("failed to parse JSON");
        // image envelope를 받았다면 잘못된 것이다
        assert!(
            json.get("body").and_then(|b| b.get("image")).is_none(),
            "should NOT have image envelope before release"
        );
    }

    // layout 교체는 sidecar가 보기 전에 진행 중인 response를 stale로 만들 수 있다.
    // 그래도 그 response는 직렬화된 전송을 해제하여 dirty screen을 보낼 수 있게 해야 한다.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"error":"stale","name":"view","generation":2,"raster":9,"sequence":7}}}
"#).await.unwrap();

    // 2초 안에 두 번째 image envelope(sequence 2)를 기다린다
    let mut found_sequence_2 = false;
    for _ in 0..20 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
        {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(img) = json.get("body").and_then(|b| b.get("image")) {
                    if img["sequence"] == 2 {
                        found_sequence_2 = true;
                        break;
                    }
                }
            }
        }
    }

    assert!(
        found_sequence_2,
        "should get image envelope with sequence 2 after release"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test I3: host image response(consumed/error)를 처리하고 error를 반환하지 않는다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // image field를 포함하여 open한다
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    let _state_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    // output을 push한다
    port.push_event(DaemonEvent::Output {
        session_id: fake_session_id.clone(),
        data: b"test\r\n".to_vec(),
        sequence: 0,
        truncated: false,
    });

    // image envelope를 기다린다
    let _image_line =
        tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

    // Test 1: consumed response를 보낸다("error" 없음)
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // error response가 아니라 screen 또는 image envelope만 받아야 한다
    let mut found_error_response = false;
    for _ in 0..10 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
        {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(error) = json.get("body").and_then(|b| b.get("error")) {
                    if error
                        .as_str()
                        .map(|e| e.contains("unknown operation"))
                        .unwrap_or(false)
                    {
                        found_error_response = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(
        !found_error_response,
        "should NOT return 'unknown operation' error for consumed response"
    );

    // Test 2: error response를 보낸다
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"error":"forbidden","name":"view","generation":1,"raster":1,"sequence":1}}}
"#).await.unwrap();

    // 알 수 없는 operation에 대한 error response를 받지 않아야 한다
    found_error_response = false;
    for _ in 0..10 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
        {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(error) = json.get("body").and_then(|b| b.get("error")) {
                    if error
                        .as_str()
                        .map(|e| e.contains("unknown operation"))
                        .unwrap_or(false)
                    {
                        found_error_response = true;
                        break;
                    }
                }
            }
        }
    }
    assert!(
        !found_error_response,
        "should NOT return 'unknown operation' error for error response"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: image field가 있는 open은 host response가 아니라 request이다
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
    assert_eq!(calls_lock.opens.len(), 1, "should have called open exactly once - open with image was misclassified as host response!");
    assert!(
        calls_lock.opens[0].0 > 0 && calls_lock.opens[0].1 > 0,
        "should have valid cols and rows"
    );

    let output = String::from_utf8(writer).unwrap();
    let lines: Vec<&str> = output.lines().collect();
    assert!(!lines.is_empty(), "should have output");

    // error가 아니라 state response를 받았는지 확인한다
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

/// width가 숫자가 아닌 host raster configuration은 거부된다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":"800","height":384,"scale":1.0}}}}
"#).await.unwrap();

    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    assert!(
        error_json["body"]["error"] == "invalidParams",
        "should return invalidParams error"
    );
    assert_eq!(reason, "invalid image configure");

    // 핵심은 daemon이 호출되지 않았어야 한다는 것이다
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

/// Test: bytes나 keys가 없는 input은 거부된다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 먼저 open한다
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

    // bytes도 keys도 없는 input을 보낸다
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input"}}
"#,
        )
        .await
        .unwrap();

    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    // 누락된 field에 대한 error를 받아야 한다
    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    let error_code = error_json["body"]["error"].as_str().unwrap_or("");
    assert!(
        error_code.contains("invalidParams") && reason.contains("bytes or keys"),
        "should report that bytes or keys is required, got error={} reason={}",
        error_code,
        reason
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: string이 아닌 bytes를 가진 input은 거부된다
#[tokio::test]
async fn test_input_with_non_string_bytes_rejected() {
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 먼저 open한다
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

    // bytes를 string 대신 number로 하여 input을 보낸다
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input","bytes":123}}
"#,
        )
        .await
        .unwrap();

    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    // bytes type에 대한 error를 받아야 한다
    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    let error_code = error_json["body"]["error"].as_str().unwrap_or("");
    assert!(
        error_code.contains("invalidParams") && reason.contains("bytes must be a string"),
        "should report that bytes must be a string, got error={} reason={}",
        error_code,
        reason
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: array가 아닌 keys를 가진 input은 거부된다
#[tokio::test]
async fn test_input_with_non_array_keys_rejected() {
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 먼저 open한다
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

    // keys를 array 대신 object로 하여 input을 보낸다
    to_serve
        .write_all(
            br#"{"surface":"s1","body":{"operation":"input","keys":{"key":"Up"}}}
"#,
        )
        .await
        .unwrap();

    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    // keys type에 대한 error를 받아야 한다
    let reason = error_json["body"]["reason"].as_str().unwrap_or("");
    let error_code = error_json["body"]["error"].as_str().unwrap_or("");
    assert!(
        error_code.contains("invalidParams") && reason.contains("keys must be an array"),
        "should report that keys must be an array, got error={} reason={}",
        error_code,
        reason
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// height가 없는 교체 raster는 거부된다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 먼저 open한다
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

    // height 없는 새 raster를 보낸다.
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":1024,"scale":1.0}}}}
"#).await.unwrap();

    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    assert_eq!(
        error_json["body"]["error"], "invalidParams",
        "should return invalidParams"
    );
    assert_eq!(error_json["body"]["reason"], "invalid image configure");

    // daemon resize가 호출되지 않았는지 검증한다
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

/// Test: cellWidth/cellHeight는 고정값이 아니라 metrics에서 계산된다
#[tokio::test]
async fn test_cell_dimensions_from_metrics() {
    use soksak_sidecar_vt_core::platform::metrics;

    // scale=2.0으로 테스트한다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

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

    // platform에서 실제 metrics를 얻는다
    let m = metrics(13.0, scale);

    // cellWidth와 cellHeight는 CSS pixel = device_pixels / scale 이어야 한다
    let expected_cell_width = m.cell_width as f64 / scale as f64;
    let expected_cell_height = m.cell_height as f64 / scale as f64;

    let cell_width = state_json["body"]["cellWidth"]
        .as_f64()
        .expect("cellWidth should be present and numeric");
    let cell_height = state_json["body"]["cellHeight"]
        .as_f64()
        .expect("cellHeight should be present and numeric");

    // metrics와 정확히 일치해야 한다(부동소수점 허용 오차 이내)
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

    // cols/rows 계산을 검증한다: cols = width_px / cell_width
    // (width_px와 cell_width는 모두 device pixel이며, scale은 이미 metrics에 반영되어 있다)
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

    // 다른 scale에서도 동작하는지 검증하도록 scale=1.0으로 테스트한다
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

    let (mut to_serve2, serve_in2) = tokio::io::duplex(64 * 1024);
    let (serve_out2, from_serve2) = tokio::io::duplex(64 * 1024);

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

/// 크기가 0인 host raster는 거부되고, 이후의 유효한 raster가 session을 연다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":0,"height":0,"scale":1.0}}}}
"#).await.unwrap();

    // error response를 받아야 한다
    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error JSON");

    assert_eq!(
        error_json["body"]["error"], "invalidParams",
        "expected invalidParams error"
    );
    assert_eq!(error_json["body"]["reason"], "invalid image configure");

    // daemon이 호출되지 않았는지 검증한다
    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.opens.len(),
            0,
            "daemon open should not be called for invalid params"
        );
    }

    // 원래 open request는 유효한 host raster가 도착할 때까지 대기 상태로 남는다.
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

/// 크기가 0인 교체 raster는 거부된다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 보통 크기로 open을 보낸다
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // state response를 읽는다
    let _state_line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let _initial_image = next_image_envelope(&mut lines).await;

    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":0,"height":0,"scale":1.0}}}}
"#).await.unwrap();

    // response를 읽는다 - error여야 한다
    let error_line = next_line_except_screen(&mut lines).await;

    let error_json: serde_json::Value =
        serde_json::from_str(&error_line).expect("failed to parse error response");

    assert_eq!(
        error_json["body"]["error"], "invalidParams",
        "zero raster should return invalidParams"
    );

    // daemon resize가 호출되지 않았는지 검증한다
    {
        let calls_lock = calls.lock().unwrap();
        assert_eq!(
            calls_lock.resizes.len(),
            0,
            "daemon resize should not be called for an invalid raster"
        );
    }

    // task가 아직 살아 있는지 검증하도록 screen.read를 보낸다
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

/// Test: cell 하나보다 작은 raster는 error 없이 session을 닫힌 상태로
/// 유지하고, 다음 유효한 raster가 session을 연다. collapse 구간을 지나는 drag는
/// frame마다 이런 raster를 보내며, 그때 error를 내면 잘못된 surface error를
/// 표시하고 host presentation barrier가 오지 않는 frame을
/// 계속 기다리게 된다(V5-96-14-6-4-8).
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 이 raster는 양수 크기이지만 terminal cell 하나를 담기에는 너무 작다.
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":3,"height":3,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":2,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // sub-cell raster는 error로 응답하지 않아야 하므로, 처음 돌아오는 line은
    // 유효한 raster의 open에 대한 state event이다.
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

/// Test: sub-cell 교체 raster는 현재 grid를 유지하면서도
/// frame을 present하므로 host presentation barrier가 완료된다. PTY는
/// 변하지 않은 grid로 resize되므로 SIGWINCH를 보내지 않는다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

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

    // sub-cell raster는 present한다: error 대신 state event(같은 grid)와 image
    // envelope가 도착한다. frame의 screen event는 image와 함께 오므로
    // state를 기다리는 동안 screen line은 건너뛴다.
    // sub-cell raster는 여전히 present한다: 전송이 acknowledge된 뒤
    // 대기 중인 configuration이 적용되어 변하지 않은 grid로 state event를 보내고,
    // 3x3 raster에 대한 frame을 그린다. 이전 raster의 중간
    // envelope는 host와 같은 방식으로 acknowledge한다.
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

/// Test: panic하는 engine은 surface event로 error를 보고한다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // engine에서 panic을 활성화한다
    panic_engine_flag.store(true, std::sync::atomic::Ordering::Relaxed);

    // open을 보낸다 - resize에서 panic을 일으켜야 한다
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();

    // error event를 기다린다
    let mut found_error = false;
    for _ in 0..10 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line()).await
        {
            let json: serde_json::Value =
                serde_json::from_str(&line).expect("failed to parse JSON");

            if let Some(event) = json.get("body").and_then(|b| b.get("event")) {
                if event == "error" {
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

/// 이미지 봉투가 나올 때까지 출력 줄을 읽는다. screen 이벤트 줄은 건너뛴다.
async fn next_image_envelope(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
) -> serde_json::Value {
    for _ in 0..20 {
        let line = tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line())
            .await
            .expect("timeout waiting for an image envelope")
            .expect("failed to read an output line")
            .expect("output ended");
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
            if json["body"]["image"].is_object() {
                return json;
            }
        }
    }
    panic!("no image envelope arrived");
}

// 호스트처럼 이미지 봉투에 consumed 로 답한다. 답하기 전까지 사이드카는 다음 raster 구성을 적용하지 않는다.
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

// 지정한 raster 의 이미지 봉투를 기다린다. 커서 틱처럼 이전 raster 로 그린 이미지는 답하고 넘긴다.
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

/// 교체 raster는 이전 전송이 acknowledge될 때까지 사용되지 않는다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();

    // 800x384 image로 open한다
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

    // host는 raster를 교체하기 전에 불변 snapshot을 acknowledge한다.
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

/// Test: release cycle 뒤, image가 미해결인 동안의 output은 다시 present하지 않는다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

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

    // 첫 image
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

    // sequence 1을 release한다
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":1}}}}
"#).await.unwrap();

    // release 뒤의 두 번째 image (frame이 다시 비어 있었다)
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

    // sequence 2가 미해결인 동안의 output은 그 release 전에 다시 present하면 안 된다
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

    // sequence 2를 release하면 그사이 도착한 내용을 present한다
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"consumed":{"name":"view","generation":1,"raster":1,"sequence":2}}}}
"#).await.unwrap();

    let mut caught_up = false;
    for _ in 0..20 {
        if let Ok(Ok(Some(line))) =
            tokio::time::timeout(std::time::Duration::from_millis(100), lines.next_line()).await
        {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if json["body"]["image"]["sequence"].as_u64() == Some(3) {
                    caught_up = true;
                    break;
                }
            }
        }
    }
    assert!(
        caught_up,
        "release of sequence 2 should present the caught-up image"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// Test: error response는 다음 output에서 그리기를 다시 활성화한다
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

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

    // host가 image를 거부한다
    to_serve.write_all(br#"{"surface":"s1","body":{"image":{"error":"scale","name":"view","generation":1,"raster":1,"sequence":1}}}
"#).await.unwrap();

    // 다음 output은 다시 그려야 한다
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
        {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if json["body"]["image"]["sequence"].as_u64() == Some(2) {
                    drew_again = true;
                    break;
                }
            }
        }
    }
    assert!(
        drew_again,
        "output after an image error must present a new image envelope"
    );

    drop(to_serve);
    task.await.unwrap().unwrap();
}

/// 교체 raster 뒤의 state event는 전체 session field를 담는다.
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

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);

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

    // resize 의 state 이벤트까지 읽는다. 커서 틱이 만든 이미지가 먼저 올 수 있으며, 호스트처럼 각 이미지에
    // consumed 로 답해야 대기 중인 raster 2 구성이 적용된다.
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

/// state 이벤트나 오류 응답이 올 때까지 줄을 읽는다.
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

/// 글꼴 크기는 칸 크기를 정한다(docs/spec/text-size.md). 크기가 두 배면 칸 높이도 두 배이고, 잘못된 크기는 오류다.
#[tokio::test]
async fn test_font_size_sets_the_cell_size() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(
        "test-session-font-size".to_string(),
        calls.clone(),
    ));
    let factory = Arc::new(move || port.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
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

/// 화면 이벤트 한 줄의 셀 글자를 이어 붙인다. JSON 에서는 글자마다 셀 객체이므로 문자열 검색으로 찾을 수 없다.
fn screen_text(line: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(line).expect("screen event JSON");
    value["body"]["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row.as_array().into_iter().flatten())
        .filter_map(|cell| cell["ch"].as_str())
        .collect()
}

#[tokio::test]
async fn output_while_the_host_holds_a_raster_sends_one_screen_with_the_next_presentation() {
    // 호스트가 앞 래스터를 복사하는 동안 온 출력은 화면 JSON 을 보내지 않는다. 보내면 출력이 몰릴 때 페이지가
    // 지난 화면을 뒤늦게 처리하느라 입력과 스크롤이 밀린다. 답을 받은 뒤 한 번 그리고 그 화면 하나를 보낸다.
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
    // 열기의 첫 표시까지 읽는다. 그 래스터는 아직 호스트에 있다.
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
    // 첫 표시의 화면은 그 래스터와 함께 온다.
    let opened = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout waiting for the first screen")
        .unwrap()
        .unwrap();
    assert!(
        opened.contains(r#""event":"screen""#),
        "the first presentation came without its screen: {opened}"
    );
    for index in 0..50 {
        port.push_event(DaemonEvent::Output {
            session_id: session_id.clone(),
            data: format!("line{index}\r\n").into_bytes(),
            sequence: index,
            truncated: false,
        });
    }
    // 출력 채널과 요청 채널은 순서가 없으므로, 읽기 답이 마지막 출력을 보일 때까지 기다린 뒤 consumed 를 보낸다.
    // 그 사이의 화면 이벤트는 모두 읽기 요청의 답이어야 한다.
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
                if screen_text(&line).contains("line49") {
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
    drop(to_serve);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
    assert_eq!(
        presentations, 1,
        "the held raster must be followed by exactly one presentation"
    );
    assert_eq!(
        screens.len(),
        1,
        "the answer sent {} screen events",
        screens.len()
    );
    assert!(
        screen_text(&screens[0]).contains("line49"),
        "the screen after the presentation is not the latest output: {}",
        screens[0]
    );
}

#[tokio::test]
async fn the_page_screen_event_carries_only_text_width_links_and_set_attributes() {
    // 페이지는 글자, 폭, 링크, 선택(반전)만 쓰고 색은 래스터가 그린다. 기본값 필드를 셀마다 보내면 글자로 찬
    // 122×18 화면 하나가 약 240 KB 가 되어 스크롤 프레임마다 호스트와 페이지를 지난다.
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
        if screen_text(&line).contains("text") {
            screen = Some(line);
            break;
        }
    }
    drop(to_serve);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
    let line = screen.expect("200 screen reads did not show the output");
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    for cell in value["body"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row.as_array().unwrap())
    {
        for (key, field) in cell.as_object().unwrap() {
            assert!(
                matches!(key.as_str(), "ch" | "width" | "link")
                    || (matches!(key.as_str(), "bold" | "italic" | "underline" | "inverse")
                        && field == &serde_json::json!(true)),
                "the page cell carries {key}: {field} in {line}"
            );
        }
    }
}

#[tokio::test]
async fn a_focus_change_sends_the_screen_with_the_presentation_that_draws_it() {
    // 초점은 커서의 그린 모양을 바꾼다. 그 래스터와 함께 화면을 보내야 페이지의 커서 상태가 화면과 같다.
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
    // 호스트처럼 표시마다 consumed 로 답하고, 화면 읽기의 답 앞에 표시가 없을 때까지 읽어 사이드카를 한가하게 한다.
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
                // 표시와 함께 온 화면을 읽는다.
                let screen = next!();
                assert!(
                    screen.contains(r#""event":"screen""#),
                    "a presentation came without its screen: {screen}"
                );
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
    to_serve.write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"input\",\"focus\":{\"focused\":true}}}\n").await.unwrap();
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
        if value["body"]["event"] == "screen" {
            assert!(
                presented && value["body"]["cursor"]["focused"] == true,
                "the focused screen did not follow its presentation: {seen:#?}"
            );
            break;
        }
        assert!(
            value["body"]["ack"] != true,
            "the focus change was answered without its screen: {seen:#?}"
        );
    }
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

/// 페이지는 누름, 움직임, 선택 연산, 뗌 순서로 보낸다.
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
        format!("{{\"surface\":\"s1\",\"body\":{{\"operation\":\"input\",\"focus\":{{\"focused\":{focused}}}}}}}\n")
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
    let keys = "{\"surface\":\"s1\",\"body\":{\"operation\":\"input\",\"keys\":[{\"key\":\"Keypad5\",\"text\":\"\",\"shift\":false,\"alt\":false,\"ctrl\":false},{\"key\":\"KeypadEnter\",\"text\":\"\",\"shift\":false,\"alt\":false,\"ctrl\":false}]}}\n";
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
    // 페이지의 그림 상태는 그 그림을 그린 래스터와 함께 바뀐다. 먼저 오면 페이지가 그림 없는 화면을 그림으로 여긴다.
    let calls = Arc::new(Mutex::new(Calls::default()));
    let session = "inline-order".to_string();
    let engine_factory = Arc::new(|| Box::new(MockEngine::new()) as Box<dyn Engine>);
    let port = Arc::new(FakeSessionPort::new(session.clone(), calls));
    let port_for_factory = port.clone();
    let factory = Arc::new(move || port_for_factory.clone() as Arc<dyn SessionPort>);
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(serve(engine_factory, serve_in, serve_out, factory));
    let mut lines = tokio::io::BufReader::new(from_serve).lines();
    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    // 열기의 표시를 받되 아직 consumed 로 답하지 않는다. 그 동안 그림 출력이 온다.
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
        format!("{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"sequence\":{sequence}}}}}}}}}\n")
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
                    // 그림 출력이 처리될 시간을 준 뒤 첫 표시를 소비한다.
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

/// selection point는 가장 가까운 cell 경계로 가므로, pointer가 cell의 중간점을 지나면 그 cell이 선택된다.
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

/// host 는 native region 의 가장자리를 device pixel 에 맞추므로 region 은 view 보다 한 device pixel 미만 작을 수
/// 있다. 그 띠의 point 는 오류가 아니고, 더 바깥의 point 는 그 mouse 연산의 inputId 를 담은 결과로 오류를 알린다.
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
