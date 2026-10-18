//! The service takes the directory of its event files from `--log-dir` and refuses to start without it.

use std::io::{BufRead, BufReader, Write};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

/// The limit that ends a stalled check. Success is judged by the exit status and the files, not by this time.
const STALL: Duration = Duration::from_secs(60);

/// The limit within which a service ends after SIGTERM before the check ends it with SIGKILL.
const END: Duration = Duration::from_secs(10);

/// A folder that a check created. Its drop removes the folder and reports a removal that fails, and it fails the check
/// unless the check is already panicking.
struct Folder(PathBuf);

impl Deref for Folder {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("error: the folder {} remains: {error}", self.0.display());
                if !std::thread::panicking() {
                    panic!("the check left {}", self.0.display());
                }
            }
        }
    }
}

/// The configuration folder of a service: its policy file stores every level of every class, so the events below error that
/// the checks read are stored. The returned folder is removed when it is dropped.
fn directory(name: &str) -> Folder {
    let path = std::env::temp_dir().join(format!("vt-log-dir-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create the test directory");
    std::fs::write(
        path.join("log-policy.json"),
        r#"{"schema":1,"classes":{"lifecycle":"trace","input":"trace","call":"trace","io":"trace","frame":"trace"}}"#,
    )
    .expect("write the policy file");
    Folder(path)
}

/// A service that a check started. Whether the check passes or fails, the drop ends the process, reaps it and removes the
/// socket folder named by its endpoint if the service left it after SIGKILL, and it fails the check when the process or
/// the socket folder remains.
struct Service {
    child: Child,
    /// The socket folder of the service, read from the ready line.
    socket_dir: Option<PathBuf>,
}

impl Service {
    /// The process id of the service.
    fn id(&self) -> u32 {
        self.child.id()
    }

    /// Waits for the service to end by itself within `limit` and returns its status and output.
    fn wait_within(&mut self, limit: Duration) -> Output {
        let status = reaped_within(&mut self.child, limit)
            .unwrap_or_else(|| panic!("the service {} did not end within {limit:?}", self.id()));
        let mut output = Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        if let Some(mut stdout) = self.child.stdout.take() {
            std::io::Read::read_to_end(&mut stdout, &mut output.stdout)
                .expect("read the standard output");
        }
        if let Some(mut stderr) = self.child.stderr.take() {
            std::io::Read::read_to_end(&mut stderr, &mut output.stderr)
                .expect("read the standard error");
        }
        output
    }
}

/// Waits for `child` to end within `limit`. The standard library offers no wait with a bound and no event of the end of a
/// child, so the wait checks `try_wait` every 10 ms.
fn reaped_within(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("check the service") {
            return Some(status);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let pid = self.id();
        let mut failures = Vec::new();
        let mut killed = false;
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                // SAFETY: the check asks the child process it started to end.
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                if reaped_within(&mut self.child, END).is_none() {
                    eprintln!(
                        "error: the service {pid} did not end within {END:?} of SIGTERM; it is killed"
                    );
                    killed = true;
                    if let Err(error) = self
                        .child
                        .kill()
                        .and_then(|()| self.child.wait().map(|_| ()))
                    {
                        failures.push(format!("the service {pid} remains: {error}"));
                    }
                }
            }
            Err(error) => {
                failures.push(format!(
                    "the state of the service {pid} is unknown: {error}"
                ));
            }
        }
        if let Some(directory) = &self.socket_dir {
            match std::fs::remove_dir_all(directory) {
                // A service that ended by SIGTERM removes its own socket folder, so a folder that remains is a failure.
                // After a SIGKILL the process left the folder, and it is removed here.
                Ok(()) if !killed => failures.push(format!(
                    "the service {pid} left its socket folder {}",
                    directory.display()
                )),
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => failures.push(format!(
                    "the socket folder {} remains: {error}",
                    directory.display()
                )),
            }
        }
        for failure in &failures {
            eprintln!("error: {failure}");
        }
        // A second panic during the panic of a failed check aborts the run, so only a passing check panics here.
        if !failures.is_empty() && !std::thread::panicking() {
            panic!("the check left what it started: {failures:?}");
        }
    }
}

