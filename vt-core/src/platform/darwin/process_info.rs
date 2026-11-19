//! Reads the executable path and the working directory of a process (core docs/spec/terminal-read.md#session-state).
//!
//! The libproc crate 0.15 reads the executable path with `proc_pidpath` and returns an error for the working directory
//! on macOS (`pidcwd` is not implemented there), so both reads call the libproc functions of the system through libc,
//! as `process_group.rs` does.

use crate::protocol::Foreground;

/// The leader `pid` of the foreground process group with the path of its executable and its working directory. A read
/// that the system refuses, such as for a process of another user, leaves `command` and `cwd` empty and names the
/// read and its reason in `error`.
pub fn foreground(pid: i32) -> Foreground {
    match executable(pid).and_then(|command| Ok((command, working_directory(pid)?))) {
        Ok((command, cwd)) => Foreground {
            pid,
            command: Some(command),
            cwd: Some(cwd),
            error: None,
        },
        Err(error) => Foreground {
            pid,
            command: None,
            cwd: None,
            error: Some(error),
        },
    }
}

fn executable(pid: i32) -> Result<String, String> {
    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length =
        unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if length <= 0 {
        return Err(format!(
            "read the executable of process {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    buffer.truncate(length as usize);
    String::from_utf8(buffer)
        .map_err(|error| format!("read the executable of process {pid}: {error}"))
}

fn working_directory(pid: i32) -> Result<String, String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size,
        )
    };
    if read != size {
        return Err(format!(
            "read the working directory of process {pid}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // vip_path holds MAXPATHLEN bytes as 32 rows of 32, and the path ends at its first zero byte.
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    String::from_utf8(path)
        .map_err(|error| format!("read the working directory of process {pid}: {error}"))
}
