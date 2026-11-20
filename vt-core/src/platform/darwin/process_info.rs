//! Reads the executable path, the arguments and the working directory of a process (core
//! docs/spec/process-list.md#process).
//!
//! The libproc crate 0.15 reads the executable path with `proc_pidpath` and returns an error for the working directory
//! on macOS (`pidcwd` is not implemented there), so both reads call the libproc functions of the system through libc,
//! as `process_group.rs` does. The sysinfo crate reads the arguments with `KERN_PROCARGS2` and returns empty arguments
//! when the system refuses the read, which a process without arguments also returns; the contract requires a refused
//! read to be an error, so the arguments are read here.

use crate::process_list::Process;

/// The process `pid` of the group `group` with its start time, the path of its executable, its arguments and its
/// working directory. A read that the system refuses, such as for a process of another effective user, leaves
/// `command`, `arguments`, `cwd` and `started` empty and names the read and its reason in `error`. The start is read
/// first, because the system refuses it for every process of another effective user, as it may refuse the other reads.
pub fn process(group: i32, pid: i32) -> Process {
    let read = || -> Result<(u64, String, Vec<String>, String), String> {
        Ok((
            start(pid)?,
            executable(pid)?,
            arguments(pid)?,
            working_directory(pid)?,
        ))
    };
    match read() {
        Ok((started, command, arguments, cwd)) => Process {
            group,
            pid,
            command: Some(command),
            arguments: Some(arguments),
            cwd: Some(cwd),
            started: Some(started),
            error: None,
        },
        Err(error) => Process {
            group,
            pid,
            command: None,
            arguments: None,
            cwd: None,
            started: None,
            error: Some(error),
        },
    }
}

/// The start time in milliseconds since the Unix epoch.
fn start(pid: i32) -> Result<u64, String> {
    let info = crate::platform::darwin::process_group::info(pid)
        .map_err(|error| format!("read the start of process {pid}: {error}"))?;
    Ok(info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000)
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

/// The arguments after the program name. `KERN_PROCARGS2` gives the argument count as a 32-bit
/// integer, the path of the executable and its terminating zero bytes, then the arguments, each
/// ending with a zero byte, then the environment.
fn arguments(pid: i32) -> Result<Vec<String>, String> {
    let failure = |reason: String| format!("read the arguments of process {pid}: {reason}");
    let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    let sized = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if sized != 0 {
        return Err(failure(std::io::Error::last_os_error().to_string()));
    }
    let mut buffer = vec![0u8; size];
    let read = unsafe {
        libc::sysctl(
            name.as_mut_ptr(),
            name.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return Err(failure(std::io::Error::last_os_error().to_string()));
    }
    buffer.truncate(size);
    let count_bytes = buffer
        .first_chunk::<4>()
        .ok_or_else(|| failure(format!("the answer holds {size} bytes and no count")))?;
    let count = i32::from_ne_bytes(*count_bytes);
    if count < 0 {
        return Err(failure(format!("the answer holds the count {count}")));
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    let rest = &buffer[4..];
    let path_end = rest
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| failure("the executable path has no end".to_string()))?;
    let start = rest[path_end..]
        .iter()
        .position(|byte| *byte != 0)
        .map(|offset| path_end + offset)
        .ok_or_else(|| failure(format!("the answer ends before {count} arguments")))?;
    let mut items = rest[start..].split(|byte| *byte == 0);
    let mut arguments = Vec::new();
    for index in 0..count {
        let item = items
            .next()
            .ok_or_else(|| failure(format!("the answer ends before {count} arguments")))?;
        // The first item is the program name.
        if index > 0 {
            arguments.push(
                String::from_utf8(item.to_vec())
                    .map_err(|error| failure(format!("argument {index}: {error}")))?,
            );
        }
    }
    Ok(arguments)
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
