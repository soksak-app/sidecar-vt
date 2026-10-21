pub mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::Engine;
use soksak_sidecar_vt_core::arguments;
use soksak_sidecar_vt_core::events::EventLog;
use soksak_sidecar_vt_core::platform::darwin::stall_sample::sample_threads;
use soksak_sidecar_vt_core::service::{canonical_config_dir, record_end, serve_persistent};
use soksak_sidecar_vt_core::stall;
use std::sync::Arc;
use std::time::Duration;

/// How long the end of the process waits for the writer to put the queued events on disk.
const FLUSH_DEADLINE: Duration = Duration::from_secs(5);

/// The time of now in the form of the time of a text record: the UTC time `YYYY-MM-DDTHH:MM:SS.mmmZ` of the diagnostics
/// specification of the soksak core repository (docs/spec/diagnostics.md#forms).
fn text_record_time() -> String {
    let format = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    time::OffsetDateTime::now_utc()
        .format(&format)
        .expect("the format description is valid")
}

fn invalid_input(error: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // The first action of the process, before any other statement and before any setup: one text record
    // (docs/spec/diagnostics.md#forms) on the standard error, which the host stores as a record of the layer sidecar, so
    // the time from the spawn of the process to its first own record is visible (S69). The record goes to the standard
    // error and not to the event file, because the file of the contract starts with log.open and the writer that writes it
    // exists only after the setup reads the arguments and the platform.
    eprintln!("{} info sidecar vt: started", text_record_time());
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let arguments = arguments::parse(std::env::args().skip(1)).map_err(invalid_input)?;
    let config_dir = canonical_config_dir(&arguments.service_dir).map_err(invalid_input)?;
    // The engine trace folder of a diagnostic build is read here, once, and given to the writer.
    let trace_dir = std::env::var_os("SOKSAK_VT_TRACE_DIR").map(std::path::PathBuf::from);
    let events =
        EventLog::start_service(&arguments.log_dir, trace_dir).map_err(std::io::Error::other)?;
    // A stall of the runtime is recorded with the stacks of the threads (core docs/spec/logging.md#stalls).
    stall::watch(&events, &tokio::runtime::Handle::current(), sample_threads)
        .map_err(std::io::Error::other)?;
    let served = serve_persistent(&config_dir, engine_factory, events.clone())
        .await
        .map_err(std::io::Error::other);
    record_end(&events, &served);
    // Every exit path puts the queued events on disk before the process ends.
    events
        .flush(FLUSH_DEADLINE)
        .map_err(std::io::Error::other)?;
    served
}
