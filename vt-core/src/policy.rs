//! The policy of the event writer (core docs/spec/logging.md#classes-and-policy and #policy-file).
//!
//! The policy says, for each of the five classes, the least severe level that is stored. [`parse_policy`] reads the text of
//! the policy file with the rules and the error texts of the contract, and [`Gate`] holds the policy in force for the
//! producers: one atomic read per question, and a count for each event that the policy refused.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::events::Level;

/// The class of an event: how it is stored and which setting selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Lifecycle,
    Input,
    Call,
    Io,
    Frame,
}

impl Class {
    /// The classes in the order of the contract.
    pub const ALL: [Class; 5] = [
        Class::Lifecycle,
        Class::Input,
        Class::Call,
        Class::Io,
        Class::Frame,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Class::Lifecycle => "lifecycle",
            Class::Input => "input",
            Class::Call => "call",
            Class::Io => "io",
            Class::Frame => "frame",
        }
    }

    pub fn from_name(name: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|class| class.name() == name)
    }
}

/// The names of the six values of a class, from the one that stores nothing to the one that stores everything.
const VALUES: &str = "off, error, warn, info, debug, trace";

/// The level of every class: `None` stores no event of the class, a level stores that level and every less verbose one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    levels: [Option<Level>; 5],
}

impl Default for Policy {
    /// The policy in force before the first policy and while there is no file: every class at `error`
    /// (docs/spec/logging.md#policy).
    fn default() -> Self {
        Self {
            levels: [Some(Level::Error); 5],
        }
    }
}

impl Policy {
    /// This policy with the value of `class` replaced; `None` is `off`.
    pub fn with(mut self, class: Class, level: Option<Level>) -> Self {
        self.levels[class as usize] = level;
        self
    }

    /// Whether an event of `class` at `level` is stored.
    pub fn allows(&self, class: Class, level: Level) -> bool {
        self.levels[class as usize].is_some_and(|stored| level <= stored)
    }

    /// The value of every class as the policy file and the event `log.policy` state it.
    pub fn classes(&self) -> Value {
        let mut classes = serde_json::Map::new();
        for class in Class::ALL {
            let value = self.levels[class as usize].map_or("off", Level::name);
            classes.insert(class.name().to_string(), json!(value));
        }
        Value::Object(classes)
    }
}

/// Whether `text` is a file that its writer has truncated and is still writing: empty, or white space only. A reader waits
/// for the next notification and reports nothing.
pub fn is_being_written(text: &str) -> bool {
    text.trim().is_empty()
}

/// The text of a value as the error messages of the contract show it: JSON, and nothing for a member that is missing.
fn shown(value: Option<&Value>) -> String {
    value.map_or_else(String::new, Value::to_string)
}

/// Reads the text of the policy file `file`. A text that breaks a rule is refused with the error of the contract, which
/// names the file and what was found.
pub fn parse_policy(file: &str, text: &str) -> Result<Policy, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("{file}: not JSON: {error}"))?;
    let Value::Object(members) = value else {
        return Err(format!(
            "{file}: not JSON: expected an object, found {value}"
        ));
    };
    let mut names: Vec<&String> = members.keys().collect();
    names.sort();
    if let Some(name) = names
        .into_iter()
        .find(|name| !matches!(name.as_str(), "schema" | "classes"))
    {
        return Err(format!(
            "{file}: the member {name} is not one of schema, classes"
        ));
    }
    let schema = members.get("schema");
    match schema.and_then(Value::as_f64) {
        Some(1.0) => {}
        Some(number) if number.fract() == 0.0 => {
            return Err(format!("{file}: schema {} is not 1", shown(schema)));
        }
        _ => {
            return Err(format!(
                "{file}: schema must be the number 1, found {}",
                shown(schema)
            ));
        }
    }
    let Some(Value::Object(classes)) = members.get("classes") else {
        return Err(format!(
            "{file}: classes must be an object that names every class"
        ));
    };
    let mut names: Vec<&String> = classes.keys().collect();
    names.sort();
    if let Some(name) = names
        .into_iter()
        .find(|name| Class::from_name(name).is_none())
    {
        return Err(format!(
            "{file}: the class {name} is not one of lifecycle, input, call, io, frame"
        ));
    }
    let mut policy = Policy::default();
    for class in Class::ALL {
        let value = classes
            .get(class.name())
            .ok_or_else(|| format!("{file}: the class {} is missing", class.name()))?;
        let name = value.as_str().ok_or_else(|| {
            format!(
                "{file}: the level of the class {} must be text, found {value}",
                class.name()
            )
        })?;
        let level = match name {
            "off" => None,
            other => Some(Level::from_name(other).ok_or_else(|| {
                format!(
                    "{file}: the level {value} of the class {} is not one of {VALUES}",
                    class.name()
                )
            })?),
        };
        policy = policy.with(class, level);
    }
    Ok(policy)
}

/// The policy in force for the producers of one writer, and the count of the events it refused.
pub struct Gate {
    /// For each class the rank of the least severe stored level: 0 stores nothing, `Level::rank` of a level stores it.
    ranks: [AtomicU8; 5],
    /// The refused events since they were last taken, for each class and level.
    refused: [[AtomicU64; 5]; 5],
}

impl Gate {
    pub fn new(policy: &Policy) -> Self {
        let gate = Self {
            ranks: std::array::from_fn(|_| AtomicU8::new(0)),
            refused: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        };
        gate.set(policy);
        gate
    }

    /// Puts `policy` in force.
    pub fn set(&self, policy: &Policy) {
        for class in Class::ALL {
            let rank = policy.levels[class as usize].map_or(0, Level::rank);
            self.ranks[class as usize].store(rank, Ordering::Relaxed);
        }
    }

    /// Whether an event of `class` at `level` is stored; it counts nothing.
    pub fn enabled(&self, class: Class, level: Level) -> bool {
        level.rank() <= self.ranks[class as usize].load(Ordering::Relaxed)
    }

    /// Whether an event of `class` at `level` is stored; an event that is refused is counted.
    pub fn allows(&self, class: Class, level: Level) -> bool {
        let allowed = self.enabled(class, level);
        if !allowed {
            self.refused[class as usize][level.rank() as usize - 1].fetch_add(1, Ordering::Relaxed);
        }
        allowed
    }

    /// Takes the counts of the refused events: the class, the level and the exact number since the earlier call.
    pub fn take_refused(&self) -> Vec<(Class, Level, u64)> {
        let mut counts = Vec::new();
        for class in Class::ALL {
            for level in Level::ALL {
                let count = self.refused[class as usize][level.rank() as usize - 1]
                    .swap(0, Ordering::Relaxed);
                if count > 0 {
                    counts.push((class, level, count));
                }
            }
        }
        counts
    }
}
