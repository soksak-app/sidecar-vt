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
