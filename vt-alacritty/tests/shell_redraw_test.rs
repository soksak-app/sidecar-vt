// Checks that resizing a real shell input line leaves no rows of the previous width on the engine
// screen. As the service does, the engine is resized first, then the PTY, and the output the shell
// redraws is fed to the engine. The selector list of the engine file is used by engine_test.
#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::events::{EventLog, Options};
use soksak_sidecar_vt_core::pty::{PtyService, ShellOwner};
use soksak_sidecar_vt_core::{DaemonEvent, Engine, EngineEvent, Screen};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::time::{Duration, timeout};

// Runs one at a time with the PTY checks of the same workspace. The shell environment comes from
// the process environment.
fn native_pty_test_lock() -> std::fs::File {
    static LOCAL: OnceLock<Mutex<()>> = OnceLock::new();
    let _local = LOCAL.get_or_init(|| Mutex::new(())).lock().unwrap();
    let path = std::env::temp_dir().join("soksak-vt-core-pty-tests.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .expect("open native PTY test lock");
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(result, 0, "lock native PTY tests");
    file
}

/// Pads each row to the column count and joins them. Rows wrapped automatically join into one
/// string.
fn screen_text(screen: &Screen, cols: usize) -> String {
    screen
        .lines
        .iter()
        .map(|line| {
            let mut row: String = line
                .iter()
                .map(|cell| {
                    cell.ch
                        .as_deref()
                        .map_or_else(|| " ".repeat(cell.width.max(1) as usize), str::to_string)
                })
                .collect();
            while row.chars().count() < cols {
                row.push(' ');
            }
            row
        })
        .collect()
}

struct Session {
    service: PtyService,
    id: String,
    rx: tokio::sync::mpsc::Receiver<DaemonEvent>,
    engine: AlacrittyEngine,
    cols: u16,
    rows: u16,
    /// The errors that the engine reported.
    errors: Vec<String>,
}

impl Session {
    /// Keeps the errors among the events of the engine.
    fn drain_errors(&mut self) {
        let errors = self
            .engine
            .drain_events()
            .into_iter()
            .filter_map(|event| match event {
                EngineEvent::Error(error) => Some(error),
                _ => None,
            });
        self.errors.extend(errors);
    }

    /// Feeds the output to the engine and waits until the screen satisfies the predicate. Returns
    /// the screen text if it is not satisfied.
    async fn until(
        &mut self,
        predicate: impl Fn(&str, &AlacrittyEngine) -> bool,
    ) -> Result<(), String> {
        let cols = self.cols as usize;
        let reached = timeout(Duration::from_secs(10), async {
            loop {
                self.drain_errors();
                let text = screen_text(&self.engine.screen(), cols);
                if predicate(&text, &self.engine) {
                    return true;
                }
                match self.rx.recv().await {
                    Some(DaemonEvent::Output { data, .. }) => {
                        self.engine.feed(&data);
                    }
                    Some(_) => {}
                    None => return false,
                }
            }
        })
        .await
        .unwrap_or(false);
        if reached {
            Ok(())
        } else {
            Err(screen_text(&self.engine.screen(), cols)
                .trim_end()
                .to_string())
        }
    }

    fn resize(&mut self, cols: u16) {
        self.cols = cols;
        self.engine.resize(cols, self.rows);
        self.service.resize(&self.id, cols, self.rows).unwrap();
    }

    /// The output of the session that arrived, one read of the PTY for each element, without
    /// waiting for more.
    fn arrived_reads(&mut self) -> Vec<Vec<u8>> {
        let mut reads = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            if let DaemonEvent::Output { data, .. } = event {
                reads.push(data);
            }
        }
        reads
    }
}

