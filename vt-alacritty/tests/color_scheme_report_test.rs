// The color scheme notification of DEC private mode 2031 (docs/spec/terminal-protocols.md#vendor-contracts) through the
// service with the real engine and a real PTY: a program sets the mode, the page changes the theme, and the program that
// reads the report asks OSC 11 again and receives the new background.
#[path = "../src/engine.rs"]
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::events::{EventLog, Options, read_run_lines};
use soksak_sidecar_vt_core::protocol::{Engine, make_default_session_port_factory, serve};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Writes the policy file of the folder that stores every level of every class: the test reads the input events below
/// error, which the default policy refuses.
fn all_levels_policy(directory: &std::path::Path) -> std::path::PathBuf {
    let file = directory.join("log-policy.json");
    std::fs::write(
        &file,
        r#"{"schema":1,"classes":{"lifecycle":"trace","input":"trace","call":"trace","io":"trace","frame":"trace"}}"#,
    )
    .expect("write the policy file");
    file
}

/// The program of the session. It sets mode 2031, waits for the nine bytes of a color scheme report, then asks the
/// background with OSC 11, as a program that repaints on the report does, and reads its input until the session ends.
const PROGRAM: &str = "#!/bin/sh\nstty raw -echo\nprintf '\\033[?2031h'\ndd bs=1 count=9 >/dev/null 2>&1\nprintf '\\033]11;?\\033\\\\'\nexec cat >/dev/null\n";

/// The answer of the host that it presented image `image` of the view, which the configure of the session declares with
/// generation 1 and raster 1.
fn consumed(image: u64) -> String {
    format!(
        "{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"consumed\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"sequence\":{image}}}}}}}}}\n"
    )
}

fn events<'a>(lines: &'a [Value], name: &str) -> Vec<&'a Value> {
    lines.iter().filter(|line| line["event"] == name).collect()
}

/// Reads the events until `done` holds for them or `limit` passes, and answers each new image as the host does, so the
/// next draw is released. The service writes no event to its page for a write to the program, so the test reads the
/// `sidecar.vt.pty_write` records of the event file, which hold the bytes of each write. The result is judged by the
/// events that the caller asserts; the limit only ends the wait of a run whose program never answers.
async fn read_until(
    log: &EventLog,
    to_serve: &mut tokio::io::DuplexStream,
    answered: &mut u64,
    limit: Duration,
    done: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let started = std::time::Instant::now();
    loop {
        log.flush(Duration::from_secs(10))
            .expect("flush the writer");
        let lines = read_run_lines(log.path()).expect("read the events");
        let image = events(&lines, "sidecar.vt.frame")
            .iter()
            .filter_map(|line| line["fields"]["image"].as_u64())
            .max()
            .unwrap_or(0);
        if image > *answered {
            *answered = image;
            to_serve
                .write_all(consumed(image).as_bytes())
                .await
                .unwrap();
        }
        if done(&lines) || started.elapsed() > limit {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

const SET_MODE_HEX: &str = "1b5b3f3230333168";
const LIGHT_REPORT_HEX: &str = "1b5b3f3939373b326e";
/// `ESC ] 11 ; rgb:f7f7/f7f7/f5f5 ESC \`, the answer with the background of the light theme of the test.
const LIGHT_BACKGROUND_HEX: &str = "1b5d31313b7267623a663766372f663766372f663566351b5c";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_theme_change_reports_the_scheme_before_the_program_asks_the_new_background() {
    let directory = tempfile::tempdir().expect("create the folder of the test");
    let program = directory.path().join("scheme-program");
    std::fs::write(&program, PROGRAM).expect("write the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make the program executable");
    let mut options = Options::service(directory.path().join("logs"), "scheme".to_string());
    options.policy_file = Some(all_levels_policy(directory.path()));
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
    let drain = tokio::spawn(async move {
        let mut lines = BufReader::new(from_serve).lines();
        while lines.next_line().await.unwrap().is_some() {}
    });
    let mut answered = 0u64;
    let opening = format!(
        "{{\"surface\":\"s1\",\"root\":\"/tmp\",\"body\":{{\"operation\":\"open\",\"shell\":{},\"image\":\"view\"}}}}\n{{\"surface\":\"s1\",\"body\":{{\"image\":{{\"configure\":{{\"name\":\"view\",\"generation\":1,\"raster\":1,\"width\":800,\"height\":384,\"scale\":1.0}}}}}}}}\n",
        Value::String(program.to_string_lossy().into_owned())
    );
    to_serve.write_all(opening.as_bytes()).await.unwrap();
    let lines = read_until(
        &log,
        &mut to_serve,
        &mut answered,
        Duration::from_secs(20),
        |lines| {
            events(lines, "sidecar.vt.pty_read").iter().any(|line| {
                line["fields"]["hex"]
                    .as_str()
                    .is_some_and(|hex| hex.contains(SET_MODE_HEX))
            })
        },
    )
    .await;
    assert!(
        events(&lines, "sidecar.vt.pty_read")
            .iter()
            .any(|line| line["fields"]["hex"]
                .as_str()
                .is_some_and(|hex| hex.contains(SET_MODE_HEX))),
        "the program did not set mode 2031: {:?}",
        events(&lines, "sidecar.vt.pty_read")
    );

    let theme = r##"{"surface":"s1","body":{"operation":"theme","mode":"light","background":"#f7f7f5","foreground":"#242424","cursor":"#242424","selection":"#c8ccd8"}}
"##;
    to_serve.write_all(theme.as_bytes()).await.unwrap();
    let lines = read_until(
        &log,
        &mut to_serve,
        &mut answered,
        Duration::from_secs(10),
        |lines| {
            events(lines, "sidecar.vt.pty_write").iter().any(|line| {
                line["fields"]["hex"]
                    .as_str()
                    .is_some_and(|hex| hex.contains("1b5d31313b"))
            })
        },
    )
    .await;
    let themes = events(&lines, "sidecar.vt.theme");
    assert_eq!(themes.len(), 2, "the start theme and the light theme");
    let theme_sequence = themes[1]["fields"]["sequence"].as_u64().unwrap();
    let mut writes: Vec<(u64, &str)> = events(&lines, "sidecar.vt.pty_write")
        .iter()
        .map(|line| {
            (
                line["fields"]["sequence"].as_u64().unwrap(),
                line["fields"]["hex"].as_str().unwrap(),
            )
        })
        .filter(|(sequence, _)| *sequence > theme_sequence)
        .collect();
    writes.sort();
    let written: Vec<&str> = writes.iter().map(|(_, hex)| *hex).collect();
    assert_eq!(
        written,
        vec![LIGHT_REPORT_HEX, LIGHT_BACKGROUND_HEX],
        "after the theme input the program receives the light report, then the light background in answer to its OSC 11"
    );

    drop(to_serve);
    drain.await.unwrap();
    task.await.unwrap();
    drop(directory);
}
