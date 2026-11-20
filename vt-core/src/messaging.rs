//! Messaging between terminals (core docs/spec/messaging.md): the registration of a folder by a surface, the watch of
//! that folder with the crate `notify`, the file events of its messages, and the reads of the messages.
//!
//! A message is a file `<from>.<token>.txt` directly in the folder. [`name_rule`] decides each name; [`Registration`]
//! turns the events of the watch into file events numbered from 1, and answers the reads from the folder when the reply
//! is built. The module depends on no engine and no wire: the surface task of the protocol owns one registration and
//! sends what it returns.

use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// The largest message in bytes; a larger file is refused with `message-too-large` (core docs/spec/messaging.md#message).
pub const MESSAGE_LIMIT: u64 = 64 * 1024;

/// What a file name directly in the folder is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameRule {
    /// The name does not end in `.txt`: not a message, and no event.
    Ignored,
    /// The name ends in `.txt` and gives no `from` or no `token`.
    InvalidSender,
    /// A message of the surface `from`.
    Message { from: String },
}

/// Decides a file name: `from` is the text before the last `.` of the name without `.txt`, and `token` the text after it.
pub fn name_rule(name: &str) -> NameRule {
    let Some(stem) = name.strip_suffix(".txt") else {
        return NameRule::Ignored;
    };
    match stem.rsplit_once('.') {
        Some((from, token)) if !from.is_empty() && !token.is_empty() => NameRule::Message {
            from: from.to_string(),
        },
        _ => NameRule::InvalidSender,
    }
}

/// An explicit refusal of a messaging request: the code of the wire and the folder or path it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub reason: String,
}

impl Refusal {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }

    /// The body of the error reply.
    pub fn body(&self) -> Value {
        json!({"error": self.code, "reason": self.reason})
    }
}

/// The folders that the surfaces of one service have registered, so that two surfaces never hold one folder.
#[derive(Debug, Default)]
pub struct FolderClaims {
    folders: Mutex<HashMap<PathBuf, String>>,
}

impl FolderClaims {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn claim(&self, folder: &Path, surface: &str) -> Result<(), Refusal> {
        let mut folders = self
            .folders
            .lock()
            // default: the map of claims changes only by whole inserts and removals, so a panic of another holder leaves
            // it consistent.
            .unwrap_or_else(|error| error.into_inner());
        match folders.get(folder) {
            Some(holder) => Err(Refusal::new(
                "already-registered",
                format!("{} is registered by the surface {holder}", folder.display()),
            )),
            None => {
                folders.insert(folder.to_path_buf(), surface.to_string());
                Ok(())
            }
        }
    }

    fn release(&self, folder: &Path) {
        self.folders
            .lock()
            // default: the map of claims changes only by whole inserts and removals, so a panic of another holder leaves
            // it consistent.
            .unwrap_or_else(|error| error.into_inner())
            .remove(folder);
    }
}

/// The kind of a change of a message file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Created,
    Modified,
    Removed,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Created => "created",
            Kind::Modified => "modified",
            Kind::Removed => "removed",
        }
    }
}

/// One file event of a registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEvent {
    pub seq: u64,
    pub path: String,
    pub from: String,
    pub kind: Kind,
}

impl FileEvent {
    /// The members of the event body and of its log record, without the event name.
    pub fn fields(&self, registration: &str) -> Value {
        json!({"registration": registration, "path": self.path, "from": self.from, "kind": self.kind.name(), "seq": self.seq})
    }
}

/// What the watch of a registration reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Changed(FileEvent),
    /// The folder is gone, by removal or rename; the registration has ended.
    Closed,
    /// The watch reported an error, which the caller records.
    Failed(String),
}

/// The identity of a message file as last seen, so that a repeated report of the same content is not a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    inode: u64,
    length: u64,
    modified: (i64, i64),
}

fn stamp(path: &Path) -> Option<Stamp> {
    // silent: a file that cannot be described is absent, which the caller reports as removed when it was known.
    let metadata = std::fs::symlink_metadata(path).ok()?;
    metadata.is_file().then(|| Stamp {
        inode: metadata.ino(),
        length: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
    })
}