fn open(shell: &str, variable: &str, directory: &Path) -> Session {
    // The writer of the events has a folder of its own; the folder goes away with the test
    // directory of the system.
    let logs = tempfile::tempdir().expect("create the folder of the events");
    let events = std::sync::Arc::new(
        EventLog::start(Options::service(
            logs.path().to_path_buf(),
            format!("redraw-{}", std::process::id()),
        ))
        .expect("start the writer of the events"),
    );
    let service = PtyService::new(events).with_environment(vec![(
        variable.to_string(),
        Some(directory.as_os_str().to_os_string()),
    )]);
    let (tx, rx) = soksak_sidecar_vt_core::pty::output_channel();
    let opened = service.open_shell(
        ShellOwner {
            owner: "",
            surface: "",
        },
        shell,
        None,
        60,
        12,
        tx,
    );
    let id = opened.expect("open a shell session");
    let mut engine = AlacrittyEngine::new();
    engine.resize(60, 12);
    Session {
        service,
        id,
        rx,
        engine,
        cols: 60,
        rows: 12,
        errors: Vec::new(),
    }
}

fn startup_directory(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    for (file, content) in files {
        std::fs::write(directory.join(file), content).unwrap();
    }
    directory
}

/// Narrows and then widens an input line that spans two rows. At each step the prompt and the input
/// must appear joined once, and the cursor must be at the end of the input.
async fn resize_wrapped_input(mut session: Session) -> Result<(), String> {
    let input = format!("true START-{}-END", "x".repeat(50));
    let line = format!("P> {input}");
    session
        .until(|text, _| text.contains("P> "))
        .await
        .map_err(|screen| format!("no prompt: {screen:?}"))?;
    // The output row directly above the prompt must also remain unchanged after the resize.
    session
        .service
        .write(
            &session.id,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            &b"printf 'ABOVE\\n'\n"[..],
        )
        .unwrap();
    session
        .until(|text, _| text.contains("ABOVE") && text.matches("P> ").count() == 2)
        .await
        .map_err(|screen| format!("no output line: {screen:?}"))?;
    session
        .service
        .write(
            &session.id,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            input.as_bytes(),
        )
        .unwrap();
    // Each step changes the size once. If the size changes again before the shell redraws, readline
    // redraws at the previous width.
    for (step, cols) in [("before", 60u16), ("narrow", 30), ("wide", 60)] {
        if session.cols != cols {
            session.resize(cols);
        }
        let expected = line.clone();
        session
            .until(move |text, engine| {
                let Some(start) = text.find(&expected) else {
                    return false;
                };
                let end = start + expected.len();
                let cursor = engine.cursor();
                let width = usize::from(cols);
                let above = format!("{:width$}", "ABOVE");
                start >= width
                    && start % width == 0
                    && text[start - width..start] == above
                    && text.matches("START-").count() == 1
                    && usize::from(cursor.row) == end / usize::from(cols)
                    && usize::from(cursor.col) == end % usize::from(cols)
            })
            .await
            .map_err(|screen| format!("{step} at {cols} columns: {screen:?}"))?;
    }
    let _ = session.service.close(&session.id);
    Ok(())
}

/// The rows of a screen text of `cols` columns without their trailing blanks.
fn text_rows(text: &str, cols: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(cols)
        .map(|row| row.iter().collect::<String>().trim_end().to_string())
        .collect()
}

/// The text of the output line above the prompt, which the alias `show` of the startup files prints.
const ABOVE: &str = "ABOVE-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-END";
/// The input line that the test types at the second prompt.
const INPUT: &str = "true START-xxxxxxxxxxxxxxxxxxxx-END";

/// The rows that a screen of `cols` columns shows: the first prompt of two lines with the command
/// `show`, the output line, the second prompt with the input, and empty rows.
fn two_line_prompt_rows(cols: usize, rows: usize) -> Vec<String> {
    let wrap = |line: &str| -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        chars
            .chunks(cols)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect()
    };
    let mut expected = vec!["LINE-ONE".to_string(), "P> show".to_string()];
    expected.extend(wrap(ABOVE));
    expected.push("LINE-ONE".to_string());
    expected.extend(wrap(&format!("P> {INPUT}")));
    expected.resize(rows, String::new());
    expected
}

