// Checks the owned-session operations of core docs/spec/persistent-services.md#owned-sessions through the persistent service,
// a real engine and a real PTY (S241-2, core F1062-4). Each request is an owned-session body (`open` with an object `session`,
// `resize` or `end`) on the owner surface; each reply is found by its correlation identifier, and a wait ends after a timeout,
// so a missing reply fails the assertion instead of blocking the test.
#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::AlacrittyEngine;
use serde_json::{Value, json};
use soksak_sidecar_vt_core::events::{EventLog, Events, Options};
use soksak_sidecar_vt_core::protocol::{
    PersistentOwner, PersistentRegistry, SessionPort, serve_with_registry,
};
use soksak_sidecar_vt_core::{Engine, LocalSessionPort, pty::PtyService};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

/// The longest wait for one reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// A persistent service whose input and output are the two ends of the test.
struct Service {
    to_serve: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    _task: tokio::task::JoinHandle<std::io::Result<()>>,
    _logs: tempfile::TempDir,
}

fn start_service(registry: Arc<PersistentRegistry>, owner: &str) -> Service {
    let logs = tempfile::tempdir().expect("create the folder of the events");
    let events: Events = Arc::new(
        EventLog::start(Options::service(
            logs.path().to_path_buf(),
            format!("owned-sessions-{}", std::process::id()),
        ))
        .expect("start the writer of the events"),
    );
    let (to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(16 * 1024 * 1024);
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let port_events = events.clone();
    let factory = Arc::new(move || {
        Arc::new(LocalSessionPort::new_with_owner(
            Arc::new(PtyService::new(port_events.clone())),
            String::new(),
        )) as Arc<dyn SessionPort>
    });
    let owner = PersistentOwner {
        close: Arc::new(|| Ok(())),
        registry,
        owner: owner.to_string(),
        log: events,
    };
    let task = tokio::spawn(serve_with_registry(
        engine_factory,
        serve_in,
        serve_out,
        factory,
        owner,
    ));
    Service {
        to_serve,
        lines: BufReader::new(from_serve).lines(),
        _task: task,
        _logs: logs,
    }
}

impl Service {
    /// Sends one owned-session body from `surface` with the correlation identifier `cid`.
    async fn send(&mut self, surface: &str, cid: &str, body: Value) {
        let line = json!({"surface": surface, "root": "/project", "cids": [cid], "body": body});
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the request line was not written");
    }

    /// Reads lines until the reply of `cid` arrives and returns its `body`.
    async fn reply(&mut self, cid: &str) -> Value {
        loop {
            let line = tokio::time::timeout(REPLY_TIMEOUT, self.lines.next_line())
                .await
                .unwrap_or_else(|_| panic!("no reply to {cid} within {REPLY_TIMEOUT:?}"))
                .expect("read a line of the service")
                .expect("the service closed its output");
            let value: Value = serde_json::from_str(&line).expect("the service writes JSON lines");
            let answers = value["cids"]
                .as_array()
                .is_some_and(|cids| cids.iter().any(|c| c == cid));
            if answers {
                return value["body"].clone();
            }
        }
    }
}

fn open_body(creation: &str, size: Value, params: Value) -> Value {
    json!({"operation": "open", "session": {"creation": creation}, "size": size, "params": params, "plugin": "terminal"})
}

/// A program that waits on its terminal input, so a session stays open until its `end` or the end of the service.
fn cat_params() -> Value {
    json!({"program": "/bin/cat"})
}

fn size(width: f64, height: f64, scale: f64) -> Value {
    json!({"width": width, "height": height, "scale": scale})
}

#[tokio::test]
async fn open_answers_the_session_and_its_creation() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let body = service.reply("c1").await;
    let session = body["session"].as_str().expect("the reply names a session");
    assert!(
        !session.is_empty(),
        "the session identifier is empty: {body}"
    );
    assert_eq!(
        body["creation"], "k1",
        "the reply names another creation: {body}"
    );
    assert!(body.get("error").is_none(), "the open was refused: {body}");
}

