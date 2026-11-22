// Checks the program scroll read of core docs/spec/terminal-read.md#program-scroll through the real engine, a real PTY and a
// real program (S208-3, core F899-8.7). The test is the host of the image region: it answers each frame request of the surface
// with the tick of the next display frame, and it holds the ticks when a check needs the read to wait. Every wait ends on an
// output line of the sidecar.
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

/// The sidecar of one test: the writer of its requests, the lines of its output and the folder of its events.
struct Sidecar {
    to_serve: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    _task: tokio::task::JoinHandle<std::io::Result<()>>,
    _logs: tempfile::TempDir,
    /// The ticks sent so far, which number the next one.
    ticks: u64,
    /// Whether a frame request is answered at once with a tick.
    answer_frames: bool,
    /// The frame requests that wait for their tick while `answer_frames` is false.
    held: Vec<Value>,
    /// Whether a `screen.changed` event came since `screen_until` last read the screen.
    changed: bool,
    /// The quiet interval of a read whose ticks follow the draws of the program, or `None` while the ticks do not.
    paced: Option<u32>,
    /// Whether the program drew since the last step of a paced read ended.
    drawn: bool,
    /// The ticks sent since the last draw of the program in a paced read.
    since_draw: u32,
}

impl Sidecar {
    fn start() -> Sidecar {
        let logs = tempfile::tempdir().expect("create the folder of the events");
        let events: Events = Arc::new(
            EventLog::start(Options::service(
                logs.path().to_path_buf(),
                format!("scroll-read-{}", std::process::id()),
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
            owner: "scroll-read-owner".to_string(),
            log: events,
        };
        let task = tokio::spawn(serve_with_registry(
            engine_factory,
            serve_in,
            serve_out,
            factory,
            owner,
        ));
        Sidecar {
            to_serve,
            lines: BufReader::new(from_serve).lines(),
            _task: task,
            _logs: logs,
            ticks: 0,
            answer_frames: true,
            held: Vec::new(),
            changed: false,
            paced: None,
            drawn: false,
            since_draw: 0,
        }
    }

    async fn send(&mut self, body: Value) {
        let line = json!({"surface": "s1", "body": body}).to_string();
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the request line was not written");
    }

    async fn tick(&mut self, want: &Value) {
        self.ticks += 1;
        let tick = json!({"surface": "s1", "body": {"image": {"tick": {
            "name": want["name"], "generation": want["generation"], "sequence": self.ticks, "refreshRate": 120}}}});
        self.to_serve
            .write_all(format!("{tick}\n").as_bytes())
            .await
            .expect("the tick was not written");
    }

    /// Sends the ticks of the held frame requests and answers the next ones at once.
    async fn release_frames(&mut self) {
        self.answer_frames = true;
        self.send_held_ticks().await;
    }

    /// Sends the ticks of the held frame requests and keeps holding the next ones.
    async fn send_held_ticks(&mut self) {
        for want in std::mem::take(&mut self.held) {
            self.tick(&want).await;
        }
    }

    /// Sends a tick of a paced read: after `quiet` ticks without a draw the step of the read ends, and the next step receives
    /// ticks only after the program drew in it.
    async fn paced_tick(&mut self, want: &Value, quiet: u32) {
        self.tick(want).await;
        self.since_draw += 1;
        if self.since_draw >= quiet {
            self.drawn = false;
            self.since_draw = 0;
        }
    }

    /// A scroll read by `by` whose ticks come only after the program drew in each step. A tick of this test comes at once and not
    /// at a display frame, so `quiet` ticks right after the input could end a step before a program under load draws; the host
    /// of a window spaces the ticks by its display frames (core docs/spec/terminal-read.md#draw-of-the-program-scroll). The
    /// program must draw without synchronized output, so that each step ends after `quiet` ticks.
    async fn scroll_read_after_draws(&mut self, by: i64) -> Value {
        self.paced = Some(6);
        self.drawn = false;
        self.since_draw = 0;
        self.send_read(by).await;
        let reply = self.read_reply().await;
        self.paced = None;
        assert!(
            self.held.is_empty(),
            "a paced read left held ticks: {reply}"
        );
        reply
    }

    /// Reads lines of the surface until one satisfies `matches`, answering or holding each frame request on the way.
    async fn until(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .expect("failed to read a sidecar line")
                .expect("the sidecar closed its output");
            let value: Value = serde_json::from_str(&line).expect("the sidecar line is not JSON");
            if let Some(want) = value["body"]["image"].get("want").cloned() {
                if let Some(quiet) = self.paced {
                    if self.drawn {
                        self.paced_tick(&want, quiet).await;
                    } else {
                        self.held.push(want);
                    }
                } else if self.answer_frames {
                    self.tick(&want).await;
                } else {
                    self.held.push(want);
                }
                continue;
            }
            if value["body"]["event"] == "screen.changed" {
                self.changed = true;
                if let Some(quiet) = self.paced {
                    self.drawn = true;
                    self.since_draw = 0;
                    for want in std::mem::take(&mut self.held) {
                        self.paced_tick(&want, quiet).await;
                    }
                }
            }
            if value["surface"] == "s1" && matches(&value) {
                return value;
            }
        }
    }

    /// Opens a shell with the image region `view` and watches its screen.
    async fn open(&mut self) {
        self.send(
            json!({"operation": "open", "shell": "/bin/sh", "directory": "/tmp", "image": "view"}),
        )
        .await;
        self.send(json!({"image": {"configure": {"name": "view", "generation": 1, "raster": 1, "width": 800, "height": 384, "scale": 1.0}}}))
            .await;
        self.send(json!({"operation": "screen.watch", "enabled": true}))
            .await;
    }

    async fn type_text(&mut self, text: &str) {
        self.send(json!({"operation": "input", "actions": [{"type": "insert", "text": text, "replacementRange": null, "attributed": false}],
            "preedit": {"text": "", "markedRange": null, "selectedRange": null, "replacementRange": null, "attributed": false}}))
            .await;
    }

    /// Reads the visible screen as the plain text of each row: the reply of `screen.read` is the full form, whose cells are in
    /// row-major order.
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

    /// Waits until the visible screen satisfies `ready`, reading it after each change of the screen.
    async fn screen_until(&mut self, ready: impl Fn(&[String]) -> bool) -> Vec<String> {
        loop {
            self.changed = false;
            let screen = self.screen().await;
            if ready(&screen) {
                return screen;
            }
            // A change that came while the screen was read is a reason to read it again at once.
            if !self.changed {
                self.until(|value| value["body"]["event"] == "screen.changed")
                    .await;
            }
        }
    }

    async fn scroll_read(&mut self, by: i64) -> Value {
        self.send_read(by).await;
        self.read_reply().await
    }

    /// Sends a scroll read by `by` with the correlation identifier `scroll-read`.
    async fn send_read(&mut self, by: i64) {
        let line = json!({"surface": "s1", "cids": ["scroll-read"],
            "body": {"operation": "screen.scroll.read", "by": by, "quiet": 6}})
        .to_string();
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the request line was not written");
    }

    /// The reply of the scroll read, which answers its request: it carries the correlation identifier of the request.
    async fn read_reply(&mut self) -> Value {
        let reply = self
            .until(|value| {
                value["body"]["event"] == "screen.scroll" || value["body"]["error"].is_string()
            })
            .await;
        assert_eq!(
            reply["cids"],
            json!(["scroll-read"]),
            "the reply answers the scroll read: {reply}"
        );
        reply
    }
}

fn numbered_file() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("a file for less");
    let numbered: String = (1..=400).map(|line| format!("line {line:04}\n")).collect();
    std::fs::write(file.path(), numbered).expect("the numbered lines are written");
    file
}

