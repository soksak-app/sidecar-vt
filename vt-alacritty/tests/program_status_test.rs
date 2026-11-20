// The program status of core docs/spec/terminal-state.md#program-status through the real engine and the program status
// tracker. Each case of the shared cases (`tests/fixtures/terminal-state-program-status.json`, the copy of core
// `docs/spec/cases/terminal-state/program-status.json`) runs twice: with the output of each step fed in one piece, and
// fed one byte at a time, so a case does not depend on how a read cuts the output.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::{Value, json};
use soksak_sidecar_vt_core::program_status::ProgramStatusTracker;
use soksak_sidecar_vt_core::{Engine, EngineEvent};

const CASES: &str = include_str!("fixtures/terminal-state-program-status.json");
const SESSION: &str = "session";

struct Outcome {
    events: Vec<Value>,
    read: Value,
    rejected: usize,
    written: Vec<String>,
}

fn run_case(case: &Value, bytewise: bool) -> Outcome {
    let mut engine = AlacrittyEngine::new();
    let mut tracker = ProgramStatusTracker::default();
    let mut outcome = Outcome {
        events: Vec::new(),
        read: Value::Null,
        rejected: 0,
        written: Vec::new(),
    };
    for step in case["steps"].as_array().expect("a case has steps") {
        if let Some(output) = step["output"].as_str() {
            let mut events = Vec::new();
            if bytewise {
                for byte in output.as_bytes() {
                    engine.feed(std::slice::from_ref(byte));
                    events.extend(engine.drain_events());
                }
            } else {
                engine.feed(output.as_bytes());
                events.extend(engine.drain_events());
            }
            for event in events {
                match &event {
                    EngineEvent::Error(_) => outcome.rejected += 1,
                    EngineEvent::PtyWrite(bytes) => outcome
                        .written
                        .push(String::from_utf8(bytes.clone()).expect("a reply is UTF-8")),
                    _ => {
                        if let Some(change) = tracker.apply(SESSION, &event) {
                            outcome.events.push(event_of(change.current.to_json()));
                        }
                    }
                }
            }
        } else if let Some(focused) = step["focus"].as_bool() {
            if let Some(change) = tracker.focus(SESSION, focused) {
                outcome.events.push(event_of(change.current.to_json()));
            }
        } else if step["input"].is_string() {
            if let Some(change) = tracker.typed(SESSION) {
                outcome.events.push(event_of(change.current.to_json()));
            }
        } else {
            panic!("unknown step {step}");
        }
    }
    outcome.read = tracker.current(SESSION).to_json();
    outcome
}

fn event_of(mut body: Value) -> Value {
    body["event"] = json!("program.status");
    body
}

