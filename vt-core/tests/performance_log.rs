// 성능 트레이스 기록기의 계약(docs/spec/performance-trace.md, V5-104).
// 플래그 파일은 대상 로그 경로를 한 줄로 담고, 기록기는 그 파일에 NDJSON 줄을
// 덧붙인다. 플래그가 없으면 어떤 파일 작업도 하지 않는다.
use soksak_sidecar_vt_core::performance::PerformanceTrace;
use std::fs;
use std::path::PathBuf;

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("vt-core-performance-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn trace_appends_ndjson_to_the_flagged_target() {
    let service = temp_dir("flagged");
    let log = service.join("performance.ndjson");
    fs::write(service.join("performance"), format!("{}\n", log.display())).unwrap();

    let trace = PerformanceTrace::from_service_dir(&service);
    assert!(
        trace.enabled(),
        "a flag file pointing at a target enables the trace"
    );
    trace.line("session_start", serde_json::json!({"role": "vt-core"}));
    trace.line(
        "frame",
        serde_json::json!({"reason": "output", "draw_us": 830}),
    );

    let text = fs::read_to_string(&log).expect("the flagged target receives the lines");
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("every line is one JSON object"))
        .collect();
    assert_eq!(lines.len(), 2);
    for (expected, i) in [("session_start", 0), ("frame", 1)] {
        assert_eq!(lines[i]["event"], expected, "line {i} carries its event");
        assert_eq!(lines[i]["layer"], "vt-core");
        assert_eq!(lines[i]["pid"], serde_json::json!(std::process::id()));
        assert!(
            lines[i]["ts"].as_str().unwrap().len() >= 20,
            "ts is ISO-8601 with milliseconds"
        );
    }
    assert_eq!(lines[1]["reason"], "output");
    assert_eq!(lines[1]["draw_us"], 830);
    let _ = fs::remove_dir_all(&service);
}

#[test]
fn trace_without_a_flag_writes_no_file() {
    let service = temp_dir("unflagged");
    let trace = PerformanceTrace::from_service_dir(&service);
    assert!(!trace.enabled(), "no flag file leaves the trace off");
    trace.line("frame", serde_json::json!({}));
    let entries: Vec<_> = fs::read_dir(&service).unwrap().collect();
    assert!(
        entries.is_empty(),
        "an off trace creates and writes no file"
    );
    let _ = fs::remove_dir_all(&service);
}

#[test]
fn trace_rechecks_the_flag_on_each_service_dir_read() {
    let service = temp_dir("recheck");
    let log = service.join("performance.ndjson");
    let off = PerformanceTrace::from_service_dir(&service);
    assert!(!off.enabled());
    fs::write(service.join("performance"), format!("{}\n", log.display())).unwrap();
    let on = PerformanceTrace::from_service_dir(&service);
    assert!(
        on.enabled(),
        "the flag is read again on the next construction"
    );
    let _ = fs::remove_dir_all(&service);
}

// 프레임 생산자: 트레이스가 켜진 serve 는 프레임마다 reason 과 그리기 시간을 남긴다.
use soksak_sidecar_vt_core::protocol::{
    serve_with_performance, Cursor, CursorShape, Engine, EngineEvent, Modes, Screen, SessionPort,
    ShellRequest,
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

#[tokio::test]
async fn a_served_surface_records_its_frames_with_reasons() {
    let service = temp_dir("frames");
    let log = service.join("performance.ndjson");
    std::fs::write(service.join("performance"), format!("{}\n", log.display())).unwrap();
    let trace = PerformanceTrace::from_service_dir(&service);
    assert!(trace.enabled());

    let engine_factory = Arc::new(|| Box::new(TraceEngine::default()) as Box<dyn Engine>);
    let port = Arc::new(TracePort);
    let factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> =
        Arc::new(move || port.clone() as Arc<dyn SessionPort>);

    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        let _ = serve_with_performance(engine_factory, serve_in, serve_out, factory, trace).await;
    });
    let mut lines = BufReader::new(from_serve).lines();

    to_serve.write_all(br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#).await.unwrap();
    // 상태 이벤트와 이미지 봉투가 올 때까지 줄을 읽는다(프레임은 그 사이에 그려진다).
    for _ in 0..3 {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line()).await;
    }
    drop(to_serve);
    let _ = task.await;

    let text = std::fs::read_to_string(&log).expect("a served frame reaches the flagged log");
    let frames: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &serde_json::Value| v["event"] == "frame")
        .collect();
    assert!(!frames.is_empty(), "at least one frame is recorded: {text}");
    assert!(
        frames
            .iter()
            .all(|f| f["reason"].is_string() && f["draw_us"].is_u64()),
        "every frame carries a reason and a draw time: {frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|f| f["reason"] == "open" || f["reason"] == "resize"),
        "the open and configure paths name themselves: {frames:?}"
    );
    let _ = std::fs::remove_dir_all(&service);
}

#[test]
fn trace_stops_writing_once_the_flag_is_removed() {
    // 연결을 받을 때 켜진 기록기도 host 가 플래그를 지우면 더 쓰지 않는다(docs/spec/performance-trace.md).
    let service = temp_dir("removed");
    let log = service.join("performance.ndjson");
    fs::write(service.join("performance"), format!("{}\n", log.display())).unwrap();
    let trace = PerformanceTrace::from_service_dir(&service);
    trace.line("frame", serde_json::json!({"reason": "output"}));
    let before = fs::read_to_string(&log).expect("the flagged target receives the first line");
    fs::remove_file(service.join("performance")).unwrap();
    trace.line("frame", serde_json::json!({"reason": "cursor"}));
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        before,
        "a removed flag did not stop the trace"
    );
    let _ = fs::remove_dir_all(&service);
}
