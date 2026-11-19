use super::*;

/// The output of a writer with its main file started.
fn new_output(path: &Path, debug: &Path, rotate_bytes: u64, open: &Pending) -> Output {
    let mut output = Output::new(
        path.to_path_buf(),
        debug.to_path_buf(),
        rotate_bytes,
        2,
        open,
        PolicyState::start(None),
    );
    output.start().expect("create writer file");
    output
}

/// A reader of the run takes the debug file as soon as it exists (read_run_lines), and every file of the contract starts
/// with `log.open`. The writer creates the debug file for the first debug event, so the file must hold its `log.open`
/// line from the moment it exists, also while the event waits in the batch for the next flush.
#[test]
fn the_debug_file_holds_its_open_line_from_the_moment_it_exists() {
    let folder = tempfile::tempdir().expect("create writer folder");
    let path = folder.path().join("writer.jsonl");
    let debug = folder.path().join("writer.debug.jsonl");
    let open = Pending::of_writer(
        Level::Info,
        "log.open",
        to_raw_value(&json!({"schema": SCHEMA})).expect("encode open record"),
    );
    let mut output = new_output(&path, &debug, 1 << 20, &open);
    let event = Pending::of_writer(
        Level::Debug,
        "sidecar.vt.test.debug",
        to_raw_value(&json!({})).expect("encode debug record"),
    );
    output.push(&event);
    assert!(
        debug.exists(),
        "the first debug event creates the debug file"
    );
    let lines = read_lines(&debug).expect("the debug file starts with log.open");
    assert_eq!(
        lines.len(),
        1,
        "only log.open is on disk before the flush: {lines:?}"
    );
    output.flush();
    let lines = read_lines(&debug).expect("read the debug file");
    assert_eq!(
        lines
            .iter()
            .map(|line| line["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["log.open", "sidecar.vt.test.debug"],
        "the debug file holds log.open once and the event after the flush"
    );
}

/// The main file exists when the log is open, so it holds its `log.open` line when the constructor returns, before the
/// first flush of the writer.
#[test]
fn the_main_file_holds_its_open_line_when_the_log_is_open() {
    let folder = tempfile::tempdir().expect("create writer folder");
    let mut options = Options::service(folder.path().to_path_buf(), "file-start".into());
    options.batch_interval = Duration::from_secs(600);
    let log = EventLog::start(options).expect("open the log");
    read_lines(log.path()).expect("the main file starts with log.open");
}

/// A rotation moves the full file away and starts the file of the path again, which a reader can take at once, so the
/// new file holds its `log.open` line from the moment it exists.
#[test]
fn a_rotated_file_holds_its_open_line_from_the_moment_it_exists() {
    let folder = tempfile::tempdir().expect("create writer folder");
    let path = folder.path().join("writer.jsonl");
    let open = Pending::of_writer(
        Level::Info,
        "log.open",
        to_raw_value(&json!({"schema": SCHEMA})).expect("encode open record"),
    );
    let mut output = new_output(&path, &folder.path().join("writer.debug.jsonl"), 400, &open);
    let event = Pending::of_writer(
        Level::Info,
        "sidecar.vt.test.info",
        to_raw_value(&json!({"text": "a line of about one hundred bytes"})).expect("encode record"),
    );
    let rotated = path.with_extension("jsonl.1");
    while !rotated.exists() {
        output.push(&event);
        output.flush();
    }
    output.push(&event);
    let lines = read_lines(&path).expect("the rotated file starts with log.open");
    assert_eq!(
        lines.len(),
        1,
        "only log.open is on disk before the flush: {lines:?}"
    );
}
