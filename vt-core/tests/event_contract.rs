// The lines of the event writer satisfy the event contract of core (docs/spec/logging.md). The golden fixtures in
// tests/fixtures/log-contract are copies of the fixtures of the contract; a line that the writer produces has the same
// structure and satisfies the same catalog.
use serde_json::{json, Value};
use soksak_sidecar_vt_core::events::{bytes_fields, EventLog, Level, Options, Subject};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(10);

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/log-contract")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn fixture_lines(name: &str) -> Vec<Value> {
    fixture(name)
        .lines()
        .map(|line| serde_json::from_str(line).expect("a fixture line is JSON"))
        .collect()
}

fn directory(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("vt-event-contract-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn options(directory: &Path) -> Options {
    Options {
        directory: directory.to_path_buf(),
        boot: "b1".into(),
        queue_events: 64,
        queue_bytes: 1 << 20,
        batch_interval: Duration::from_millis(20),
        rotate_bytes: 1 << 20,
        generations: 2,
    }
}

fn lines_of(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .map(|line| serde_json::from_str(line).expect("a written line is JSON"))
        .collect()
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The keys of a line, without the optional `mono_ns`, in order.
fn keys(line: &Value) -> Vec<String> {
    line.as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>()
}

/// The structure the fixtures fix: integer `ts_us`, `mono_ns` and `seq`, string `level`, `layer` and `event`, and the
/// required fields of the catalog with their types.
fn assert_satisfies_catalog(line: &Value) {
    for key in ["ts_us", "mono_ns", "seq"] {
        assert!(line[key].is_u64(), "{key} must be an integer: {line}");
    }
    for key in ["level", "layer", "event"] {
        assert!(line[key].is_string(), "{key} must be a string: {line}");
    }
    let catalog: Value = serde_json::from_str(&fixture("catalog.json")).unwrap();
    let name = line["event"].as_str().unwrap();
    let declared = &catalog["events"][name];
    assert!(declared.is_object(), "{name} is not declared: {line}");
    assert_eq!(line["layer"], declared["layer"], "{line}");
    for (field, kind) in declared["fields"].as_object().unwrap() {
        let value = &line["fields"][field];
        assert_eq!(
            type_name(value),
            kind.as_str().unwrap(),
            "field {field} of {name}: {line}"
        );
    }
}

#[test]
fn the_first_line_opens_the_file_with_the_schema_the_pid_the_role_and_the_boot() {
    let dir = directory("open");
    let log = EventLog::start(options(&dir)).unwrap();
    log.flush(DEADLINE).unwrap();
    let lines = lines_of(log.path());
    assert_eq!(log.path(), dir.join("vt-b1.jsonl"));
    let golden = &fixture_lines("key.jsonl")[0];
    assert_eq!(golden["event"], "log.open");
    let open = &lines[0];
    assert_eq!(open["event"], "log.open");
    assert_eq!(open["seq"], 1);
    assert_eq!(open["layer"], "sidecar");
    assert_eq!(open["fields"]["schema"], 1);
    assert_eq!(open["fields"]["pid"], std::process::id());
    assert_eq!(open["fields"]["role"], "vt");
    assert_eq!(open["fields"]["boot"], "b1");
    // The golden `log.open` declares the same fields with the same types.
    for (field, value) in golden["fields"].as_object().unwrap() {
        assert_eq!(
            type_name(&open["fields"][field]),
            type_name(value),
            "{field}"
        );
    }
    assert_satisfies_catalog(&json!({
        "ts_us": open["ts_us"], "mono_ns": open["mono_ns"], "seq": open["seq"],
        "level": open["level"], "layer": "host", "event": open["event"], "fields": open["fields"],
    }));
}

#[test]
fn the_folder_is_private_and_the_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = directory("modes");
    let log = EventLog::start(options(&dir)).unwrap();
    log.flush(DEADLINE).unwrap();
    let folder = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    let file = std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!((folder, file), (0o700, 0o600));
}

#[test]
fn an_event_has_the_structure_of_the_golden_line_of_the_sidecar() {
    let golden = fixture_lines("key.jsonl")
        .into_iter()
        .find(|line| line["layer"] == "sidecar")
        .expect("the golden input has a line of the sidecar");
    let dir = directory("golden");
    let log = EventLog::start(options(&dir)).unwrap();
    let cids: Vec<String> = golden["cids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cid| cid.as_str().unwrap().to_string())
        .collect();
    assert!(log.emit_for(
        Level::Info,
        golden["event"].as_str().unwrap(),
        Subject {
            surface: golden["surface"].as_str(),
            session: None,
            cids: &cids,
        },
        &golden["fields"],
    ));
    log.flush(DEADLINE).unwrap();
    let line = lines_of(log.path()).remove(1);
    assert_eq!(
        keys(&line),
        keys(&golden),
        "the keys and their order are those of the golden line"
    );
    assert_eq!(line["event"], golden["event"]);
    assert_eq!(line["level"], golden["level"]);
    assert_eq!(line["layer"], golden["layer"]);
    assert_eq!(line["surface"], golden["surface"]);
    assert_eq!(line["cids"], golden["cids"]);
    assert_eq!(line["fields"], golden["fields"]);
    assert_satisfies_catalog(&line);
    assert!(line["event"].as_str().unwrap().starts_with("sidecar.vt."));
}

#[test]
fn seq_is_consecutive_from_one_and_ts_us_is_in_microseconds() {
    let dir = directory("seq");
    let log = EventLog::start(options(&dir)).unwrap();
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    for index in 0..5 {
        assert!(log.emit(Level::Debug, "sidecar.vt.test", &json!({ "index": index })));
    }
    log.flush(DEADLINE).unwrap();
    let lines = lines_of(log.path());
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, (1..=6).collect::<Vec<u64>>());
    for line in &lines {
        let ts = line["ts_us"].as_u64().unwrap();
        assert!(
            ts >= before - 1_000_000 && ts < before + 3_600_000_000,
            "ts_us {ts} is not microseconds since the epoch"
        );
    }
}

#[test]
fn a_refused_event_is_counted_and_the_gap_of_seq_is_explained_by_log_dropped() {
    let dir = directory("dropped");
    let mut small = options(&dir);
    small.queue_bytes = 100;
    let log = EventLog::start(small).unwrap();
    assert!(log.emit(Level::Info, "sidecar.vt.test", &json!({"index": 1})));
    // The bound of the queue is 100 bytes of fields: this event cannot be queued.
    assert!(!log.emit(
        Level::Info,
        "sidecar.vt.test",
        &json!({"text": "x".repeat(500)})
    ));
    log.flush(DEADLINE).unwrap();
    let lines = lines_of(log.path());
    let dropped = lines
        .iter()
        .find(|line| line["event"] == "log.dropped")
        .expect("the refused event is recorded as log.dropped");
    let golden = fixture_lines("dropped.jsonl")
        .into_iter()
        .find(|line| line["event"] == "log.dropped")
        .unwrap();
    for (field, value) in golden["fields"].as_object().unwrap() {
        assert_eq!(
            type_name(&dropped["fields"][field]),
            type_name(value),
            "{field}"
        );
    }
    assert_eq!(dropped["level"], golden["level"]);
    assert_eq!(dropped["fields"]["count"], 1);
    // Every gap of seq is a range that a log.dropped names.
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    for pair in seqs.windows(2) {
        if pair[1] != pair[0] + 1 {
            assert_eq!(
                (
                    dropped["fields"]["from_seq"].as_u64().unwrap(),
                    dropped["fields"]["to_seq"].as_u64().unwrap()
                ),
                (pair[0] + 1, pair[1] - 1),
                "seq {} follows seq {}",
                pair[1],
                pair[0]
            );
        }
    }
}

#[test]
fn an_event_outside_the_namespace_of_the_service_is_refused() {
    let dir = directory("namespace");
    let log = EventLog::start(options(&dir)).unwrap();
    assert!(!log.emit(Level::Info, "host.window.event", &json!({})));
    log.flush(DEADLINE).unwrap();
    let lines = lines_of(log.path());
    assert!(lines
        .iter()
        .all(|line| line["event"] != "host.window.event"));
    assert!(lines.iter().any(|line| line["event"] == "log.dropped"));
}

#[test]
fn a_full_file_rotates_and_the_next_file_continues_seq() {
    let dir = directory("rotation");
    let mut rotating = options(&dir);
    rotating.rotate_bytes = 2000;
    let log = EventLog::start(rotating).unwrap();
    for index in 0..30 {
        assert!(log.emit(
            Level::Info,
            "sidecar.vt.test",
            &json!({ "index": index, "text": "y".repeat(60) })
        ));
        log.flush(DEADLINE).unwrap();
    }
    let current = lines_of(log.path());
    assert_eq!(current[0]["event"], "log.open");
    assert_eq!(current[1]["event"], "log.rotated");
    let rotated = &current[1]["fields"];
    assert_eq!(rotated["kept"], 2);
    assert!(rotated["bytes"].as_u64().unwrap() <= 2000);
    let first = lines_of(&dir.join("vt-b1.jsonl.1"));
    assert_eq!(
        current[0]["seq"].as_u64().unwrap(),
        first.last().unwrap()["seq"].as_u64().unwrap() + 1
    );
    assert!(dir.join("vt-b1.jsonl.2").exists());
    assert!(!dir.join("vt-b1.jsonl.3").exists());
}

// An event of bytes holds every byte: nothing is cut, because a defect of the input is located from what was written.
#[test]
fn the_fields_of_bytes_hold_every_byte_of_a_long_chunk() {
    let data: Vec<u8> = (0..8192u32)
        .map(|index| b'a' + (index % 26) as u8)
        .collect();
    let fields = bytes_fields(&data);
    assert_eq!(fields["bytes"], 8192);
    assert_eq!(fields["text"].as_str().unwrap().len(), 8192);
    assert_eq!(fields["hex"].as_str().unwrap().len(), 8192 * 2);
    assert!(
        fields.get("truncated").is_none(),
        "fields that are cut are not the fields of the bytes"
    );
    let multibyte = bytes_fields("한a\n".as_bytes());
    assert_eq!(multibyte["bytes"], 5);
    assert_eq!(multibyte["text"], "한a\n");
    assert_eq!(multibyte["hex"], "ed959c610a");
}

// The records of the service (what it reports to its operator) are events with the fields `where` and `text`; the text is
// stored as it is, with its line feeds.
#[test]
fn a_record_is_an_event_with_the_place_and_the_text() {
    let dir = directory("records");
    let log = EventLog::start(options(&dir)).unwrap();
    log.record_error("surface close", "first\nsecond");
    log.record_info("font", "no listed font is installed");
    log.flush(DEADLINE).unwrap();
    let lines = lines_of(log.path());
    let error = &lines[1];
    assert_eq!(error["event"], "sidecar.vt.record.error");
    assert_eq!(error["level"], "error");
    assert_eq!(error["layer"], "sidecar");
    assert_eq!(error["fields"]["where"], "surface close");
    assert_eq!(error["fields"]["text"], "first\nsecond");
    let info = &lines[2];
    assert_eq!(info["event"], "sidecar.vt.record.info");
    assert_eq!(info["level"], "info");
    assert_eq!(info["fields"]["where"], "font");
    assert_eq!(info["fields"]["text"], "no listed font is installed");
}