#[tokio::test]
async fn a_repeated_open_answers_the_same_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let first = service.reply("c1").await;
    let session = first["session"]
        .as_str()
        .expect("the first open names a session")
        .to_string();
    service
        .send(
            "o1",
            "c2",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let second = service.reply("c2").await;
    assert_eq!(
        first, second,
        "a repeated open of the same creation answers another reply"
    );
    assert_eq!(
        second["session"],
        session.as_str(),
        "the repeated open names another session"
    );
}

#[tokio::test]
async fn resize_answers_the_size_of_the_request() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let session = service.reply("c1").await["session"].clone();
    service
        .send(
            "o1",
            "c2",
            json!({"operation": "resize", "session": session, "size": size(600.0, 300.0, 2.0)}),
        )
        .await;
    let body = service.reply("c2").await;
    assert_eq!(
        body["session"], session,
        "the reply names another session: {body}"
    );
    assert_eq!(
        body["size"],
        size(600.0, 300.0, 2.0),
        "the reply does not echo the size: {body}"
    );
}

#[tokio::test]
async fn resize_refuses_an_unknown_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            json!({"operation": "resize", "session": "missing", "size": size(600.0, 300.0, 2.0)}),
        )
        .await;
    assert_eq!(
        service.reply("c1").await["error"],
        "session missing is not open"
    );
}

#[tokio::test]
async fn resize_refuses_a_session_of_another_owner() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let session = service.reply("c1").await["session"].clone();
    let session = session
        .as_str()
        .expect("the session is a string")
        .to_string();
    service
        .send(
            "o2",
            "c2",
            json!({"operation": "resize", "session": session, "size": size(600.0, 300.0, 2.0)}),
        )
        .await;
    assert_eq!(
        service.reply("c2").await["error"],
        format!("surface o2 does not own session {session}")
    );
}

#[tokio::test]
async fn end_answers_ended_and_a_repeated_end_answers_the_same() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let session = service.reply("c1").await["session"].clone();
    service
        .send("o1", "c2", json!({"operation": "end", "session": session}))
        .await;
    assert_eq!(
        service.reply("c2").await,
        json!({"session": session, "ended": true})
    );
    service
        .send("o1", "c3", json!({"operation": "end", "session": session}))
        .await;
    assert_eq!(
        service.reply("c3").await,
        json!({"session": session, "ended": true})
    );
    service
        .send(
            "o1",
            "c4",
            json!({"operation": "resize", "session": session, "size": size(600.0, 300.0, 2.0)}),
        )
        .await;
    let session = session
        .as_str()
        .expect("the session is a string")
        .to_string();
    assert_eq!(
        service.reply("c4").await["error"],
        format!("session {session} is not open")
    );
}

#[tokio::test]
async fn open_refuses_a_size_with_no_whole_cell() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(1.0, 1.0, 1.0), cat_params()),
        )
        .await;
    assert_eq!(
        service.reply("c1").await["error"],
        "size is smaller than one cell"
    );
}

#[tokio::test]
async fn open_refuses_a_size_that_is_not_positive() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 0.0), cat_params()),
        )
        .await;
    assert_eq!(
        service.reply("c1").await["error"],
        "size width, height and scale must be positive numbers"
    );
}

#[tokio::test]
async fn open_refuses_a_program_that_is_not_found() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body(
                "k1",
                size(400.0, 200.0, 2.0),
                json!({"program": "/no/such/program-owned-session"}),
            ),
        )
        .await;
    let error = service.reply("c1").await["error"]
        .as_str()
        .expect("the refusal is a text")
        .to_string();
    assert!(
        error.starts_with("open failed:") && error.contains("program not found"),
        "unexpected refusal: {error}"
    );
}

impl Service {
    /// Reads lines until one answers `keep`, and returns it; a line that is not answered within the timeout fails the test.
    async fn line_where(&mut self, what: &str, keep: impl Fn(&Value) -> bool) -> Value {
        loop {
            let line = tokio::time::timeout(REPLY_TIMEOUT, self.lines.next_line())
                .await
                .unwrap_or_else(|_| panic!("no {what} within {REPLY_TIMEOUT:?}"))
                .expect("read a line of the service")
                .expect("the service closed its output");
            let value: Value = serde_json::from_str(&line).expect("the service writes JSON lines");
            if keep(&value) {
                return value;
            }
        }
    }

