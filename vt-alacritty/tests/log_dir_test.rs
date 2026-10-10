//! The service takes the directory of its event files from `--log-dir` and refuses to start without it.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The limit that ends a stalled check. Success is judged by the exit status and the files, not by this time.
const STALL: Duration = Duration::from_secs(60);

fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vt-log-dir-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create the test directory");
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
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start the service");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().expect("service stdout"))
        .read_line(&mut ready)
        .expect("read the ready line");
    assert!(ready.contains("\"socket\""), "no ready line: {ready:?}");
    // SAFETY: the test asks the child process it started to end.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let status = child.wait().expect("wait for the service");
    assert!(status.success(), "{status:?}");
    let files: Vec<_> = std::fs::read_dir(&logs)
        .expect("the service created the log directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("vt-") && name.ends_with(&format!("-{}.jsonl", child.id())),
        "{name}"
    );
    let text = std::fs::read_to_string(&files[0]).expect("read the event file");
    let open: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a first line")).expect("JSON line");
    assert_eq!(open["event"], "log.open");
    assert_eq!(open["fields"]["role"], "vt");
    assert_eq!(open["fields"]["pid"], child.id());
    std::fs::remove_dir_all(&root).expect("remove the test directory");
}
