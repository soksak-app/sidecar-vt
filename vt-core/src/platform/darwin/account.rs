//! Account information from the user database.

use std::ffi::CStr;

/// Login shell of the process user. This is the shell field of the user database (getpwuid); a
/// missing or empty field is an error.
pub fn login_shell() -> Result<String, String> {
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    if entry.is_null() {
        return Err("terminal shell login: the user database has no entry for this user".into());
    }
    let shell = unsafe { CStr::from_ptr((*entry).pw_shell) }
        .to_str()
        .map_err(|_| "terminal shell login: the login shell is not UTF-8".to_string())?;
    if shell.is_empty() {
        return Err("terminal shell login: the user database entry has no login shell".into());
    }
    Ok(shell.to_string())
}

/// The home directory of the process user: the HOME variable of the environment, else the home field of the user database.
/// A session starts in it when its open names no directory, so a relative program path resolves from it.
pub fn home_directory() -> Result<String, String> {
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        return home
            .into_string()
            .map_err(|_| "terminal home directory: HOME is not UTF-8".to_string());
    }
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    if entry.is_null() {
        return Err("terminal home directory: the user database has no entry for this user".into());
    }
    let home = unsafe { CStr::from_ptr((*entry).pw_dir) }
        .to_str()
        .map_err(|_| "terminal home directory: the home directory is not UTF-8".to_string())?;
    if home.is_empty() {
        return Err(
            "terminal home directory: the user database entry has no home directory".into(),
        );
    }
    Ok(home.to_string())
}