    /// Reads the visible rows of the tab surface as text.
    async fn screen_text(&mut self, surface: &str) -> String {
        self.send(surface, "screen", json!({"operation": "screen.read"}))
            .await;
        let line = self
            .line_where("the screen of the surface", |value| {
                value["surface"] == surface
                    && value["body"]["event"] == "screen"
                    && value["body"]["base"] == true
            })
            .await;
        let cols = line["body"]["cols"].as_u64().expect("the screen has cols") as usize;
        let cells = line["body"]["cells"]
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
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ends the view of a surface: the close envelope of core.
    async fn close_surface(&mut self, surface: &str) {
        let line = json!({"surface": surface, "closed": true});
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the close line was not written");
    }

    /// Opens an owned session of `owner` running `params` and returns its session id.
    async fn open_session(&mut self, owner: &str, cid: &str, params: Value) -> String {
        self.send(
            owner,
            cid,
            open_body("k-attach", size(400.0, 200.0, 2.0), params),
        )
        .await;
        let body = self.reply(cid).await;
        body["session"]
            .as_str()
            .expect("the open names a session")
            .to_string()
    }

    /// Attaches `surface` to `session`.
    async fn attach(&mut self, surface: &str, cid: &str, session: &str) -> Value {
        self.send(
            surface,
            cid,
            json!({"operation": "attach", "session": session}),
        )
        .await;
        self.reply(cid).await
    }
}

fn shell_params(script: &str) -> Value {
    json!({"program": "/bin/sh", "args": ["-c", script]})
}

#[tokio::test]
async fn attach_answers_the_session_as_attached() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    let body = service.attach("t1", "a1", &session).await;
    assert_eq!(body, json!({"session": session, "attached": true}));
}

#[tokio::test]
async fn attach_refuses_an_unknown_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let body = service.attach("t1", "a1", "missing").await;
    assert_eq!(body["error"], "session missing is not open");
}

#[tokio::test]
async fn attach_shows_the_output_the_session_had_before_the_attach() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session("o1", "c1", shell_params("printf READY; exec cat"))
        .await;
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    let mut screen = String::new();
    for _ in 0..50 {
        screen = service.screen_text("t1").await;
        if screen.contains("READY") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        screen.contains("READY"),
        "the attached surface shows no output: {screen:?}"
    );
}

#[tokio::test]
async fn an_attached_surface_sends_its_input_to_the_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    service
        .send(
            "t1",
            "p1",
            json!({"operation": "paste", "text": "hello-owned"}),
        )
        .await;
    let paste = service.reply("p1").await;
    assert!(
        paste.get("error").is_none(),
        "the paste was refused: {paste}"
    );
    let mut screen = String::new();
    for _ in 0..50 {
        screen = service.screen_text("t1").await;
        if screen.contains("hello-owned") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        screen.contains("hello-owned"),
        "the input did not reach the session: {screen:?}"
    );
}

#[tokio::test]
async fn ending_the_view_detaches_and_keeps_the_session_open() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    service.close_surface("t1").await;
    service
        .send(
            "o1",
            "c2",
            json!({"operation": "resize", "session": session, "size": size(600.0, 300.0, 2.0)}),
        )
        .await;
    assert_eq!(service.reply("c2").await["session"], session.as_str());
    assert_eq!(service.attach("t2", "a2", &session).await["attached"], true);
}

#[tokio::test]
async fn a_second_attach_is_refused_while_a_view_shows_the_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    assert_eq!(
        service.attach("t2", "a2", &session).await["error"],
        format!("session {session} is shown by surface t1")
    );
}

#[tokio::test]
async fn the_end_of_the_shell_ends_the_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session("o1", "c1", shell_params("printf DONE"))
        .await;
    let mut refusal = String::new();
    for attempt in 0..50 {
        let cid = format!("r{attempt}");
        service
            .send(
                "o1",
                &cid,
                json!({"operation": "resize", "session": session, "size": size(600.0, 300.0, 2.0)}),
            )
            .await;
        let body = service.reply(&cid).await;
        if let Some(error) = body["error"].as_str() {
            refusal = error.to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(refusal, format!("session {session} is not open"));
}

#[tokio::test]
async fn the_end_of_the_shell_sends_the_session_ended_event_to_the_owner() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session("o1", "c1", shell_params("printf DONE"))
        .await;
    let event = service
        .line_where("the session.ended event", |value| {
            value["body"]["event"] == "session.ended"
        })
        .await;
    assert_eq!(event["surface"], "o1");
    assert_eq!(event["body"]["session"], session.as_str());
    assert_eq!(event["body"]["reason"], "ended");
}

