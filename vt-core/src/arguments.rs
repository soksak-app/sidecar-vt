//! The command line of the service: `--service-dir <dir> --log-dir <dir>`, both required, in either order. The host
//! passes the directory of the event files with `--log-dir` (core docs/spec/sidecars.md); the service has no default for
//! it.

use std::path::PathBuf;

/// The directories the service starts with.
#[derive(Debug, PartialEq, Eq)]
pub struct Arguments {
    /// The directory of the endpoint and the lock of the service.
    pub service_dir: PathBuf,
    /// The directory of the event files `vt-<boot>.jsonl`.
    pub log_dir: PathBuf,
}

/// Reads the arguments after the program name. A missing option, an option without a value, an option that is given
/// twice and an unknown argument are errors that name the argument.
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Arguments, String> {
    let mut service_dir = None;
    let mut log_dir = None;
    let mut arguments = arguments.into_iter().peekable();
    while let Some(option) = arguments.next() {
        let slot = match option.as_str() {
            "--service-dir" => &mut service_dir,
            "--log-dir" => &mut log_dir,
            other => return Err(format!("unknown argument {other:?}")),
        };
        // A value that starts with `--` is the next option, so the option before it has no value.
        let value = match arguments.next_if(|value| !value.starts_with("--")) {
            Some(value) if !value.is_empty() => value,
            _ => return Err(format!("{option} requires a directory")),
        };
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(format!("{option} is given twice"));
        }
    }
    Ok(Arguments {
        service_dir: service_dir.ok_or("--service-dir is required")?,
        log_dir: log_dir.ok_or(
            "--log-dir is required: the service writes its events to <log-dir>/vt-<boot>.jsonl",
        )?,
    })
}
