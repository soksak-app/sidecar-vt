// The `open` request with `program` and `args` (core docs/spec/agent-roles.md#launch-parameters), judged on the real engine and a
// real PTY: a program named by its path or by its name in PATH starts with its arguments and without a shell, and each invalid
// value is refused with an explicit error. Each wait ends on the first line that answers it, a notice or an error, so a refused
// request fails the assertion instead of blocking the test.
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

/// The number of lines a wait reads before it fails.
const LINE_LIMIT: usize = 200;

/// A running service whose input and output are the two ends of the test.
struct Service {
    to_serve: tokio::io::DuplexStream,
    lines: Lines<BufReader<tokio::io::DuplexStream>>,
    task: tokio::task::JoinHandle<()>,
    _directory: tempfile::TempDir,
}

/// Starts the service with the real engine and a real PTY in a folder of its own.
fn start_service() -> Service {
    let directory = tempfile::tempdir().expect("create the folder of the test");
    let options = Options::service(directory.path().join("logs"), "open-program".to_string());
    let log = Arc::new(EventLog::start(options).expect("start the writer"));
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let factory = make_default_session_port_factory(log.clone());
    let (to_serve, serve_in) = tokio::io::duplex(1 << 20);
    let (serve_out, from_serve) = tokio::io::duplex(1 << 20);
    let task = tokio::spawn(async move {
        let _ = serve(engine_factory, serve_in, serve_out, factory, log).await;
    });
    Service {
        to_serve,
        lines: BufReader::new(from_serve).lines(),
        task,
        _directory: directory,
    }
}

impl Service {
    /// Writes the lines of the host.
    async fn send(&mut self, lines: &[Value]) {
        let mut text = String::new();
        for line in lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        self.to_serve
            .write_all(text.as_bytes())
            .await
            .expect("write the lines of the host");
    }

    /// Reads the lines of the service until the first line that is a notice of the session or an error reply, and returns it.
    async fn next_answer(&mut self) -> Value {
        next_answer(&mut self.lines).await
    }

    /// Ends the input and waits for the service to end.
    async fn end(self) {
        drop(self.to_serve);
        self.task
            .await
            .expect("the service task ends after its input ends");
    }
}

/// Reads the lines of a service until the first line that is a notice of a session or an error reply, and returns it.
async fn next_answer<R: tokio::io::AsyncRead + Unpin>(lines: &mut Lines<BufReader<R>>) -> Value {
    let mut skipped = Vec::new();
    for _ in 0..LINE_LIMIT {
        let line = lines
            .next_line()
            .await
            .expect("read a line of the service")
            .unwrap_or_else(|| panic!("the service ended before its answer; lines: {skipped:?}"));
        let value: Value = serde_json::from_str(&line).expect("a line of the service is JSON");
        if value["body"]["event"] == json!("notification") || value["body"]["error"].is_string() {
            return value;
        }
        skipped.push(value);
    }
    panic!("no answer within {LINE_LIMIT} lines; lines: {skipped:?}");
}

