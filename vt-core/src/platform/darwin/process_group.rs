//! Reads the members of a process group and their state.

/// One member of a process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub pid: i32,
    /// A process that has ended and is not reaped yet.
    pub zombie: bool,
    /// A process that is handling exit(). For example, it waits while a terminal with unread output
    /// left is closed.
    pub exiting: bool,
}

impl Member {
    /// A member with nothing left to end. macOS answers a signal to a group that holds only such
    /// members with EPERM.
    pub fn ended(&self) -> bool {
        self.zombie || self.exiting
    }
}

// PROC_FLAG_INEXIT from sys/proc_info.h. The libc crate does not export this value.
const PROC_FLAG_INEXIT: u32 = 4;

// PROC_PGRP_ONLY from sys/proc_info.h. The libc crate does not export this value.
const PROC_PGRP_ONLY: u32 = 2;

/// Reads the members and state of a process group. A process reaped during the read is not in the
/// result.
pub fn members(group: i32) -> Result<Vec<Member>, String> {
    let size =
        unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group as u32, std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(format!(
            "list process group {group}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // Leaves room for members added between the first call and the second call.
    let capacity = size as usize / std::mem::size_of::<i32>() + 16;
    let mut pids = vec![0i32; capacity];
    let filled = unsafe {
        libc::proc_listpids(
            PROC_PGRP_ONLY,
            group as u32,
            pids.as_mut_ptr().cast(),
            (capacity * std::mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if filled < 0 {
        return Err(format!(
            "list process group {group}: {}",
            std::io::Error::last_os_error()
        ));
    }
    pids.truncate(filled as usize / std::mem::size_of::<i32>());
    let mut members = Vec::new();
    for pid in pids.into_iter().filter(|pid| *pid > 0) {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // PROC_PIDTBSDINFO answers ESRCH for a zombie when arg is 0, and finds the zombie too when
        // arg is 1.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read == size {
            members.push(Member {
                pid,
                zombie: info.pbi_status == libc::SZOMB,
                exiting: info.pbi_flags & PROC_FLAG_INEXIT != 0,
            });
            continue;
        }
        // A process reaped after the list was read gives no information. Any other failure means
        // the state is unknown.
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!("read process {pid} of group {group}: {error}"));
        }
    }
    Ok(members)
}
