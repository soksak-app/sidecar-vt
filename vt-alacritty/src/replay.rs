//! The replay executable of the service (core docs/spec/replay.md#replay-executable).
//!
//! It reads the events of one session from the standard input, one JSON line each, applies the replay inputs (the output of
//! the program, the resizes; a write to the program does not change the screen) to an empty engine in the order of
//! `sequence`, and writes `{"sequence": <n>, "hash": <hash>}` for each frame record, with the screen hash of the engine at
//! that sequence. Any other event is ignored. A malformed line, a gap or a repeat in the sequences, or an input that lacks
//! its fields ends the run with an error that names the line.

#[path = "engine.rs"]
// The replay binary uses the engine through the trait only.
#[allow(dead_code)]
mod engine;

use engine::AlacrittyEngine;
use serde_json::Value;
use soksak_sidecar_vt_core::screen_hash::screen_hash;
use soksak_sidecar_vt_core::Engine;
use std::io::{BufRead, Write};

/// An input of the session: the bytes the engine is fed with, a resize, or a write to the program.
enum Input {
    Output(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Write,
}

fn decode_hex(text: &str, line: usize) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("line {line}: hex has an odd length"));
    }
    (0..text.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(&text[at..at + 2], 16)
                .map_err(|_| format!("line {line}: hex holds a character that is not a digit"))
        })
        .collect()
}

fn sequence_of(event: &Value, line: usize) -> Result<u64, String> {
    event["fields"]["sequence"]
        .as_u64()
        .filter(|sequence| *sequence >= 1)
        .ok_or_else(|| format!("line {line}: fields.sequence must be an integer from 1"))
}

fn dimension(event: &Value, name: &str, line: usize) -> Result<u16, String> {
    event["fields"][name]
        .as_u64()
        // silent: a value that does not fit u16 becomes None, and the ok_or_else after the filter reports the field.
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("line {line}: fields.{name} must be a positive integer"))
}

fn run(input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    let mut inputs: Vec<(u64, Input)> = Vec::new();
    let mut frames: Vec<u64> = Vec::new();
    for (index, text) in input.lines().enumerate() {
        let line = index + 1;
        let text = text.map_err(|error| format!("line {line}: {error}"))?;
        let event: Value = serde_json::from_str(&text)
            .map_err(|error| format!("line {line}: not a JSON object: {error}"))?;
        match event["event"].as_str() {
            Some("sidecar.vt.pty_read") => {
                let hex = event["fields"]["hex"]
                    .as_str()
                    .ok_or_else(|| format!("line {line}: fields.hex must be a string"))?;
                inputs.push((
                    sequence_of(&event, line)?,
                    Input::Output(decode_hex(hex, line)?),
                ));
            }
            Some("sidecar.vt.resize") => inputs.push((
                sequence_of(&event, line)?,
                Input::Resize {
                    rows: dimension(&event, "rows", line)?,
                    cols: dimension(&event, "cols", line)?,
                },
            )),
            Some("sidecar.vt.pty_write") => inputs.push((sequence_of(&event, line)?, Input::Write)),
            Some("sidecar.vt.frame") => frames.push(sequence_of(&event, line)?),
            _ => {}
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
            match &inputs[applied].1 {
                Input::Output(bytes) => {
                    engine.feed(bytes);
                    // The engine queues its answers and events; the surface takes them after each feed.
                    engine.drain_events();
                }
                Input::Resize { rows, cols } => engine.resize(*cols, *rows),
                Input::Write => {}
            }
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