#[test]
fn a_service_without_log_dir_refuses_to_start_and_says_so() {
    let root = directory("missing");
    let child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--service-dir")
        .arg(&*root)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the service");
    let mut service = Service {
        child,
        socket_dir: None,
    };
    let output = service.wait_within(STALL);
    assert!(
        !output.status.success(),
        "the service started without --log-dir"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--log-dir is required"), "{stderr}");
    assert!(
        !root.join("endpoint.json").exists(),
        "a refused service published an endpoint"
    );
}

#[test]
fn a_service_with_log_dir_writes_log_open_and_flushes_it_when_it_ends() {
    let root = directory("started");
    let logs = root.join("logs");
    let (mut service, ready) = start(&root);
    let pid = service.id();
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
    let output = service.wait_within(STALL);
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
}

/// Starts the service and returns it with its ready line.
fn start(root: &std::path::Path) -> (Service, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--log-dir")
        .arg(root.join("logs"))
        .arg("--service-dir")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the service");
    // The service is owned before the ready line is read, so a failed read still ends the process.
    let mut service = Service {
        child,
        socket_dir: None,
    };
    let mut ready = String::new();
    BufReader::new(service.child.stdout.take().expect("service stdout"))
        .read_line(&mut ready)
        .expect("read the ready line");
    assert!(ready.contains("\"socket\""), "no ready line: {ready:?}");
    let endpoint: serde_json::Value = serde_json::from_str(&ready).expect("ready endpoint");
    let socket = std::path::PathBuf::from(endpoint["socket"].as_str().expect("socket path"));
    service.socket_dir = socket.parent().map(std::path::Path::to_path_buf);
    (service, ready)
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
    let (mut service, _ready) = start(&root);
    // SAFETY: the test asks the child process it started to end.
    assert_eq!(unsafe { libc::kill(service.id() as i32, libc::SIGINT) }, 0);
    let output = service.wait_within(STALL);
    assert!(output.status.success(), "{:?}", output.status);
    let events = events_of(&root);
    let stop = events
        .iter()
        .find(|event| event["event"] == "sidecar.vt.session_stop")
        .expect("the service records its stop");
    assert_eq!(stop["fields"]["reason"], "SIGINT");
    assert_eq!(stop["fields"]["clients"], 0);
}

// A client that sends `shutdown` and leaves stops the service when no client remains: the disconnect of the client and the
// stop of the service are events, in this order.
#[test]
fn a_client_that_disconnects_and_a_shutdown_are_events() {
    let root = directory("shutdown");
    let (mut service, ready) = start(&root);
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
    let output = service.wait_within(STALL);
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
}

/// The socket path of the ready line.
fn endpoint_socket(ready: &str) -> String {
    let endpoint: serde_json::Value = serde_json::from_str(ready).expect("ready endpoint");
    endpoint["socket"]
        .as_str()
        .expect("socket path")
        .to_string()
}

/// Whether the process `pid` exists, a zombie included.
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 checks that the process exists and sends nothing.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

// A check that fails after it started the service ends that service and removes its folder.
#[test]
fn a_failed_check_ends_its_service_and_removes_its_folder() {
    let root = directory("failed");
    let path = root.to_path_buf();
    let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let seen = pid.clone();
    let failed = std::panic::catch_unwind(move || {
        let (service, _ready) = start(&root);
        seen.store(service.id(), std::sync::atomic::Ordering::SeqCst);
        panic!("the check fails after it started the service");
    });
    assert!(failed.is_err());
    let pid = pid.load(std::sync::atomic::Ordering::SeqCst);
    let remained = alive(pid);
    if remained {
        // SAFETY: the test ends the child process it started, which the failed check left.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
    let folder = path.exists();
    if folder {
        std::fs::remove_dir_all(&path).expect("remove the test directory");
    }
    assert!(!remained, "the failed check left its service {pid} running");
    assert!(!folder, "the failed check left {}", path.display());
}
