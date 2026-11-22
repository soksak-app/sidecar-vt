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
    json!({"operation": "open", "session": {"creation": creation}, "size": size, "params": params})
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
