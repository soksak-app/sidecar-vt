// The writer applies the policy file of core (docs/spec/logging.md#policy-file): it reads the file when it starts, learns of a
// change through the file system events without a timer, refuses a file that breaks the rules and keeps the policy in force,
// and states each policy and each count of gated events as events of its own.
use serde_json::{json, Value};
use soksak_sidecar_vt_core::events::{read_lines, EventLog, Events, Level, Options};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(10);

const IO_OFF: &str = r#"{"schema":1,"classes":{"lifecycle":"trace","input":"trace","call":"trace","io":"off","frame":"trace"}}"#;
const FRAME_OFF: &str = r#"{"schema":1,"classes":{"lifecycle":"trace","input":"trace","call":"trace","io":"trace","frame":"off"}}"#;

/// A folder that holds the log folder and the policy file, as `<config-dir>` does.
struct Config {
    directory: tempfile::TempDir,
}

impl Config {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("create the configuration folder"),
        }
    }

    fn policy_file(&self) -> PathBuf {
        self.directory.path().join("log-policy.json")
    }

    /// Writes the policy as the host does: a new file in the same folder, renamed over the path.
    fn write(&self, text: &str) {
        let staging = self.directory.path().join("log-policy.json.new");
        std::fs::write(&staging, text).expect("write the staging file");
        std::fs::rename(&staging, self.policy_file()).expect("rename over the policy file");
    }

    fn start(&self) -> Events {
        let mut options = Options::service(self.directory.path().join("logs"), "p1".into());
        options.policy_file = Some(self.policy_file());
        options.batch_interval = Duration::from_millis(20);
        Arc::new(EventLog::start(options).expect("start the writer"))
    }
}

fn lines(log: &Events) -> Vec<Value> {
    log.flush(DEADLINE).expect("flush the writer");
    read_lines(log.path()).expect("read the events")
}

/// The lines of `event` that the file holds now.
fn of(lines: &[Value], event: &str) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["event"] == event)
        .cloned()
        .collect()
}