impl Service {
    /// Reads the screen of an owned session of `owner` and returns the reply body.
    async fn read_session(&mut self, owner: &str, cid: &str, session: &str) -> Value {
        self.send(owner, cid, json!({"operation": "read", "session": session}))
            .await;
        self.reply(cid).await
    }

    /// Reads the owned session until its rows contain `text`, and returns the last reply body.
    async fn read_until(&mut self, owner: &str, session: &str, text: &str) -> Value {
        let mut body = Value::Null;
        for attempt in 0..50 {
            let cid = format!("read{attempt}");
            body = self.read_session(owner, &cid, session).await;
            let shown = body["rows"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row.as_str().is_some_and(|row| row.contains(text)))
            });
            if shown {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        body
    }
}

#[tokio::test]
async fn read_answers_the_rows_and_the_cursor_of_the_session() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session("o1", "c1", shell_params("printf READY; exec cat"))
        .await;
    let body = service.read_until("o1", &session, "READY").await;
    let rows = body["rows"].as_array().expect("the reply has rows");
    assert_eq!(rows[0], "READY", "the first row is not the output: {body}");
    assert_eq!(body["cursor"], json!({"row": 0, "column": 5}));
}

#[tokio::test]
async fn read_refuses_a_session_of_another_owner() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    let body = service.read_session("o2", "r1", &session).await;
    assert_eq!(
        body["error"],
        format!("surface o2 does not own session {session}")
    );
}

#[tokio::test]
async fn input_writes_the_text_to_the_session_of_its_owner() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    service
        .send(
            "o1",
            "i1",
            json!({"operation": "input", "session": session, "text": "typed-owned"}),
        )
        .await;
    assert_eq!(service.reply("i1").await, json!({"session": session}));
    let body = service.read_until("o1", &session, "typed-owned").await;
    assert!(
        body["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row.as_str().is_some_and(|row| row.contains("typed-owned")))),
        "the input did not reach the session: {body}"
    );
}

#[tokio::test]
async fn input_judges_the_line_as_the_input_of_a_surface() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session("o1", "c1", json!({"shell": "/bin/zsh"}))
        .await;
    service
        .send(
            "o1",
            "i1",
            json!({"operation": "input", "session": session, "text": "rm -rf /\r"}),
        )
        .await;
    let body = service.reply("i1").await;
    assert_eq!(
        body["error"], "guardRefused",
        "the destructive line was not refused: {body}"
    );
}

impl Service {
    /// Sends `text` as the input of `session` and returns the reply.
    async fn input(&mut self, cid: &str, session: &str, text: &str) -> Value {
        self.send(
            "o1",
            cid,
            json!({"operation": "input", "session": session, "text": text}),
        )
        .await;
        self.reply(cid).await
    }
}

#[tokio::test]
async fn a_program_that_reads_keys_at_a_main_screen_prompt_accepts_an_arrow_then_a_return() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session(
            "o1",
            "c1",
            shell_params("printf 'TRUST? '; read -r answer; printf 'ANSWER%s' ED; exec cat"),
        )
        .await;
    service.read_until("o1", &session, "TRUST?").await;
    assert_eq!(
        service.input("i1", &session, "\u{1b}[B").await,
        json!({"session": session})
    );
    assert_eq!(
        service.input("i2", &session, "\r").await,
        json!({"session": session})
    );
    let body = service.read_until("o1", &session, "ANSWERED").await;
    assert!(
        body["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row.as_str().is_some_and(|row| row.contains("ANSWERED")))),
        "the return did not reach the program: {body}"
    );
}

