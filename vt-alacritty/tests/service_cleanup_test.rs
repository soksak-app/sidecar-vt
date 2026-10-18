//! Checks that a persistent service leaves no socket folder when it ends, and that the next service removes the socket folder
//! of a service that was killed.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// The limit of a wait for a service to end. A success is judged by the process status and the folders, not by this time.
const STALL: Duration = Duration::from_secs(60);

/// A service that a check started. Its drop ends the process with SIGTERM, and with SIGKILL when the process does not end
/// within `STALL`. It removes the socket folder that the service named by its endpoint, and reports each failure as `error:`.
struct Service {
    child: Child,
    /// The socket folder of the service, read from the ready line.
    socket_dir: Option<PathBuf>,
}

impl Drop for Service {
    fn drop(&mut self) {
        let pid = self.child.id();
        let mut failures = Vec::new();
        let mut killed = false;
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                // SAFETY: the check asks the child process it started to end.
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                if wait_within(&mut self.child, STALL).is_none() {
                    eprintln!(
                        "error: the service {pid} did not end within {STALL:?} of SIGTERM; it is killed"
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
            Err(error) => failures.push(format!(
                "the state of the service {pid} is unknown: {error}"
            )),
        }
        if let Some(directory) = &self.socket_dir {
            match std::fs::remove_dir_all(directory) {
                // A service that ended by SIGTERM removes its own socket folder, so a folder that remains is a failure.
                // After a SIGKILL the folder is left by the process and is removed here.
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

/// Waits for `child` to end within `limit` and returns its status, or None when it did not end. The standard library offers
/// no wait with a bound, so the wait checks `try_wait` every 10 ms.
fn wait_within(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("check the service") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A folder that a check created. Its drop removes the folder and reports a removal that fails, and it fails the check
/// unless the check is already panicking.
struct Directory(PathBuf);

impl Drop for Directory {
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

/// Starts a service in `service_dir` and returns it with the socket path of its ready line.
fn start(service_dir: &Path) -> (Service, PathBuf) {
    start_with_logs(service_dir, &service_dir.join("logs"))
}

/// Starts a service in `service_dir` that writes its event files to `logs`.
fn start_with_logs(service_dir: &Path, logs: &Path) -> (Service, PathBuf) {
    let child = Command::new(env!("CARGO_BIN_EXE_soksak-vt-alacritty"))
        .arg("--service-dir")
        .arg(service_dir)
        .arg("--log-dir")
        .arg(logs)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the service");
    // The service is owned before the ready line is read, so a failed read still ends the process.
    let mut service = Service {
        child,
        socket_dir: None,
    };
    let stdout = service.child.stdout.take().expect("service stdout");
    let (sent, received) = channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        // The receiver is gone only when the check has ended, and then the line is not used.
        let _ = sent.send(read);
    });
    let line = received
        .recv_timeout(STALL)
        .expect("the service printed no ready line")
        .expect("read the ready line");
    assert!(!line.is_empty(), "the service printed no ready line");
    let endpoint: serde_json::Value = serde_json::from_str(&line).expect("ready endpoint");
    let socket = PathBuf::from(endpoint["socket"].as_str().expect("socket path"));
    service.socket_dir = socket.parent().map(Path::to_path_buf);
    (service, socket)
}

/// Waits for the service to end within `STALL`.
fn wait(service: &mut Service) -> ExitStatus {
    wait_within(&mut service.child, STALL).expect("the service did not end")
}

/// The path of a service folder for a check.
fn service_directory_name() -> PathBuf {
    std::env::temp_dir().join(format!("vt-cleanup-{}-{}", std::process::id(), uuid()))
}

fn service_dir() -> Directory {
    let directory = service_directory_name();
    std::fs::create_dir_all(&directory).expect("create the service directory");
    std::fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("secure the service directory");
    Directory(directory)
}

/// A name fragment that does not repeat within this process. The clock has microsecond resolution, so names made at the
/// same instant repeat, and an increasing number is added.
fn uuid() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{time}-{next}")
}

#[test]
fn a_terminated_service_removes_its_socket_directory() {
    let directory = service_dir();
    let (mut service, socket) = start(&directory.0);
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    assert!(
        socket_dir.exists(),
        "the service did not create {}",
        socket_dir.display()
    );
    // SAFETY: the check asks the child process it started to end.
    assert_eq!(
        unsafe { libc::kill(service.child.id() as i32, libc::SIGTERM) },
        0,
        "SIGTERM failed"
    );
    wait(&mut service);
    let left = socket_dir.exists();
    if left {
        std::fs::remove_dir_all(&socket_dir).expect("remove the socket directory the service left");
    }
    assert!(
        !left,
        "the terminated service left {}",
        socket_dir.display()
    );
    assert!(
        !directory.0.join("endpoint.json").exists(),
        "the terminated service left its endpoint"
    );
}

#[test]
fn a_new_service_removes_the_socket_directory_of_a_killed_one() {
    let directory = service_dir();
    let (mut service, socket) = start(&directory.0);
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    // The check kills the service so that it cannot clean up: this is the crash that the next service recovers from.
    service.child.kill().expect("kill the first service");
    wait(&mut service);
    assert!(
        socket_dir.exists(),
        "the killed service removed {}",
        socket_dir.display()
    );
    let (mut next, _) = start(&directory.0);
    let left = socket_dir.exists();
    if left {
        std::fs::remove_dir_all(&socket_dir).expect("remove the socket directory the service left");
    }
    assert!(!left, "the next service left {}", socket_dir.display());
    // SAFETY: the check asks the child process it started to end.
    assert_eq!(
        unsafe { libc::kill(next.child.id() as i32, libc::SIGTERM) },
        0,
        "SIGTERM failed"
    );
    wait(&mut next);
}

/// The policy that stores the `info` event of the stop, so the check reads the reason of the stop.
const POLICY: &str = r#"{"schema":1,"classes":{"lifecycle":"trace","input":"trace","call":"trace","io":"trace","frame":"trace"}}"#;

/// A folder whose child `logs` receives the event files of a service. The service reads the policy file from this folder.
fn service_dir_with_policy() -> Directory {
    let directory = service_dir();
    std::fs::write(directory.0.join("log-policy.json"), POLICY).expect("write the policy file");
    directory
}

/// The limit within which a service that lost its endpoint ends by itself. Success is judged by the exit status, the socket
/// folder and the recorded reason, not by this time.
const END: Duration = Duration::from_secs(10);

/// Waits within `END` for the service to end by itself, without a signal from the check.
fn ended_by_itself(service: &mut Service) -> ExitStatus {
    wait_within(&mut service.child, END)
        .expect("the service did not end by itself after its endpoint left")
}

/// The reason of the stop that the one event file in `logs` records.
fn stop_reason(logs: &Path) -> String {
    let files: Vec<_> = std::fs::read_dir(logs)
        .expect("read the log folder")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let events = soksak_sidecar_vt_core::events::read_lines(&files[0]).expect("read the events");
    let stop = events
        .iter()
        .find(|event| event["event"] == "sidecar.vt.session_stop")
        .expect("the service records its stop");
    stop["fields"]["reason"]
        .as_str()
        .expect("the reason of the stop")
        .to_string()
}

#[test]
fn a_service_ends_when_its_folder_is_removed() {
    let directory = service_dir();
    let logs = service_dir_with_policy();
    let (mut service, socket) = start_with_logs(&directory.0, &logs.0.join("logs"));
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    std::fs::remove_dir_all(&directory.0).expect("remove the service folder");
    let status = ended_by_itself(&mut service);
    assert!(status.success(), "{status:?}");
    assert!(
        !socket_dir.exists(),
        "the service left {}",
        socket_dir.display()
    );
    let reason = stop_reason(&logs.0.join("logs"));
    assert!(
        reason == "folder-removed" || reason == "endpoint-removed",
        "{reason}"
    );
}

#[test]
fn a_service_ends_when_its_endpoint_is_removed() {
    let directory = service_dir();
    let logs = service_dir_with_policy();
    let (mut service, socket) = start_with_logs(&directory.0, &logs.0.join("logs"));
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    std::fs::remove_file(directory.0.join("endpoint.json")).expect("remove the endpoint");
    let status = ended_by_itself(&mut service);
    assert!(status.success(), "{status:?}");
    assert!(
        !socket_dir.exists(),
        "the service left {}",
        socket_dir.display()
    );
    assert_eq!(stop_reason(&logs.0.join("logs")), "endpoint-removed");
}

#[test]
fn a_service_ends_when_another_endpoint_replaces_its_own_and_keeps_that_endpoint() {
    let directory = service_dir();
    let logs = service_dir_with_policy();
    let (mut service, socket) = start_with_logs(&directory.0, &logs.0.join("logs"));
    let socket_dir = socket.parent().expect("socket directory").to_path_buf();
    // Another service publishes its endpoint by a rename, as the service does.
    let foreign = serde_json::json!({
        "protocol": 1,
        "pid": 1,
        "socket": "/tmp/spv-foreign/service.sock",
        "token": "foreign-token"
    });
    let temporary = directory.0.join("endpoint.json.foreign.tmp");
    std::fs::write(&temporary, foreign.to_string()).expect("write the foreign endpoint");
    std::fs::rename(&temporary, directory.0.join("endpoint.json"))
        .expect("publish the foreign endpoint");
    let status = ended_by_itself(&mut service);
    assert!(status.success(), "{status:?}");
    assert!(
        !socket_dir.exists(),
        "the service left {}",
        socket_dir.display()
    );
    assert_eq!(stop_reason(&logs.0.join("logs")), "endpoint-replaced");
    let kept: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.0.join("endpoint.json")).expect("the foreign endpoint remains"),
    )
    .expect("the foreign endpoint is JSON");
    assert_eq!(kept["token"], "foreign-token");
}

// The service folders of the checks that run in parallel in one process must differ. An equal name makes one check's
// service read the lock or the endpoint of another check and fail (S15).
#[test]
fn service_directories_are_unique_within_the_test_process() {
    // The checks make names from several threads at the same instant.
    let start = std::sync::Arc::new(std::sync::Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                (0..20)
                    .map(|_| service_directory_name())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let names: Vec<PathBuf> = threads
        .into_iter()
        .flat_map(|thread| thread.join().expect("name thread"))
        .collect();
    let unique: std::collections::HashSet<&PathBuf> = names.iter().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "service directory names repeated"
    );
}
