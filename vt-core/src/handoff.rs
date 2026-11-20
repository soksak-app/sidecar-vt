//! The hand-off record of a task (core docs/spec/messaging.md#hand-off-record): the file `<task>.handoff.json` directly in a
//! registered folder, which holds the current attempt of the task and its state. The module reads the record, judges each
//! change against the current attempt and writes the new record under a temporary name and a rename, as a message is written.
//! It holds no timer and no lock.

use crate::messaging::Refusal;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The state of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Assigned,
    Started,
    Result,
    Integrated,
    Abandoned,
}

impl State {
    /// The name of the state in the record.
    pub fn name(self) -> &'static str {
        match self {
            State::Assigned => "assigned",
            State::Started => "started",
            State::Result => "result",
            State::Integrated => "integrated",
            State::Abandoned => "abandoned",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [
            State::Assigned,
            State::Started,
            State::Result,
            State::Integrated,
            State::Abandoned,
        ]
        .into_iter()
        .find(|state| state.name() == name)
    }
}

/// A change of the current attempt; an assignment is [`assign`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Started,
    /// The result of the attempt, with its text.
    Result(String),
    Integrated,
    Abandoned,
}

impl Change {
    fn state(&self) -> State {
        match self {
            Change::Started => State::Started,
            Change::Result(_) => State::Result,
            Change::Integrated => State::Integrated,
            Change::Abandoned => State::Abandoned,
        }
    }
}

/// The record of a task: its current attempt, the state of that attempt and the text of its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub task: String,
    pub attempt: u64,
    pub state: State,
    pub result: Option<String>,
}

impl Record {
    fn body(&self) -> Value {
        json!({"task": self.task, "attempt": self.attempt, "state": self.state.name(), "result": self.result})
    }
}

fn refusal(code: &'static str, reason: String) -> Refusal {
    Refusal { code, reason }
}

/// The path of the record of `task` in `folder`; a name with `/`, `.` or NUL, or an empty name, is refused.
fn record_path(folder: &Path, task: &str) -> Result<PathBuf, Refusal> {
    if task.is_empty() || task.contains(['/', '.', '\0']) {
        return Err(refusal("invalid-task", format!("{task:?}")));
    }
    Ok(folder.join(format!("{task}.handoff.json")))
}

/// Reads the record of `task` from its file: `None` when the task has no record.
pub fn read(folder: &Path, task: &str) -> Result<Option<Record>, Refusal> {
    let path = record_path(folder, task)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(refusal(
                "invalid-record",
                format!("{}: {error}", path.display()),
            ));
        }
    };
    let invalid = |found: &str| refusal("invalid-record", format!("{}: {found}", path.display()));
    let value: Value = serde_json::from_str(&text).map_err(|error| invalid(&error.to_string()))?;
    let Some(members) = value.as_object() else {
        return Err(invalid(&format!(
            "a record must be an object, found {value}"
        )));
    };
    if members.len() != 4 {
        return Err(invalid(&format!(
            "a record has the members task, attempt, state and result, found {value}"
        )));
    }
    let named = members.get("task").and_then(Value::as_str);
    if named != Some(task) {
        return Err(invalid(&format!(
            "the task of the record is not {task}, found {value}"
        )));
    }
    let attempt = members
        .get("attempt")
        .and_then(Value::as_u64)
        .filter(|attempt| *attempt >= 1)
        .ok_or_else(|| {
            invalid(&format!(
                "the attempt is an integer of at least 1, found {value}"
            ))
        })?;
    let state = members
        .get("state")
        .and_then(Value::as_str)
        .and_then(State::named)
        .ok_or_else(|| {
            invalid(&format!(
                "the state is not one of the five states, found {value}"
            ))
        })?;
    let result = match (state, members.get("result")) {
        (State::Result | State::Integrated, Some(Value::String(text))) => Some(text.clone()),
        (State::Assigned | State::Started | State::Abandoned, Some(Value::Null)) => None,
        _ => {
            return Err(invalid(&format!(
                "the result is a text in the states result and integrated and null in the others, found {value}"
            )));
        }
    };
    Ok(Some(Record {
        task: task.to_string(),
        attempt,
        state,
        result,
    }))
}

/// Writes `record` under a temporary name in `folder` and renames it to its name, so a reader reads the earlier or the new record.
fn write(folder: &Path, record: &Record) -> Result<(), Refusal> {
    let path = record_path(folder, &record.task)?;
    let temporary = folder.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let failed = |error: std::io::Error| {
        refusal(
            "record-write-failed",
            format!("{}: {error}", path.display()),
        )
    };
    std::fs::write(&temporary, record.body().to_string()).map_err(failed)?;
    std::fs::rename(&temporary, &path).map_err(|error| {
        // The temporary file holds no record until the rename, so a failed rename removes it.
        if let Err(removal) = std::fs::remove_file(&temporary) {
            return refusal(
                "record-write-failed",
                format!(
                    "{}: {error}; the temporary file {} was not removed: {removal}",
                    path.display(),
                    temporary.display()
                ),
            );
        }
        failed(error)
    })
}

/// Assigns `task`: attempt 1 when it has no record, the next attempt when its current attempt is abandoned.
pub fn assign(folder: &Path, task: &str) -> Result<Record, Refusal> {
    let attempt = match read(folder, task)? {
        None => 1,
        Some(current) if current.state == State::Abandoned => current.attempt + 1,
        Some(current) => {
            return Err(refusal(
                "invalid-transition",
                format!(
                    "attempt {} of {task} is {} and has not been abandoned",
                    current.attempt,
                    current.state.name()
                ),
            ));
        }
    };
    let record = Record {
        task: task.to_string(),
        attempt,
        state: State::Assigned,
        result: None,
    };
    write(folder, &record)?;
    Ok(record)
}

/// Changes the current attempt of `task`, which `attempt` must name.
pub fn update(folder: &Path, task: &str, attempt: u64, change: Change) -> Result<Record, Refusal> {
    let Some(current) = read(folder, task)? else {
        return Err(refusal("unknown-task", format!("{task} has no record")));
    };
    if attempt != current.attempt {
        return Err(refusal(
            "stale-attempt",
            format!(
                "attempt {attempt} of {task} is not the current attempt {}",
                current.attempt
            ),
        ));
    }
    let to = change.state();
    let allowed = matches!(
        (current.state, to),
        (State::Assigned, State::Started)
            | (State::Started, State::Result)
            | (State::Result, State::Integrated)
            | (
                State::Assigned | State::Started | State::Result,
                State::Abandoned
            )
    );
    if !allowed {
        return Err(refusal(
            "invalid-transition",
            format!(
                "attempt {attempt} of {task} is {} and does not change to {}",
                current.state.name(),
                to.name()
            ),
        ));
    }
    let result = match change {
        Change::Result(text) => Some(text),
        Change::Integrated => current.result,
        Change::Started | Change::Abandoned => None,
    };
    let record = Record {
        task: task.to_string(),
        attempt,
        state: to,
        result,
    };
    write(folder, &record)?;
    Ok(record)
}
