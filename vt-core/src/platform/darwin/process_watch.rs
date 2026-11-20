//! Watches the processes of the background jobs of a session for their exits, executions and signals, so that
//! the process list follows them without output (core docs/spec/process-list.md#session-state).
//!
//! The kqueue crate has no `NOTE_SIGNAL` on macOS and registers all its watches in one call, which a
//! process that has ended stops, and the `Kqueue` type of nix gives no descriptor for the tokio
//! reactor, so this module calls `kqueue` and `kevent` through libc.

use std::collections::BTreeSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use tokio::io::unix::AsyncFd;

/// One `kqueue` of a session and the processes that it watches.
pub struct ProcessWatch {
    queue: AsyncFd<OwnedFd>,
    watched: BTreeSet<i32>,
}

/// The result of an update of the watches.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// A process ended before its watch began, so the list is read again at once.
    pub ended: bool,
    /// The watches that the system refused for another reason, each with the pid and the reason.
    pub refused: Vec<String>,
}

impl ProcessWatch {
    /// Creates the `kqueue` of a session. It closes when the watch is dropped.
    pub fn new() -> Result<ProcessWatch, String> {
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(format!(
                "create the process watch: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: kqueue returned a new descriptor that nothing else owns.
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        // A kqueue descriptor reports only readiness to read; the system refuses a write filter for it.
        let queue = AsyncFd::with_interest(queue, tokio::io::Interest::READABLE)
            .map_err(|error| format!("register the process watch: {error}"))?;
        Ok(ProcessWatch {
            queue,
            watched: BTreeSet::new(),
        })
    }

    /// The processes that the watch holds.
    pub fn watched(&self) -> &BTreeSet<i32> {
        &self.watched
    }

    /// Watches each process of `pids` that it does not watch yet with `NOTE_EXIT`, `NOTE_EXEC` and `NOTE_SIGNAL`; a
    /// process that ended before its watch began (`ESRCH`) sets `ended`. A watch ends only at the exit of its process:
    /// a read omits a process during its `exec`, and the removal of its watch would drop the event of that `exec`.
    pub fn add(&mut self, pids: &BTreeSet<i32>) -> Update {
        let mut update = Update::default();
        let added: Vec<i32> = pids.difference(&self.watched).copied().collect();
        for pid in added {
            match self.change(
                pid,
                libc::EV_ADD,
                libc::NOTE_EXIT | libc::NOTE_EXEC | libc::NOTE_SIGNAL,
            ) {
                Ok(()) => {
                    self.watched.insert(pid);
                }
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => update.ended = true,
                Err(error) => update.refused.push(format!("watch process {pid}: {error}")),
            }
        }
        update
    }

    /// Removes every watch, after a failed read of the watch. The system removed the watch of a process that ended
    /// (`ENOENT`); each other refusal is returned with the pid and the reason.
    pub fn clear(&mut self) -> Vec<String> {
        let mut refused = Vec::new();
        for pid in std::mem::take(&mut self.watched) {
            match self.change(pid, libc::EV_DELETE, 0) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
                Err(error) => refused.push(format!("remove the watch of process {pid}: {error}")),
            }
        }
        refused
    }

    /// Waits until a watched process ends, executes a program or receives a signal, and takes every event that waits. A process that ended
    /// leaves the watch, because the system removed its watch.
    pub async fn changed(&mut self) -> Result<(), String> {
        let mut ready = self
            .queue
            .readable()
            .await
            .map_err(|error| format!("wait for the process watch: {error}"))?;
        let mut events: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
        let none = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            let taken = unsafe {
                libc::kevent(
                    self.queue.get_ref().as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as libc::c_int,
                    &none,
                )
            };
            if taken < 0 {
                return Err(format!(
                    "read the process watch: {}",
                    std::io::Error::last_os_error()
                ));
            }
            for event in &events[..taken as usize] {
                if event.fflags & libc::NOTE_EXIT != 0 {
                    self.watched.remove(&(event.ident as i32));
                }
            }
            if (taken as usize) < events.len() {
                break;
            }
        }
        ready.clear_ready();
        Ok(())
    }

    fn change(&self, pid: i32, flags: u16, filter_flags: u32) -> Result<(), std::io::Error> {
        let change = libc::kevent {
            ident: pid as libc::uintptr_t,
            filter: libc::EVFILT_PROC,
            flags,
            fflags: filter_flags,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let result = unsafe {
            libc::kevent(
                self.queue.get_ref().as_raw_fd(),
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
