//! The replay executable of the service (core docs/spec/replay.md#replay-executable).
//!
//! It reads the events of one session from the standard input, one JSON line each, applies the replay inputs (the theme, the
//! output of the program, the resizes, the selection and the viewport moves; a write to the program does not change the
//! screen) to an empty engine in the order of `sequence`, and writes `{"sequence": <n>, "hash": <hash>}` for each frame
//! record, with the screen hash of the engine at that sequence. The inputs are applied through the calls of the engine that
//! the service itself makes. An event with a `sequence` of a kind that this executable does not know is refused, and so are a
//! malformed line, a gap or a repeat in the sequences, a session that does not start with a theme and an input that breaks
//! the rules of its kind: the run ends with an error that names the line.

#[path = "engine.rs"]
// The replay binary uses the engine through the trait only.
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::{Map, Value};
use soksak_sidecar_vt_core::screen_hash::screen_hash;
use soksak_sidecar_vt_core::{parse_hex, Engine, TerminalTheme};
use std::io::{BufRead, Write};

/// An input of the session: the bytes the engine is fed with, a resize, a write to the program, or a view change.
enum Input {
    Output(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Write,
    Theme(Box<TerminalTheme>),
    Selection(Selection),
    Viewport(Viewport),
}

enum Selection {
    Start { edge: u16, row: u16 },
    Update { edge: u16, row: u16 },
    End,
    Clear,
}

enum Viewport {
    /// The change of the offset into the scrollback, positive toward older lines.
    Lines(i32),
    Newest,
}

impl Input {
    /// The kind of the contract (core docs/spec/replay.md#records), which names the input in a refusal.
    fn kind(&self) -> &'static str {
        match self {
            Input::Output(_) => "output",
            Input::Write => "write",
            Input::Resize { .. } => "resize",
            Input::Theme(_) => "theme",
            Input::Selection(_) => "selection",
            Input::Viewport(_) => "viewport",
        }
    }
}

/// The text of a value as the refusals of the contract show it: JSON, and `undefined` for a member that is missing.
fn shown(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_string(), Value::to_string)
}

/// An integer member from `minimum` to `maximum`, or the refusal of the contract.
fn integer(
    kind: &str,
    fields: &Value,
    name: &str,
    minimum: i64,
    maximum: i64,
) -> Result<i64, String> {
    fields[name]
        .as_i64()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| {
            format!(
                "{kind}: {name} must be an integer from {minimum}, found {}",
                shown(fields.get(name))
            )
        })
}

fn hex_bytes(kind: &str, fields: &Value) -> Result<Vec<u8>, String> {
    let refusal = || {
        format!(
            "{kind}: hex must be lowercase hexadecimal text of whole bytes, found {}",
            shown(fields.get("hex"))
        )
    };
    let text = fields["hex"].as_str().ok_or_else(refusal)?;
    if text.is_empty()
        || !text.len().is_multiple_of(2)
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(refusal());
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).map_err(|_| refusal()))
        .collect()
}

fn selection_input(fields: &Value) -> Result<Input, String> {
    let action = fields["action"].as_str().unwrap_or_default();
    match action {
        "start" | "update" => {
            let edge = integer("selection", fields, "edge", 0, i64::from(u16::MAX))? as u16;
            let row = integer("selection", fields, "row", 0, i64::from(u16::MAX))? as u16;
            Ok(Input::Selection(if action == "start" {
                Selection::Start { edge, row }
            } else {
                Selection::Update { edge, row }
            }))
        }
        "end" | "clear" => {
            if fields.get("edge").is_some() || fields.get("row").is_some() {
                return Err(format!("selection: {action} takes no edge and no row"));
            }
            Ok(Input::Selection(if action == "end" {
                Selection::End
            } else {
                Selection::Clear
            }))
        }
        _ => Err(format!(
            "selection: action must be start, update, end or clear, found {}",
            shown(fields.get("action"))
        )),
    }
}

