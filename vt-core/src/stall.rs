//! The watch of the runtime of the service (core docs/spec/logging.md#stalls). A stall is the absence of events, which no
//! event of the stalled runtime can report, so a thread outside the runtime compares a heartbeat that a task of the
//! runtime writes with the clock: there is no event source for a runtime that does not run, which is why this watch uses
//! a timer. A heartbeat later than STALL is `sidecar.vt.runtime_stall`, the stacks of the threads of the process are
//! written into a file of the logs folder and recorded as `sidecar.vt.stall_dump`, and the next heartbeat is
//! `sidecar.vt.runtime_resumed` with the length of the stall.

use crate::events::{Events, Level};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
#[path = "../tests/support/stall_test.rs"]
mod tests;