#[tokio::test]
async fn a_command_that_runs_in_the_shell_accepts_an_arrow_then_a_return() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session("o1", "c1", json!({"shell": "/bin/zsh"}))
        .await;
    let command = "printf 'RUN%s' NING; read -r answer; printf 'ANSWER%s' ED\r";
    assert_eq!(
        service.input("i1", &session, command).await,
        json!({"session": session})
    );
    service.read_until("o1", &session, "RUNNING").await;
    assert_eq!(
        service.input("i2", &session, "\u{1b}[B").await,
        json!({"session": session})
    );
    assert_eq!(
        service.input("i3", &session, "\r").await,
        json!({"session": session})
    );
    let body = service.read_until("o1", &session, "ANSWERED").await;
    assert!(
        body["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row.as_str().is_some_and(|row| row.contains("ANSWERED")))),
        "the return did not reach the program: {body}"
    );
}

#[tokio::test]
async fn an_attached_surface_of_a_program_sends_an_arrow_then_a_return() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session(
            "o1",
            "c1",
            shell_params("printf 'TRUST? '; read -r answer; printf 'ANSWER%s' ED; exec cat"),
        )
        .await;
    service.read_until("o1", &session, "TRUST?").await;
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    let keys = |key: &str| {
        json!({"operation": "input", "actions": [{"type": "key", "key": key, "shift": false, "alt": false, "ctrl": false}],
            "preedit": {"text": "", "markedRange": null, "selectedRange": null, "replacementRange": null, "attributed": false}})
    };
    service.send("t1", "k1", keys("Down")).await;
    service.send("t1", "k2", keys("Enter")).await;
    let body = service.read_until("o1", &session, "ANSWERED").await;
    assert!(
        body["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row.as_str().is_some_and(|row| row.contains("ANSWERED")))),
        "the return of the attached surface did not reach the program: {body}"
    );
}

#[tokio::test]
async fn paste_writes_the_text_to_the_session_of_its_owner() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", cat_params()).await;
    service
        .send(
            "o1",
            "p1",
            json!({"operation": "paste", "session": session, "text": "pasted-owned"}),
        )
        .await;
    assert_eq!(service.reply("p1").await, json!({"session": session}));
    let body = service.read_until("o1", &session, "pasted-owned").await;
    assert!(
        body["rows"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row.as_str().is_some_and(|row| row.contains("pasted-owned")))),
        "the paste did not reach the session: {body}"
    );
}

#[tokio::test]
async fn the_program_state_of_an_owned_session_goes_to_its_owner() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let script = "printf '\\033]7501;state=working\\007'; sleep 1; printf '\\033]7501;state=blocked:kind=permission\\007'; sleep 1; printf '\\033]7501;state=blocked:kind=permission\\007'; exec cat";
    let session = service.open_session("o1", "c1", shell_params(script)).await;
    let first = service
        .line_where("the first session.program event", |value| {
            value["body"]["event"] == "session.program"
        })
        .await;
    assert_eq!(first["surface"], "o1");
    assert_eq!(
        first["body"],
        json!({"event": "session.program", "session": session, "state": "working"})
    );
    let second = service
        .line_where("the second session.program event", |value| {
            value["body"]["event"] == "session.program"
        })
        .await;
    assert_eq!(
        second["body"],
        json!({"event": "session.program", "session": session, "state": "blocked"})
    );
    // The repeated blocked report leaves the state unchanged, so no further event arrives during the wait.
    let repeated = tokio::time::timeout(
        Duration::from_secs(2),
        service.line_where("a repeated session.program event", |value| {
            value["body"]["event"] == "session.program"
        }),
    )
    .await;
    assert!(
        repeated.is_err(),
        "a repeated state sent an event: {repeated:?}"
    );
}

/// The hexadecimal bytes of the answer `ESC ] 7501 ; ? BEL` to the status query.
const QUERY_ANSWER_HEX: &str = "1b5d373530313b3f07";