fn viewport_input(fields: &Value) -> Result<Input, String> {
    match (fields.get("lines"), fields.get("newest")) {
        (Some(_), Some(_)) => Err("viewport: lines and newest are exclusive".to_string()),
        (None, None) => {
            Err("viewport: lines (a non-zero integer) or newest (true) is required".to_string())
        }
        (Some(lines), None) => lines
            .as_i64()
            .filter(|lines| *lines != 0)
            .and_then(|lines| i32::try_from(lines).ok())
            .map(|lines| Input::Viewport(Viewport::Lines(lines)))
            .ok_or_else(|| {
                format!(
                    "viewport: lines must be a non-zero integer, found {}",
                    shown(Some(lines))
                )
            }),
        (None, Some(newest)) => {
            if newest == &Value::Bool(true) {
                Ok(Input::Viewport(Viewport::Newest))
            } else {
                Err(format!(
                    "viewport: newest must be true, found {}",
                    shown(Some(newest))
                ))
            }
        }
    }
}

/// The color of the theme that a member of a theme input names: the four named colors and the 256 entries of the palette.
fn color_slot<'a>(theme: &'a mut TerminalTheme, name: &str) -> Option<&'a mut [u8; 3]> {
    match name {
        "foreground" => Some(&mut theme.foreground),
        "background" => Some(&mut theme.background),
        "cursor" => Some(&mut theme.cursor),
        "selection" => Some(&mut theme.selection),
        _ => {
            let index: usize = name.strip_prefix("ansi")?.parse().ok()?;
            // A name that is not the canonical decimal form of an index (`ansi07`, `ansi+7`) names no slot.
            (name == format!("ansi{index}")).then_some(())?;
            theme.palette.get_mut(index)
        }
    }
}

fn theme_input(fields: &Value) -> Result<Input, String> {
    let colors: &Map<String, Value> = fields["colors"].as_object().ok_or_else(|| {
        format!(
            "theme: colors must be an object of colors, found {}",
            shown(fields.get("colors"))
        )
    })?;
    for required in ["foreground", "background", "selection"] {
        if !colors.contains_key(required) {
            return Err(format!("theme: colors.{required} is required"));
        }
    }
    // The theme starts from the default theme of the service; the input states every color the service resolves.
    let mut theme = TerminalTheme::dark();
    for (name, value) in colors {
        let color = value
            .as_str()
            .filter(|text| !text.bytes().any(|byte| byte.is_ascii_uppercase()))
            .and_then(parse_hex)
            .ok_or_else(|| {
                format!(
                    "theme: colors.{name} must be \"#rrggbb\" in lowercase, found {}",
                    shown(Some(value))
                )
            })?;
        let slot = color_slot(&mut theme, name)
            .ok_or_else(|| format!("theme: colors.{name} is not a color of the theme"))?;
        *slot = color;
    }
    Ok(Input::Theme(Box::new(theme)))
}

/// The input of an event of the session, or none for an event that is no input.
fn input_of(event: &Value) -> Result<Option<(u64, Input)>, String> {
    let name = event["event"].as_str().unwrap_or_default();
    let fields = &event["fields"];
    let kind = match name {
        "sidecar.vt.pty_read" => "output",
        "sidecar.vt.pty_write" => "write",
        "sidecar.vt.resize" => "resize",
        "sidecar.vt.selection" => "selection",
        "sidecar.vt.viewport" => "viewport",
        "sidecar.vt.theme" => "theme",
        "sidecar.vt.frame" => return Ok(None),
        _ => {
            // An event that carries a sequence is part of the order of the inputs, so one that is not known is refused.
            return if fields.get("sequence").is_some() {
                Err(format!(
                    "the event {name} carries a sequence and is not a replay input that this executable knows"
                ))
            } else {
                Ok(None)
            };
        }
    };
    let sequence = integer(kind, fields, "sequence", 1, i64::MAX)? as u64;
    let input = match kind {
        "output" => Input::Output(hex_bytes(kind, fields)?),
        "write" => {
            hex_bytes(kind, fields)?;
            Input::Write
        }
        "resize" => {
            let rows = integer(kind, fields, "rows", 1, i64::from(u16::MAX))? as u16;
            let cols = integer(kind, fields, "cols", 1, i64::from(u16::MAX))? as u16;
            Input::Resize { rows, cols }
        }
        "selection" => selection_input(fields)?,
        "viewport" => viewport_input(fields)?,
        _ => theme_input(fields)?,
    };
    Ok(Some((sequence, input)))
}

