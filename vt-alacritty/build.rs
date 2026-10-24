use std::{fs, io};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=sidecar.json");
    let declaration = fs::read("sidecar.json")
        .map_err(|error| io::Error::other(format!("sidecar.json: read declaration: {error}")))?;
    let manifest: serde_json::Value = serde_json::from_slice(&declaration)
        .map_err(|error| io::Error::other(format!("sidecar.json: invalid JSON: {error}")))?;
    let display_name = manifest
        .get("displayName")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| io::Error::other("sidecar.json: displayName must be a non-empty string"))?;
    println!("cargo:rustc-env=SOKSAK_SIDECAR_DISPLAY_NAME={display_name}");

    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        pkg_config::Config::new()
            .statik(true)
            .probe("soksak-process-identity")
            .map_err(|error| {
                io::Error::other(format!(
                    "the declared soksak process identity package must be available through pkg-config: {error}"
                ))
            })?;
    }
    Ok(())
}
