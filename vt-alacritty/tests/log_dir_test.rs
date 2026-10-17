//! The service takes the directory of its event files from `--log-dir` and refuses to start without it.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The limit that ends a stalled check. Success is judged by the exit status and the files, not by this time.
const STALL: Duration = Duration::from_secs(60);

/// The configuration folder of a service: its policy file stores every level of every class, so the events below error that
/// the checks read are stored.
fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vt-log-dir-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create the test directory");
    std::fs::write(
        path.join("log-policy.json"),
        r#"{"schema":1,"classes":{"lifecycle":"trace","input":"trace","call":"trace","io":"trace","frame":"trace"}}"#,
    )
    .expect("write the policy file");
    path
}

#[test]
fn a_service_without_log_dir_refuses_to_start_and_says_so() {
    let service = directory("missing");
    let child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--service-dir")
        .arg(&service)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the service");
    let pid = child.id();
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The receiver is gone only when the test already failed.
        let _ = sent.send(child.wait_with_output());
    });
    let output = match received.recv_timeout(STALL) {
        Ok(output) => output.expect("wait for the service"),
        Err(_) => {
            // SAFETY: the test ends the child process it started, which did not end by itself.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            panic!("the service without --log-dir did not end: it started");
        }
    };
    assert!(
        !output.status.success(),
        "the service started without --log-dir"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--log-dir is required"), "{stderr}");
    assert!(
        !service.join("endpoint.json").exists(),
        "a refused service published an endpoint"
    );
    std::fs::remove_dir_all(&service).expect("remove the test directory");
}

#[test]
fn a_service_with_log_dir_writes_log_open_and_flushes_it_when_it_ends() {
    let root = directory("started");
    let logs = root.join("logs");
    let mut child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--log-dir")
        .arg(&logs)
        .arg("--service-dir")
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the service");
    let pid = child.id();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("service stdout"))
        .read_line(&mut ready)
        .expect("read the ready line");
    assert!(ready.contains("\"socket\""), "no ready line: {ready:?}");
    // A client with a wrong token is an error record, and the standard error stays empty.
    let mut rejected = std::os::unix::net::UnixStream::connect(endpoint_socket(&ready))
        .expect("connect to the service");
    writeln!(
        rejected,
        "{}",
        serde_json::json!({"operation": "hello", "protocol": 1, "token": "wrong", "client": "owner-y"})
    )
    .expect("send the hello");
    let mut rejection = String::new();
    BufReader::new(&rejected)
        .read_line(&mut rejection)
        .expect("read the rejection");
    assert!(rejection.contains("\"ok\":false"), "{rejection}");
    // A client that authenticates is the event `client_connect` with its owner.
    let endpoint: serde_json::Value = serde_json::from_str(&ready).expect("ready endpoint");
    let mut client = std::os::unix::net::UnixStream::connect(endpoint["socket"].as_str().unwrap())
        .expect("connect to the service");
    writeln!(
        client,
        "{}",
        serde_json::json!({"operation": "hello", "protocol": 1, "token": endpoint["token"], "client": "owner-x"})
    )
    .expect("send the hello");
    let mut reply = String::new();
    BufReader::new(&client)
        .read_line(&mut reply)
        .expect("read the hello reply");
    assert!(reply.contains("\"ok\":true"), "{reply}");
    // SAFETY: the test asks the child process it started to end.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
    let output = child.wait_with_output().expect("wait for the service");
    let status = output.status;
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "the standard error holds only what the process or the system writes"
    );
    assert!(status.success(), "{status:?}");
    let files: Vec<_> = std::fs::read_dir(&logs)
        .expect("the service created the log directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("vt-") && name.ends_with(&format!("-{}.jsonl", pid)),
        "{name}"
    );
    let text = std::fs::read_to_string(&files[0]).expect("read the event file");
    let open: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a first line")).expect("JSON line");
    assert_eq!(open["event"], "log.open");
    assert_eq!(open["fields"]["role"], "vt");
    assert_eq!(open["fields"]["pid"], pid);
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    let start = events
        .iter()
        .find(|event| event["event"] == "sidecar.vt.session_start")
        .expect("the service records its start");
    assert_eq!(start["level"], "info");
    let rejected = events
        .iter()
        .find(|event| event["event"] == "sidecar.vt.record.error")
        .expect("the service records the failed authentication");
    assert_eq!(rejected["fields"]["where"], "client authentication");
    assert_eq!(
        rejected["fields"]["text"],
        "authentication or protocol mismatch"
    );
    let connect = events
        .iter()
        .find(|event| event["event"] == "sidecar.vt.client_connect")
        .expect("the service records the client that connected");
    assert_eq!(connect["fields"]["owner"], "owner-x");
    // The stop names the signal that asked for it, and the end follows it.
    let names: Vec<&str> = events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    let stop = names
        .iter()
        .position(|name| *name == "sidecar.vt.session_stop")
        .expect("the service records its stop");
    let end = names
        .iter()
        .position(|name| *name == "sidecar.vt.session_end")
        .expect("the service records its end");
    assert!(stop < end, "{names:?}");
    assert_eq!(events[stop]["fields"]["reason"], "SIGTERM");
    assert_eq!(events[end]["fields"]["ok"], true);
    std::fs::remove_dir_all(&root).expect("remove the test directory");
}