/// Narrows and then widens a terminal whose shell shows a prompt of two lines below an output line.
/// After the redraw of each step the screen must show each row once: the output line rewrapped at
/// the width, both prompts with their two lines, no empty or left-over row between them, an empty
/// history, and the cursor at the end of the input; the output line joins again exactly when the
/// width returns. The engine must report no error.
async fn resize_two_line_prompt(mut session: Session) -> Result<(), String> {
    session
        .until(|text, _| text.contains("P> "))
        .await
        .map_err(|screen| format!("no prompt: {screen:?}"))?;
    session
        .service
        .write(
            &session.id,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            &b"show\n"[..],
        )
        .unwrap();
    session
        .until(|text, _| text.contains("-END") && text.matches("P> ").count() == 2)
        .await
        .map_err(|screen| format!("no output line: {screen:?}"))?;
    session
        .service
        .write(
            &session.id,
            soksak_sidecar_vt_core::pty::WriteOrigin::Person,
            INPUT.as_bytes(),
        )
        .unwrap();
    for (step, cols) in [("before", 60u16), ("narrow", 30), ("wide", 60)] {
        if session.cols != cols {
            session.resize(cols);
        }
        let rows = usize::from(session.rows);
        let expected = two_line_prompt_rows(usize::from(cols), rows);
        let end = format!("P> {INPUT}").chars().count();
        let cursor_row = expected.len()
            - expected
                .iter()
                .rev()
                .position(|row| !row.is_empty())
                .unwrap()
            - 1;
        let reached = session
            .until(|text, engine| {
                let cursor = engine.cursor();
                text_rows(text, usize::from(cols)) == expected
                    && usize::from(cursor.row) == cursor_row
                    && usize::from(cursor.col) == end % usize::from(cols)
            })
            .await;
        let errors = std::mem::take(&mut session.errors);
        if let Err(screen) = reached {
            return Err(format!(
                "{step} at {cols} columns: {screen:?}, expected {expected:?}, errors {errors:?}"
            ));
        }
        let history = session.engine.scrollback().1;
        if history != 0 || !errors.is_empty() {
            return Err(format!(
                "{step} at {cols} columns: {history} history rows, errors {errors:?}"
            ));
        }
    }
    let _ = session.service.close(&session.id);
    Ok(())
}

/// The prompt of the zsh drag test, a home folder path as zsh shows it, in the form of PS1.
const PATH_PROMPT: &str = "~/ai/test %% ";
/// The prompt as zsh draws it; the cursor stands after it.
const PATH_PROMPT_TEXT: &str = "~/ai/test % ";

/// The state that the service can present after a resize or after a read of the PTY: a
/// description of the step, the text of the cursor row and the cursor. The cursor is off the
/// prompt when the cursor row shows the prompt and the cursor does not stand on the cell after it.
struct PresentedState {
    step: String,
    row: String,
    cursor: (u16, u16),
}

impl PresentedState {
    fn of(step: String, engine: &mut AlacrittyEngine) -> Self {
        let screen = engine.screen();
        let cursor = engine.cursor();
        let row = screen.lines[usize::from(cursor.row)]
            .iter()
            .map(|cell| cell.ch.as_deref().unwrap_or(" "))
            .collect::<String>()
            .trim_end()
            .to_string();
        PresentedState {
            step,
            row,
            cursor: (cursor.row, cursor.col),
        }
    }

    fn cursor_off_the_prompt(&self) -> bool {
        self.row == PATH_PROMPT_TEXT.trim_end()
            && usize::from(self.cursor.1) != PATH_PROMPT_TEXT.chars().count()
    }
}

