// An image configured after a detach draws the later program output of its surface (docs/spec/vt-core.md#terminal-sidecar-vt-core),
// judged on the real engine, a real PTY and a real shell (S261-1). The test is the host of the image region: it answers each drawn
// frame with `consumed` at once and each frame request with a tick at once. Every wait ends on an output line of the sidecar and
// counts no reads or time; the limit of .config/nextest.toml bounds a hung test, and each changed screen goes to stderr so that the
// output of a hung test shows the screen it waited on.
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

/// The line that the shell prints after a delay, once the second image is configured.
const MARKER: &str = "after-reload-marker";

/// The sidecar of the test and the frames that the test, as its host, has answered.
struct Hosted {
    to_serve: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    _task: tokio::task::JoinHandle<std::io::Result<()>>,
    _logs: tempfile::TempDir,
    ticks: u64,
    /// The image envelopes of the drawn frames since the last `count_from`.
    frames: Vec<Value>,
}

impl Hosted {
    fn start() -> Hosted {
        let logs = tempfile::tempdir().expect("create the folder of the events");
        let events: Events = Arc::new(
            EventLog::start(Options::service(
                logs.path().to_path_buf(),
                format!("image-reattach-{}", std::process::id()),
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
            owner: "image-reattach-owner".to_string(),
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
            frames: Vec::new(),
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

    async fn type_line(&mut self, text: &str) {
        self.send(json!({"operation": "input", "actions": [{"type": "insert", "text": format!("{text}\r"),
            "replacementRange": null, "attributed": false}],
            "preedit": {"text": "", "markedRange": null, "selectedRange": null, "replacementRange": null, "attributed": false}}))
            .await;
    }

    fn count_from(&mut self) {
        self.frames.clear();
    }

    /// Reads lines of the surface until one satisfies `matches`: it answers each frame request with a tick and each drawn frame
    /// with `consumed`, and keeps each drawn frame, which `matches` also sees.
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
                self.ticks += 1;
                let tick = json!({"surface": "s1", "body": {"image": {"tick": {
                    "name": want["name"], "generation": want["generation"], "sequence": self.ticks, "refreshRate": 120}}}});
                self.write(tick).await;
                continue;
            }
            if image["sequence"].is_u64() && image["token"].is_object() {
                let consumed = json!({"surface": "s1", "body": {"image": {"consumed": {
                    "name": image["name"], "generation": image["generation"], "raster": image["raster"], "sequence": image["sequence"]}}}});
                self.frames.push(image.clone());
                self.write(consumed).await;
                if matches(&value) {
                    return value;
                }
                continue;
            }
            if value["body"]["error"].is_string() {
                panic!("the sidecar answered an error: {line}");
            }
            if value["surface"] == "s1" && matches(&value) {
                return value;
            }
        }
    }

    /// Reads the screen with `screen.read` and returns the plain text of each row.
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

    /// Waits for the next drawn frame.
    async fn next_frame(&mut self) {
        self.until(|value| value["body"]["image"]["sequence"].is_u64())
            .await;
    }

    /// Reads the screen until a row satisfies `matches`. A read that does not match waits for the next drawn frame, which
    /// follows any later output, unless a frame arrived during the read, whose output the read may not show.
    async fn screen_until(&mut self, matches: impl Fn(&str) -> bool) {
        let mut shown: Vec<String> = Vec::new();
        loop {
            let drawn = self.frames.len();
            let rows = self.screen().await;
            if rows.iter().any(|row| matches(row)) {
                return;
            }
            if rows != shown {
                eprintln!("screen: {rows:?}");
                shown = rows;
            }
            if self.frames.len() == drawn {
                self.next_frame().await;
            }
        }
    }
}

/// A page reload detaches the image of the surface and configures a new image of the same name with the next generation; the
/// output that the shell prints after the configure is drawn in a frame of that generation.
#[tokio::test]
async fn an_image_configured_after_a_detach_draws_later_shell_output() {
    let mut hosted = Hosted::start();
    hosted
        .send(
            json!({"operation": "open", "shell": "/bin/sh", "directory": "/tmp", "image": "view"}),
        )
        .await;
    hosted
        .send(json!({"image": {"configure": {"name": "view", "generation": 1, "raster": 1, "width": 800, "height": 384, "scale": 1.0}}}))
        .await;
    while !hosted.frames.iter().any(|frame| frame["generation"] == 1) {
        hosted.next_frame().await;
    }

    hosted
        .send(json!({"image": {"detach": {"request": "reload", "name": "view", "generation": 1}}}))
        .await;
    let detached = hosted
        .until(|value| value["body"]["image"]["detached"]["request"] == "reload")
        .await;
    assert_eq!(
        detached["body"]["image"]["detached"]["result"], "detached",
        "{detached}"
    );

    hosted.count_from();
    hosted
        .send(json!({"image": {"configure": {"name": "view", "generation": 2, "raster": 1, "width": 800, "height": 384, "scale": 1.0}}}))
        .await;
    while !hosted.frames.iter().any(|frame| frame["generation"] == 2) {
        hosted.next_frame().await;
    }

    hosted.type_line(&format!("sleep 0.5; echo {MARKER}")).await;
    hosted
        .screen_until(|row| row.ends_with(&format!("echo {MARKER}")))
        .await;
    hosted.screen().await;
    hosted.count_from();
    hosted.screen_until(|row| row == MARKER).await;
    eprintln!("waiting for a frame of generation 2 after the shell output");
    while !hosted.frames.iter().any(|frame| frame["generation"] == 2) {
        hosted.next_frame().await;
    }
}
