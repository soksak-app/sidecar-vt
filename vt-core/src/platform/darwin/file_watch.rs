//! The watch of a file through the kqueue of the kernel (core docs/spec/logging.md#classes-and-policy). The folder of the
//! file is watched for the creation, replacement and removal of its entry, and the file for the writes in place, so no
//! timer reads the file. No Rust library is used: notify 8.2 (its kqueue backend) delivers a kernel event only when a later
//! event wakes its loop and watches each new subfolder of a watched folder recursively, and the kqueue 1.2 crate has no user
//! event that ends a blocked wait.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

/// O_EVTONLY: the descriptor is only for the notifications of the kernel and keeps neither the volume busy nor the file open
/// for reading.
const OPEN_EVENT_ONLY: libc::c_int = 0x8000;

/// The identifier of the user event that ends the watch.
const STOP_IDENT: libc::uintptr_t = 1;

/// The changes of a folder or a file that the watch reports: a write or an extension, a change of the attributes, and the
/// removal, the rename or the revocation of the entry.
const VNODE_EVENTS: u32 = libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_DELETE
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;

/// The names of the flags of a vnode event.
const FLAG_NAMES: [(u32, &str); 6] = [
    (libc::NOTE_WRITE, "write"),
    (libc::NOTE_EXTEND, "extend"),
    (libc::NOTE_ATTRIB, "attrib"),
    (libc::NOTE_DELETE, "delete"),
    (libc::NOTE_RENAME, "rename"),
    (libc::NOTE_REVOKE, "revoke"),
];

/// The callback that receives the changes of each wait of the kernel.
pub type Changed = Arc<dyn Fn(&[Change]) + Send + Sync>;

/// The callback that receives a failure of the kernel calls of the watch.
pub type Failed = Arc<dyn Fn(String) + Send + Sync>;

/// One change that the watch reports: the vnode that fired (`folder` or `file`) and the names of its flags.
pub struct Change {
    pub vnode: &'static str,
    pub flags: Vec<&'static str>,
}

fn flag_names(fflags: u32) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = FLAG_NAMES
        .iter()
        .filter(|(bit, _)| fflags & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if fflags & !VNODE_EVENTS != 0 {
        names.push("unrecognized");
    }
    names
}