/// A read of 50 lines toward newer content in `less` shows line 51 and later, two reads at the same offset show the same text,
/// and the screen after each read equals the screen before it.
#[tokio::test]
async fn a_scroll_read_in_less_shows_the_later_lines_and_restores_the_screen() {
    let file = numbered_file();
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    sidecar
        .type_text(&format!("/usr/bin/less {}\r", file.path().display()))
        .await;
    sidecar
        .screen_until(|rows| rows.first().is_some_and(|row| row == "line 0001"))
        .await;
    // less shows the name of the file in its first prompt and replaces it with `:` after any command, so `g` (the first line,
    // where less already is) gives the prompt that a scroll leaves.
    sidecar.type_text("g").await;
    let before = sidecar
        .screen_until(|rows| {
            rows.first().is_some_and(|row| row == "line 0001")
                && rows.last().is_some_and(|row| row == ":")
        })
        .await;

    let first = sidecar.scroll_read_after_draws(-50).await;
    let body = &first["body"];
    assert_eq!(body["event"], "screen.scroll", "{first}");
    assert_eq!(body["route"], "alternate_scroll", "{first}");
    assert_eq!(body["changed"], true, "{first}");
    assert_eq!(body["restored"], true, "{first}");
    assert_eq!(body["quiet"], 6, "{first}");
    assert_eq!(body["refreshRate"], 120, "{first}");
    assert_eq!(body["text"][0], "line 0051", "{first}");
    for step in ["scroll", "restore"] {
        assert_eq!(body["settledBy"][step], "quiet", "{step}: {first}");
    }
    assert_eq!(sidecar.screen().await, before, "the screen after the read");

    let second = sidecar.scroll_read_after_draws(-50).await;
    assert_eq!(
        second["body"]["text"], body["text"],
        "a second read at the same offset: {second}"
    );
    assert_eq!(second["body"]["restored"], true, "{second}");
    assert_eq!(
        sidecar.screen().await,
        before,
        "the screen after the second read"
    );
    sidecar.type_text("q").await;
}

