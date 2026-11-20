// The process query of core docs/spec/sidecars.md#process-identity, judged on the real engine and a real PTY: a process that
// a session of a surface started answers with its surface and its start time, a process that no surface started answers with
// the surface null, and a process of a session answers null after its surface closed and the process was reaped.
// Each wait ends on the line it awaits; the limit of .config/nextest.toml bounds a hung test.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::{Value, json};
use soksak_sidecar_vt_core::events::{EventLog, Options};
use soksak_sidecar_vt_core::protocol::{Engine, make_default_session_port_factory, serve};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};

/// The number of lines a wait reads before it fails. A wait that reads this many lines without its line has failed.
const LINE_LIMIT: usize = 200;

/// The milliseconds since the Unix epoch now.
fn now_ms() -> u64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch");
    u64::try_from(elapsed.as_millis()).expect("the milliseconds fit")
}

/// Reads the lines of the service until the line that `matches` accepts, and returns it. The lines before it are kept for
/// the failure text.
async fn next_matching<R, F>(lines: &mut Lines<BufReader<R>>, what: &str, matches: F) -> Value
where
    R: tokio::io::AsyncRead + Unpin,
    F: Fn(&Value) -> bool,
{
    let mut skipped = Vec::new();
    for _ in 0..LINE_LIMIT {
        let line = lines
            .next_line()
            .await
            .expect("read a line of the service")
            .unwrap_or_else(|| panic!("the service ended before {what}; lines: {skipped:?}"));
        let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
        if matches(&value) {
            return value;
        }
        skipped.push(value);
    }
    panic!("no line of {what} within {LINE_LIMIT} lines; lines: {skipped:?}");
}

/// Sends the process query `request` for `process` and returns its answer, or None when the service gives no answer.
/// The query is followed by a close of a surface that no session holds, whose reply the service sends after every reply
/// to an earlier line of the connection. Reading ends on that reply, so a missing answer fails the assertion instead of
/// blocking the test.
async fn process_answer<W, R>(
    to_serve: &mut W,
    lines: &mut Lines<BufReader<R>>,
    process: u32,
    request: u64,
) -> Option<Value>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
{
    let sentinel = format!("sentinel-{request}");
    to_serve
        .write_all(
            format!(
                "{{\"process\":{process},\"request\":{request}}}\n{{\"surface\":\"{sentinel}\",\"root\":\"/tmp\",\"closed\":true}}\n"
            )
            .as_bytes(),
        )
        .await
        .expect("write the process query and its sentinel");
    let mut answer = None;
    let mut skipped = Vec::new();
    loop {
        let line = lines
            .next_line()
            .await
            .expect("read a line of the service")
            .unwrap_or_else(|| panic!("the service ended before the sentinel; lines: {skipped:?}"));
        let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
        if value["surface"] == json!(sentinel) && value["closed"] == json!(true) {
            return answer;
        }
        if value.get("process") == Some(&json!(process))
            && value.get("request") == Some(&json!(request))
        {
            answer = Some(value);
        } else {
            skipped.push(value);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_query_names_the_surface_and_start_time_of_a_session_process_and_null_after_its_exit()
 {
    let directory = tempfile::tempdir().expect("create the folder of the test");
    // The session program announces its process id through the notice of OSC 9, then stays until its input ends.
    let program = directory.path().join("session-program");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '\\033]9;session-ready %s\\007' \"$$\"\nexec cat\n",
    )
    .expect("write the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make the program executable");
    let options = Options::service(directory.path().join("logs"), "process-query".to_string());
    let log = Arc::new(EventLog::start(options).expect("start the writer"));
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(log.clone());
    let (mut to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(1 << 20);
    let handle = log.clone();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });

    let before = now_ms();
    let opening = format!(
        "{{\"surface\":\"tab-a\",\"root\":\"/tmp\",\"body\":{{\"operation\":\"open\",\"shell\":{},\"image\":\"view\"}}}}\n{{\"surface\":\"tab-a\",\"body\":{{\"image\":{{\"configure\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"width\":800,\"height\":384,\"scale\":1.0}}}}}}}}\n",
        Value::String(program.to_string_lossy().into_owned())
    );
    to_serve
        .write_all(opening.as_bytes())
        .await
        .expect("write the opening of the surface");
    let mut lines = BufReader::new(from_serve).lines();
    let ready = next_matching(&mut lines, "the notice of the session program", |value| {
        value["body"]["event"] == json!("notification")
            && value["body"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("session-ready "))
    })
    .await;
    let after = now_ms();
    let message = ready["body"]["message"]
        .as_str()
        .expect("the notice is text");
    let process: u32 = message["session-ready ".len()..]
        .parse()
        .unwrap_or_else(|error| panic!("the notice names no process id: {message}: {error}"));

    // A process of a session of tab-a answers with its surface and the start time of the process table, which is at most
    // 1 ms before the start of the opening, because the table holds microseconds.
    let answer = process_answer(&mut to_serve, &mut lines, process, 1).await;
    let answer = answer.unwrap_or_else(|| {
        panic!("the service gave no answer to the query of the session process")
    });
    assert_eq!(
        answer["surface"],
        json!("tab-a"),
        "the answer names the surface of the session: {answer}"
    );
    let started = answer["started"]
        .as_u64()
        .unwrap_or_else(|| panic!("the answer has no start time: {answer}"));
    assert!(
        before - 1 <= started && started <= after,
        "the start time {started} is not between {before} and {after}: {answer}"
    );

    // The test process was not started by a surface, so its answer names no surface.
    let own = std::process::id();
    assert_eq!(
        process_answer(&mut to_serve, &mut lines, own, 2).await,
        Some(json!({"process": own, "request": 2, "surface": null})),
        "a process that no surface started names no surface"
    );

    // The close of tab-a ends and reaps the session process, so its later answer names no surface.
    to_serve
        .write_all(b"{\"surface\":\"tab-a\",\"root\":\"/tmp\",\"closed\":true}\n")
        .await
        .expect("write the close of the surface");
    next_matching(&mut lines, "the answer to the close of tab-a", |value| {
        value["surface"] == json!("tab-a") && value["closed"] == json!(true)
    })
    .await;
    assert_eq!(
        process_answer(&mut to_serve, &mut lines, process, 3).await,
        Some(json!({"process": process, "request": 3, "surface": null})),
        "the reaped session process names no surface"
    );

    drop(to_serve);
    task.await
        .expect("the service task ends after its input ends");
}
