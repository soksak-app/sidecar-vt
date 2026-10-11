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
    assert!(error.contains("no sequence 2"), "{error}");
}

/// The input line of the service that writes "한a" and a newline to the program.
const WRITE_LINE: &str = r#"{"surface":"s1","body":{"operation":"input","bytes":"7ZWcYQo="}}"#;

/// The highest sequence among the events of the session that are replay inputs.
fn last_input(lines: &[Value]) -> u64 {
    lines
        .iter()
        .filter(|line| {
            [
                "sidecar.vt.pty_read",
                "sidecar.vt.pty_write",
                "sidecar.vt.resize",
                "sidecar.vt.selection",
                "sidecar.vt.viewport",
                "sidecar.vt.theme",
            ]
            .contains(&line["event"].as_str().unwrap())
        })
        .map(|line| line["fields"]["sequence"].as_u64().unwrap())
        .max()
        .unwrap_or(0)
}

/// A session in which the page scrolls the viewport, selects text and changes the theme, served with the real engine. Each
/// step is sent after the service stored the inputs of the step before it, and the frames are answered as the host does.
async fn viewed_session() -> Vec<Value> {
    let directory = tempfile::tempdir().expect("create the folder of the events");
    let mut options = Options::service(directory.path().join("logs"), "view".to_string());
    options.batch_interval = Duration::from_millis(20);
    let log = Arc::new(EventLog::start(options).expect("start the writer"));
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(log.clone());
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(1 << 20);
    let handle = log.clone();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    let mut answered = 0u64;
    // Sends `step`, then answers the frames until `done` holds for the events and a frame includes the last input.
    async fn step(
        log: &EventLog,
        to_serve: &mut tokio::io::DuplexStream,
        answered: &mut u64,
        send: &str,
        done: impl Fn(&[Value]) -> bool,
    ) {
        to_serve.write_all(send.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                log.flush(Duration::from_secs(10)).expect("flush the writer");
                let lines = read_lines(log.path()).expect("read the events");
                let image = lines
                    .iter()
                    .filter(|line| line["event"] == "sidecar.vt.frame")
                    .filter_map(|line| line["fields"]["image"].as_u64())
                    .max()
                    .unwrap_or(0);
                if image > *answered {
                    *answered = image;
                    let answer = format!(
                        "{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"sequence\":{image}}}}}}}}}\n"
                    );
                    to_serve.write_all(answer.as_bytes()).await.unwrap();
                }
                let included = lines.iter().any(|line| {
                    line["event"] == "sidecar.vt.frame"
                        && line["fields"]["sequence"].as_u64().unwrap() >= last_input(&lines)
                });
                if done(&lines) && included {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the step did not finish");
    }
    let opening = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/cat","image":"view"}}
{"surface":"s1","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}
"#;
    step(&log, &mut to_serve, &mut answered, opening, |lines| {
        lines.iter().any(|line| line["event"] == "sidecar.vt.frame")
    })
    .await;
    let output = format!("{WRITE_LINE}\n").repeat(40);
    step(&log, &mut to_serve, &mut answered, &output, |lines| {
        lines
            .iter()
            .filter(|line| line["event"] == "sidecar.vt.pty_write")
            .count()
            == 40
            && lines.iter().any(|line| {
                line["event"] == "sidecar.vt.pty_read"
                    && line["fields"]["hex"].as_str().unwrap().ends_with("0d0a")
            })
    })
    .await;
    let scroll = "{\"surface\":\"s1\",\"body\":{\"operation\":\"scroll\",\"lines\":5,\"col\":0,\"row\":0}}\n";
    step(&log, &mut to_serve, &mut answered, scroll, |lines| {
        lines
            .iter()
            .any(|line| line["event"] == "sidecar.vt.viewport")
    })
    .await;
    let select = r#"{"surface":"s1","body":{"operation":"selection.start","x":10.0,"y":20.0}}
{"surface":"s1","body":{"operation":"selection.update","x":90.0,"y":40.0}}
{"surface":"s1","body":{"operation":"selection.end"}}
"#;
    step(&log, &mut to_serve, &mut answered, select, |lines| {
        lines.iter().any(|line| {
            line["event"] == "sidecar.vt.selection" && line["fields"]["action"] == "end"
        })
    })
    .await;
    let theme = r##"{"surface":"s1","body":{"operation":"theme","mode":"light","background":"#f7f7f5","foreground":"#242424","cursor":"#242424","selection":"#c8ccd8"}}
"##;
    step(&log, &mut to_serve, &mut answered, theme, |lines| {
        lines
            .iter()
            .filter(|line| line["event"] == "sidecar.vt.theme")
            .count()
            == 2
    })
    .await;
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

/// The events of a replay input kind of the session in the order of `sequence`.
fn inputs_of(lines: &[Value]) -> Vec<&Value> {
    let mut inputs: Vec<&Value> = lines
        .iter()
        .filter(|line| {
            [
                "sidecar.vt.pty_read",
                "sidecar.vt.pty_write",
                "sidecar.vt.resize",
                "sidecar.vt.selection",
                "sidecar.vt.viewport",
                "sidecar.vt.theme",
            ]
            .contains(&line["event"].as_str().unwrap())
        })
        .collect();
    inputs.sort_by_key(|line| line["fields"]["sequence"].as_u64().unwrap());
    inputs
}

#[tokio::test]
async fn the_view_changes_of_a_session_are_inputs_in_the_order_of_application() {
    let lines = sidecar_lines(viewed_session().await);
    let inputs = inputs_of(&lines);
    let sequences: Vec<u64> = inputs
        .iter()
        .map(|line| line["fields"]["sequence"].as_u64().unwrap())
        .collect();
    assert_eq!(sequences, (1..=inputs.len() as u64).collect::<Vec<_>>());
    assert_eq!(
        inputs[0]["event"], "sidecar.vt.theme",
        "the first input is a theme"
    );
    let colors = &inputs[0]["fields"]["colors"];
    assert_eq!(colors["foreground"], "#d0d0d0");
    assert_eq!(colors["selection"], "#44475a");
    assert_eq!(colors["ansi1"], "#cd0000");
    assert_eq!(colors.as_object().unwrap().len(), 4 + 256);
    let view: Vec<(&str, &Value)> = inputs
        .iter()
        .filter(|line| {
            [
                "sidecar.vt.selection",
                "sidecar.vt.viewport",
                "sidecar.vt.theme",
            ]
            .contains(&line["event"].as_str().unwrap())
        })
        .map(|line| (line["event"].as_str().unwrap(), &line["fields"]))
        .collect();
    assert_eq!(view.len(), 6, "{view:?}");
    assert_eq!(view[1].0, "sidecar.vt.viewport");
    assert_eq!(view[1].1["lines"], 5);
    assert_eq!(view[2].0, "sidecar.vt.selection");
    assert_eq!(view[2].1["action"], "start");
    assert_eq!(view[3].1["action"], "update");
    assert_eq!(view[4].1["action"], "end");
    assert_eq!(view[5].0, "sidecar.vt.theme");
    assert_eq!(view[5].1["colors"]["background"], "#f7f7f5");
    assert!(inputs.iter().all(|line| line["session"].is_string()));
}

#[tokio::test]
async fn a_session_with_a_scroll_a_selection_and_a_theme_replays_to_the_stored_hashes() {
    let lines = sidecar_lines(viewed_session().await);
    let mut stored: Vec<(u64, String)> = lines
        .iter()
        .filter(|line| line["event"] == "sidecar.vt.frame")
        .map(|line| {
            (
                line["fields"]["sequence"].as_u64().unwrap(),
                line["fields"]["hash"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    stored.sort();
    let (ok, hashes, error) = replay(&lines);
    assert!(ok, "{error}");
    let mut replayed: Vec<(u64, String)> = hashes
        .iter()
        .map(|line| {
            (
                line["sequence"].as_u64().unwrap(),
                line["hash"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    replayed.sort();
    assert_eq!(replayed, stored);
    // The frames after the scroll, the selection and the theme differ from each other, so the equality checks them.
    let after_scroll = lines
        .iter()
        .find(|line| line["event"] == "sidecar.vt.viewport")
        .unwrap()["fields"]["sequence"]
        .as_u64()
        .unwrap();
    let hashes_after: std::collections::HashSet<&String> = stored
        .iter()
        .filter(|(sequence, _)| *sequence >= after_scroll)
        .map(|(_, hash)| hash)
        .collect();
    assert!(hashes_after.len() >= 3, "{hashes_after:?}");
}

/// An event of the session as the replay executable reads it.
fn event(name: &str, fields: Value) -> Value {
    serde_json::json!({
        "ts_us": 1, "mono_ns": 1, "seq": 1, "level": "info", "layer": "sidecar",
        "event": name, "session": "s1", "fields": fields,
    })
}

/// The event name of a replay input kind of the contract.
fn event_of_kind(kind: &str) -> &'static str {
    match kind {
        "output" => "sidecar.vt.pty_read",
        "write" => "sidecar.vt.pty_write",
        "resize" => "sidecar.vt.resize",
        "selection" => "sidecar.vt.selection",
        "viewport" => "sidecar.vt.viewport",
        "theme" => "sidecar.vt.theme",
        other => panic!("kind {other}"),
    }
}

fn golden_inputs() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vt-core/tests/fixtures/log-contract/replay-inputs.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn an_event_with_a_sequence_of_an_unknown_kind_is_refused() {
    let lines = vec![event(
        "sidecar.vt.pointer",
        serde_json::json!({"sequence": 1, "x": 3}),
    )];
    let (ok, hashes, error) = replay(&lines);
    assert!(!ok);
    assert!(hashes.is_empty());
    assert!(
        error.contains("sidecar.vt.pointer") && error.contains("not a replay input"),
        "{error}"
    );
}

#[test]
fn every_input_that_the_contract_refuses_is_refused_with_its_text() {
    let golden = golden_inputs();
    let invalid = golden["invalid"].as_array().unwrap();
    assert!(invalid.len() >= 15);
    for case in invalid {
        let name = case["name"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        if kind == "pointer" {
            let (ok, _, error) = replay(&[event("sidecar.vt.pointer", case["fields"].clone())]);
            assert!(!ok, "{name}");
            assert!(error.contains("not a replay input"), "{name}: {error}");
            continue;
        }
        let (ok, hashes, error) = replay(&[event(event_of_kind(kind), case["fields"].clone())]);
        assert!(!ok, "{name}");
        assert!(hashes.is_empty(), "{name}");
        assert!(
            error.contains(case["error"].as_str().unwrap()),
            "{name}: {error}"
        );
    }
}

#[test]
fn the_valid_inputs_and_sessions_of_the_contract_are_applied() {
    let golden = golden_inputs();
    for case in golden["sessions"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut lines: Vec<Value> = case["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| {
                event(
                    event_of_kind(input["kind"].as_str().unwrap()),
                    input["fields"].clone(),
                )
            })
            .collect();
        let last = lines.len() as u64;
        lines.push(event(
            "sidecar.vt.frame",
            serde_json::json!({"sequence": last, "rows": 4, "cols": 10, "hash": "0".repeat(64)}),
        ));
        let (ok, hashes, error) = replay(&lines);
        match case["problem"].as_str() {
            None => {
                assert!(ok, "{name}: {error}");
                assert_eq!(hashes.len(), 1, "{name}");
            }
            // The session rules about the grid and the gesture belong to the replay command of core; the executable refuses a
            // session that does not start with a theme and a session with a gap.
            Some(problem)
                if problem.contains("first input of a session is a theme")
                    || problem.contains("the sequence after") =>
            {
                assert!(!ok, "{name}");
            }
            Some(_) => {}
        }
    }
}