/// A program that draws later than the ticks of the test after each input is read at its draw when the ticks follow its draws,
/// as at the display frames of a window: the program on the alternate screen draws `drawn` 0.2 s after the scroll and `ready`
/// 0.2 s after the restore.
#[tokio::test]
async fn a_scroll_read_paced_by_the_draws_reads_a_program_that_draws_late() {
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    sidecar
        .type_text(concat!(
            "printf '\\033[?1049h\\033[H%s%s' rea dy; stty raw -echo; ",
            "dd bs=3 count=1 >/dev/null 2>&1; sleep 0.2; printf '\\033[H%s%s' dra wn; ",
            "dd bs=3 count=1 >/dev/null 2>&1; sleep 0.2; printf '\\033[H%s%s' rea dy; dd bs=1 count=1 >/dev/null 2>&1\r"
        ))
        .await;
    sidecar
        .screen_until(|rows| rows.first().is_some_and(|row| row == "ready"))
        .await;
    let read = sidecar.scroll_read_after_draws(1).await;
    let body = &read["body"];
    assert_eq!(body["route"], "alternate_scroll", "{read}");
    assert_eq!(body["text"][0], "drawn", "{read}");
    assert_eq!(body["restored"], true, "{read}");
    sidecar.type_text("q").await;
}

/// Input of the person to the session during a read ends the read with an error that names the input, and the input runs.
#[tokio::test]
async fn input_of_the_person_during_a_scroll_read_refuses_the_read() {
    let file = numbered_file();
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    sidecar
        .type_text(&format!("/usr/bin/less {}\r", file.path().display()))
        .await;
    sidecar
        .screen_until(|rows| rows.first().is_some_and(|row| row == "line 0001"))
        .await;
    sidecar.answer_frames = false;
    sidecar.send_read(-50).await;
    sidecar.type_text("q").await;
    let refused = sidecar.read_reply().await;
    assert_eq!(
        refused["body"]["error"], "scrollReadInterrupted",
        "{refused}"
    );
    assert_eq!(
        refused["body"]["reason"],
        "the input request of the person reached the session during the scroll read",
        "{refused}"
    );
    sidecar.release_frames().await;
    // The input ran: `q` ended less, and the shell shows its prompt again.
    sidecar
        .screen_until(|rows| !rows.iter().any(|row| row.starts_with("line 0")))
        .await;
}

/// A program with mouse tracking receives the SGR wheel reports of the read: the program prints each byte that it reads as hex.
#[tokio::test]
async fn a_scroll_read_of_a_program_with_mouse_tracking_sends_wheel_reports() {
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    sidecar
        .type_text(concat!(
            "printf '\\033[?1000h\\033[?1006h%s%s' RE ADY; stty raw -echo; ",
            "while :; do printf '<%s>' \"$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' ')\"; done\r"
        ))
        .await;
    // READY follows the modes in the same output, so the engine has the modes when the screen shows it.
    sidecar
        .screen_until(|rows| rows.iter().any(|row| row.ends_with("READY")))
        .await;
    let read = sidecar.scroll_read(1).await;
    assert_eq!(read["body"]["route"], "program", "{read}");
    // The scroll sends the SGR report of a wheel up (button 64) and the restore that of a wheel down (button 65).
    let screen = sidecar
        .screen_until(|rows| {
            let text = rows.concat();
            text.contains("<1b><5b><3c><36><34><3b>") && text.contains("<1b><5b><3c><36><35><3b>")
        })
        .await;
    let text = screen.concat();
    assert!(
        text.find("<36><34><3b>") < text.find("<36><35><3b>"),
        "the report of the scroll comes before the report of the restore: {screen:?}"
    );
}