#[tokio::test]
async fn an_owned_session_answers_the_status_query_before_the_device_attributes() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    // The program sends the query and the primary device attributes request in one write, as Claude Code does, and reports
    // only when the first bytes that it reads are the answer to the query.
    let script = format!(
        "stty raw -echo; printf '\\033]7501;?\\007\\033[c'; a=$(dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
         if [ \"$a\" = {QUERY_ANSWER_HEX} ]; then printf '\\033]7501;state=working\\007'; else printf 'ANSWER %s' \"$a\"; fi; exec cat >/dev/null"
    );
    let session = service
        .open_session("o1", "c1", shell_params(&script))
        .await;
    let event = service
        .line_where("the session.program event", |value| {
            value["body"]["event"] == "session.program"
        })
        .await;
    assert_eq!(
        event["body"],
        json!({"event": "session.program", "session": session, "state": "working"})
    );
}

#[tokio::test]
async fn an_attached_surface_does_not_answer_the_queries_of_the_replay() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    // The program prints each byte that it reads as hexadecimal, so each answer that it receives is visible on the screen.
    let script = "stty raw -echo; printf '\\033]7501;?\\007'; while :; do b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); printf '%s' \"$b\"; done";
    let session = service.open_session("o1", "c1", shell_params(script)).await;
    let before = service.read_until("o1", &session, QUERY_ANSWER_HEX).await;
    assert!(
        before.to_string().contains(QUERY_ANSWER_HEX),
        "the session answered no query before the attach: {before}"
    );
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after = service
        .read_session("o1", "after", &session)
        .await
        .to_string();
    assert_eq!(
        after.matches(QUERY_ANSWER_HEX).count(),
        1,
        "the query was answered again after the attach: {after}"
    );
}

impl Service {
    /// Sends `{operation: "sessions", request}` and returns the answer that names the request.
    async fn sessions(&mut self, request: &str) -> Value {
        let line = json!({"operation": "sessions", "request": request});
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the sessions line was not written");
        loop {
            let line = tokio::time::timeout(REPLY_TIMEOUT, self.lines.next_line())
                .await
                .unwrap_or_else(|_| panic!("no sessions answer within {REPLY_TIMEOUT:?}"))
                .expect("read a line of the service")
                .expect("the service closed its output");
            let value: Value = serde_json::from_str(&line).expect("the service writes JSON lines");
            if value["operation"] == "sessions" && value["request"] == request {
                return value;
            }
        }
    }
}

#[tokio::test]
async fn sessions_lists_each_open_owned_session_with_its_owner_root_creation_size_and_plugin() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let kept = service.reply("c1").await["session"].clone();
    service
        .send(
            "o2",
            "c2",
            open_body("k2", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let ended = service.reply("c2").await["session"].clone();
    service
        .send("o2", "c3", json!({"operation": "end", "session": ended}))
        .await;
    assert_eq!(service.reply("c3").await["ended"], true);
    assert_eq!(
        service.sessions("r1").await,
        json!({"operation": "sessions", "request": "r1", "ok": true, "sessions": [
            {"session": kept, "surface": "o1", "root": "/project", "creation": "k1", "size": size(400.0, 200.0, 2.0), "plugin": "terminal"}
        ]})
    );
}

#[tokio::test]
async fn sessions_refuses_a_request_without_an_identifier() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    service
        .to_serve
        .write_all(b"{\"operation\":\"sessions\"}\n")
        .await
        .expect("the sessions line was not written");
    let line = tokio::time::timeout(REPLY_TIMEOUT, service.lines.next_line())
        .await
        .expect("no answer to the sessions line")
        .expect("read a line of the service")
        .expect("the service closed its output");
    let value: Value = serde_json::from_str(&line).expect("the service writes JSON lines");
    assert_eq!(
        value,
        json!({"error": "invalidParams", "reason": "the owner request has no request id"})
    );
}

#[tokio::test]
async fn two_service_processes_name_their_first_sessions_differently() {
    // Each registry is the state of one service process, so a session of a later process never takes the identifier of a
    // session of an earlier one that a stored layout may still name.
    let mut first = start_service(PersistentRegistry::new(), "o1");
    let mut second = start_service(PersistentRegistry::new(), "o1");
    first
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    second
        .send(
            "o1",
            "c1",
            open_body("k1", size(400.0, 200.0, 2.0), cat_params()),
        )
        .await;
    let a = first.reply("c1").await["session"].clone();
    let b = second.reply("c1").await["session"].clone();
    assert!(
        a.is_string() && b.is_string(),
        "an open named no session: {a} {b}"
    );
    assert_ne!(a, b, "two service processes named the same session");
}

