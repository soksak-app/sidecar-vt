// The wheel routes of core docs/spec/terminal-read.md#wheel-input through the real engine, the wheel route of the service, a real
// PTY and a real program (S216). Each case of the shared cases (`tests/fixtures/terminal-read-wheel-routes.json`, the copy of core
// `docs/spec/cases/terminal-read/wheel-routes.json`) is the output of a program, the modes of the engine after it, and the scrolls
// with the exact text that the session receives for each. The program of the session writes the output, an OSC 9 notice that
// the engine has read it, and then the hexadecimal bytes that it read before each `Z` as an OSC 9 notice, so each wait ends on
// the notice that it awaits; the limit of .config/nextest.toml bounds a hung test.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::{Value, json};
use soksak_sidecar_vt_core::events::{EventLog, Options};
use soksak_sidecar_vt_core::protocol::{Engine, make_default_session_port_factory, serve};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

const CASES: &str = include_str!("fixtures/terminal-read-wheel-routes.json");

/// The notice that the program writes after the output of the case, so the test knows that the engine has read the output.
const READY_NOTICE: &str = "wheel-case-ready";
/// The start of the notice that carries the bytes that the program read before the marker `Z`.
const GOT: &str = "got:";

fn cases() -> Vec<Value> {
    let cases: Value = serde_json::from_str(CASES).expect("the shared cases are JSON");
    cases["cases"]
        .as_array()
        .expect("the cases are a list")
        .clone()
}

fn flag(value: &Value, name: &str, case: &str) -> bool {
    value[name]
        .as_bool()
        .unwrap_or_else(|| panic!("the modes of the case {case} state {name}"))
}

/// The modes of the engine after the output of each case are the modes of the case: 1000, 1002 and 1003 replace each other.
#[test]
fn each_shared_case_sets_the_modes_of_its_output() {
    for case in cases() {
        let name = case["name"].as_str().expect("a case has a name");
        let mut engine = AlacrittyEngine::new();
        engine.feed(
            case["output"]
                .as_str()
                .expect("a case has an output")
                .as_bytes(),
        );
        let modes = engine.modes();
        let expected = &case["modes"];
        let found = [
            modes.mouse_click,
            modes.mouse_drag,
            modes.mouse_motion,
            modes.sgr_mouse,
            modes.alternate_scroll,
            modes.alt_screen,
            modes.app_cursor,
        ];
        let wanted = [
            "mouseClick",
            "mouseDrag",
            "mouseMotion",
            "sgrMouse",
            "alternateScroll",
            "altScreen",
            "appCursor",
        ]
        .map(|member| flag(expected, member, name));
        assert_eq!(found, wanted, "the modes of the case {name}");
    }
}

/// The lines of the service and the writer of its requests for one case.
struct Service {
    to_serve: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
}

impl Service {
    async fn send(&mut self, body: Value) {
        let line = json!({"surface": "s1", "body": body}).to_string();
        self.to_serve
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write a request to the service");
    }

    /// Reads lines until the OSC 9 notice whose message satisfies `matches`, and returns that message.
    async fn notice(&mut self, matches: impl Fn(&str) -> bool) -> String {
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .expect("read a line of the service")
                .expect("the service ended before the notice");
            let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
            let body = &value["body"];
            if body["error"].is_string() {
                panic!("the service answered an error: {line}");
            }
            if body["event"] == "notification"
                && let Some(message) = body["message"].as_str()
                && matches(message)
            {
                return message.to_string();
            }
        }
    }
}

/// The hexadecimal text of the bytes of `text`, as the program reports them.
fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// Runs the output of one case in a session, sends each of its scrolls followed by the marker `Z`, and returns the hexadecimal
/// bytes that the program read before each marker.
async fn run_case(case: &Value) -> Vec<String> {
    let directory = tempfile::tempdir().expect("create the folder of the test");
    let output_file = directory.path().join("output");
    std::fs::write(
        &output_file,
        case["output"].as_str().expect("a case has an output"),
    )
    .expect("write the output of the case");
    let program = directory.path().join("wheel-program");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nstty raw -echo\ncat '{}'\nprintf '\\033]9;{READY_NOTICE}\\007'\ngot=\nwhile :; do\n  byte=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n')\n  if [ \"$byte\" = 5a ]; then printf '\\033]9;{GOT}%s\\007' \"$got\"; got=; else got=\"$got$byte\"; fi\ndone\n",
            output_file.display()
        ),
    )
    .expect("write the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make the program executable");
    let options = Options::service(directory.path().join("logs"), "wheel".to_string());
    let log = Arc::new(EventLog::start(options).expect("start the writer"));
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(log.clone());
    let (to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(1 << 20);
    let handle = log.clone();
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, handle).await;
    });
    let mut service = Service {
        to_serve,
        lines: BufReader::new(from_serve).lines(),
    };
    let opening = json!({"surface": "s1", "root": "/tmp", "body": {"operation": "open", "shell": program.to_string_lossy(), "image": "view"}});
    service
        .to_serve
        .write_all(format!("{opening}\n").as_bytes())
        .await
        .expect("write the open request to the service");
    service
        .send(json!({"image": {"configure": {"name": "view", "generation": 1, "raster": 1, "width": 800, "height": 384, "scale": 1.0}}}))
        .await;
    service.notice(|message| message == READY_NOTICE).await;
    let mut received = Vec::new();
    for scroll in case["scrolls"].as_array().expect("a case has scrolls") {
        service
            .send(json!({"operation": "scroll", "lines": scroll["lines"], "col": scroll["col"], "row": scroll["row"], "shift": scroll["shift"]}))
            .await;
        service
            .send(json!({"operation": "input", "actions": [{"type": "insert", "text": "Z", "replacementRange": null, "attributed": false}],
                "preedit": {"text": "", "markedRange": null, "selectedRange": null, "replacementRange": null, "attributed": false}}))
            .await;
        let message = service.notice(|message| message.starts_with(GOT)).await;
        received.push(message[GOT.len()..].to_string());
    }
    let Service {
        to_serve,
        mut lines,
    } = service;
    drop(to_serve);
    let drain = tokio::spawn(async move { while lines.next_line().await.unwrap().is_some() {} });
    task.await.unwrap();
    drain.await.unwrap();
    drop(log);
    drop(directory);
    received
}

/// The session receives the exact text of each scroll of each case: the mouse report of the wheel, the cursor keys of the
/// alternate scroll, or nothing when the scroll moves the viewport.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_shared_case_writes_the_text_of_each_scroll_to_the_session() {
    for case in cases() {
        let name = case["name"].as_str().expect("a case has a name");
        let expected: Vec<String> = case["scrolls"]
            .as_array()
            .expect("a case has scrolls")
            .iter()
            .map(|scroll| scroll["write"].as_str().map(hex).unwrap_or_default())
            .collect();
        assert_eq!(
            run_case(&case).await,
            expected,
            "the text that the session received in the case {name}"
        );
    }
}
