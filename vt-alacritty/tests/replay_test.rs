// The replay executable (core docs/spec/replay.md#replay-executable) rebuilds the screens of a session from the stored inputs:
// a session served with the real engine and a real PTY is replayed from its event file, and every hash that the executable
// prints equals the hash that the service stored in its frame record.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::events::{read_lines, EventLog, Options};
use soksak_sidecar_vt_core::protocol::{make_default_session_port_factory, serve, Engine};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The events of one served session: a PTY running `cat` is opened, configured, and fed the line "한a" with a newline.
async fn served_session() -> Vec<Value> {
    let directory = tempfile::tempdir().expect("create the folder of the events");
    let mut options = Options::service(directory.path().join("logs"), "replay".to_string());
    options.batch_interval = Duration::from_millis(20);
    let log = Arc::new(EventLog::start(options).expect("start the writer"));
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(log.clone());
    let (mut to_serve, serve_in) = tokio::io::duplex(64 * 1024);
    let (serve_out, from_serve) = tokio::io::duplex(64 * 1024);
    let handle = log.clone();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    to_serve
        .write_all(
            br#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
{"surface":"s1","body":{"image":{"consumed":{"name":"view","sequence":1}}}}
{"surface":"s1","body":{"operation":"input","bytes":"7ZWcYQo="}}
"#,
        )
        .await
        .unwrap();
    // The surface task takes the output when it chooses and nothing tells the test; it asks the file until a frame includes the
    // echo of the line.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            log.flush(Duration::from_secs(10))
                .expect("flush the writer");
            let lines = read_lines(log.path()).expect("read the events");
            let echo = lines.iter().find_map(|line| {
                (line["event"] == "sidecar.vt.pty_read"
                    && line["fields"]["hex"]
                        .as_str()
                        .is_some_and(|hex| hex.contains("ed959c61")))
                .then(|| line["fields"]["sequence"].as_u64().unwrap())
            });
            if let Some(echo) = echo {
                if lines.iter().any(|line| {
                    line["event"] == "sidecar.vt.frame"
                        && line["fields"]["sequence"].as_u64().unwrap() >= echo
                }) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no frame includes the echo");
    drop(to_serve);
    let mut lines = BufReader::new(from_serve).lines();
    while lines.next_line().await.unwrap().is_some() {}
    task.await.unwrap();
    log.flush(Duration::from_secs(10))
        .expect("flush the writer");
    let lines = read_lines(log.path()).expect("read the events");
    drop(directory);
    lines
}

/// Runs the replay executable on `lines` and returns its status, its lines of hashes and its standard error.
fn replay(lines: &[Value]) -> (bool, Vec<Value>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-replay"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the replay executable");
    let mut stdin = child.stdin.take().unwrap();
    for line in lines {
        writeln!(stdin, "{line}").expect("write an event");
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .expect("wait for the replay executable");
    let hashes = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a line of hashes is JSON"))
        .collect();
    (
        output.status.success(),
        hashes,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn sidecar_lines(lines: Vec<Value>) -> Vec<Value> {
    lines
        .into_iter()
        .filter(|line| line["layer"] == "sidecar" && line["session"].is_string())
        .collect()
}

#[tokio::test]
async fn every_hash_that_the_replay_prints_equals_the_hash_of_the_frame_record() {
    let lines = sidecar_lines(served_session().await);
    let frames: Vec<&Value> = lines
        .iter()
        .filter(|line| line["event"] == "sidecar.vt.frame")
        .collect();
    assert!(!frames.is_empty(), "the session stored no frame");
    let (ok, hashes, error) = replay(&lines);
    assert!(ok, "{error}");
    assert_eq!(hashes.len(), frames.len(), "{hashes:?}");
    let mut stored: Vec<(u64, &str)> = frames
        .iter()
        .map(|frame| {
            (
                frame["fields"]["sequence"].as_u64().unwrap(),
                frame["fields"]["hash"].as_str().unwrap(),
            )
        })
        .collect();
    stored.sort_unstable();
    let replayed: Vec<(u64, &str)> = hashes
        .iter()
        .map(|line| {
            (
                line["sequence"].as_u64().unwrap(),
                line["hash"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(replayed, stored);
}

#[tokio::test]
async fn an_altered_input_changes_the_hashes_that_follow_it() {
    let lines = sidecar_lines(served_session().await);
    let (_, original, _) = replay(&lines);
    let mut altered = lines.clone();
    let read = altered
        .iter_mut()
        .find(|line| {
            line["event"] == "sidecar.vt.pty_read"
                && line["fields"]["hex"]
                    .as_str()
                    .is_some_and(|hex| hex.contains("ed959c61"))
        })
        .expect("the echo is an input");
    read["fields"]["hex"] = Value::String(
        read["fields"]["hex"]
            .as_str()
            .unwrap()
            .replace("ed959c61", "7a7a7a7a"),
    );
    let (ok, changed, error) = replay(&altered);
    assert!(ok, "{error}");
    assert_ne!(original, changed, "the altered input left every hash equal");
}

#[tokio::test]
async fn a_gap_in_the_sequences_of_the_inputs_is_an_error() {
    let lines = sidecar_lines(served_session().await);
    let without: Vec<Value> = lines
        .into_iter()
        .filter(|line| !(line["event"] == "sidecar.vt.resize"))
        .collect();
    let (ok, hashes, error) = replay(&without);
    assert!(!ok);
    assert!(hashes.is_empty());
    assert!(error.contains("no sequence 1"), "{error}");
}