#[test]
fn shared_program_status_cases_run_through_the_engine_and_the_tracker() {
    let cases: Value = serde_json::from_str(CASES).expect("the shared cases are JSON");
    let mut failures = Vec::new();
    for case in cases["cases"].as_array().expect("the file has cases") {
        let name = case["name"].as_str().expect("a case has a name");
        let expected: Vec<&Value> = case["events"]
            .as_array()
            .expect("a case has events")
            .iter()
            .filter(|event| event["event"] == "program.status")
            .collect();
        let written: Vec<&str> = case["written"]
            .as_array()
            .map(|values| values.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for bytewise in [false, true] {
            let outcome = run_case(case, bytewise);
            let label = if bytewise {
                "byte by byte"
            } else {
                "in one piece"
            };
            let events: Vec<&Value> = outcome.events.iter().collect();
            if events != expected {
                failures.push(format!(
                    "{name} ({label}): events {events:?}, expected {expected:?}"
                ));
            }
            if outcome.read != case["read"] {
                failures.push(format!(
                    "{name} ({label}): read {}, expected {}",
                    outcome.read, case["read"]
                ));
            }
            if outcome.rejected as u64
                != case["rejected"]
                    .as_u64()
                    .expect("a case counts its rejections")
            {
                failures.push(format!(
                    "{name} ({label}): {} rejections, expected {}",
                    outcome.rejected, case["rejected"]
                ));
            }
            if outcome.written != written {
                failures.push(format!(
                    "{name} ({label}): written {:?}, expected {written:?}",
                    outcome.written
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A program writes the status query and the primary device attributes request in one write, and takes the answer only
/// before the reply to the request (core docs/spec/terminal-state.md#records).
#[test]
fn the_status_query_is_answered_before_the_device_attributes() {
    let mut engine = AlacrittyEngine::new();
    engine.feed(b"\x1b]7501;?\x07\x1b[c");
    let written: Vec<Vec<u8>> = engine
        .drain_events()
        .into_iter()
        .filter_map(|event| match event {
            EngineEvent::PtyWrite(bytes) => Some(bytes),
            _ => None,
        })
        .collect();
    assert_eq!(written.len(), 2, "two replies: {written:?}");
    assert_eq!(
        written[0], b"\x1b]7501;?\x07",
        "the answer comes first: {written:?}"
    );
    assert!(
        written[1].starts_with(b"\x1b[?"),
        "then the device attributes: {written:?}"
    );
}

mod session {
    //! A real program in a real PTY through the service: it asks the status query and reports the answer in its title,
    //! rings the bell, writes a progress report and an OSC 9 notification, reports a `blocked` record and waits for the
    //! answer of the person, then reports `working`. Each wait ends on the line it awaits; the limit of
    //! .config/nextest.toml bounds a hung test.
    use super::engine::AlacrittyEngine;
    use serde_json::{Value, json};
    use soksak_sidecar_vt_core::events::{EventLog, Options};
    use soksak_sidecar_vt_core::protocol::{Engine, make_default_session_port_factory, serve};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

    const PROGRAM: &str = "#!/bin/sh\nstty raw -echo\nprintf '\\033]7501;?\\007'\nanswer=$(dd bs=1 count=9 2>/dev/null | od -An -tx1 | tr -d ' \\n')\nprintf '\\033]2;answer %s\\007' \"$answer\"\nprintf '\\007'\nprintf '\\033]9;4;1;50\\007'\nprintf '\\033]9;Approval requested\\007'\nprintf '\\033]7501;state=blocked:kind=permission:app=agent\\033\\\\'\ndd bs=1 count=1 >/dev/null 2>&1\nprintf '\\033]7501;state=working:app=agent\\033\\\\'\nexec cat >/dev/null\n";

    async fn next_body(lines: &mut Lines<BufReader<DuplexStream>>) -> Value {
        let line = lines
            .next_line()
            .await
            .expect("read a line of the service")
            .expect("the service ended early");
        let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
        value["body"].clone()
    }

    fn status(records: Value, notice: bool, sequence: u64) -> Value {
        json!({"event": "program.status", "records": records, "notice": notice, "sequence": sequence})
    }

    fn record(state: &str, kind: Option<&str>) -> Value {
        json!([{"id": null, "state": state, "kind": kind, "progress": null, "app": "agent", "title": null, "msg": null}])
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_program_answers_its_query_calls_waits_and_works_again() {
        let directory = tempfile::tempdir().expect("create the folder of the test");
        let program = directory.path().join("agent");
        std::fs::write(&program, PROGRAM).expect("write the program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make the program executable");
        let options = Options::service(directory.path().join("logs"), "status".to_string());
        let log = Arc::new(EventLog::start(options).expect("start the writer"));
        let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
        let factory = make_default_session_port_factory(log.clone());
        let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
        let (serve_out, from_serve) = tokio::io::duplex(1 << 20);
        let handle = log.clone();
        let task = tokio::spawn(async move {
            let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
        });
        let opening = format!(
            "{{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{{\"operation\":\"open\",\"shell\":{},\"image\":\"view\"}}}}\n{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"configure\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"width\":800,\"height\":384,\"scale\":1.0}}}}}}}}\n",
            Value::String(program.to_string_lossy().into_owned())
        );
        to_serve.write_all(opening.as_bytes()).await.unwrap();
        let mut lines = BufReader::new(from_serve).lines();
        let mut seen = Vec::new();
        let mut answer = None;
        // The program answers nothing until the person types, so the blocked record is the last change before it.
        while seen.last() != Some(&status(record("blocked", Some("permission")), true, 2)) {
            let body = next_body(&mut lines).await;
            match body["event"].as_str() {
                Some("title") => answer = body["title"].as_str().map(str::to_string),
                Some("program.status") => seen.push(body),
                Some("notification") => assert_eq!(
                    body["message"], "Approval requested",
                    "a progress report is no notification"
                ),
                Some("sequence.rejected") => panic!("the engine rejected a sequence: {body}"),
                _ => {}
            }
            assert!(seen.len() <= 2, "the changes before the answer: {seen:?}");
        }
        assert_eq!(
            answer.as_deref(),
            Some("answer 1b5d373530313b3f07"),
            "the program read the answer of its query"
        );
        assert_eq!(
            seen[0],
            status(json!([]), true, 1),
            "the bell without the focus is a notice"
        );
        to_serve
            .write_all(b"{\"surface\":\"s1\",\"body\":{\"operation\":\"input\",\"actions\":[{\"type\":\"insert\",\"text\":\"y\",\"replacementRange\":null,\"attributed\":false}],\"preedit\":{\"text\":\"\",\"markedRange\":null,\"selectedRange\":null,\"replacementRange\":null,\"attributed\":false}}}\n")
            .await
            .unwrap();
        let mut after = Vec::new();
        while after.len() < 2 {
            let body = next_body(&mut lines).await;
            if body["event"] == "program.status" {
                after.push(body);
            }
        }
        assert_eq!(
            after,
            vec![
                status(record("blocked", Some("permission")), false, 3),
                status(record("working", None), false, 4)
            ],
            "typing ends the notice, and the answer ends the wait"
        );
        to_serve
            .write_all(b"{\"surface\":\"s1\",\"cids\":[\"read\"],\"body\":{\"operation\":\"program.status.read\"}}\n")
            .await
            .unwrap();
        let reply = loop {
            let body = next_body(&mut lines).await;
            if body["event"] == "program.status.current" || body.get("error").is_some() {
                break body;
            }
        };
        let mut expected = status(record("working", None), false, 4);
        expected["event"] = json!("program.status.current");
        assert_eq!(reply, expected, "the read gives the current status");
        drop(to_serve);
        let drain =
            tokio::spawn(async move { while lines.next_line().await.unwrap().is_some() {} });
        task.await.unwrap();
        drain.await.unwrap();
        drop(log);
        drop(directory);
    }
}