/// Waits until the file holds `count` lines of `event`; the writer stores the line when it has applied the change.
fn wait_for(log: &Events, event: &str, count: usize) -> Vec<Value> {
    let started = Instant::now();
    loop {
        let found = of(&lines(log), event);
        if found.len() >= count {
            return found;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "the file holds {} lines of {event}, expected {count}",
            found.len()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn emit_read(log: &Events) -> bool {
    log.emit(
        Level::Debug,
        "sidecar.vt.pty_read",
        &json!({"sequence": 1, "hex": "61"}),
    )
}

#[test]
fn without_a_file_the_default_policy_is_in_force_and_is_stated() {
    let config = Config::new();
    let log = config.start();
    assert!(emit_read(&log));
    let lines = lines(&log);
    let policy = of(&lines, "log.policy");
    assert_eq!(policy.len(), 1, "{lines:?}");
    assert_eq!(policy[0]["fields"]["source"], "default");
    assert_eq!(policy[0]["fields"]["classes"]["io"], "trace");
    assert_eq!(policy[0]["class"], "lifecycle");
    assert_eq!(of(&lines, "sidecar.vt.pty_read").len(), 1);
    assert_eq!(of(&lines, "log.gated").len(), 0);
}

#[test]
fn a_policy_file_at_the_start_is_in_force_and_the_gated_events_are_counted() {
    let config = Config::new();
    config.write(IO_OFF);
    let log = config.start();
    assert!(!emit_read(&log));
    assert!(!emit_read(&log));
    let lines = lines(&log);
    let policy = of(&lines, "log.policy");
    assert_eq!(policy.len(), 1, "{lines:?}");
    assert_eq!(policy[0]["fields"]["source"], "file");
    assert_eq!(policy[0]["fields"]["classes"]["io"], "off");
    assert_eq!(of(&lines, "sidecar.vt.pty_read").len(), 0);
    let gated = of(&lines, "log.gated");
    assert_eq!(gated.len(), 1, "{lines:?}");
    assert_eq!(
        gated[0]["fields"],
        json!({"class": "io", "level": "debug", "count": 2})
    );
    assert_eq!(gated[0]["level"], "info");
    assert_eq!(gated[0]["class"], "lifecycle");
}

#[test]
fn a_line_of_a_declared_event_carries_the_class_of_the_catalog() {
    let config = Config::new();
    let log = config.start();
    assert!(emit_read(&log));
    assert!(log.emit(
        Level::Info,
        "sidecar.vt.session_start",
        &json!({"role": "vt"})
    ));
    let lines = lines(&log);
    assert_eq!(of(&lines, "sidecar.vt.pty_read")[0]["class"], "io");
    assert_eq!(
        of(&lines, "sidecar.vt.session_start")[0]["class"],
        "lifecycle"
    );
    assert_eq!(of(&lines, "log.open")[0]["class"], "lifecycle");
}

#[test]
fn a_change_of_the_file_is_learned_from_the_file_system_events() {
    let config = Config::new();
    let log = config.start();
    wait_for(&log, "log.policy", 1);
    assert!(emit_read(&log));

    config.write(IO_OFF);
    let changed = wait_for(&log, "log.policy", 2);
    assert_eq!(changed[1]["fields"]["source"], "file");
    assert_eq!(changed[1]["fields"]["classes"]["io"], "off");
    assert!(!emit_read(&log));

    // The same policy again changes nothing, and the next change is the third statement.
    config.write(IO_OFF);
    config.write(FRAME_OFF);
    let changed = wait_for(&log, "log.policy", 3);
    assert_eq!(changed.len(), 3, "{changed:?}");
    assert_eq!(changed[2]["fields"]["classes"]["frame"], "off");
    assert!(emit_read(&log));
}

#[test]
fn a_removed_file_puts_the_default_policy_in_force() {
    let config = Config::new();
    config.write(IO_OFF);
    let log = config.start();
    assert!(!emit_read(&log));
    std::fs::remove_file(config.policy_file()).expect("remove the policy file");
    let changed = wait_for(&log, "log.policy", 2);
    assert_eq!(changed[1]["fields"]["source"], "default");
    assert_eq!(changed[1]["fields"]["classes"]["io"], "trace");
    assert!(emit_read(&log));
}

#[test]
fn a_refused_file_is_an_error_record_and_the_policy_in_force_stays() {
    let config = Config::new();
    config.write(IO_OFF);
    let log = config.start();
    config.write(r#"{"schema":2,"classes":{}}"#);
    let errors = wait_for(&log, "sidecar.vt.record.error", 1);
    assert_eq!(errors[0]["fields"]["where"], "log policy");
    let text = errors[0]["fields"]["text"].as_str().unwrap();
    assert!(
        text.ends_with("log-policy.json: schema 2 is not 1"),
        "{text}"
    );
    assert!(text.starts_with('/'), "{text}");
    assert!(!emit_read(&log), "the earlier policy stays in force");
    assert_eq!(of(&lines(&log), "log.policy").len(), 1);
}

#[test]
fn a_file_that_a_writer_is_still_writing_is_waited_on_and_reports_nothing() {
    let config = Config::new();
    let log = config.start();
    wait_for(&log, "log.policy", 1);
    config.write("");
    config.write(" \n");
    config.write(FRAME_OFF);
    let changed = wait_for(&log, "log.policy", 2);
    assert_eq!(changed[1]["fields"]["classes"]["frame"], "off");
    assert_eq!(of(&lines(&log), "sidecar.vt.record.error").len(), 0);
}

#[test]
fn the_service_reads_the_policy_file_in_the_parent_of_its_log_folder() {
    let config = Config::new();
    config.write(IO_OFF);
    let log = EventLog::start_service(&config.directory.path().join("logs")).expect("start");
    let lines = lines(&log);
    let policy = of(&lines, "log.policy");
    assert_eq!(policy.len(), 1, "{lines:?}");
    assert_eq!(policy[0]["fields"]["classes"]["io"], "off");
    assert_eq!(
        Path::new(log.path()).parent(),
        Some(config.directory.path().join("logs").as_path())
    );
}
