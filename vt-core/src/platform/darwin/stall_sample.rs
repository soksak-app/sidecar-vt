//! The dump of a stall of the service on macOS: the system tool `sample` writes the stacks of every thread of this process.

use std::path::Path;

/// Writes the stacks of the threads of this process, sampled for one second, into `path`.
pub fn sample_threads(path: &Path) -> Result<(), String> {
    let output = std::process::Command::new("/usr/bin/sample")
        .arg(std::process::id().to_string())
        .arg("1")
        .arg("-file")
        .arg(path)
        .output()
        .map_err(|error| format!("start /usr/bin/sample: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "/usr/bin/sample ended with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
