use portable_pty::MasterPty;

#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(unix)]
use nix::sys::signal::{Signal, kill};
#[cfg(unix)]
use nix::unistd::Pid;

/// Returns the process-group leader that the active PTY platform provides.
pub fn process_group_leader(
    #[cfg(unix)] master: &dyn MasterPty,
    #[cfg(not(unix))] _master: &dyn MasterPty,
) -> Option<i32> {
    #[cfg(unix)]
    {
        master.process_group_leader()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The number of bytes in the PTY input queue that the master wrote and the child has not read yet.
/// This is a diagnostic measurement.
/// A non-blocking duplicate of the master, which the session task reads and writes. A duplicate
/// shares the file status flags of the master, and the master then serves only ioctl calls.
pub fn master_stream(master: &dyn MasterPty) -> Result<std::fs::File, String> {
    use std::os::fd::BorrowedFd;
    let fd = master.as_raw_fd().ok_or("PTY master fd is unavailable")?;
    // SAFETY: `master` owns `fd` and stays open during this call, and the duplicate owns its own
    // descriptor.
    let duplicate = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .map_err(|error| format!("duplicate the PTY master: {error}"))?;
    nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK | nix::fcntl::OFlag::O_RDWR),
    )
    .map_err(|error| format!("set the PTY master non-blocking: {error}"))?;
    Ok(duplicate.into())
}

pub fn pending_input(
    #[cfg(unix)] master: &dyn MasterPty,
    #[cfg(not(unix))] _master: &dyn MasterPty,
) -> Result<usize, String> {
    #[cfg(unix)]
    {
        let fd = master.as_raw_fd().ok_or("PTY master fd is unavailable")?;
        let mut pending: libc::c_int = 0;
        // The xnu (BSD-derived) PTY shares a struct tty between the master and the slave. A write
        // by the master goes into t_rawq/t_canq through line-discipline input processing, and
        // FIONREAD (ttnread) returns their sum: the bytes the master wrote that the child has not
        // read yet. TIOCOUTQ reads t_outq, the output the child wrote, so it is not this value
        // (both were measured in the ownership check to tell them apart). In canonical mode, a line
        // that is not complete yet stays in the assembly buffer of t_rawq and is not counted.
        let result = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut pending) };
        if result < 0 {
            return Err(format!(
                "read PTY pending input: {}",
                std::io::Error::last_os_error()
            ));
        }
        usize::try_from(pending).map_err(|_| format!("PTY pending input {pending} is negative"))
    }
    #[cfg(not(unix))]
    {
        Err("PTY pending input is not implemented on this platform".to_string())
    }
}

/// What a signal to a process or a group reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillResult {
    /// The signal was sent.
    Sent,
    /// The operating system answered ESRCH: nothing with that identifier exists any more.
    NoSuchProcess,
    /// The operating system answered EPERM because every member of the group is ending and waits to
    /// be reaped.
    Ending,
}

impl KillResult {
    /// The value of the field `result` of the event `sidecar.vt.kill`.
    pub fn name(self) -> &'static str {
        match self {
            KillResult::Sent => "sent",
            KillResult::NoSuchProcess => "ESRCH",
            KillResult::Ending => "EPERM: the members are ending",
        }
    }
}

/// Kills the PTY process group when the platform exposes it.
pub fn kill_process_group(
    #[cfg(unix)] group: Option<i32>,
    #[cfg(not(unix))] _group: Option<i32>,
) -> Result<KillResult, String> {
    #[cfg(unix)]
    {
        let group = group.ok_or("PTY process group is unavailable")?;
        match kill(Pid::from_raw(-group), Signal::SIGKILL) {
            Ok(()) => Ok(KillResult::Sent),
            Err(nix::errno::Errno::ESRCH) => Ok(KillResult::NoSuchProcess),
            #[cfg(target_os = "macos")]
            Err(nix::errno::Errno::EPERM) => {
                // On macOS, a signal to a group whose members are all handling exit() or waiting to
                // be reaped (zombies) returns EPERM. A shell that ends with unread output left is
                // ending while it closes the terminal and waits for the output to drain. Such a
                // group has no process left to end. If a member has not ended, the call fails and
                // records its state.
                let members = crate::platform::darwin::process_group::members(group)
                    .map_err(|error| format!("kill PTY process group {group}: EPERM; {error}"))?;
                if members.iter().all(|member| member.ended()) {
                    return Ok(KillResult::Ending);
                }
                Err(format!(
                    "kill PTY process group {group}: EPERM with members {members:?}"
                ))
            }
            Err(error) => Err(format!("kill PTY process group {group}: {error}")),
        }
    }
    #[cfg(not(unix))]
    {
        Ok(KillResult::NoSuchProcess)
    }
}

/// Waits until the child pid ends, without reaping it (WNOWAIT of waitid). The child stays a zombie
/// until it is reaped, so the pid is not reused by another process.
pub fn wait_for_exit(#[cfg(unix)] pid: u32, #[cfg(not(unix))] _pid: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("wait for PTY child {pid} to exit: {error}"));
            }
        }
    }
    #[cfg(not(unix))]
    {
        Err("waiting for a PTY child is not implemented on this platform".to_string())
    }
}

/// Serializes the real PTY tests within the Rust test process and across processes.
#[cfg(test)]
pub(crate) fn native_pty_test_lock() -> NativePtyTestLock {
    static LOCAL: OnceLock<Mutex<()>> = OnceLock::new();
    let local = LOCAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        // default: this lock serves the tests only; an earlier test that failed holding it was
        // reported, and the later tests go on.
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let path = std::env::temp_dir().join("soksak-vt-core-pty-tests.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .expect("PTY test lock file must open");
        nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)
            .expect("PTY test lock must acquire");
        NativePtyTestLock {
            _local: local,
            _file: file,
        }
    }
    #[cfg(not(unix))]
    {
        NativePtyTestLock { _local: local }
    }
}

#[cfg(test)]
pub(crate) struct NativePtyTestLock {
    _local: MutexGuard<'static, ()>,
    #[cfg(unix)]
    _file: std::fs::File,
}