#[tokio::test]
async fn open_refuses_a_body_without_a_plugin() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    for (cid, plugin) in [
        ("c1", None),
        ("c2", Some(json!(""))),
        ("c3", Some(json!(7))),
    ] {
        let mut body = open_body("k1", size(400.0, 200.0, 2.0), cat_params());
        match plugin {
            Some(plugin) => body["plugin"] = plugin,
            None => {
                body.as_object_mut()
                    .expect("the open body is an object")
                    .remove("plugin");
            }
        }
        service.send("o1", cid, body).await;
        assert_eq!(
            service.reply(cid).await,
            json!({"error": "open session requires a non-empty plugin"}),
            "the open {cid} was not refused"
        );
    }
    assert_eq!(service.sessions("r1").await["sessions"], json!([]));
}

/// A program that prints the numbered lines 1 to 80 and then waits on its terminal input.
fn numbered_params() -> Value {
    shell_params("i=1; while [ $i -le 80 ]; do echo line$i; i=$((i+1)); done; exec cat")
}

#[tokio::test]
async fn read_with_from_and_count_answers_the_retained_rows() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", numbered_params()).await;
    service.read_until("o1", &session, "line80").await;
    service
        .send(
            "o1",
            "r1",
            json!({"operation": "read", "session": session, "from": 0, "count": 3}),
        )
        .await;
    assert_eq!(
        service.reply("r1").await,
        json!({"from": 0, "count": 3, "rows": ["line1", "line2", "line3"]})
    );
}

#[tokio::test]
async fn read_refuses_an_invalid_range_with_an_explicit_error() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", numbered_params()).await;
    service.read_until("o1", &session, "line80").await;
    let range = "read requires an integer from and a count from 1 to 200";
    for (cid, members, error) in [
        (
            "r1",
            json!({"count": 2}),
            "read count requires from".to_string(),
        ),
        ("r2", json!({"from": 0}), range.to_string()),
        ("r3", json!({"from": 0, "count": 0}), range.to_string()),
        ("r4", json!({"from": 0, "count": 201}), range.to_string()),
        ("r5", json!({"from": -1, "count": 2}), range.to_string()),
    ] {
        let mut body = json!({"operation": "read", "session": session});
        for (key, value) in members.as_object().expect("the members are an object") {
            body[key] = value.clone();
        }
        service.send("o1", cid, body).await;
        assert_eq!(
            service.reply(cid).await,
            json!({"error": error}),
            "the read {cid} was not refused"
        );
    }
    service
        .send(
            "o1",
            "r6",
            json!({"operation": "read", "session": session, "from": 100000, "count": 2}),
        )
        .await;
    let body = service.reply("r6").await;
    let error = body["error"]
        .as_str()
        .unwrap_or_else(|| panic!("a range outside the rows was answered: {body}"));
    assert!(error.contains("outside"), "unexpected refusal: {error}");
    assert!(body.get("rows").is_none(), "a refusal carries rows: {body}");
}

/// A program that prints, at each line of its input, the columns of its terminal and a row of that many `x`.
fn width_params() -> Value {
    shell_params(
        "while read line; do cols=$(stty size | cut -d' ' -f2); echo cols$cols; i=0; row=; while [ $i -lt $cols ]; do row=${row}x; i=$((i+1)); done; echo $row; done",
    )
}

