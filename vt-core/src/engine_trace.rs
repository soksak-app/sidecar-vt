//! Records engine input in diagnostic builds. When the trace folder is set (the entry point reads
//! `SOKSAK_VT_TRACE_DIR` once and passes it to the writer), each surface writes one line per engine
//! input, in order, to `<dir>/<surface>.trace`: `resize <cols> <rows>`, `reset`, `feed <base64>`.
//! The records become fixtures of the engine replay test. Normal builds contain no recording code.

/// Records one engine input line of a surface. A failed write is recorded as an error event.
#[cfg(feature = "diagnostics")]
pub fn record(log: &crate::events::Events, surface: &str, line: impl FnOnce() -> String) {
    use std::io::Write;
    let Some(dir) = log.trace_dir() else {
        return;
    };
    let path = dir.join(format!("{surface}.trace"));
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{}", line()));
    if let Err(error) = written {
        log.record_error("engine trace", format!("{}: {error}", path.display()));
    }
}

/// Normal builds record nothing.
#[cfg(not(feature = "diagnostics"))]
pub fn record(_log: &crate::events::Events, _surface: &str, _line: impl FnOnce() -> String) {}
