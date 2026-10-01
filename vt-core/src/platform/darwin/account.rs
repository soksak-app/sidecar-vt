//! 사용자 데이터베이스의 계정 정보.

use std::ffi::CStr;

/// 이 프로세스 사용자의 로그인 셸. 사용자 데이터베이스(getpwuid)의 셸 항목이며, 항목이 없거나 비어 있으면 오류다.
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