/// A program that ends each redraw with the reset of a synchronized update settles each step by `sync`: the program draws the
/// value of each byte that it reads inside an update on the alternate screen.
#[tokio::test]
async fn a_scroll_read_of_a_program_with_synchronized_output_settles_by_sync() {
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    sidecar
        .type_text(concat!(
            "printf '\\033[?1049h%s%s' RE ADY; stty raw -echo; ",
            "while :; do printf '\\033[?2026h\\033[H\\033[2Kkey %s\\033[?2026l' \"$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' ')\"; done\r"
        ))
        .await;
    sidecar
        .screen_until(|rows| rows.iter().any(|row| row.ends_with("READY")))
        .await;
    // The ticks wait until the program has drawn each step, because the program starts processes for each byte and a tick of
    // this test comes at once, not at a display frame.
    sidecar.answer_frames = false;
    sidecar.send_read(1).await;
    sidecar
        .screen_until(|rows| rows.first().is_some_and(|row| row == "key 41"))
        .await;
    sidecar.send_held_ticks().await;
    sidecar
        .screen_until(|rows| rows.first().is_some_and(|row| row == "key 42"))
        .await;
    sidecar.release_frames().await;
    let read = sidecar.read_reply().await;
    let body = &read["body"];
    assert_eq!(body["route"], "alternate_scroll", "{read}");
    assert_eq!(body["settledBy"]["scroll"], "sync", "{read}");
    assert_eq!(body["settledBy"]["restore"], "sync", "{read}");
    assert_eq!(body["changed"], true, "{read}");
    assert!(
        body["text"][0]
            .as_str()
            .is_some_and(|row| row.starts_with("key ")),
        "{read}"
    );
}

/// A program that writes nothing within the quiet interval after the scroll settles by `quiet` with `changed: false`; its later
/// output is not part of the read, which the reply states by `settledBy`.
#[tokio::test]
async fn a_scroll_read_of_a_program_that_answers_after_the_interval_settles_by_quiet() {
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    sidecar
        .type_text(concat!(
            "printf '\\033[?1049h%s%s' RE ADY; stty raw -echo; ",
            "dd bs=3 count=1 >/dev/null 2>&1; sleep 1; printf '%s%s' LA TE; dd bs=1 count=3 >/dev/null 2>&1\r"
        ))
        .await;
    sidecar
        .screen_until(|rows| rows.iter().any(|row| row.ends_with("READY")))
        .await;
    let read = sidecar.scroll_read(1).await;
    let body = &read["body"];
    assert_eq!(body["settledBy"]["scroll"], "quiet", "{read}");
    assert_eq!(body["changed"], false, "{read}");
    assert_eq!(body["restored"], true, "{read}");
    sidecar
        .screen_until(|rows| rows.iter().any(|row| row.contains("LATE")))
        .await;
}

/// On the primary screen of a shell the read moves the viewport: the rows above it show the older lines, the reply answers at
/// once with `viewport`, and the viewport returns; a `by` of 0 is refused before any scroll.
#[tokio::test]
async fn a_scroll_read_on_the_primary_screen_moves_the_viewport_and_returns() {
    let mut sidecar = Sidecar::start();
    sidecar.open().await;
    // The prompt of the shell is set, so the screen is complete when it shows the prompt after the last row.
    sidecar
        .type_text("PS1='done$ '; seq -f 'row %04g' 1 200\r")
        .await;
    let before = sidecar
        .screen_until(|rows| {
            rows.windows(2)
                .any(|pair| pair[0] == "row 0200" && pair[1] == "done$")
        })
        .await;
    let refused = sidecar.scroll_read(0).await;
    assert_eq!(refused["body"]["error"], "invalidParams", "{refused}");
    let read = sidecar.scroll_read(50).await;
    let body = &read["body"];
    assert_eq!(body["route"], "viewport", "{read}");
    assert_eq!(
        body["settledBy"],
        json!({"scroll": "viewport", "restore": "viewport"}),
        "{read}"
    );
    assert_eq!(body["restored"], true, "{read}");
    assert_eq!(body["refreshRate"], Value::Null, "{read}");
    let first = before
        .iter()
        .position(|row| row.starts_with("row "))
        .expect("a row of seq on the screen");
    let older: u32 = before[first][4..].parse::<u32>().expect("a row number") - 50;
    assert!(
        body["text"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row == &json!(format!("row {older:04}")))),
        "{read}"
    );
    assert_eq!(sidecar.screen().await, before, "the screen after the read");
}
