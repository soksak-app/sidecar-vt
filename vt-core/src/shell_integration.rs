//! Shell integration: sets the startup environment so that zsh and bash send OSC 133 marks, without
//! changing the user's files. The scripts are included in the executable and are written to a
//! temporary directory named by the content hash before a session starts.

use portable_pty::CommandBuilder;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ZSH_ENV: &str = include_str!("shell-integration/zshenv");
const BASH_INTEGRATION: &str = include_str!("shell-integration/bash-integration.bash");

/// The PROMPT_COMMAND entry that bash runs at the first prompt. The script finds the same string
/// and removes it. A DEBUG trap is not visible inside a file read with source, so the user's trap
/// is recorded first. The parent's trap is shown in a command substitution only when the
/// substituted command is trap, so no builtin is attached.
const BASH_ENTRY: &str =
    "_soksak_debug_trap=$(trap -p DEBUG); builtin source \"$SOKSAK_BASH_INTEGRATION\"";

/// FNV-1a 64 hash of the script contents. When the executable changes and the contents differ,
/// another directory is used.
fn content_hash() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in ZSH_ENV.bytes().chain([0]).chain(BASH_INTEGRATION.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Checks that the directory is a 0700 directory owned by this account. Another owner or a symbolic
/// link is not used.
fn check_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("shell integration directory {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "shell integration path {} is not a directory",
            path.display()
        ));
    }
    if metadata.uid() != unsafe { libc::getuid() } {
        return Err(format!(
            "shell integration directory {} has another owner",
            path.display()
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "shell integration directory {} is accessible to other users",
            path.display()
        ));
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), String> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "create shell integration directory {}: {error}",
                path.display()
            ));
        }
    }
    check_private_directory(path)
}

/// Leaves a file whose contents are the same as it is. A different or missing file is written to a
/// temporary file and then renamed, so the replacement happens in one step.
fn write_file(path: &Path, content: &str) -> Result<(), String> {
    // silent: a file that cannot be read is written again below, and a failed write is an error.
    if fs::read(path).ok().as_deref() == Some(content.as_bytes()) {
        return Ok(());
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "write shell integration file {}: {error}",
                temporary.display()
            )
        })?;
    file.write_all(content.as_bytes()).map_err(|error| {
        format!(
            "write shell integration file {}: {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("install shell integration file {}: {error}", path.display()))
}

/// Prepares the integration directory and returns its path.
pub fn install() -> Result<PathBuf, String> {
    let root = std::env::temp_dir().join(format!("soksak-shell-{:016x}", content_hash()));
    private_directory(&root)?;
    let zsh = root.join("zsh");
    private_directory(&zsh)?;
    write_file(&zsh.join(".zshenv"), ZSH_ENV)?;
    write_file(&root.join("bash-integration.bash"), BASH_INTEGRATION)?;
    Ok(root)
}

/// Adds the integration environment for the shell name to command. A shell other than zsh and bash
/// is not changed.
pub fn apply(
    shell: &str,
    command: &mut CommandBuilder,
    log: &crate::events::Events,
) -> Result<(), String> {
    let name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        // default: a shell whose name cannot be read is neither zsh nor bash, so it gets no
        // integration.
        .unwrap_or_default();
    match name {
        "zsh" => {
            let root = install()?;
            if let Some(original) = command.get_env("ZDOTDIR").map(|value| value.to_owned()) {
                command.env("SOKSAK_ZSH_ZDOTDIR", original);
            }
            command.env("ZDOTDIR", root.join("zsh"));
        }
        "bash" => {
            let root = install()?;
            command.env(
                "SOKSAK_BASH_INTEGRATION",
                root.join("bash-integration.bash"),
            );
            let inherited = match command.get_env("PROMPT_COMMAND") {
                None => String::new(),
                Some(value) => value
                    .to_str()
                    .ok_or("the inherited PROMPT_COMMAND is not UTF-8")?
                    .to_string(),
            };
            // No separator is placed after the entry. The syntax stays valid when a startup file
            // appends "$PROMPT_COMMAND; x".
            let value = if inherited.is_empty() {
                BASH_ENTRY.to_string()
            } else {
                format!("{BASH_ENTRY}\n{inherited}")
            };
            command.env("PROMPT_COMMAND", value);
        }
        other => log.record_info(
            "shell integration",
            format!("{other} has no shell integration; the session starts without it"),
        ),
    }
    Ok(())
}