fn run(input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    let mut inputs: Vec<(u64, Input)> = Vec::new();
    let mut frames: Vec<u64> = Vec::new();
    for (index, text) in input.lines().enumerate() {
        let line = index + 1;
        let text = text.map_err(|error| format!("line {line}: {error}"))?;
        let event: Value = serde_json::from_str(&text)
            .map_err(|error| format!("line {line}: not a JSON object: {error}"))?;
        if event["event"] == "sidecar.vt.frame" {
            let sequence = integer("frame", &event["fields"], "sequence", 1, i64::MAX)
                .map_err(|error| format!("line {line}: {error}"))?;
            frames.push(sequence as u64);
        }
        if let Some(input) = input_of(&event).map_err(|error| format!("line {line}: {error}"))? {
            inputs.push(input);
        }
    }
    inputs.sort_by_key(|(sequence, _)| *sequence);
    for (index, (sequence, _)) in inputs.iter().enumerate() {
        if *sequence != index as u64 + 1 {
            return Err(format!(
                "the inputs of the session have no sequence {} (found {sequence} after {} inputs)",
                index + 1,
                index
            ));
        }
    }
    if let Some((_, first)) = inputs.first() {
        if !matches!(first, Input::Theme(_)) {
            return Err(format!(
                "input 1 is a {} and the first input of a session is a theme",
                first.kind()
            ));
        }
    }
    frames.sort_unstable();
    let mut engine = AlacrittyEngine::new();
    let mut applied = 0usize;
    for frame in frames {
        if frame as usize > inputs.len() {
            return Err(format!(
                "a frame record names sequence {frame}, after the last input {}",
                inputs.len()
            ));
        }
        while applied < frame as usize {
            let (sequence, input) = &inputs[applied];
            apply(&mut engine, input)
                .map_err(|error| format!("input {sequence} is a {}: {error}", input.kind()))?;
            applied += 1;
        }
        let hash = screen_hash(&engine.screen())?;
        writeln!(
            output,
            "{}",
            serde_json::json!({"sequence": frame, "hash": hash})
        )
        .map_err(|error| format!("write the hash of sequence {frame}: {error}"))?;
    }
    Ok(())
}

/// Applies one input to the engine through the call that the service makes for it.
fn apply(engine: &mut AlacrittyEngine, input: &Input) -> Result<(), String> {
    match input {
        Input::Output(bytes) => {
            engine.feed(bytes);
            // The engine queues its answers and events; the surface takes them after each feed.
            engine.drain_events();
        }
        Input::Resize { rows, cols } => engine.resize(*cols, *rows),
        Input::Write => {}
        Input::Theme(theme) => engine.set_theme(**theme),
        Input::Selection(Selection::Start { edge, row }) => engine.selection_start(*edge, *row)?,
        Input::Selection(Selection::Update { edge, row }) => {
            engine.selection_update(*edge, *row)?
        }
        Input::Selection(Selection::End) => {
            engine.selection_end()?;
        }
        Input::Selection(Selection::Clear) => {
            engine.selection_clear();
        }
        Input::Viewport(Viewport::Lines(lines)) => engine.scroll_viewport(*lines),
        Input::Viewport(Viewport::Newest) => {
            engine.scroll_to_newest();
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match run(stdin.lock(), stdout.lock()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soksak-vt-replay: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
