//! The watch of the runtime of the service (core docs/spec/logging.md#stalls). A stall is the absence of events, which no
//! event of the stalled runtime can report, so a thread outside the runtime compares a heartbeat that a task of the
//! runtime writes with the clock: there is no event source for a runtime that does not run, which is why this watch uses
//! a timer. A heartbeat later than STALL is `sidecar.vt.runtime_stall`, the stacks of the threads of the process are
//! written into a file of the logs folder and recorded as `sidecar.vt.stall_dump`, and the next heartbeat is
//! `sidecar.vt.runtime_resumed` with the length of the stall.

use crate::events::{Events, Level};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The interval of the heartbeat and of the check.
const BEAT: Duration = Duration::from_millis(250);
/// A heartbeat this much later than the clock is a stall.
const STALL: Duration = Duration::from_secs(1);

/// Watches `runtime`: a task of the runtime writes the heartbeat and a thread of the service records each stall, its dump
/// and its end. `dump(path)` writes the stacks of the threads of the process into `path`.
pub fn watch<D>(log: &Events, runtime: &tokio::runtime::Handle, dump: D) -> Result<(), String>
where
    D: Fn(&Path) -> Result<(), String> + Send + 'static,
{
    let start = Instant::now();
    let beat = Arc::new(AtomicU64::new(0));
    let written = Arc::clone(&beat);
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(BEAT);
        loop {
            interval.tick().await;
            written.store(start.elapsed().as_millis() as u64, Ordering::Release);
        }
    });
    let folder = log
        .path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("the event file {} has no folder", log.path().display()))?;
    let events = log.clone();
    crate::thread::start(log, "stall watch", move || {
        let mut stalled: Option<u64> = None;
        loop {
            std::thread::sleep(BEAT);
            let now = start.elapsed().as_millis() as u64;
            let last = beat.load(Ordering::Acquire);
            let late = now.saturating_sub(last);
            match stalled {
                None if late >= STALL.as_millis() as u64 => {
                    stalled = Some(last);
                    events.emit(
                        Level::Warn,
                        "sidecar.vt.runtime_stall",
                        &json!({ "late_ms": late }),
                    );
                    let path = dump_path(&folder);
                    match dump(&path) {
                        Ok(()) => events.emit(
                            Level::Warn,
                            "sidecar.vt.stall_dump",
                            &json!({ "path": path }),
                        ),
                        Err(error) => events.emit(
                            Level::Warn,
                            "sidecar.vt.stall_dump",
                            &json!({ "path": path, "error": error }),
                        ),
                    };
                }
                Some(from) if last > from => {
                    stalled = None;
                    events.emit(
                        Level::Info,
                        "sidecar.vt.runtime_resumed",
                        &json!({ "stalled_ms": last - from }),
                    );
                }
                _ => {}
            }
        }
    })?;
    Ok(())
}

/// The file of a dump: `stall-vt-<pid>-<ms>.txt` in the logs folder, the form of the dumps of the host.
fn dump_path(folder: &Path) -> PathBuf {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    folder.join(format!("stall-vt-{}-{ms}.txt", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_blocked_runtime_is_recorded_with_the_stacks_of_its_threads_and_its_end() {
        let events = crate::events::test_support::TestEvents::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let dumps = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&dumps);
        watch(
            &events.handle(),
            runtime.handle(),
            move |path: &std::path::Path| {
                recorded.lock().unwrap().push(path.to_path_buf());
                Ok(())
            },
        )
        .unwrap();
        runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            // The only thread of the runtime is held by a blocking call, so no task of the runtime runs.
            std::thread::sleep(std::time::Duration::from_millis(1600));
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        });
        let lines = events.lines();
        let named = |name: &str| {
            lines
                .iter()
                .filter(|line| line["event"] == name)
                .collect::<Vec<_>>()
        };
        let stalls = named("sidecar.vt.runtime_stall");
        assert_eq!(
            stalls.len(),
            1,
            "one stall of the blocked runtime: {lines:?}"
        );
        assert!(stalls[0]["fields"]["late_ms"].as_u64().unwrap() >= 1000);
        let dump = named("sidecar.vt.stall_dump");
        assert_eq!(dump.len(), 1);
        let path = dump[0]["fields"]["path"].as_str().unwrap();
        assert!(
            path.contains("stall-vt-"),
            "the dump file is named after the service: {path}"
        );
        assert_eq!(dumps.lock().unwrap().len(), 1);
        let resumed = named("sidecar.vt.runtime_resumed");
        assert_eq!(resumed.len(), 1, "the end of the stall is recorded");
        assert!(resumed[0]["fields"]["stalled_ms"].as_u64().unwrap() >= 1000);
    }
}