/// The folder registration of one surface.
pub struct Registration {
    id: String,
    folder: PathBuf,
    claims: Arc<FolderClaims>,
    // The canonical path of the folder, which the watch names in its reports (a temporary folder of macOS is under
    // `/var`, which the watch reports as `/private/var`).
    watched: PathBuf,
    // Kept for its life: dropping the watcher ends the watch.
    _watcher: notify::RecommendedWatcher,
    seq: u64,
    events: Vec<FileEvent>,
    stamps: HashMap<String, Stamp>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.claims.release(&self.folder);
    }
}

/// The reports of the watch of a registration, which the owner of the registration receives and gives to
/// [`Registration::apply`].
pub type Reports = mpsc::UnboundedReceiver<notify::Result<notify::Event>>;

/// A messages reply: the current registration, the messages after the cursor and the greatest `seq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Messages {
    pub body: Value,
    pub empty: bool,
}

impl Registration {
    /// Registers `folder`, an absolute path, for `surface`.
    pub fn register(
        claims: &Arc<FolderClaims>,
        surface: &str,
        folder: &Path,
    ) -> Result<(Self, Reports), Refusal> {
        // The page of a terminal has no file operation, so the sidecar creates the folder of the terminal and its missing parents
        // (core docs/spec/messaging.md#folder-of-a-terminal). A path that is a file stays a file, and the check below refuses it.
        if !folder.exists() {
            std::fs::create_dir_all(folder).map_err(|error| {
                Refusal::new(
                    "folder-create-failed",
                    format!("{}: {error}", folder.display()),
                )
            })?;
        }
        let metadata = std::fs::metadata(folder).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                Refusal::new("folder-missing", folder.display().to_string())
            }
            _ => Refusal::new("folder-missing", format!("{}: {error}", folder.display())),
        })?;
        if !metadata.is_dir() {
            return Err(Refusal::new(
                "not-a-directory",
                folder.display().to_string(),
            ));
        }
        let watched = std::fs::canonicalize(folder).map_err(|error| {
            Refusal::new("folder-missing", format!("{}: {error}", folder.display()))
        })?;
        claims.claim(folder, surface)?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let watcher = notify::recommended_watcher(move |event| {
            // silent: the receiver ends with its registration, after which no event has a reader.
            let _ = events_tx.send(event);
        })
        .and_then(|mut watcher| {
            watcher
                .watch(folder, RecursiveMode::NonRecursive)
                .map(|()| watcher)
        });
        let watcher = match watcher {
            Ok(watcher) => watcher,
            Err(error) => {
                claims.release(folder);
                return Err(Refusal::new(
                    "folder-missing",
                    format!("{}: the watch failed: {error}", folder.display()),
                ));
            }
        };
        let mut registration = Self {
            id: uuid::Uuid::new_v4().to_string(),
            folder: folder.to_path_buf(),
            claims: claims.clone(),
            watched,
            _watcher: watcher,
            seq: 0,
            events: Vec::new(),
            stamps: HashMap::new(),
        };
        // The files present at the registration are known, so a later change of one is a change and not a creation.
        for (name, _) in registration
            .message_names()
            .map_err(|error| Refusal::new("folder-missing", error))?
        {
            if let Some(found) = stamp(&registration.folder.join(&name)) {
                registration.stamps.insert(name, found);
            }
        }
        Ok((registration, events_rx))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The `.txt` names directly in the folder with their rule.
    fn message_names(&self) -> Result<Vec<(String, NameRule)>, String> {
        let entries = std::fs::read_dir(&self.folder)
            .map_err(|error| format!("{}: {error}", self.folder.display()))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", self.folder.display()))?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let rule = name_rule(&name);
            if rule != NameRule::Ignored {
                names.push((name, rule));
            }
        }
        names.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(names)
    }

    /// Applies the next report of the watch, None when the reports ended, and returns the signals it gives, in order.
    pub fn apply(&mut self, report: Option<notify::Result<notify::Event>>) -> Vec<Signal> {
        let Some(report) = report else {
            return vec![Signal::Failed(format!(
                "the watch of {} ended",
                self.folder.display()
            ))];
        };
        let event = match report {
            Ok(event) => event,
            Err(error) => {
                return vec![Signal::Failed(format!(
                    "the watch of {} failed: {error}",
                    self.folder.display()
                ))];
            }
        };
        if !self.folder.is_dir() {
            return vec![Signal::Closed];
        }
        let names: Vec<String> = if event.need_rescan() {
            let mut names: Vec<String> = self.stamps.keys().cloned().collect();
            match self.message_names() {
                Ok(found) => names.extend(found.into_iter().map(|(name, _)| name)),
                Err(error) => return vec![Signal::Failed(error)],
            }
            names
        } else {
            event
                .paths
                .iter()
                .filter(|path| path.parent() == Some(self.watched.as_path()))
                .filter_map(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                })
                .collect()
        };
        let mut signals = Vec::new();
        for name in names {
            let NameRule::Message { from } = name_rule(&name) else {
                continue;
            };
            let now = stamp(&self.folder.join(&name));
            let kind = match (self.stamps.get(&name).copied(), now) {
                (None, Some(found)) => {
                    self.stamps.insert(name.clone(), found);
                    Kind::Created
                }
                (Some(known), Some(found)) if known != found => {
                    self.stamps.insert(name.clone(), found);
                    Kind::Modified
                }
                (Some(_), None) => {
                    self.stamps.remove(&name);
                    Kind::Removed
                }
                _ => continue,
            };
            self.seq += 1;
            let event = FileEvent {
                seq: self.seq,
                path: name,
                from,
                kind,
            };
            self.events.push(event.clone());
            signals.push(Signal::Changed(event));
        }
        signals
    }

    /// The reply of `messages.read` and of an arrived `messages.wait`: the messages with a `seq` above `after`, read from
    /// the folder now. `registration` is the cursor epoch of the caller.
    pub fn read(&self, after: u64, registration: Option<&str>) -> Result<Messages, Refusal> {
        match registration {
            Some(given) if given != self.id => {
                return Err(Refusal::new(
                    "epoch-changed",
                    format!(
                        "the registration {given} is not the current registration {}",
                        self.id
                    ),
                ));
            }
            _ => {}
        }
        let names = self
            .message_names()
            .map_err(|error| Refusal::new("folder-missing", error))?;
        if let Some((name, _)) = names
            .iter()
            .find(|(_, rule)| *rule == NameRule::InvalidSender)
        {
            return Err(Refusal::new(
                "invalid-sender",
                self.folder.join(name).display().to_string(),
            ));
        }
        let mut messages = Vec::new();
        for event in self
            .events
            .iter()
            .filter(|event| event.seq > after && event.kind != Kind::Removed)
        {
            let path = self.folder.join(&event.path);
            let Ok(metadata) = std::fs::metadata(&path) else {
                // A message whose file is gone is not a message of the reply (core docs/spec/messaging.md#message).
                continue;
            };
            if metadata.len() > MESSAGE_LIMIT {
                return Err(Refusal::new(
                    "message-too-large",
                    path.display().to_string(),
                ));
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(Refusal::new(
                        "invalid-text",
                        format!("{}: {error}", path.display()),
                    ));
                }
            };
            if bytes.len() as u64 > MESSAGE_LIMIT {
                return Err(Refusal::new(
                    "message-too-large",
                    path.display().to_string(),
                ));
            }
            let text = String::from_utf8(bytes).map_err(|_| {
                Refusal::new("invalid-text", format!("{} is not UTF-8", path.display()))
            })?;
            messages.push(
                json!({"seq": event.seq, "path": event.path, "text": text, "from": event.from}),
            );
        }
        Ok(Messages {
            empty: messages.is_empty(),
            body: json!({"registration": self.id, "messages": messages, "last": self.seq}),
        })
    }
}
