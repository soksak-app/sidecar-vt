// The writer of events for the tests of this crate: a writer in a folder of its own, which the test reads through the
// file the writer produced.
#![allow(dead_code)]

use serde_json::Value;
use soksak_sidecar_vt_core::events::{EventLog, Events, Options};
use std::time::Duration;

/// A writer with its folder. The folder is removed when this value is dropped.
pub struct TestEvents {
    handle: Events,
    _directory: tempfile::TempDir,
}

impl Default for TestEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl TestEvents {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().expect("create the folder of the events");
        let mut options = Options::service(directory.path().join("logs"), "test".to_string());
        options.batch_interval = Duration::from_millis(20);
        let handle = std::sync::Arc::new(EventLog::start(options).expect("start the writer"));
        Self {
            handle,
            _directory: directory,
        }
    }

    /// The writer to hand to the code under test.
    pub fn handle(&self) -> Events {
        self.handle.clone()
    }

    /// The events written so far, after the writer put everything it holds on disk.
    pub fn lines(&self) -> Vec<Value> {
        self.handle
            .flush(Duration::from_secs(10))
            .expect("flush the writer");
        std::fs::read_to_string(self.handle.path())
            .expect("read the file of the events")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
            .collect()
    }

    /// The events with the given name.
    pub fn named(&self, event: &str) -> Vec<Value> {
        self.lines()
            .into_iter()
            .filter(|line| line["event"] == event)
            .collect()
    }
}