/// Drags the divider of a zsh terminal at its prompt through widths wider than the prompt, as the
/// service handles a drag: each resize is presented, and each read of the PTY that arrived is fed
/// to the engine and presented, so every state between a resize and the redraw of zsh is a frame
/// (core docs/spec/terminal-state.md#prompt-region). No presented state may show the prompt with
/// the cursor off the cell after it, where zsh left it.
async fn drag_at_path_prompt(mut session: Session) -> Result<(), String> {
    let prompt = PATH_PROMPT_TEXT.to_string();
    let width = PATH_PROMPT_TEXT.chars().count() as u16;
    session
        .until(|text, engine| {
            text.starts_with(&prompt) && engine.cursor().col == width && engine.cursor().row == 0
        })
        .await
        .map_err(|screen| format!("no prompt: {screen:?}"))?;
    let mut widths: Vec<u16> = (14..60).rev().step_by(3).collect();
    widths.extend((14..=60).step_by(3));
    widths.extend([59, 60]);
    let mut states = Vec::new();
    for cols in widths {
        session.resize(cols);
        states.push(PresentedState::of(
            format!("resize to {cols}"),
            &mut session.engine,
        ));
        tokio::task::yield_now().await;
        for read in session.arrived_reads() {
            session.engine.feed(&read);
            states.push(PresentedState::of(
                format!("read {:?} at {cols}", String::from_utf8_lossy(&read)),
                &mut session.engine,
            ));
        }
    }
    // The drag ends at the width of the start; the screen settles when zsh has drawn its prompt
    // again with the cursor after it.
    session
        .until(|text, engine| {
            text.starts_with(&prompt) && engine.cursor().col == width && engine.cursor().row == 0
        })
        .await
        .map_err(|screen| format!("no prompt after the drag: {screen:?}"))?;
    let off: Vec<String> = states
        .iter()
        .filter(|state| state.cursor_off_the_prompt())
        .map(|state| {
            format!(
                "{}: row {:?}, cursor {:?}",
                state.step, state.row, state.cursor
            )
        })
        .collect();
    let errors = std::mem::take(&mut session.errors);
    let _ = session.service.close(&session.id);
    if !off.is_empty() || !errors.is_empty() {
        return Err(format!(
            "{} of {} presented states show the cursor off the prompt:\n{}\nerrors {errors:?}",
            off.len(),
            states.len(),
            off.join("\n")
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_presented_state_of_a_drag_keeps_the_zsh_cursor_after_its_prompt() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory(
        "soksak-zsh-drag-test",
        &[(".zshrc", &format!("PS1='{PATH_PROMPT}'\n"))],
    );
    let result = drag_at_path_prompt(open("/bin/zsh", "ZDOTDIR", &directory)).await;
    let _ = std::fs::remove_dir_all(&directory);
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_redraws_a_wrapped_zsh_input_line_without_rows_of_the_previous_width() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory("soksak-zsh-redraw-test", &[(".zshrc", "PS1='P> '\n")]);
    let result = resize_wrapped_input(open("/bin/zsh", "ZDOTDIR", &directory)).await;
    let _ = std::fs::remove_dir_all(&directory);
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_redraws_a_wrapped_bash_input_line_without_rows_of_the_previous_width() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory(
        "soksak-bash-redraw-test",
        &[(
            ".bash_profile",
            "export BASH_SILENCE_DEPRECATION_WARNING=1\nPS1='P> '\n",
        )],
    );
    let result = resize_wrapped_input(open("/bin/bash", "HOME", &directory)).await;
    let _ = std::fs::remove_dir_all(&directory);
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_keeps_one_zsh_prompt_of_two_lines_and_rejoins_the_output_above_it() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory(
        "soksak-zsh-two-line-prompt-test",
        &[(
            ".zshrc",
            &format!("PS1=$'LINE-ONE\\nP> '\nalias show='print -r -- {ABOVE}'\n"),
        )],
    );
    let result = resize_two_line_prompt(open("/bin/zsh", "ZDOTDIR", &directory)).await;
    let _ = std::fs::remove_dir_all(&directory);
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resize_keeps_one_bash_prompt_of_two_lines_and_rejoins_the_output_above_it() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory(
        "soksak-bash-two-line-prompt-test",
        &[(
            ".bash_profile",
            &format!(
                "export BASH_SILENCE_DEPRECATION_WARNING=1\nPS1='LINE-ONE\\nP> '\nalias show='printf \"%s\\n\" {ABOVE}'\n"
            ),
        )],
    );
    let result = resize_two_line_prompt(open("/bin/bash", "HOME", &directory)).await;
    let _ = std::fs::remove_dir_all(&directory);
    result.unwrap();
}
