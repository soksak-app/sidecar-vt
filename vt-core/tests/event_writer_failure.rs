// The failures of the event writer itself are events of the file (core docs/spec/logging.md#writers): a write that fails
// loses the lines of its batch, and the writer records the lost range of `seq` as `log.dropped` when writing works again;
// a rotation step that fails is an error record in the file. The test binary limits the size of the files of the process to
// make a write fail with a partial write, as a full disk does; it is a test binary of its own for that reason.
use serde_json::json;
use soksak_sidecar_vt_core::events::{EventLog, Level, Options, read_lines};
use std::path::PathBuf;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(10);

/// The limit of the file size belongs to the process, so the tests of this file run one at a time.
static PROCESS_LIMIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vt-event-failure-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn options(directory: &std::path::Path) -> Options {
    Options {
        queue_events: 64,
        queue_bytes: 1 << 20,
        batch_interval: Duration::from_millis(20),
        rotate_bytes: 1 << 20,
        generations: 2,
        ..Options::service(directory.to_path_buf(), "b1".into())
    }
}

/// Limits the size of a file that this process writes; a write past the limit is a partial write and then the error EFBIG.
fn limit_file_size(bytes: libc::rlim_t) {
    // SAFETY: SIGXFSZ is ignored so that the write fails with an error instead of ending the process.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        let limit = libc::rlimit {
            rlim_cur: bytes,
            rlim_max: libc::RLIM_INFINITY,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
    }
}

#[test]
fn a_failed_write_is_recorded_as_log_dropped_when_writing_works_again() {
    let _serial = PROCESS_LIMIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = directory("write");
    let log = EventLog::start(options(&dir)).unwrap();
    log.flush(DEADLINE).unwrap();
    let written = std::fs::metadata(log.path()).unwrap().len();
    limit_file_size(written + 100);
    assert!(log.emit(
        Level::Info,
        "sidecar.vt.test",
        &json!({"text": "x".repeat(1000)})
    ));
    log.flush(DEADLINE).unwrap();
    limit_file_size(libc::RLIM_INFINITY);
    assert!(log.emit(Level::Info, "sidecar.vt.test", &json!({"text": "after"})));
    log.flush(DEADLINE).unwrap();
    // Every line of the file is whole: the partial write was removed.
    let lines = read_lines(log.path()).unwrap();
    let events: Vec<(u64, String)> = lines
        .iter()
        .map(|line| {
            (
                line["seq"].as_u64().unwrap(),
                line["event"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        events,
        vec![
            (1, "log.open".to_string()),
            (2, "log.policy".to_string()),
            (4, "sidecar.vt.test".to_string()),
            (5, "log.dropped".to_string()),
        ]
    );
    let dropped = &lines[3]["fields"];
    assert_eq!(
        (dropped["from_seq"].clone(), dropped["to_seq"].clone()),
        (json!(3), json!(3))
    );
    assert_eq!(dropped["count"], 1);
    assert!(dropped["bytes"].as_u64().unwrap() > 1000);
    let reason = dropped["reason"].as_str().unwrap();
    assert!(reason.contains("the write failed"), "{reason}");
}

#[cfg(unix)]
#[test]
fn a_failed_rotation_step_is_an_error_record_in_the_file() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = PROCESS_LIMIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = directory("rotation");
    let mut rotating = options(&dir);
    rotating.rotate_bytes = 1500;
    let log = EventLog::start(rotating).unwrap();
    log.flush(DEADLINE).unwrap();
    // The folder refuses new names, so the file cannot move to its first generation.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    for index in 0..10 {
        assert!(log.emit(
            Level::Info,
            "sidecar.vt.test",
            &json!({"index": index, "text": "y".repeat(300)})
        ));
    }
    log.flush(DEADLINE).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let lines = read_lines(log.path()).unwrap();
    let record = lines
        .iter()
        .find(|line| line["event"] == "sidecar.vt.record.error")
        .expect("the failed rotation is an error record");
    assert_eq!(record["level"], "error");
    assert_eq!(record["fields"]["where"], "event writer");
    let text = record["fields"]["text"].as_str().unwrap();
    assert!(text.contains("rotation could not move"), "{text}");
    assert!(text.contains("vt-b1.jsonl"), "{text}");
}
