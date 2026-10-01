// 실제 셸의 입력 행을 크기 변경했을 때 엔진 화면에 이전 폭의 행이 남지 않는지 검사한다.
// 서비스처럼 엔진 크기를 먼저 바꾸고 PTY 크기를 바꾼 뒤, 셸이 다시 그린 출력을 엔진에 넣는다.
// 엔진 파일의 선택자 목록은 engine_test 가 쓴다.
#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::pty::PtyService;
use soksak_sidecar_vt_core::{DaemonEvent, Engine, Screen};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{timeout, Duration};

// 같은 워크스페이스의 PTY 검사들과 한 번에 하나씩 실행한다. 셸 환경은 프로세스 환경에서 온다.
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

/// 행을 열 수까지 채워 이어 붙인다. 자동 줄바꿈 행은 하나의 문자열로 이어진다.
fn screen_text(screen: &Screen, cols: usize) -> String {
    screen
        .lines
        .iter()
        .map(|line| {
            let mut row: String = line
                .iter()
                .map(|cell| {
                    cell.ch
                        .clone()
                        .unwrap_or_else(|| " ".repeat(cell.width.max(1) as usize))
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
    rx: UnboundedReceiver<DaemonEvent>,
    engine: AlacrittyEngine,
    cols: u16,
    rows: u16,
}

impl Session {
    /// 출력을 엔진에 넣으며 화면이 조건을 만족할 때까지 기다린다. 만족하지 않으면 화면 글자를 돌려준다.
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

fn open(shell: &str, variable: &str, directory: &PathBuf) -> Session {
    let original = std::env::var_os(variable);
    std::env::set_var(variable, directory);
    let service = PtyService::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let opened = service.open_shell("", shell, None, 60, 12, tx);
    match original {
        Some(value) => std::env::set_var(variable, value),
        None => std::env::remove_var(variable),
    }
    let (id, _) = opened.expect("open a shell session");
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

/// 두 행에 걸친 입력 행을 좁혔다가 넓힌다. 각 단계에서 프롬프트와 입력이 한 번만 이어져 있고 커서가 입력 끝에 있어야 한다.
async fn resize_wrapped_input(mut session: Session) -> Result<(), String> {
    let input = format!("true START-{}-END", "x".repeat(50));
    let line = format!("P> {input}");
    session
        .until(|text, _| text.contains("P> "))
        .await
        .map_err(|screen| format!("no prompt: {screen:?}"))?;
    // 프롬프트 바로 위의 출력 행도 크기 변경 뒤 그대로 있어야 한다.
    session
        .service
        .write(&session.id, b"printf 'ABOVE\\n'\n")
        .unwrap();
    session
        .until(|text, _| text.contains("ABOVE") && text.matches("P> ").count() == 2)
        .await
        .map_err(|screen| format!("no output line: {screen:?}"))?;
    session
        .service
        .write(&session.id, input.as_bytes())
        .unwrap();
    // 각 단계는 크기를 한 번 바꾼다. 셸이 다시 그리기 전에 크기가 다시 바뀌면 readline 이 지난 폭으로 그린다.
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

#[tokio::test]
async fn a_resize_redraws_a_wrapped_zsh_input_line_without_rows_of_the_previous_width() {
    let _lock = native_pty_test_lock();
    let directory = startup_directory("soksak-zsh-redraw-test", &[(".zshrc", "PS1='P> '\n")]);
    let result = resize_wrapped_input(open("/bin/zsh", "ZDOTDIR", &directory)).await;
    let _ = std::fs::remove_dir_all(&directory);
    result.unwrap();
}

#[tokio::test]
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
