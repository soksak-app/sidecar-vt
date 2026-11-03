// Checks that resizing a real shell input line leaves no rows of the previous width on the engine
// screen. As the service does, the engine is resized first, then the PTY, and the output the shell
// redraws is fed to the engine. The selector list of the engine file is used by engine_test.
#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::events::{EventLog, Options};
use soksak_sidecar_vt_core::pty::PtyService;
use soksak_sidecar_vt_core::{DaemonEvent, Engine, Screen};
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
}

impl Session {
    /// Feeds the output to the engine and waits until the screen satisfies the predicate. Returns
    /// the screen text if it is not satisfied.
    async fn until(
        &mut self,
        predicate: impl Fn(&str, &AlacrittyEngine) -> bool,
    ) -> Result<(), String> {
        let cols = self.cols as usize;
        let reached = timeout(Duration::from_secs(10), async {
            loop {
                let text = screen_text(&self.engine.screen(), cols);
                if predicate(&text, &self.engine) {
                    return true;
                }
                match self.rx.recv().await {
                    Some(DaemonEvent::Output { data, .. }) => {
                        self.engine.feed(&data);
                        self.engine.drain_events();
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
    let opened = service.open_shell("", shell, None, 60, 12, tx);
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
        .write(&session.id, &b"printf 'ABOVE\\n'\n"[..])
        .unwrap();
    session
        .until(|text, _| text.contains("ABOVE") && text.matches("P> ").count() == 2)
        .await
        .map_err(|screen| format!("no output line: {screen:?}"))?;
    session
        .service
        .write(&session.id, input.as_bytes())
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