/// The time limit of one step of a child service: the wait for a reply or for the end of the service process.
const STEP_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A persistent service that runs as a child process of the test, with its own environment. The service publishes its endpoint
/// on its ready line, and the test connects to the socket of that endpoint and sends the hello of the persistent protocol
/// (core docs/spec/persistent-services.md#service-transport).
struct ChildService {
    child: tokio::process::Child,
    stdout: Lines<BufReader<tokio::process::ChildStdout>>,
    lines: Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

/// Starts the service binary of the crate as a child process whose `HOME` is `home`, with the folders of `root`, and says hello.
async fn start_child_service(home: &std::path::Path, root: &std::path::Path) -> ChildService {
    let service_dir = root.join("service");
    let log_dir = root.join("logs");
    std::fs::create_dir_all(&service_dir).expect("create the service folder");
    std::fs::create_dir_all(&log_dir).expect("create the log folder");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_sok-vt-alacritty"))
        .arg("--service-dir")
        .arg(&service_dir)
        .arg("--log-dir")
        .arg(&log_dir)
        .env("HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("start the service binary");
    let stdout = child
        .stdout
        .take()
        .expect("the output of the service is piped");
    let mut stdout = BufReader::new(stdout).lines();
    let ready = stdout
        .next_line()
        .await
        .expect("read the ready line of the service")
        .expect("the service ended before its ready line");
    let endpoint: Value = serde_json::from_str(&ready).expect("the ready line is JSON");
    let socket = endpoint["socket"]
        .as_str()
        .unwrap_or_else(|| panic!("the ready line names no socket: {ready}"));
    let token = endpoint["token"]
        .as_str()
        .unwrap_or_else(|| panic!("the ready line names no token: {ready}"));
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .expect("connect to the socket of the service");
    let (read, mut writer) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({"operation": "hello", "protocol": 1, "token": token, "client": root.to_string_lossy()});
    writer
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .expect("write the hello of the service");
    let answer = lines
        .next_line()
        .await
        .expect("read the hello answer")
        .expect("the service ended before its hello answer");
    let answer: Value = serde_json::from_str(&answer).expect("the hello answer is JSON");
    assert_eq!(
        answer["ok"],
        json!(true),
        "the service accepts the hello: {answer}"
    );
    ChildService {
        child,
        stdout,
        lines,
        writer,
    }
}

impl ChildService {
    /// Writes the lines of the host.
    async fn send(&mut self, lines: &[Value]) {
        let mut text = String::new();
        for line in lines {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        self.writer
            .write_all(text.as_bytes())
            .await
            .expect("write the lines of the host");
    }

    /// Reads the lines of the service until the first notice or error reply, and returns it within the time limit of a step.
    async fn next_answer(&mut self) -> Value {
        tokio::time::timeout(STEP_LIMIT, next_answer(&mut self.lines))
            .await
            .expect("the service answers within the time limit of a step")
    }

    /// Sends one request and reads the lines until its reply, which must report success. Notices of the sessions may come
    /// before the reply. The reply names its operation in `reply`, which is `closed-owner` for `close-owner`.
    async fn request_ok(&mut self, operation: &str, reply: &str) {
        self.send(&[json!({"operation": operation, "request": format!("end-{operation}")})])
            .await;
        let answer = tokio::time::timeout(STEP_LIMIT, async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .expect("read the answer of the service")
                    .unwrap_or_else(|| panic!("the service ended before the {operation} answer"));
                let value: Value =
                    serde_json::from_str(&line).expect("a line of the service is JSON");
                if value["operation"] == json!(reply) {
                    break value;
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("the service answers {operation} within the time limit of a step")
        });
        assert_eq!(
            answer["ok"],
            json!(true),
            "the service accepts {operation}: {answer}"
        );
    }

    /// Drops the connection of the client without a close-owner, so the service sees no client, and returns the service process.
    fn close_client(self) -> tokio::process::Child {
        let ChildService {
            child,
            stdout,
            lines,
            writer,
        } = self;
        drop(writer);
        drop(lines);
        drop(stdout);
        child
    }

    /// Closes the owner and shuts the service down, as the host stops a persistent service, then waits for the service process to
    /// end with success (core docs/spec/host-contract.md, sidecars-transport.stop.close-owner-then-shutdown).
    async fn end(mut self) {
        self.request_ok("close-owner", "closed-owner").await;
        self.request_ok("shutdown", "shutdown").await;
        drop(self.writer);
        drop(self.lines);
        let status = tokio::time::timeout(STEP_LIMIT, self.child.wait())
            .await
            .expect("the service process ends within the time limit of a step")
            .expect("wait for the service process");
        assert!(status.success(), "the service process ends with {status}");
        drop(self.stdout);
    }
}

/// The open line and the configure line of a surface that draws its screen through the image `view`.
fn open_lines(open: Value) -> Vec<Value> {
    let mut body = open;
    body["operation"] = json!("open");
    body["image"] = json!("view");
    vec![
        json!({"surface": "tab-a", "root": "/tmp", "body": body}),
        json!({"surface": "tab-a", "body": {"image": {"configure": {"name": "view", "generation": 1, "raster": 1, "width": 800, "height": 384, "scale": 1.0}}}}),
    ]
}

/// Writes a program that prints its argument count and its arguments as the notice `argv-<count>-<arguments>`, then stays until its
/// input ends, and returns its path.
fn argument_program(directory: &std::path::Path) -> std::path::PathBuf {
    let program = directory.join("argument-program");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '\\033]9;argv-%s-%s\\007' \"$#\" \"$*\"\nexec cat\n",
    )
    .expect("write the program");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make the program executable");
    program
}

/// The notice text of a notice answer.
fn notice(answer: &Value) -> String {
    answer["body"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("the answer is not a notice: {answer}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_program_named_by_its_path_starts_with_its_arguments_and_no_shell() {
    let mut service = start_service();
    let directory = tempfile::tempdir().expect("create the folder of the program");
    let program = argument_program(directory.path());
    let open = json!({"program": program.to_string_lossy(), "args": ["one", "two"]});
    service.send(&open_lines(open)).await;
    let answer = service.next_answer().await;
    assert_eq!(
        notice(&answer),
        "argv-2-one two",
        "the program received its arguments: {answer}"
    );
    service.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_program_named_by_its_name_resolves_through_path() {
    let mut service = start_service();
    // `sh` is found through the PATH of the test process; the script receives the two arguments after its name.
    let script = "printf '\\033]9;args-%s-%s\\007' \"$1\" \"$2\"; exec cat";
    let open = json!({"program": "sh", "args": ["-c", script, "sh", "x", "y"]});
    service.send(&open_lines(open)).await;
    let answer = service.next_answer().await;
    assert_eq!(
        notice(&answer),
        "args-x-y",
        "the program named in PATH received its arguments: {answer}"
    );
    service.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relative_program_path_resolves_from_the_directory_of_the_session() {
    let mut service = start_service();
    // The program exists only in the directory of the session, which is not the working directory of the sidecar.
    let directory = tempfile::tempdir().expect("create the directory of the session");
    argument_program(directory.path());
    let open = json!({
        "program": "./argument-program",
        "args": ["one"],
        "directory": directory.path().to_string_lossy(),
    });
    service.send(&open_lines(open)).await;
    let answer = service.next_answer().await;
    assert_eq!(
        notice(&answer),
        "argv-1-one",
        "the relative path names the program of the directory: {answer}"
    );
    service.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_program_that_the_path_does_not_contain_fails_the_open_with_its_name() {
    let mut service = start_service();
    let open = json!({"program": "no-such-program-of-the-sidecar-test"});
    service.send(&open_lines(open)).await;
    let answer = service.next_answer().await;
    assert_eq!(
        answer["body"]["error"],
        json!("openFailed"),
        "the open fails: {answer}"
    );
    assert_eq!(
        answer["body"]["reason"],
        json!("program not found: no-such-program-of-the-sidecar-test"),
        "the reason names the program: {answer}"
    );
    service.end().await;
}

/// Sends one open line without a surface image and returns the first error reply to it.
async fn refused(body: Value) -> Value {
    let mut service = start_service();
    let mut open = body;
    open["operation"] = json!("open");
    service
        .send(&[json!({"surface": "tab-a", "root": "/tmp", "body": open})])
        .await;
    let answer = service.next_answer().await;
    service.end().await;
    answer
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_invalid_open_value_is_refused_with_an_explicit_error() {
    let cases = [
        (
            json!({"program": ""}),
            "invalidParams",
            "open program must be a non-empty text",
        ),
        (
            json!({"program": "sh", "args": ["ok", 7]}),
            "invalidParams",
            "open args must be an array of texts",
        ),
        (
            json!({"program": "sh", "args": "ok"}),
            "invalidParams",
            "open args must be an array of texts",
        ),
        (
            json!({"args": ["ok"], "shell": "login"}),
            "invalidParams",
            "open args require program",
        ),
        (
            json!({"program": "sh", "shell": "login"}),
            "invalidParams",
            "open program and shell are both set",
        ),
    ];
    for (body, error, reason) in cases {
        let answer = refused(body.clone()).await;
        assert_eq!(
            answer["body"]["error"],
            json!(error),
            "the refusal of {body}: {answer}"
        );
        assert_eq!(
            answer["body"]["reason"],
            json!(reason),
            "the reason of {body}: {answer}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relative_program_without_directory_resolves_from_home() {
    // The service runs with HOME set to the folder that holds the program. The test process keeps its own HOME, and the
    // working directory of the service is the crate folder, where the program does not exist.
    let root = tempfile::tempdir().expect("create the folder of the test");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("create the home folder");
    argument_program(&home);
    let mut service = start_child_service(&home, root.path()).await;
    service
        .send(&open_lines(
            json!({"program": "./argument-program", "args": ["one"]}),
        ))
        .await;
    let answer = service.next_answer().await;
    assert_eq!(
        notice(&answer),
        "argv-1-one",
        "the relative path names the program of HOME: {answer}"
    );
    service.end().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shutdown_with_a_live_session_ends_the_service_once_the_client_leaves() {
    // The service stays running with the session of the client, and the shutdown request is sent without close-owner. The
    // service exits once no client remains, so the disconnect of the client ends the service, and its live session does not
    // hold the connection (core docs/spec/persistent-services.md#service-transport).
    let root = tempfile::tempdir().expect("create the folder of the test");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("create the home folder");
    argument_program(&home);
    let mut service = start_child_service(&home, root.path()).await;
    service
        .send(&open_lines(
            json!({"program": "./argument-program", "args": ["one"]}),
        ))
        .await;
    assert_eq!(
        notice(&service.next_answer().await),
        "argv-1-one",
        "the session starts its program"
    );
    service.request_ok("shutdown", "shutdown").await;
    let mut child = service.close_client();
    let status = tokio::time::timeout(STEP_LIMIT, child.wait())
        .await
        .expect("the service exits within the time limit of a step once its client has left")
        .expect("wait for the service process");
    assert!(status.success(), "the service process ends with {status}");
}