fn event(ident: libc::uintptr_t, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn vnode(descriptor: libc::c_int) -> libc::kevent {
    event(
        descriptor as libc::uintptr_t,
        libc::EVFILT_VNODE,
        libc::EV_ADD | libc::EV_CLEAR,
        VNODE_EVENTS,
    )
}

/// Applies one change to the kqueue.
fn register(queue: libc::c_int, change: &libc::kevent) -> Result<(), String> {
    let result =
        unsafe { libc::kevent(queue, change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if result < 0 {
        Err(format!("kevent: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn close_descriptor(descriptor: libc::c_int) -> Result<(), String> {
    if unsafe { libc::close(descriptor) } < 0 {
        Err(format!("close: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

/// Opens `path` for notifications. `Ok(None)` is a path that does not exist.
fn open_event_only(path: &Path) -> Result<Option<libc::c_int>, String> {
    let text = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("{} holds a NUL character", path.display()))?;
    let descriptor = unsafe { libc::open(text.as_ptr(), OPEN_EVENT_ONLY | libc::O_CLOEXEC) };
    if descriptor >= 0 {
        return Ok(Some(descriptor));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(format!("open {}: {error}", path.display()))
    }
}

/// Creates the kqueue, registers the stop event and the vnodes of the folder and of the file, and returns the descriptors
/// as (queue, folder, file); the file is -1 while the file does not exist. A failure closes every descriptor that was opened.
fn open_watch(
    path: &Path,
    directory: &Path,
) -> Result<(libc::c_int, libc::c_int, libc::c_int), String> {
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return Err(format!("kqueue: {}", std::io::Error::last_os_error()));
    }
    let mut opened = vec![queue];
    let result = (|| -> Result<(libc::c_int, libc::c_int), String> {
        register(
            queue,
            &event(
                STOP_IDENT,
                libc::EVFILT_USER,
                libc::EV_ADD | libc::EV_CLEAR,
                0,
            ),
        )?;
        let folder = open_event_only(directory)?
            .ok_or_else(|| format!("{} does not exist", directory.display()))?;
        opened.push(folder);
        register(queue, &vnode(folder))?;
        let file = match open_event_only(path)? {
            Some(descriptor) => {
                opened.push(descriptor);
                register(queue, &vnode(descriptor))?;
                descriptor
            }
            None => -1,
        };
        Ok((folder, file))
    })();
    match result {
        Ok((folder, file)) => Ok((queue, folder, file)),
        Err(error) => {
            for descriptor in opened {
                if let Err(close_error) = close_descriptor(descriptor) {
                    eprintln!("watch {}: {close_error}", path.display());
                }
            }
            Err(error)
        }
    }
}

/// The loop of the watch thread. It waits for the kqueue, reports the changes of each wait through `changed`, and reports a
/// failed kernel call through `failed`; it returns when the stop event fires or when the wait fails.
fn watch_loop(
    queue: libc::c_int,
    directory: libc::c_int,
    mut file: libc::c_int,
    path: &Path,
    changed: &(dyn Fn(&[Change]) + Send + Sync),
    failed: &(dyn Fn(String) + Send + Sync),
) {
    let mut events: [libc::kevent; 8] = unsafe { std::mem::zeroed() };
    loop {
        let count = unsafe {
            libc::kevent(
                queue,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as libc::c_int,
                std::ptr::null(),
            )
        };
        if count < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            failed(format!(
                "watch {}: wait for events: {error}",
                path.display()
            ));
            break;
        }
        let mut stopped = false;
        let mut replaced = false;
        let mut changes = Vec::new();
        for event in &events[..count as usize] {
            if event.filter == libc::EVFILT_USER {
                stopped = true;
            } else if event.filter == libc::EVFILT_VNODE {
                let vnode = if event.ident == file as libc::uintptr_t {
                    "file"
                } else if event.ident == directory as libc::uintptr_t {
                    "folder"
                } else {
                    "unknown"
                };
                if vnode == "file"
                    && event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE)
                        != 0
                {
                    replaced = true;
                }
                changes.push(Change {
                    vnode,
                    flags: flag_names(event.fflags),
                });
            } else {
                failed(format!(
                    "watch {}: an event of the filter {} is not registered",
                    path.display(),
                    event.filter
                ));
            }
        }
        if stopped {
            break;
        }
        // After a replacement the path names another file, and a removed file has none: the descriptor is renewed before
        // the change is reported, so the reader sees the file that is at the path now.
        if replaced || file < 0 {
            if file >= 0 {
                if let Err(error) = close_descriptor(file) {
                    failed(format!("watch {}: {error}", path.display()));
                }
                file = -1;
            }
            match open_event_only(path) {
                Ok(Some(descriptor)) => match register(queue, &vnode(descriptor)) {
                    Ok(()) => file = descriptor,
                    Err(error) => {
                        if let Err(close_error) = close_descriptor(descriptor) {
                            failed(format!("watch {}: {close_error}", path.display()));
                        }
                        failed(format!("watch {}: {error}", path.display()));
                    }
                },
                Ok(None) => {}
                Err(error) => failed(format!("watch {}: {error}", path.display())),
            }
        }
        if !changes.is_empty() {
            changed(&changes);
        }
    }
    if file >= 0
        && let Err(error) = close_descriptor(file)
    {
        failed(format!("watch {}: {error}", path.display()));
    }
}

/// Watches the file at `path` and calls `changed` on a thread of the watch with the changes of each wait of the kernel,
/// after the file was created, written, replaced, renamed or removed. A failure of a kernel call is passed to `failed`.
/// The returned function stops the watch, waits until its thread ended and closes the kernel descriptors.
pub fn watch_file(
    path: &Path,
    changed: Changed,
    failed: Failed,
) -> Result<Box<dyn FnOnce() + Send>, String> {
    let fail = |error: String| format!("watch {}: {error}", path.display());
    let directory = path
        .parent()
        .ok_or_else(|| fail("the path has no folder".into()))?;
    let (queue, folder, file) = open_watch(path, directory).map_err(fail)?;
    let watched = path.to_path_buf();
    let thread_changed = Arc::clone(&changed);
    let thread_failed = Arc::clone(&failed);
    let spawned = std::thread::Builder::new()
        .name("file-watch".into())
        .spawn(move || {
            watch_loop(
                queue,
                folder,
                file,
                &watched,
                &*thread_changed,
                &*thread_failed,
            )
        });
    let handle = match spawned {
        Ok(handle) => handle,
        Err(error) => {
            // The thread owns none of the descriptors yet, so they are closed here.
            for descriptor in [queue, folder, file] {
                if descriptor >= 0
                    && let Err(close_error) = close_descriptor(descriptor)
                {
                    eprintln!("watch {}: {close_error}", path.display());
                }
            }
            return Err(fail(format!("the watch thread: {error}")));
        }
    };
    let stop_path = path.to_path_buf();
    Ok(Box::new(move || {
        let trigger = event(STOP_IDENT, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
        if let Err(error) = register(queue, &trigger) {
            // Without the trigger the thread keeps waiting, so the thread and the descriptors are left as they are.
            failed(format!("watch {}: the stop: {error}", stop_path.display()));
            return;
        }
        if handle.join().is_err() {
            failed(format!(
                "watch {}: the thread of the watch ended with a panic",
                stop_path.display()
            ));
        }
        for descriptor in [folder, queue] {
            if let Err(error) = close_descriptor(descriptor) {
                failed(format!("watch {}: {error}", stop_path.display()));
            }
        }
    }))
}