/// Starts the service and returns it with its ready line.
fn start(root: &std::path::Path) -> (std::process::Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--log-dir")
        .arg(root.join("logs"))
        .arg("--service-dir")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the service");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("service stdout"))
        .read_line(&mut ready)
        .expect("read the ready line");
    assert!(ready.contains("\"socket\""), "no ready line: {ready:?}");
    (child, ready)
}

/// Connects a client that authenticates as `owner`.
fn connect(ready: &str, owner: &str) -> std::os::unix::net::UnixStream {
    let endpoint: serde_json::Value = serde_json::from_str(ready).expect("ready endpoint");
    let mut client = std::os::unix::net::UnixStream::connect(endpoint["socket"].as_str().unwrap())
        .expect("connect to the service");
    writeln!(
        client,
        "{}",
        serde_json::json!({"operation": "hello", "protocol": 1, "token": endpoint["token"], "client": owner})
    )
    .expect("send the hello");
    let mut reply = String::new();
    BufReader::new(&client)
        .read_line(&mut reply)
        .expect("read the hello reply");
    assert!(reply.contains("\"ok\":true"), "{reply}");
    client
}

/// The events of the one event file of the service in `root`, after the service ended.
fn events_of(root: &std::path::Path) -> Vec<serde_json::Value> {
    let files: Vec<_> = std::fs::read_dir(root.join("logs"))
        .expect("the service created the log directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    soksak_sidecar_vt_core::events::read_lines(&files[0]).expect("read the event file")
}

#[test]
fn a_service_stopped_by_sigint_records_the_signal() {
    let root = directory("sigint");
    let (child, _ready) = start(&root);
    // SAFETY: the test asks the child process it started to end.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().expect("wait for the service");
    assert!(output.status.success(), "{:?}", output.status);
    let events = events_of(&root);
    let stop = events
        .iter()
        .find(|event| event["event"] == "sidecar.vt.session_stop")
        .expect("the service records its stop");
    assert_eq!(stop["fields"]["reason"], "SIGINT");
    assert_eq!(stop["fields"]["clients"], 0);
    std::fs::remove_dir_all(&root).expect("remove the test directory");
}

// A client that sends `shutdown` and leaves stops the service when no client remains: the disconnect of the client and the
// stop of the service are events, in this order.
#[test]
fn a_client_that_disconnects_and_a_shutdown_are_events() {
    let root = directory("shutdown");
    let (child, ready) = start(&root);
    let mut client = connect(&ready, "owner-z");
    writeln!(
        client,
        "{}",
        serde_json::json!({"operation": "shutdown", "request": "r1"})
    )
    .expect("send the shutdown request");
    let mut reply = String::new();
    BufReader::new(&client)
        .read_line(&mut reply)
        .expect("read the shutdown reply");
    assert!(reply.contains("\"ok\":true"), "{reply}");
    let output = child.wait_with_output().expect("wait for the service");
    assert!(output.status.success(), "{:?}", output.status);
    let events = events_of(&root);
    let names: Vec<&str> = events
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    let disconnect = names
        .iter()
        .position(|name| *name == "sidecar.vt.client_disconnect")
        .expect("the service records the client that disconnected");
    let stop = names
        .iter()
        .position(|name| *name == "sidecar.vt.session_stop")
        .expect("the service records its stop");
    assert!(disconnect < stop, "{names:?}");
    assert_eq!(events[disconnect]["fields"]["owner"], "owner-z");
    assert_eq!(events[disconnect]["fields"]["ok"], true);
    assert_eq!(events[stop]["fields"]["reason"], "shutdown");
    assert_eq!(events[stop]["fields"]["clients"], 0);
    std::fs::remove_dir_all(&root).expect("remove the test directory");
}

/// The socket path of the ready line.
fn endpoint_socket(ready: &str) -> String {
    let endpoint: serde_json::Value = serde_json::from_str(ready).expect("ready endpoint");
    endpoint["socket"]
        .as_str()
        .expect("socket path")
        .to_string()
}
