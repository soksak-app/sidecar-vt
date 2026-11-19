// The terminal state of core docs/spec/terminal-state.md through the service with the real engine and a real PTY. Each
// case of the shared cases (`tests/fixtures/terminal-state-osc133.json`, the copy of core
// `docs/spec/cases/terminal-state/osc133.json`) is the output of a program in the session; the test reads the surface
// events until the OSC 9 notice that the program writes after the output, then reads the state with `shell.state.read`.
// Each wait ends on the line it awaits; the limit of .config/nextest.toml bounds a hung test.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::events::{EventLog, Options};
use soksak_sidecar_vt_core::protocol::{Engine, make_default_session_port_factory, serve};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CASES: &str = include_str!("fixtures/terminal-state-osc133.json");

/// The notice that the program writes after the output of the case, so the test knows that the engine has read the output.
const END_NOTICE: &str = "terminal-state-case-end";

/// Runs the output of one case in a session and returns the `shell.state` events before the end notice, whether a
/// `sequence.rejected` event came before it, and the reply of `shell.state.read`.
async fn run_case(output: &str) -> (Vec<Value>, bool, Value) {
    let directory = tempfile::tempdir().expect("create the folder of the test");
    let output_file = directory.path().join("output");
    std::fs::write(&output_file, output).expect("write the output of the case");
    let program = directory.path().join("state-program");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nstty raw -echo\ncat '{}'\nprintf '\\033]9;{END_NOTICE}\\007'\nexec cat >/dev/null\n",
            output_file.display()
        ),
    )
    .expect("write the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make the program executable");
    let options = Options::service(directory.path().join("logs"), "state".to_string());
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
    let mut changes = Vec::new();
    let mut rejected = false;
    loop {
        let line = lines
            .next_line()
            .await
            .expect("read a line of the service")
            .expect("the service ended before the end notice");
        let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
        let body = &value["body"];
        match body["event"].as_str() {
            Some("shell.state") => changes.push(body.clone()),
            Some("sequence.rejected") => rejected = true,
            Some("notification") if body["message"] == END_NOTICE => break,
            _ => {}
        }
    }
    to_serve
        .write_all(b"{\"surface\":\"s1\",\"cids\":[\"read\"],\"body\":{\"operation\":\"shell.state.read\"}}\n")
        .await
        .unwrap();
    let reply = loop {
        let line = lines
            .next_line()
            .await
            .expect("read a line of the service")
            .expect("the service ended before the state read reply");
        let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
        let body = &value["body"];
        if body["event"] == "shell.state.current" || body.get("error").is_some() {
            break body.clone();
        }
    };
    drop(to_serve);
    let drain = tokio::spawn(async move { while lines.next_line().await.unwrap().is_some() {} });
    task.await.unwrap();
    drain.await.unwrap();
    drop(log);
    drop(directory);
    (changes, rejected, reply)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_shared_case_gives_its_state_changes_and_its_state_read() {
    let cases: Value = serde_json::from_str(CASES).expect("the shared cases are JSON");
    for case in cases["cases"].as_array().expect("the cases are a list") {
        let name = case["name"].as_str().expect("a case has a name");
        let (changes, rejected, reply) =
            run_case(case["output"].as_str().expect("a case has an output")).await;
        let expected: Vec<Value> = case["events"]
            .as_array()
            .expect("a case has its events")
            .iter()
            .map(|event| {
                let mut event = event.clone();
                event["event"] = Value::from("shell.state");
                event
            })
            .collect();
        assert_eq!(changes, expected, "the change events of the case {name}");
        assert_eq!(
            rejected,
            case["rejected"]
                .as_bool()
                .expect("a case states its rejection"),
            "the rejection of the case {name}"
        );
        let mut read = case["read"].clone();
        read["event"] = Value::from("shell.state.current");
        assert_eq!(reply, read, "the state read of the case {name}");
    }
}