#[tokio::test]
async fn a_session_resized_through_a_surface_of_another_font_reads_the_rows_that_its_program_drew()
{
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", width_params()).await;
    // The surface draws an image with a font smaller than the default font, so its grid holds more columns than the grid of
    // the default font for the same size.
    service
        .send("t1", "g1", json!({"image": {"configure": {"name": "view", "generation": 1, "raster": 1, "width": 800, "height": 400, "scale": 2.0}}}))
        .await;
    service
        .send(
            "t1",
            "f1",
            json!({"operation": "font", "family": "Menlo", "size": 8}),
        )
        .await;
    let font = service.reply("f1").await;
    assert_eq!(font["event"], "font", "the font was refused: {font}");
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    service
        .send(
            "o1",
            "z1",
            json!({"operation": "resize", "session": session, "size": size(600.0, 300.0, 2.0)}),
        )
        .await;
    assert_eq!(service.reply("z1").await["session"], session.as_str());
    let mut body = Value::Null;
    let mut drawn = None;
    for attempt in 0..50 {
        service
            .send(
                "o1",
                &format!("i{attempt}"),
                json!({"operation": "input", "session": session, "text": "\r"}),
            )
            .await;
        service.reply(&format!("i{attempt}")).await;
        body = service.read_until("o1", &session, "cols").await;
        let rows: Vec<String> = body["rows"]
            .as_array()
            .expect("the reply has rows")
            .iter()
            .map(|row| row.as_str().expect("a row is a text").to_string())
            .collect();
        // The width of the last report, once the resize reached the program (a report wider than the default grid).
        let last = rows.iter().rposition(|row| row.starts_with("cols"));
        if let Some(index) = last
            && let Ok(cols) = rows[index][4..].parse::<usize>()
            && index + 1 < rows.len()
            && !rows[index + 1].is_empty()
        {
            drawn = Some((
                cols,
                rows[index + 1].clone(),
                rows.get(index + 2).cloned().unwrap_or_default(),
            ));
            if cols > 60 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (cols, row, next) = drawn.unwrap_or_else(|| panic!("the program drew no width: {body}"));
    assert!(
        cols > 60,
        "the resize did not reach the program at the grid of the surface: {body}"
    );
    assert_eq!(
        row,
        "x".repeat(cols),
        "the session wrapped the row of {cols} columns: {body}"
    );
    assert!(
        !next.starts_with('x'),
        "the session wrapped the row of {cols} columns: {body}"
    );
}

/// A surface that attaches to a session shows the screen that the engine of the session holds before the program writes
/// again, also when the session wrote more output than the session keeps as bytes since the screen was drawn: the program
/// draws a prompt on the alternate screen, writes more than 1 MiB of window titles, which change no cell, and draws END.
#[tokio::test]
async fn an_attached_surface_shows_the_screen_of_the_session_before_new_output() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service
        .open_session(
            "o1",
            "c1",
            shell_params("printf '\\033[?1049h\\033[2J\\033[5;3HPROMPT READY'; i=0; while [ $i -lt 40000 ]; do printf '\\033]0;title of the conversation %s\\007' $i; i=$((i+1)); done; printf '\\033[6;3HEND'; exec sleep 600"),
        )
        .await;
    let body = service.read_until("o1", &session, "PROMPT READY").await;
    assert!(
        body.to_string().contains("PROMPT READY"),
        "the session drew no prompt: {body}"
    );
    // The program draws END after the titles, so the session holds every title once its screen shows END.
    let body = service.read_until("o1", &session, "END").await;
    assert!(
        body.to_string().contains("END"),
        "the program did not finish its titles: {body}"
    );
    assert_eq!(service.attach("t1", "a1", &session).await["attached"], true);
    let screen = service.screen_text("t1").await;
    assert!(
        screen.contains("PROMPT READY"),
        "the attached surface shows no rows of the session: {screen:?}"
    );
}

/// A read without a range answers `from`, the absolute line number of its first row, so a ranged read from `from` answers
/// the same rows.
#[tokio::test]
async fn read_without_a_range_answers_the_absolute_line_of_its_first_row() {
    let mut service = start_service(PersistentRegistry::new(), "o1");
    let session = service.open_session("o1", "c1", numbered_params()).await;
    let body = service.read_until("o1", &session, "line80").await;
    let from = body["from"]
        .as_u64()
        .unwrap_or_else(|| panic!("the read names no from: {body}"));
    let rows = body["rows"].as_array().expect("the reply has rows").clone();
    assert!(
        from > 0,
        "the first row of the screen is not below the history: {body}"
    );
    service
        .send(
            "o1",
            "r1",
            json!({"operation": "read", "session": session, "from": from, "count": 3}),
        )
        .await;
    let ranged = service.reply("r1").await;
    assert_eq!(
        ranged["rows"],
        json!(rows[..3]),
        "the rows at from differ: {ranged}"
    );
}
