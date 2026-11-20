//! The terminal state of a session, from the OSC 133 marks that the shell sends (core docs/spec/terminal-state.md).
//!
//! The marks map to three states: `idle` after the prompt start (`A`) or the prompt end (`B`), `running` after the command start
//! (`C`), and `exited` after the command finished (`D`) with the exit code of its first parameter, which the next `idle` keeps
//! until the next `running`. [`ShellStateTracker::apply`]
//! returns a [`Transition`] only when the state changes, so a repeated mark gives no change. The tracker depends on no engine:
//! each engine gives the marks as [`ShellMarker`] with their parameters, and calls [`exit_code`] to reject a malformed `D`.

use crate::protocol::ShellMarker;

/// The source of the states that the marks decide.
pub const SOURCE_OSC133: &str = "osc133";

/// A terminal state that the marks decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellState {
    Idle,
    Running,
    Exited,
}

impl ShellState {
    /// The name of the state in the events of the sidecar.
    pub fn name(self) -> &'static str {
        match self {
            ShellState::Idle => "idle",
            ShellState::Running => "running",
            ShellState::Exited => "exited",
        }
    }
}

/// The exit code of a command finished mark: the first parameter when it is present and is not a named parameter
/// `<name>=<value>`. It is a decimal integer from 0 to 255; any other first parameter is a malformed mark.
pub fn exit_code(params: &[String]) -> Result<Option<u8>, String> {
    let Some(first) = params.first() else {
        return Ok(None);
    };
    if first.contains('=') {
        return Ok(None);
    }
    if first.is_empty() || !first.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!(
            "OSC 133 exit code is not a decimal integer from 0 to 255: {first}"
        ));
    }
    first
        .parse::<u8>()
        .map(Some)
        .map_err(|_| format!("OSC 133 exit code is not a decimal integer from 0 to 255: {first}"))
}

/// A change of the state. `from` is None for the first change of the session; `exit` is the exit code of the last finished
/// command, held by `exited` and by the `idle` that follows it until the next `running`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: Option<ShellState>,
    pub to: ShellState,
    /// One more than the sequence of the change before it, starting at 1.
    pub sequence: u64,
    pub exit: Option<u8>,
}

/// The current state of a session: None before the first change, with sequence 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Current {
    pub state: Option<ShellState>,
    pub sequence: u64,
    pub exit: Option<u8>,
}

/// Tracks the state of the session of one surface. A session that differs from the tracked one starts a new state.
#[derive(Debug, Default)]
pub struct ShellStateTracker {
    session: Option<String>,
    state: Option<ShellState>,
    sequence: u64,
    exit: Option<u8>,
}

impl ShellStateTracker {
    /// Applies one mark of the session and returns the change that it causes, if any. A `D` whose exit code is malformed
    /// is an error and changes nothing.
    pub fn apply(
        &mut self,
        session: &str,
        marker: ShellMarker,
        params: &[String],
    ) -> Result<Option<Transition>, String> {
        self.follow(session);
        let (to, exit) = match marker {
            // The idle state after a finished command keeps its exit code until the next command starts, so that a page
            // that attaches later reads the failure of the last command (core docs/spec/terminal-state.md#change-event).
            ShellMarker::PromptStart | ShellMarker::PromptEnd => (ShellState::Idle, self.exit),
            ShellMarker::CommandStart => (ShellState::Running, None),
            ShellMarker::CommandFinished => (ShellState::Exited, exit_code(params)?),
        };
        if self.state == Some(to) {
            return Ok(None);
        }
        let from = self.state.replace(to);
        self.sequence += 1;
        self.exit = exit;
        Ok(Some(Transition {
            from,
            to,
            sequence: self.sequence,
            exit,
        }))
    }

    /// The current state of the session.
    pub fn current(&mut self, session: &str) -> Current {
        self.follow(session);
        Current {
            state: self.state,
            sequence: self.sequence,
            exit: self.exit,
        }
    }

    fn follow(&mut self, session: &str) {
        if self.session.as_deref() != Some(session) {
            self.session = Some(session.to_string());
            self.state = None;
            self.sequence = 0;
            self.exit = None;
        }
    }
}
