pub mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::arguments;
use soksak_sidecar_vt_core::events::EventLog;
use soksak_sidecar_vt_core::service::{canonical_config_dir, serve_persistent};
use soksak_sidecar_vt_core::Engine;
use std::sync::Arc;
use std::time::Duration;

/// How long the end of the process waits for the writer to put the queued events on disk.
const FLUSH_DEADLINE: Duration = Duration::from_secs(5);

fn invalid_input(error: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let arguments = arguments::parse(std::env::args().skip(1)).map_err(invalid_input)?;
    let config_dir = canonical_config_dir(&arguments.service_dir).map_err(invalid_input)?;
    let events = EventLog::start_service(&arguments.log_dir).map_err(std::io::Error::other)?;
    let served = serve_persistent(&config_dir, engine_factory)
        .await
        .map_err(std::io::Error::other);
    // Every exit path puts the queued events on disk before the process ends.
    events
        .flush(FLUSH_DEADLINE)
        .map_err(std::io::Error::other)?;
    served
}
