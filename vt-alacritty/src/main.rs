pub mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::service::{canonical_config_dir, serve_persistent};
use soksak_sidecar_vt_core::Engine;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let engine_factory = Arc::new(|| Box::new(AlacrittyEngine::new()) as Box<dyn Engine>);
    let mut args = std::env::args().skip(1);
    let flag = args.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--service-dir is required",
        )
    })?;
    if flag != "--service-dir" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only --service-dir is supported",
        ));
    }
    let config_dir = args.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "--service-dir requires a directory",
        )
    })?;
    if args.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unexpected argument after --service-dir",
        ));
    }
    let config_dir = canonical_config_dir(&PathBuf::from(config_dir))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    serve_persistent(&config_dir, engine_factory)
        .await
        .map_err(std::io::Error::other)
}
