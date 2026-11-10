pub mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::Engine;
use soksak_sidecar_vt_core::arguments;
use soksak_sidecar_vt_core::events::EventLog;
use soksak_sidecar_vt_core::platform::darwin::stall_sample::sample_threads;
use soksak_sidecar_vt_core::service::{
    Termination, canonical_config_dir, record_end, serve_persistent,
};
use soksak_sidecar_vt_core::stall;
use std::sync::Arc;

#[cfg(target_os = "macos")]
#[path = "platform/darwin/process_identity.rs"]
mod process_identity;

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
    // The handlers of SIGTERM and SIGINT replace the default action, which ends the process without its cleanup, before
    // the process creates anything that its exit removes. A request that arrives during the setup is held and ends the serve
    // loop on its first turn, so every exit records its stop and end, removes the endpoint and the socket folder and puts
    // the events on disk. The install writes nothing, so the record below is still the first
    // output of the process.
    let termination = Termination::install().map_err(std::io::Error::other)?;
    // The first output of the process, before any setup: one text record
    // (docs/spec/diagnostics.md#forms) on the standard error, which the host stores as a record of the layer sidecar, so
    // the time from the spawn of the process to its first own record is visible (S69). The record goes to the standard
    // error and not to the event file, because the file of the contract starts with log.open and the writer that writes it
    // exists only after the setup reads the arguments and the platform.
    eprintln!("{} info sidecar vt: started", text_record_time());
    #[cfg(target_os = "macos")]
    process_identity::set_process_display_name(env!("SOKSAK_SIDECAR_DISPLAY_NAME"))
        .map_err(invalid_input)?;
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
    let served = serve_persistent(&config_dir, engine_factory, events.clone(), termination)
        .await
        .map_err(std::io::Error::other);
    record_end(&events, &served);
    // Every exit path puts the queued events on disk before the process ends. The wait ends with the answer of the
    // writer, which it gives after its write, or with the end of the writer; it has no time limit.
    events.flush().map_err(std::io::Error::other)?;
    served
}
