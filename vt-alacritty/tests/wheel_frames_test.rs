// The frames of wheel input on the viewport route (docs/spec/vt-core.md#frame-requests) through the real engine, a real PTY and
// a real shell (S226). The test is the host of the image region: it answers each drawn frame with `consumed` at once, as a host
// copies a raster, and it holds each frame request until a step sends its tick, so the test decides where the display frames
// of the window fall. Every wait ends on an output line of the sidecar; the limit of .config/nextest.toml bounds a hung test.
#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::AlacrittyEngine;
use serde_json::{Value, json};
use soksak_sidecar_vt_core::Engine;
use soksak_sidecar_vt_core::events::{EventLog, Events, Options};
use soksak_sidecar_vt_core::protocol::{
    PersistentOwner, PersistentRegistry, SessionPort, serve_with_registry,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

/// The last line that the shell writes after its history, which tells the test that the history is on the screen.
const READY: &str = "wheel-frames-ready";

/// The sidecar of one test and what the test, as its host, has seen of it.
struct Hosted {
    to_serve: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    _task: tokio::task::JoinHandle<std::io::Result<()>>,
    _logs: tempfile::TempDir,
    /// The ticks sent so far, which number the next one.
    ticks: u64,
    /// The frame requests that wait for their tick.
    wants: Vec<Value>,
    /// The drawn frames since `count_from`: the image envelope of each.
    frames: Vec<Value>,
    /// The `view` events since `count_from`.
    views: Vec<Value>,
    /// The frame requests since `count_from`.
    requests: usize,
}

impl Hosted {
    fn start() -> Hosted {
        let logs = tempfile::tempdir().expect("create the folder of the events");
        let events: Events = Arc::new(
            EventLog::start(Options::service(
                logs.path().to_path_buf(),
                format!("wheel-frames-{}", std::process::id()),
            ))
            .expect("start the writer of the events"),
        );
        let (to_serve, serve_in) = tokio::io::duplex(1 << 20);
        let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
        let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
        let port_events = events.clone();
        let factory = Arc::new(move || {
            Arc::new(soksak_sidecar_vt_core::LocalSessionPort::new_with_owner(
                Arc::new(soksak_sidecar_vt_core::pty::PtyService::new(
                    port_events.clone(),
                )),
                String::new(),
            )) as Arc<dyn SessionPort>
        });
        let owner = PersistentOwner {
            close: Arc::new(|| Ok(())),
            registry: PersistentRegistry::new(),
            owner: "wheel-frames-owner".to_string(),
            log: events,
        };
        let task = tokio::spawn(serve_with_registry(
            engine_factory,
            serve_in,
            serve_out,
            factory,
            owner,
        ));
        Hosted {
            to_serve,
            lines: BufReader::new(from_serve).lines(),
            _task: task,
            _logs: logs,
            ticks: 0,
            wants: Vec::new(),
            frames: Vec::new(),
            views: Vec::new(),
            requests: 0,
        }
    }

    async fn write(&mut self, line: Value) {
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the request line was not written");
    }

    async fn send(&mut self, body: Value) {
        self.write(json!({"surface": "s1", "body": body})).await;
    }

    /// Sends the tick of each frame request that waits.
    async fn tick(&mut self) {
        for want in std::mem::take(&mut self.wants) {
            self.ticks += 1;
            let tick = json!({"surface": "s1", "body": {"image": {"tick": {
                "name": want["name"], "generation": want["generation"], "sequence": self.ticks, "refreshRate": 120}}}});
            self.write(tick).await;
        }
    }

    /// Starts the counts of frames, views and frame requests.
    fn count_from(&mut self) {
        self.frames.clear();
        self.views.clear();
        self.requests = 0;
    }

    /// Reads lines of the surface until one satisfies `matches`: it answers each drawn frame with `consumed`, keeps each frame
    /// request until `tick`, and counts both.
    async fn until(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .expect("failed to read a sidecar line")
                .expect("the sidecar closed its output");
            let value: Value = serde_json::from_str(&line).expect("the sidecar line is not JSON");
            let image = &value["body"]["image"];
            if let Some(want) = image.get("want") {
                self.requests += 1;
                self.wants.push(want.clone());
                continue;
            }
            if image["sequence"].is_u64() && image["token"].is_object() {
                let consumed = json!({"surface": "s1", "body": {"image": {"consumed": {
                    "name": image["name"], "generation": image["generation"], "raster": image["raster"], "sequence": image["sequence"]}}}});
                self.frames.push(image.clone());
                self.write(consumed).await;
                continue;
            }
            if value["body"]["error"].is_string() {
                panic!("the sidecar answered an error: {line}");
            }
            if value["body"]["event"] == "view" {
                self.views.push(value["body"].clone());
            }
            if value["surface"] == "s1" && matches(&value) {
                return value;
            }
        }
    }

    /// Reads the screen with `screen.read`, which the surface answers after every request sent before it, and returns the plain
    /// text of each row.
    async fn screen(&mut self) -> Vec<String> {
        self.send(json!({"operation": "screen.read"})).await;
        let screen = self
            .until(|value| value["body"]["event"] == "screen" && value["body"]["base"] == true)
            .await;
        let cols = screen["body"]["cols"]
            .as_u64()
            .expect("the screen has cols") as usize;
        let cells = screen["body"]["cells"]
            .as_array()
            .expect("the full form has cells");
        cells
            .chunks(cols.max(1))
            .map(|row| {
                row.iter()
                    .filter(|cell| cell["width"].as_u64() != Some(0))
                    .map(|cell| {
                        cell["grapheme"]
                            .as_str()
                            .filter(|text| !text.is_empty())
                            .unwrap_or(" ")
                            .to_string()
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Opens a shell with the image region `view`, writes 200 lines of history and waits until the screen shows the last line
    /// and the surface has drawn every change: no frame request waits and a read after a tick finds no new frame.
    async fn open_with_history(&mut self) {
        self.send(
            json!({"operation": "open", "shell": "/bin/sh", "directory": "/tmp", "image": "view"}),
        )
        .await;
        self.send(json!({"image": {"configure": {"name": "view", "generation": 1, "raster": 1, "width": 800, "height": 384, "scale": 1.0}}}))
            .await;
        self.send(json!({"operation": "input", "actions": [{"type": "insert", "text": format!("seq 1 200; echo {READY}\r"),
            "replacementRange": null, "attributed": false}],
            "preedit": {"text": "", "markedRange": null, "selectedRange": null, "replacementRange": null, "attributed": false}}))
            .await;
        loop {
            self.tick().await;
            let rows = self.screen().await;
            if rows.iter().any(|row| row == READY) && self.wants.is_empty() {
                break;
            }
            if self.wants.is_empty() {
                // The output has not arrived yet: the next line of the surface is its frame request or its frame.
                self.until(|value| {
                    value["body"]["image"].get("want").is_some() || value["body"]["event"] == "view"
                })
                .await;
            }
        }
        // A frame drawn at once within the input window may still be answered; one more read after it settles the counts.
        self.screen().await;
        self.count_from();
    }

    /// Sends one wheel of `lines` on the viewport route and waits until the surface has handled it.
    async fn wheel(&mut self, lines: i64) {
        self.send(json!({"operation": "scroll", "lines": lines, "col": 0, "row": 0}))
            .await;
        self.screen().await;
    }
}

/// Twelve wheels toward older output between two display frames draw no frame before the tick and one frame at it, and that
/// frame shows the viewport twelve lines up: the surface draws a scroll at the display frame of the window and not at each
/// wheel event (docs/spec/vt-core.md#frame-requests).
#[tokio::test]
async fn the_wheels_between_two_display_frames_are_drawn_at_one_tick() {
    let mut hosted = Hosted::start();
    hosted.open_with_history().await;
    for _ in 0..12 {
        hosted.wheel(1).await;
    }
    assert_eq!(
        (hosted.frames.len(), hosted.requests),
        (0, 1),
        "12 wheels before a tick drew {} frames and sent {} frame requests; expected no frame and one request",
        hosted.frames.len(),
        hosted.requests
    );
    hosted.tick().await;
    hosted.until(|value| value["body"]["event"] == "view").await;
    hosted.screen().await;
    assert_eq!(
        hosted.frames.len(),
        1,
        "the tick drew {} frames: {:?}",
        hosted.frames.len(),
        hosted.frames
    );
    let offsets: Vec<&Value> = hosted
        .views
        .iter()
        .map(|view| &view["scrollback"]["offset"])
        .collect();
    assert_eq!(
        offsets,
        vec![&json!(12)],
        "the views of the tick: {:?}",
        hosted.views
    );
}

/// Wheels toward newer output at the newest output and a `viewport` request to the current offset move nothing, so they draw
/// no frame and send no frame request.
#[tokio::test]
async fn a_wheel_that_moves_no_line_draws_nothing() {
    let mut hosted = Hosted::start();
    hosted.open_with_history().await;
    for _ in 0..6 {
        hosted.wheel(-1).await;
    }
    hosted
        .send(json!({"operation": "viewport", "offset": 0}))
        .await;
    hosted.screen().await;
    assert_eq!(
        (hosted.frames.len(), hosted.requests),
        (0, 0),
        "6 wheels and a viewport request at the newest output drew {} frames and sent {} frame requests; expected none",
        hosted.frames.len(),
        hosted.requests
    );
}

/// A `viewport` request that moves the viewport is drawn at the tick of its frame request, like a wheel.
#[tokio::test]
async fn a_viewport_request_that_moves_is_drawn_at_its_tick() {
    let mut hosted = Hosted::start();
    hosted.open_with_history().await;
    hosted
        .send(json!({"operation": "viewport", "offset": 30}))
        .await;
    hosted.screen().await;
    assert_eq!(
        (hosted.frames.len(), hosted.requests),
        (0, 1),
        "a viewport request drew {} frames and sent {} frame requests before the tick; expected no frame and one request",
        hosted.frames.len(),
        hosted.requests
    );
    hosted.tick().await;
    hosted.until(|value| value["body"]["event"] == "view").await;
    assert_eq!(
        hosted.frames.len(),
        1,
        "the tick drew {} frames",
        hosted.frames.len()
    );
    assert_eq!(
        hosted.views.last().unwrap()["scrollback"]["offset"],
        json!(30),
        "{:?}",
        hosted.views
    );
}
