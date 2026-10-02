use crate::encoding::{self, Key};
use crate::inline_image::{Dimension, InlineImageCommand};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, MissedTickBehavior};

#[derive(Clone)]
struct OutputSink {
    sender: Arc<tokio::sync::Mutex<Option<mpsc::Sender<String>>>>,
    /// 영속 서비스의 연결이다. 클라이언트의 출력이 닫히면 오류 대신 분리한다.
    detachable: bool,
}

impl OutputSink {
    fn direct(sender: mpsc::Sender<String>) -> Self {
        Self {
            sender: Arc::new(tokio::sync::Mutex::new(Some(sender))),
            detachable: false,
        }
    }

    /// 영속 서비스의 연결. 세션은 클라이언트가 끊겨도 유지되므로(docs/spec/terminal-runtime.md), 입력의 끝을
    /// 읽기 전에 출력 쪽이 먼저 닫힌 클라이언트도 분리한다. 다시 붙은 클라이언트는 현재 화면을 받는다.
    fn detachable(sender: mpsc::Sender<String>) -> Self {
        Self {
            sender: Arc::new(tokio::sync::Mutex::new(Some(sender))),
            detachable: true,
        }
    }

    async fn send(&self, message: String) -> Result<(), ()> {
        let sender = self.sender.lock().await.clone();
        match sender {
            Some(sender) => match sender.send(message).await {
                Ok(()) => Ok(()),
                Err(_) if self.detachable => {
                    let mut current = self.sender.lock().await;
                    if current
                        .as_ref()
                        .is_some_and(|current| current.same_channel(&sender))
                    {
                        *current = None;
                    }
                    Ok(())
                }
                Err(_) => Err(()),
            },
            None => Ok(()),
        }
    }

    async fn sender(&self) -> Option<mpsc::Sender<String>> {
        self.sender.lock().await.clone()
    }

    async fn replace_sender(&self, sender: Option<mpsc::Sender<String>>) {
        *self.sender.lock().await = sender;
    }

    async fn detach_sender(&self, previous: &Option<mpsc::Sender<String>>) {
        let mut current = self.sender.lock().await;
        if let (Some(sender), Some(previous)) = (current.as_ref(), previous.as_ref()) {
            if sender.same_channel(previous) {
                *current = None;
            }
        }
    }
}

struct PersistentEntry {
    tx: mpsc::Sender<SurfaceCommand>,
    output: OutputSink,
    actor: tokio::task::JoinHandle<()>,
    epoch: u64,
    owner: String,
}

pub struct PersistentRegistry {
    entries: tokio::sync::Mutex<HashMap<String, PersistentEntry>>,
    next_epoch: std::sync::atomic::AtomicU64,
    shutdown: AtomicBool,
}

const SURFACE_ACTOR_CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

async fn await_surface_actor(actor: tokio::task::JoinHandle<()>) -> Result<(), String> {
    match tokio::time::timeout(SURFACE_ACTOR_CLOSE_TIMEOUT, actor).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("surface actor join failed: {error}")),
        Err(_) => Err(format!(
            "surface actor close exceeded {:?}",
            SURFACE_ACTOR_CLOSE_TIMEOUT
        )),
    }
}

async fn close_surface_entries(
    entries: Vec<(String, PersistentEntry)>,
    operation: &str,
) -> Result<usize, String> {
    let count = entries.len();
    let mut errors = Vec::new();
    for (key, entry) in entries {
        if entry.tx.send(SurfaceCommand::SessionClose).await.is_err() {
            errors.push(format!("{key}: surface actor closed before {operation}"));
        }
        if let Err(error) = await_surface_actor(entry.actor).await {
            errors.push(format!("{key}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(count)
    } else {
        Err(errors.join("; "))
    }
}

impl PersistentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: tokio::sync::Mutex::new(HashMap::new()),
            next_epoch: std::sync::atomic::AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
        })
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    pub(crate) fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    async fn close_surface(&self, key: &str, owner: &str) -> Result<(), String> {
        let entry = {
            let mut entries = self.entries.lock().await;
            let Some(entry) = entries.get(key) else {
                return Ok(());
            };
            if entry.owner != owner {
                return Err("stale attachment".to_string());
            }
            entries.remove(key).expect("registry entry disappeared")
        };
        entry
            .tx
            .send(SurfaceCommand::SessionClose)
            .await
            .map_err(|_| "surface actor closed before close".to_string())?;
        let key = key.to_string();
        tokio::spawn(async move {
            if let Err(error) = await_surface_actor(entry.actor).await {
                eprintln!("persistent surface actor close failed for {key}: {error}");
            }
        });
        Ok(())
    }

    async fn close_owner(&self, owner: &str) -> Result<(), String> {
        let entries = {
            let mut registry = self.entries.lock().await;
            let keys = registry
                .iter()
                .filter_map(|(key, entry)| (entry.owner == owner).then_some(key.clone()))
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registry.remove(&key).map(|entry| (key, entry)))
                .collect::<Vec<_>>()
        };
        close_surface_entries(entries, "owner close")
            .await
            .map(|_| ())
    }

    /// 이 소유자의 세션 가운데 keep 에 없는 세션을 닫고 닫은 수를 반환한다. 앱이 다시 시작한 뒤, 어떤
    /// 레이아웃에도 없는 표면의 세션은 다시 붙을 곳이 없다(docs/spec/terminal-runtime.md).
    async fn retain(
        &self,
        owner: &str,
        keep: &std::collections::HashSet<String>,
    ) -> Result<usize, String> {
        let entries = {
            let mut registry = self.entries.lock().await;
            let keys = registry
                .iter()
                .filter_map(|(key, entry)| {
                    (entry.owner == owner && !keep.contains(key)).then_some(key.clone())
                })
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| registry.remove(&key).map(|entry| (key, entry)))
                .collect::<Vec<_>>()
        };
        close_surface_entries(entries, "retain").await
    }

    async fn current_epoch(&self, key: &str, owner: &str) -> Option<u64> {
        let entries = self.entries.lock().await;
        entries
            .get(key)
            .filter(|entry| entry.owner == owner)
            .map(|entry| entry.epoch)
    }

    async fn contains(&self, key: &str) -> bool {
        self.entries.lock().await.contains_key(key)
    }

    async fn attach(
        &self,
        key: &str,
        owner: &str,
        output: &OutputSink,
    ) -> Result<(mpsc::Sender<SurfaceCommand>, u64), String> {
        let mut entries = self.entries.lock().await;
        let entry = entries
            .get_mut(key)
            .ok_or_else(|| "session surface not found".to_string())?;
        if entry.owner != owner {
            return Err("stale attachment".to_string());
        }
        entry.epoch = self
            .next_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        entry.output.replace_sender(output.sender().await).await;
        Ok((entry.tx.clone(), entry.epoch))
    }

    async fn insert(
        &self,
        key: String,
        owner: String,
        tx: mpsc::Sender<SurfaceCommand>,
        output: OutputSink,
        actor: tokio::task::JoinHandle<()>,
    ) -> Result<u64, String> {
        let mut entries = self.entries.lock().await;
        if entries.contains_key(&key) {
            tx.send(SurfaceCommand::SessionClose)
                .await
                .map_err(|error| format!("close duplicate surface: {error}"))?;
            await_surface_actor(actor)
                .await
                .map_err(|error| format!("join duplicate surface actor: {error}"))?;
            return Err("session surface already exists".to_string());
        }
        let epoch = self
            .next_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        entries.insert(
            key,
            PersistentEntry {
                tx,
                output,
                actor,
                epoch,
                owner,
            },
        );
        Ok(epoch)
    }
}

/// 엔진이 구현할 트레이트. VT 처리 엔진의 계약.
pub trait Engine: Send + 'static {
    fn resize(&mut self, cols: u16, rows: u16);
    fn set_theme(&mut self, theme: crate::palette::TerminalTheme);
    fn set_cell_metrics(&mut self, width: u16, height: u16) -> Result<(), String>;
    fn feed(&mut self, bytes: &[u8]);
    fn drain_events(&mut self) -> Vec<EngineEvent>;
    fn resolve_clipboard(&mut self, request_id: u64, text: &str) -> Result<(), String>;
    fn reject_clipboard(&mut self, request_id: u64, reason: &str) -> Result<(), String>;
    /// cell 경계에서 selection을 시작한다. `edge`는 0부터 column 수까지이다. edge `c`는 column `c`의
    /// 왼쪽 경계이고, column 수는 마지막 column의 오른쪽 경계이다.
    fn selection_start(&mut self, edge: u16, row: u16) -> Result<(), String>;
    /// selection 끝을 cell 경계로 옮긴다. selection은 시작 경계와 끝 경계 사이의 cell을
    /// 포함한다. 두 경계가 같으면 아무것도 선택하지 않는다.
    fn selection_update(&mut self, edge: u16, row: u16) -> Result<(), String>;
    /// selection을 끝내고 그 text를 반환한다. text를 포함하지 않는 selection은 지워지고
    /// `None`을 반환한다. 이것은 정상 gesture이며 error가 아니다.
    fn selection_end(&mut self) -> Result<Option<String>, String>;
    /// 이전 선택을 복사하지 않고 지운다. 픽셀이 바뀌었는지 돌려준다.
    fn selection_clear(&mut self) -> bool;
    /// 현재 선택의 텍스트. 선택이 없거나 글자를 담지 않으면 `None` 이다.
    fn selection_text(&self) -> Option<String>;
    /// 기본 화면의 뷰포트를 lines 만큼 움직인다. 양수는 오래된 출력 쪽이다.
    fn scroll_viewport(&mut self, lines: i32);
    /// 뷰포트를 가장 새 출력으로 되돌리고, 움직였으면 true 를 반환한다.
    fn scroll_to_newest(&mut self) -> bool;
    fn cursor(&self) -> Cursor;
    fn screen(&mut self) -> Screen;
    fn scroll_generation(&self) -> i64 {
        0
    }
    /// 뷰포트가 가장 새 출력보다 위에 있는 줄 수. 스크롤백이 없는 엔진은 0 이다.
    fn viewport_offset(&self) -> u32 {
        0
    }
    fn modes(&self) -> Modes;
    fn reset(&mut self);
}

/// 엔진과 서비스 사이에서 전달하는 중립 이벤트.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// 인라인 그림 명령과, 그 시퀀스를 만났을 때의 커서 위치와 스크롤 세대.
    InlineImage {
        command: InlineImageCommand,
        anchor: InlineAnchor,
    },
    Title(String),
    ResetTitle,
    Directory {
        uri: String,
        /// 이 컴퓨터의 디렉터리이면 그 경로, 다른 컴퓨터의 디렉터리이면 None.
        path: Option<String>,
    },
    Hyperlink {
        id: String,
        uri: Option<String>,
    },
    Notification {
        message: String,
    },
    ShellState {
        marker: ShellMarker,
        params: Vec<String>,
    },
    ClipboardStore {
        selection: ClipboardSelection,
        text: String,
    },
    ClipboardQuery {
        request_id: u64,
        selection: ClipboardSelection,
    },
    PtyWrite(Vec<u8>),
    CursorBlinkingChange,
    Wakeup,
    Bell,
    Exit,
    ChildExit {
        success: bool,
        code: Option<i32>,
    },
    MouseCursorDirty,
    /// OSC 22 가 정한 포인터 모양. CSS cursor 값이다.
    PointerShape(String),
    Error(String),
}

/// 인라인 그림 시퀀스를 만났을 때의 커서 칸과 스크롤 세대. 그림은 이 자리에 놓인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InlineAnchor {
    pub col: u16,
    pub row: u16,
    pub scroll: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellMarker {
    PromptStart,
    PromptEnd,
    CommandStart,
    CommandFinished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    Clipboard,
    Selection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CursorBlinkPolicy {
    Never,
    #[default]
    Off,
    On,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnfocusedCursor {
    #[default]
    Hollow,
    Solid,
    Underline,
    Beam,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPolicy {
    pub shape: CursorShape,
    pub blink: CursorBlinkPolicy,
    pub interval_ms: u64,
    pub idle_timeout_ms: u64,
    pub unfocused: UnfocusedCursor,
}

impl Default for CursorPolicy {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: CursorBlinkPolicy::Off,
            interval_ms: 750,
            idle_timeout_ms: 5000,
            unfocused: UnfocusedCursor::Hollow,
        }
    }
}

/// 셀 하나의 속성
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cell {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ch: Option<String>,
    pub width: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub inverse: bool,
    /// OSC 8 하이퍼링크의 URI. 링크가 없는 셀은 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: None,
            width: 1,
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            inverse: false,
            link: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    #[serde(default)]
    pub shape: CursorShape,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub blinking: bool,
    #[serde(rename = "blinkVisible", default = "default_blink_visible")]
    pub blink_visible: bool,
    #[serde(default)]
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preedit: Option<Preedit>,
}

fn default_visible() -> bool {
    true
}

fn default_blink_visible() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRange {
    pub location: usize,
    pub length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    #[serde(
        rename = "selectedRange",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_range: Option<JsonRange>,
    #[serde(
        rename = "replacementRange",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub replacement_range: Option<JsonRange>,
    #[serde(default)]
    pub attributed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Modes {
    #[serde(default)]
    pub app_cursor: bool,
    #[serde(default)]
    pub app_keypad: bool,
    #[serde(default)]
    pub bracketed_paste: bool,
    /// ?1000: 누름과 뗌을 알린다.
    #[serde(default)]
    pub mouse_click: bool,
    /// ?1002: 누름과 뗌, 버튼을 누른 채 움직임을 알린다.
    #[serde(default)]
    pub mouse_drag: bool,
    /// ?1003: 누름과 뗌, 모든 움직임을 알린다.
    #[serde(default)]
    pub mouse_motion: bool,
    #[serde(default)]
    pub focus_in_out: bool,
    #[serde(default)]
    pub utf8_mouse: bool,
    #[serde(default)]
    pub sgr_mouse: bool,
    #[serde(default)]
    pub alternate_scroll: bool,
    #[serde(default)]
    pub alt_screen: bool,
}

/// 포인터 입력의 단계.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MousePhase {
    Down,
    Move,
    Up,
}

fn mouse_drag_motion_owned(gesture: bool, motion_owner: bool, pressed: bool) -> bool {
    gesture && motion_owner && pressed
}

impl Modes {
    /// 프로그램이 마우스 보고를 켰는지.
    pub fn mouse_report(&self) -> bool {
        self.mouse_click || self.mouse_drag || self.mouse_motion
    }
}

/// 뷰포트가 가장 새 출력보다 위에 있는 줄 수와 보관된 기록 줄 수.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Scrollback {
    pub offset: u32,
    pub history: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    #[serde(default)]
    pub scrollback: Scrollback,
    /// 현재 기본 배경색(`#rrggbb`). 프로그램이 OSC 11 로 바꾼 값을 포함한다. 래스터는 칸 밖 여백도 이 색으로 채운다.
    pub background: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImagePlacement {
    pub name: String,
    pub data: Vec<u8>,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub preserve_aspect_ratio: bool,
    pub anchor_row: i32,
    pub anchor_scroll: i64,
    pub visible: bool,
}

/// 데몬에서 받는 이벤트
#[derive(Debug, Clone)]
pub enum DaemonEvent {
    Output {
        session_id: String,
        data: Vec<u8>,
        sequence: i64,
        truncated: bool,
    },
    Exit {
        session_id: String,
    },
    Error {
        session_id: String,
        error: String,
    },
}

/// 세션 포트: 데몬과 통신하는 추상 인터페이스
/// 세션을 여는 요청의 셸과 시작 디렉터리.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellRequest {
    /// 터미널 설정 shell 의 값. `login` 이거나 셸의 절대 경로다.
    pub shell: String,
    /// 셸이 시작할 디렉터리의 절대 경로. 없으면 계정의 홈 디렉터리다.
    pub directory: Option<String>,
}

#[async_trait]
pub trait SessionPort: Send + Sync {
    async fn open(&self, request: &ShellRequest, cols: u16, rows: u16) -> Result<String, String>;
    async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String>;
    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String>;
    async fn detach(&self, session_id: &str) -> Result<(), String>;
    async fn close(&self, session_id: &str) -> Result<(), String>;
    /// 기존 session에 새 view를 붙이고 보존된 output을 재생한다.
    async fn attach(&self, _session_id: &str, _from: i64) -> Result<String, String> {
        Err("session attach is not supported".to_string())
    }
    /// 한 시점의 PTY 전송 상태 측정. pending 은 마스터가 쓰고 자식이 아직 읽지 않은 입력 바이트 수이고
    /// written 는 reader 가 읽은 자식 출력의 누적 바이트 수다. 측정 관측이므로 제공하지 않는 포트는
    /// 명시적으로 실패한다.
    async fn pty_measurement(
        &self,
        _session_id: &str,
    ) -> Result<crate::pty::PtyMeasurement, String> {
        Err("session PTY measurement is not supported".to_string())
    }
    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent>;
}

/// 호스트에서 받은 메시지 봉투
#[derive(Debug, Deserialize)]
struct Envelope {
    surface: String,
    #[allow(dead_code)]
    root: Option<String>,
    body: Option<Value>,
    closed: Option<bool>,
}

/// 표면 작업으로 보낼 명령
#[derive(Debug, Clone)]
enum SurfaceCommand {
    Open {
        image: Option<String>,
        request: ShellRequest,
    },
    Reconnect,
    Configure(ImageConfiguration),
    Input {
        bytes: Vec<u8>,
    },
    InputKeys {
        keys: Vec<InputKey>,
    },
    Paste {
        text: String,
    },
    Compose {
        preedit: Option<Preedit>,
    },
    Focus {
        focused: bool,
    },
    Theme {
        // 테마는 다른 명령보다 훨씬 크므로 명령 크기를 키우지 않도록 따로 둔다.
        theme: Box<crate::palette::TerminalTheme>,
    },
    Font {
        font: std::sync::Arc<crate::platform::TerminalFont>,
        system: bool,
        skipped: Vec<String>,
        /// 글꼴 크기(포인트).
        size: f32,
    },
    Cursor {
        policy: CursorPolicy,
    },
    ScreenRead,
    SessionClose,
    SessionDetach,
    ImageResponse {
        body: Value,
    },
    ClipboardResolve {
        request_id: u64,
        text: String,
    },
    ClipboardReject {
        request_id: u64,
        reason: String,
    },
    InlineImageDelete {
        name: String,
    },
    /// 페이지의 포인터 입력. 프로그램이 마우스 보고를 켰으면 보고로 바꾼다.
    Mouse {
        input_id: String,
        phase: MousePhase,
        x: f64,
        y: f64,
        pressed: bool,
        shift: bool,
        alt: bool,
        ctrl: bool,
    },
    SelectionStart {
        x: f64,
        y: f64,
    },
    SelectionUpdate {
        x: f64,
        y: f64,
    },
    SelectionEnd,
    /// 사용자의 복사 명령. 현재 선택의 텍스트를 copy 이벤트로 보낸다.
    Copy,
    /// 휠 스크롤. lines 는 0 이 아니며 양수는 오래된 출력 쪽이다. col, row 는 포인터 칸이다.
    Scroll {
        lines: i32,
        col: u16,
        row: u16,
    },
    /// 스크롤바 끌기. 기본 뷰포트를 이 오프셋으로 옮기며 프로그램에는 쓰지 않는다.
    Viewport {
        offset: u32,
    },
    /// 현재 세션에서 마스터가 쓰고 자식이 아직 읽지 않은 입력 바이트 수를 묻는 측정 연산.
    PtyPending,
}

use crate::platform::ImageConfiguration;

/// 입력 키 정보
#[derive(Debug, Clone, Deserialize)]
struct InputKey {
    key: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    shift: bool,
    #[serde(default)]
    alt: bool,
    #[serde(default)]
    ctrl: bool,
}

fn key_for_native_command(selector: &str) -> Result<InputKey, String> {
    let key = match selector {
        "insertNewline:" => "Enter",
        "deleteBackward:" => "Backspace",
        "deleteForward:" => "Delete",
        _ => return Err(format!("unsupported native command selector: {selector}")),
    };
    Ok(InputKey {
        key: key.to_string(),
        text: String::new(),
        shift: false,
        alt: false,
        ctrl: false,
    })
}

fn decorate_screen(
    mut screen: Screen,
    focused: bool,
    preedit: &Option<Preedit>,
    policy: &CursorPolicy,
    elapsed_ms: u64,
) -> Screen {
    let program_shape = screen.cursor.shape;
    if program_shape == CursorShape::Block {
        screen.cursor.shape = policy.shape;
    }
    screen.cursor.focused = focused;
    screen.cursor.blink_visible = crate::platform::darwin::frame::cursor_blink_visible(
        policy.blink,
        screen.cursor.blinking,
        focused,
        elapsed_ms,
        policy.interval_ms,
        policy.idle_timeout_ms,
    );
    if !focused {
        screen.cursor.shape = crate::platform::darwin::frame::effective_cursor_shape(
            screen.cursor.shape,
            false,
            policy.unfocused,
        );
    }
    screen.cursor.preedit = preedit.clone();
    screen
}

fn parse_cursor_policy(body: &Value) -> Result<CursorPolicy, String> {
    let defaults = CursorPolicy::default();
    let shape = match body.get("shape").and_then(Value::as_str) {
        None => defaults.shape,
        Some("block") => CursorShape::Block,
        Some("underline") => CursorShape::Underline,
        Some("beam") => CursorShape::Beam,
        Some(value) => {
            return Err(format!(
                "cursor.shape must be block, underline, or beam: {value}"
            ))
        }
    };
    let blink = match body.get("blink").and_then(Value::as_str) {
        None => defaults.blink,
        Some("Never") => CursorBlinkPolicy::Never,
        Some("Off") => CursorBlinkPolicy::Off,
        Some("On") => CursorBlinkPolicy::On,
        Some("Always") => CursorBlinkPolicy::Always,
        Some(value) => {
            return Err(format!(
                "cursor.blink must be Never, Off, On, or Always: {value}"
            ))
        }
    };
    let interval_ms = match body.get("interval") {
        None => defaults.interval_ms,
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| "cursor.interval must be a positive integer".to_string())?,
    };
    let idle_timeout_ms = match body.get("idleTimeout") {
        None => defaults.idle_timeout_ms,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| "cursor.idleTimeout must be a nonnegative integer".to_string())?,
    };
    let unfocused = match body.get("unfocused").and_then(Value::as_str) {
        None => defaults.unfocused,
        Some("hollow") => UnfocusedCursor::Hollow,
        Some("solid") => UnfocusedCursor::Solid,
        Some("underline") => UnfocusedCursor::Underline,
        Some("beam") => UnfocusedCursor::Beam,
        Some("unchanged") => UnfocusedCursor::Unchanged,
        Some(value) => return Err(format!("cursor.unfocused is invalid: {value}")),
    };
    Ok(CursorPolicy {
        shape,
        blink,
        interval_ms,
        idle_timeout_ms,
        unfocused,
    })
}

fn pixels_to_cells(pixels: u32, cell_size: f32) -> u16 {
    (pixels as f32 / cell_size) as u16
}

/// CSI 14t 는 글자 영역을 기기 픽셀로 알린다. 인라인 그림의 픽셀 크기도 같은 단위이므로, 프로그램이 14t 로 셀 크기를
/// 계산해 그림을 요청하면 그림이 셀에 맞는다.
fn set_engine_metrics(engine: &mut Box<dyn Engine>, state: &ImageState) -> Result<(), String> {
    let width = state.metrics.cell_width.round() as u16;
    let height = state.metrics.cell_height.round() as u16;
    engine.set_cell_metrics(width, height)
}

/// terminal 크기를 검증하고 계산한다. width/height가 유효하지 않거나 결과가 0이면 error를 반환한다.
fn calculate_terminal_size(
    width: u32,
    height: u32,
    metrics: &crate::platform::Metrics,
) -> Result<(u16, u16), String> {
    if width == 0 || height == 0 {
        return Err("width and height must be positive".to_string());
    }

    let cols = pixels_to_cells(width, metrics.cell_width);
    let rows = pixels_to_cells(height, metrics.cell_height);

    if cols == 0 || rows == 0 {
        return Err("width and height must be positive".to_string());
    }

    Ok((cols, rows))
}

/// 키 입력을 바이트로 인코딩한다
fn encode_keys(keys: &[InputKey], modes: &Modes) -> Result<Vec<u8>, String> {
    let mut all_bytes = Vec::new();

    for input_key in keys {
        let key_name = &input_key.key;
        let text = &input_key.text;

        // 수식자 비트 계산: shift=1, alt=2, ctrl=4
        let mut modifiers = 0u8;
        if input_key.shift {
            modifiers |= 1;
        }
        if input_key.alt {
            modifiers |= 2;
        }
        if input_key.ctrl {
            modifiers |= 4;
        }

        // key 이름을 Key 열거형으로 변환
        let key = match key_name.as_str() {
            "Up" => Key::Up,
            "Down" => Key::Down,
            "Left" => Key::Left,
            "Right" => Key::Right,
            "Home" => Key::Home,
            "End" => Key::End,
            "Insert" => Key::Insert,
            "Delete" => Key::Delete,
            "PageUp" => Key::PageUp,
            "PageDown" => Key::PageDown,
            "F1" => Key::F1,
            "F2" => Key::F2,
            "F3" => Key::F3,
            "F4" => Key::F4,
            "F5" => Key::F5,
            "F6" => Key::F6,
            "F7" => Key::F7,
            "F8" => Key::F8,
            "F9" => Key::F9,
            "F10" => Key::F10,
            "F11" => Key::F11,
            "F12" => Key::F12,
            "Enter" => Key::Enter,
            "Tab" => Key::Tab,
            "Backspace" => Key::Backspace,
            "Escape" => Key::Escape,
            "KeypadEnter" => Key::KeypadEnter,
            name if name.starts_with("Keypad") => match &name["Keypad".len()..] {
                digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                    Key::Keypad(digit.as_bytes()[0] as char)
                }
                "Decimal" => Key::Keypad('.'),
                "Plus" => Key::Keypad('+'),
                "Minus" => Key::Keypad('-'),
                "Multiply" => Key::Keypad('*'),
                "Divide" => Key::Keypad('/'),
                "Equals" => Key::Keypad('='),
                _ => return Err(format!("unknown key: {}", key_name)),
            },
            "Char" => {
                // "Char" 특수 처리
                if text.is_empty() {
                    return Err("unknown key: Char with empty text".to_string());
                }
                let ch = text.chars().next().unwrap();
                let encoded_bytes = if modifiers & 4 != 0 {
                    // ctrl 비트가 설정됨
                    encoding::encode_ctrl_char(ch).map_err(|error| {
                        format!(
                            "unknown key: Char with ctrl: {:?} (text {:?}, U+{:04X})",
                            error, ch, ch as u32
                        )
                    })?
                } else if modifiers & 2 != 0 {
                    // alt 비트가 설정됨
                    encoding::encode_alt_char(ch)
                } else {
                    // 일반 텍스트
                    encoding::encode_text(text)
                };
                all_bytes.extend_from_slice(&encoded_bytes);
                continue;
            }
            _ => {
                return Err(format!("unknown key: {}", key_name));
            }
        };

        // 표준 키를 인코딩
        let encoded_bytes = encoding::encode_key(key, modifiers, modes)
            .map_err(|e| format!("unknown key: {} ({:?})", key_name, e))?;
        all_bytes.extend_from_slice(&encoded_bytes);
    }

    Ok(all_bytes)
}

pub use crate::platform::ImageState;

/// 화면을 그림에 그려 봉투를 보내고 그림을 호스트에 넘긴다.
/// 그리기에 실패하면 오류 이벤트를 보내고 true 를 반환한다. 출력 통로가 닫혔으면 false 를 반환한다.
/// 글꼴 크기의 기본값과 범위(포인트). 기본 크기에 프레임과 카드의 글자 배율(각각 최대 3)을 곱한 값이 범위 안에
/// 든다(docs/spec/text-size.md).
const DEFAULT_FONT_SIZE: f32 = 13.0;
const FONT_SIZE_MIN: f64 = 4.0;
const FONT_SIZE_MAX: f64 = 128.0;

/// 페이지로 보내는 셀. 페이지는 글자, 폭, 링크, 켜진 속성만 쓰고 색은 래스터가 그린다. 기본값 필드를 셀마다
/// 보내면 글자로 찬 화면 하나가 수백 KB 가 되어 프레임마다 호스트와 페이지를 지난다.
#[derive(Serialize)]
struct PageCell<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    ch: Option<&'a str>,
    width: u8,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    underline: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    inverse: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<&'a str>,
}

fn page_lines(screen: &Screen) -> Vec<Vec<PageCell<'_>>> {
    screen
        .lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|cell| PageCell {
                    ch: cell.ch.as_deref(),
                    width: cell.width,
                    bold: cell.bold,
                    italic: cell.italic,
                    underline: cell.underline,
                    inverse: cell.inverse,
                    link: cell.link.as_deref(),
                })
                .collect()
        })
        .collect()
}

/// 화면 이벤트 본문. 뷰포트의 줄, 커서, 스크롤백 상태를 담는다.
fn screen_event(surface_id: &str, screen: &Screen) -> Value {
    json!({
        "surface": surface_id,
        "body": {
            "event": "screen",
            "cols": screen.cols,
            "rows": screen.rows,
            "cursor": screen.cursor,
            "lines": page_lines(screen),
            "scrollback": screen.scrollback,
            "background": screen.background
        }
    })
}

async fn present_screen(
    surface_id: &str,
    screen: &Screen,
    state: &mut ImageState,
    output_tx: &OutputSink,
    reason: &str,
    performance: &crate::performance::PerformanceTrace,
) -> bool {
    // 호스트는 표시 요청의 래스터를 복사한 뒤 consumed 로 답한다. 답을 받기 전에 같은 래스터에 다시 그리면
    // 복사 중인 픽셀을 덮어 글자가 빠진 프레임이 표시된다. 그 동안의 화면은 dirty 로 남겨 답을 받은 뒤 그린다.
    if state.pending_draw {
        state.dirty = true;
        return true;
    }
    let draw_started = std::time::Instant::now();
    if let Err(reason) = state.frame.draw_with_theme_and_inline_images(
        screen,
        &state.metrics,
        crate::platform::darwin::frame::CursorRender::from_protocol(&screen.cursor),
        &state.theme,
        &state.inline_images,
    ) {
        let response = json!({
            "surface": surface_id,
            "body": {"event": "error", "reason": reason}
        });
        if output_tx.send(response.to_string()).await.is_err() {
            return false;
        }
        // 그리기가 실패해도 상태 이벤트는 버리지 않는다. 오류가 그 화면에 그림이 없음을 알린다.
        for event in std::mem::take(&mut state.presentation_events) {
            if output_tx.send(event).await.is_err() {
                return false;
            }
        }
        return true;
    }
    // 프레임 한 장의 계기(V5-104): 무엇 때문에 그렸는지, 얼마나 걸렸는지, 어떤 래스터에.
    performance.line(
        "frame",
        serde_json::json!({
            "reason": reason,
            "surface": surface_id,
            "draw_us": draw_started.elapsed().as_micros() as u64,
            "raster": format!("{}x{}", state.width_px, state.height_px),
            "seq": state.sequence + 1,
        }),
    );
    state.sequence += 1;
    state.pending_draw = true;
    state.sent_at = Some(std::time::Instant::now());
    state.dirty = false;
    let nonce = state.frame.nonce();
    let nonce_b64 = base64_encode(&nonce);
    let image_envelope = json!({
        "surface": surface_id,
        "body": {
            "image": {
                "name": state.name.clone(),
                "generation": state.generation,
                "raster": state.raster,
                "token": {
                    "kind": "iosurface-global",
                    "id": state.frame.id(),
                    "nonce": nonce_b64
                },
                "width": state.width_px,
                "height": state.height_px,
                "scale": state.scale,
                "format": "bgra8",
                "sequence": state.sequence
            }
        }
    });
    // 페이지의 화면은 이 래스터가 그린 화면이다. 둘을 함께 보내야 커서와 글자 상태가 화면 픽셀과 같다.
    // 이 래스터가 그린 인라인 그림의 상태 이벤트도 그 뒤에 보낸다.
    if output_tx.send(image_envelope.to_string()).await.is_err()
        || output_tx
            .send(screen_event(surface_id, screen).to_string())
            .await
            .is_err()
    {
        return false;
    }
    for event in std::mem::take(&mut state.presentation_events) {
        if output_tx.send(event).await.is_err() {
            return false;
        }
    }
    true
}

/// 호스트가 앞 래스터를 복사하는 중인지. 그동안의 화면 변경은 dirty 로 남기고 그리거나 화면 JSON 을 보내지
/// 않는다. 출력이 몰릴 때 화면마다 JSON 을 보내면 페이지가 지난 화면을 처리하느라 입력과 스크롤이 밀린다.
/// 호스트가 답하면 그때의 화면을 한 번 그리고 보낸다.
fn hold_while_presenting(image_state: &mut Option<ImageState>) -> bool {
    match image_state.as_mut() {
        Some(state) if state.pending_draw => {
            state.dirty = true;
            true
        }
        _ => false,
    }
}

async fn send_state(
    surface_id: &str,
    session_id: &str,
    cols: u16,
    rows: u16,
    state: &ImageState,
    output_tx: &OutputSink,
) -> bool {
    let response = json!({
        "surface": surface_id,
        "body": {
            "event": "state", "sessionId": session_id, "cols": cols, "rows": rows,
            "cursor": {"col": 0, "row": 0},
            "cellWidth": state.metrics.cell_width / state.scale,
            "cellHeight": state.metrics.cell_height / state.scale
        }
    });
    output_tx.send(response.to_string()).await.is_ok()
}

fn clipboard_selection_name(selection: ClipboardSelection) -> &'static str {
    match selection {
        ClipboardSelection::Clipboard => "clipboard",
        ClipboardSelection::Selection => "selection",
    }
}

struct MultipartAssembly {
    name: String,
    data: Vec<u8>,
}

fn resolve_inline_dimension(dimension: &Dimension, cell_size: u32, frame_size: u32) -> u32 {
    match dimension {
        Dimension::Auto => 0,
        Dimension::Cells(value) => cell_size.saturating_mul(*value),
        Dimension::Pixels(value) => *value,
        Dimension::Percent(value) => frame_size.saturating_mul(u32::from(*value)) / 100,
    }
}

fn store_inline_display(
    command: InlineImageCommand,
    anchor: InlineAnchor,
    image_state: &mut Option<ImageState>,
) -> Result<(), String> {
    let InlineImageCommand::Display {
        name,
        data,
        width,
        height,
        preserve_aspect_ratio,
    } = command
    else {
        return Err("inline image command is not a display record".to_string());
    };
    let Some(state) = image_state.as_mut() else {
        return Err("inline image received before the image region was configured".to_string());
    };
    let placement = InlineImagePlacement {
        name: name.clone(),
        data,
        x: u32::from(anchor.col).saturating_mul(state.metrics.cell_width.round() as u32),
        y: u32::from(anchor.row).saturating_mul(state.metrics.cell_height.round() as u32),
        width: resolve_inline_dimension(
            &width,
            state.metrics.cell_width.round() as u32,
            state.width_px,
        ),
        height: resolve_inline_dimension(
            &height,
            state.metrics.cell_height.round() as u32,
            state.height_px,
        ),
        preserve_aspect_ratio,
        anchor_row: i32::from(anchor.row),
        anchor_scroll: anchor.scroll,
        visible: true,
    };
    if let Some(existing) = state
        .inline_images
        .iter_mut()
        .find(|image| image.name == name)
    {
        *existing = placement;
    } else {
        state.inline_images.push(placement);
    }
    Ok(())
}

fn refresh_inline_image_positions(
    engine: &mut Box<dyn Engine>,
    image_state: &mut Option<ImageState>,
) {
    let Some(state) = image_state.as_mut() else {
        return;
    };
    let current_scroll = engine.scroll_generation();
    // 스크롤백을 보는 동안 뷰포트는 그만큼 아래로 내려간 행을 보인다.
    let offset = engine.viewport_offset() as i32;
    let cell_height = state.metrics.cell_height.round() as u32;
    for image in &mut state.inline_images {
        let row = image.anchor_row - (current_scroll - image.anchor_scroll) as i32 + offset;
        image.visible = row >= 0;
        image.y = if row >= 0 {
            (row as u32).saturating_mul(cell_height)
        } else {
            state.height_px
        };
    }
}

fn apply_inline_image_command(
    command: InlineImageCommand,
    anchor: InlineAnchor,
    image_state: &mut Option<ImageState>,
    multipart: &mut Option<MultipartAssembly>,
) -> Result<(), String> {
    match command {
        InlineImageCommand::Display { .. } => store_inline_display(command, anchor, image_state),
        InlineImageCommand::Transfer { .. } => Ok(()),
        InlineImageCommand::MultipartStart { name } => {
            if multipart.is_some() {
                return Err("inline image multipart transfer is already active".to_string());
            }
            *multipart = Some(MultipartAssembly {
                name,
                data: Vec::new(),
            });
            Ok(())
        }
        InlineImageCommand::MultipartPart(data) => {
            let Some(assembly) = multipart.as_mut() else {
                return Err("inline image multipart part has no active transfer".to_string());
            };
            if assembly.data.len().saturating_add(data.len()) > crate::inline_image::MAX_IMAGE_BYTES
            {
                return Err(format!(
                    "inline image multipart payload exceeds {} bytes",
                    crate::inline_image::MAX_IMAGE_BYTES
                ));
            }
            assembly.data.extend_from_slice(&data);
            Ok(())
        }
        InlineImageCommand::MultipartEnd => {
            let Some(assembly) = multipart.take() else {
                return Err("inline image multipart end has no active transfer".to_string());
            };
            if assembly.data.is_empty() {
                return Err("inline image multipart transfer has no data".to_string());
            }
            store_inline_display(
                InlineImageCommand::Display {
                    name: assembly.name,
                    data: assembly.data,
                    width: Dimension::Auto,
                    height: Dimension::Auto,
                    preserve_aspect_ratio: true,
                },
                anchor,
                image_state,
            )
        }
    }
}

/// 표면 actor 가 응답, 세션, 성능 기록에 쓰는 대상. actor 가 사는 동안 바뀌지 않는다.
#[derive(Clone, Copy)]
struct SurfacePorts<'a> {
    surface_id: &'a str,
    session_port: &'a Arc<dyn SessionPort>,
    output_tx: &'a OutputSink,
    performance: &'a crate::performance::PerformanceTrace,
}

async fn send_engine_events(
    ports: SurfacePorts<'_>,
    session_id: Option<&str>,
    engine: &mut Box<dyn Engine>,
    emit_surface_events: bool,
    image_state: &mut Option<ImageState>,
    multipart: &mut Option<MultipartAssembly>,
) -> bool {
    let SurfacePorts {
        surface_id,
        session_port,
        output_tx,
        ..
    } = ports;
    for event in engine.drain_events() {
        match event {
            EngineEvent::InlineImage { command, anchor } => {
                let command_for_event = command.clone();
                if let Err(error) =
                    apply_inline_image_command(command, anchor, image_state, multipart)
                {
                    if !emit_surface_events {
                        continue;
                    }
                    let response = json!({
                        "surface": surface_id,
                        "body": {"event": "error", "reason": error}
                    });
                    if output_tx.send(response.to_string()).await.is_err() {
                        return false;
                    }
                    continue;
                }
                if !emit_surface_events {
                    continue;
                }
                let body = match command_for_event {
                    InlineImageCommand::Display {
                        name,
                        data,
                        width,
                        height,
                        preserve_aspect_ratio,
                    } => json!({
                        "event": "image.inline",
                        "command": "display",
                        "name": name,
                        "data": base64_encode(&data),
                        "width": inline_dimension(&width),
                        "height": inline_dimension(&height),
                        "preserveAspectRatio": preserve_aspect_ratio,
                    }),
                    InlineImageCommand::Transfer { name, data } => json!({
                        "event": "image.inline",
                        "command": "transfer",
                        "name": name,
                        "data": base64_encode(&data),
                    }),
                    InlineImageCommand::MultipartStart { name } => json!({
                        "event": "image.inline",
                        "command": "multipart.start",
                        "name": name,
                    }),
                    InlineImageCommand::MultipartPart(data) => json!({
                        "event": "image.inline",
                        "command": "multipart.part",
                        "data": base64_encode(&data),
                    }),
                    InlineImageCommand::MultipartEnd => json!({
                        "event": "image.inline",
                        "command": "multipart.end",
                    }),
                };
                let event = json!({"surface": surface_id, "body": body}).to_string();
                // 그림 영역이 있으면 그 그림을 그린 표시와 함께 보낸다. 출력 처리는 이 이벤트 뒤에 화면을 그린다.
                match image_state.as_mut() {
                    Some(state) => state.presentation_events.push(event),
                    None => {
                        if output_tx.send(event).await.is_err() {
                            return false;
                        }
                    }
                }
            }
            EngineEvent::PtyWrite(bytes) => {
                let Some(session_id) = session_id else {
                    let response = json!({"surface": surface_id, "body": {"error": "engine response without session"}});
                    return output_tx.send(response.to_string()).await.is_ok();
                };
                if let Err(error) = session_port.write(session_id, &bytes).await {
                    let response = json!({"surface": surface_id, "body": {"error": "engine response write failed", "reason": error}});
                    return output_tx.send(response.to_string()).await.is_ok();
                }
            }
            EngineEvent::Title(title) => {
                if !emit_surface_events {
                    continue;
                }
                let response =
                    json!({"surface": surface_id, "body": {"event": "title", "title": title}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ResetTitle => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "title.reset"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Directory { uri, path } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "directory", "uri": uri, "path": path}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Hyperlink { id, uri } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "hyperlink", "id": id, "uri": uri}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::PointerShape(shape) => {
                if !emit_surface_events {
                    continue;
                }
                let response =
                    json!({"surface": surface_id, "body": {"event": "pointer", "shape": shape}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Notification { message } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "notification", "message": message}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ShellState { marker, params } => {
                if !emit_surface_events {
                    continue;
                }
                let marker = match marker {
                    ShellMarker::PromptStart => "prompt.start",
                    ShellMarker::PromptEnd => "prompt.end",
                    ShellMarker::CommandStart => "command.start",
                    ShellMarker::CommandFinished => "command.finished",
                };
                let response = json!({"surface": surface_id, "body": {"event": "vendor.shell.state", "marker": marker, "params": params}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ClipboardStore { selection, text } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "clipboard.store", "selection": clipboard_selection_name(selection), "text": text}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ClipboardQuery {
                request_id,
                selection,
            } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "clipboard.query", "requestId": request_id, "selection": clipboard_selection_name(selection)}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::CursorBlinkingChange => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "cursor.blinking"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Wakeup => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "wakeup"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Bell => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "bell"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Exit => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "exit"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::ChildExit { success, code } => {
                if !emit_surface_events {
                    continue;
                }
                let response = json!({"surface": surface_id, "body": {"event": "child.exit", "success": success, "code": code}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::MouseCursorDirty => {
                if !emit_surface_events {
                    continue;
                }
                let response =
                    json!({"surface": surface_id, "body": {"event": "mouse.cursor.dirty"}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
            EngineEvent::Error(reason) => {
                if !emit_surface_events {
                    continue;
                }
                // 엔진의 거부는 프로그램 출력의 시퀀스에 대한 것이다. 페이지는 이를 터미널 오류가 아니라 기록으로 남긴다.
                let response = json!({"surface": surface_id, "body": {"event": "sequence.rejected", "reason": reason}});
                if output_tx.send(response.to_string()).await.is_err() {
                    return false;
                }
            }
        }
    }
    refresh_inline_image_positions(engine, image_state);
    true
}

fn inline_dimension(dimension: &Dimension) -> Value {
    match dimension {
        Dimension::Auto => Value::String("auto".to_string()),
        Dimension::Cells(value) => json!(value),
        Dimension::Pixels(value) => Value::String(format!("{value}px")),
        Dimension::Percent(value) => Value::String(format!("{value}%")),
    }
}

async fn open_headless(
    shell: &ShellRequest,
    session_id: &mut Option<String>,
    engine: &mut Box<dyn Engine>,
    session_port: &Arc<dyn SessionPort>,
    output_tx: &OutputSink,
) -> bool {
    if session_id.is_some() {
        return true;
    }
    engine.resize(80, 24);
    match session_port.open(shell, 80, 24).await {
        Ok(id) => {
            *session_id = Some(id);
            true
        }
        Err(error) => {
            let response =
                json!({"body": {"error": format!("Failed to open headless session: {error}")}});
            output_tx.send(response.to_string()).await.is_ok()
        }
    }
}

async fn open_if_configured(
    ports: SurfacePorts<'_>,
    requested: bool,
    requested_image: &Option<String>,
    shell: &ShellRequest,
    session_id: &mut Option<String>,
    engine: &mut Box<dyn Engine>,
    image_state: &mut Option<ImageState>,
) -> bool {
    let SurfacePorts {
        surface_id,
        session_port,
        output_tx,
        performance,
    } = ports;
    if !requested || session_id.is_some() {
        return true;
    }
    let Some(state) = image_state.as_mut() else {
        return true;
    };
    if requested_image
        .as_ref()
        .is_some_and(|name| name != &state.name)
    {
        return true;
    }
    let (cols, rows) =
        match calculate_terminal_size(state.width_px, state.height_px, &state.metrics) {
            Ok(size) => size,
            // 셀 하나의 공간도 없는 래스터는 끌기 중 카드가 무너지는 과도 상태이다. 여기서
            // 오류를 내면 열기 요청이 사라지고 나중 크기로 다시 열리지 않는다. 열지 않고
            // 기다리면 다음 configure 가 세션을 연다(V5-96-14-6-4-8).
            Err(_) => return true,
        };
    crate::engine_trace::record(surface_id, || format!("resize {cols} {rows}"));
    engine.resize(cols, rows);
    if let Err(error) = set_engine_metrics(engine, state) {
        let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
        return output_tx.send(response.to_string()).await.is_ok();
    }
    match session_port.open(shell, cols, rows).await {
        Ok(sid) => {
            *session_id = Some(sid.clone());
            if !send_state(surface_id, &sid, cols, rows, state, output_tx).await {
                return false;
            }
            let screen = engine.screen();
            present_screen(surface_id, &screen, state, output_tx, "open", performance).await
        }
        Err(error) => {
            let response = json!({"surface": surface_id, "body": {"error": format!("Failed to open: {error}")}});
            output_tx.send(response.to_string()).await.is_ok()
        }
    }
}

fn newer_configuration(configuration: &ImageConfiguration, state: &ImageState) -> bool {
    (configuration.generation, configuration.raster) > (state.generation, state.raster)
}

/// retain 요청의 surfaces 목록 `[{surface, root}]` 을 세션 키로 바꾼다. 형식이 틀리면 오류다.
fn retain_keys(value: &Value) -> Result<std::collections::HashSet<String>, String> {
    let surfaces = value
        .get("surfaces")
        .and_then(Value::as_array)
        .ok_or("retain requires a surfaces array")?;
    surfaces
        .iter()
        .map(|item| {
            let surface = item.get("surface").and_then(Value::as_str);
            let root = item.get("root").and_then(Value::as_str);
            match (surface, root) {
                (Some(surface), Some(root)) if !surface.is_empty() && !root.is_empty() => {
                    Ok(format!("{root}\0{surface}"))
                }
                _ => Err(format!("retain surface entry is invalid: {item}")),
            }
        })
        .collect()
}

fn local_surface_key(
    surface_txs: &HashMap<String, mpsc::Sender<SurfaceCommand>>,
    root: Option<&str>,
    surface: &str,
) -> String {
    if let Some(root) = root {
        return format!("{root}\0{surface}");
    }
    surface_txs
        .keys()
        .find(|key| key.ends_with(&format!("\0{surface}")))
        .cloned()
        // 기본값: 루트 없이 온 표면을 등록된 키에서 찾지 못하면 루트 없는 키가 되고, 그 키로 찾는 쪽이 없는 표면으로 알린다.
        .unwrap_or_else(|| format!("\0{surface}"))
}

/// 표면별 비동기 작업. 엔진과 데몬 연결을 소유하며 명령을 처리한다.
async fn surface_task(
    surface_id: String,
    performance: crate::performance::PerformanceTrace,
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    session_port: Arc<dyn SessionPort>,
    mut cmd_rx: mpsc::Receiver<SurfaceCommand>,
    output_tx: OutputSink,
) {
    let mut engine = engine_factory();
    let mut session_id: Option<String> = None;
    let mut daemon_events_rx = session_port.get_events().await;
    let mut image_state: Option<ImageState> = None;
    let mut preserved_inline_images = Vec::new();
    let mut multipart: Option<MultipartAssembly> = None;
    let mut open_requested = false;
    let mut requested_image: Option<String> = None;
    let mut requested_shell = ShellRequest::default();
    let mut headless = false;
    let mut pending_configuration: Option<ImageConfiguration> = None;
    let mut focused = false;
    // 마우스 보고로 시작한 누름. 뗄 때까지 선택 연산은 선택하지 않는다.
    let mut mouse_gesture = false;
    // motion 소유권은 button-down 시점에 고정된다.  terminal은 drag 진행 중에
    // mouse mode를 바꿀 수 있다. move마다 mode를 다시 읽으면
    // 하나의 gesture가 TUI와 page selection으로 나뉜다.
    let mut mouse_gesture_motion = false;
    let mut mouse_cell: Option<(u16, u16)> = None;
    // selection release는 그 직전에 생성된 raster를 기술한다.
    // 그 raster가 consume될 때까지 event를 보류하여, 이전 selection이 화면에 남아 있는 동안
    // client가 copy acknowledgement를 관찰하지 못하게 한다.
    let mut pending_selection_event: Option<String> = None;
    let mut preedit: Option<Preedit> = None;
    let mut current_theme = crate::palette::TerminalTheme::dark();
    let mut cursor_policy = CursorPolicy::default();
    // 이 표면의 터미널 글꼴. font 요청 전까지 시스템 고정폭 글꼴이다.
    let mut terminal_font = crate::platform::default_font();
    // 글꼴 크기(포인트). font 연산이 정한다(docs/spec/text-size.md).
    let mut terminal_font_size = DEFAULT_FONT_SIZE;
    let mut cursor_activity = Instant::now();
    let mut cursor_tick = tokio::time::interval(Duration::from_millis(50));
    cursor_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_cursor_frame: Option<(CursorShape, bool, bool, bool)> = None;

    loop {
        tokio::select! {
            _ = cursor_tick.tick(), if image_state.is_some() && !headless => {
                // 커서 깜빡임이 아예 꺼져 있으면 이 틱은 아무것도 하지 않는다(V5-96-14-6-4-9).
                // engine.screen() 은 전체 그리드를 복사하므로, "바뀌었나?"를 검사하려고
                // 50ms마다 전체 화면을 복사하는 것이 대기 CPU 의 원인이었다.
                // 측정: 아이들에서 blink 프레임은 초당 0.15회(무시할 만함)였지만 틱은
                // 초당 20회 돌며 매번 화면을 복사했다. 커서가 안 깜빡이면 화면이 안
                // 바뀌므로 복사할 필요도 없다.
                let blink_enabled = focused
                    && !matches!(cursor_policy.blink, crate::platform::darwin::frame::CursorBlinkPolicy::Never)
                    && (cursor_policy.idle_timeout_ms == 0
                        || (cursor_activity.elapsed().as_millis() as u64) < cursor_policy.idle_timeout_ms)
                    && engine.screen().cursor.visible;
                if !blink_enabled {
                    continue;
                }
                let screen = decorate_screen(
                    engine.screen(),
                    focused,
                    &preedit,
                    &cursor_policy,
                    cursor_activity.elapsed().as_millis() as u64,
                );
                let signature = (
                    screen.cursor.shape,
                    screen.cursor.blink_visible,
                    screen.cursor.focused,
                    screen.cursor.visible,
                );
                if last_cursor_frame != Some(signature) {
                    if let Some(state) = image_state.as_mut() {
                        if state.pending_draw {
                            state.dirty = true;
                        } else if !present_screen(&surface_id, &screen, state, &output_tx, "blink", &performance).await {
                            return;
                        } else {
                            last_cursor_frame = Some(signature);
                        }
                    }
                }
            }
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    SurfaceCommand::Open { image, request } => {
                        open_requested = true;
                        requested_image = image;
                        requested_shell = request;
                        headless = requested_image.is_none();
                        if headless && !open_headless(&requested_shell, &mut session_id, &mut engine, &session_port, &output_tx).await { return; }
                        if !open_if_configured(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, performance: &performance }, open_requested, &requested_image, &requested_shell, &mut session_id,
                            &mut engine, &mut image_state).await {
                            return;
                        }
                    }
                    SurfaceCommand::Reconnect => {
                        // PTY와 VT 상태는 persistent session에 속하고,
                        // IOSurface는 application instance에 속한다.
                        // 이전 native image만 폐기하여 재연결하는
                        // client가 새 Configure message를 제공하게 한다.
                        if let Some(previous_state) = image_state.take() {
                            preserved_inline_images = previous_state.inline_images;
                        }
                        pending_configuration = None;
                        if !headless {
                            if let Some(session_id) = session_id.as_deref() {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"event": "session", "sessionId": session_id}
                            });
                            if output_tx.send(response.to_string()).await.is_err() {
                                return;
                            }
                            }
                        }
                    }
                    SurfaceCommand::Configure(configuration) => {
                        if let Some(state) = image_state.as_ref() {
                            if !newer_configuration(&configuration, state) {
                                continue;
                            }
                            if state.pending_draw {
                                let replace = pending_configuration.as_ref().is_none_or(|pending|
                                    (configuration.generation, configuration.raster) > (pending.generation, pending.raster));
                                if replace { pending_configuration = Some(configuration); }
                                continue;
                            }
                        }
                        let new_state = match ImageState::new(&configuration, &terminal_font, terminal_font_size) {
                            Ok(state) => state,
                            Err(reason) => {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"error": "image creation failed", "reason": reason}
                                });
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                                continue;
                            }
                        };
                        let mut new_state = new_state;
                        new_state.theme = current_theme;
                        new_state.inline_images = image_state.as_ref()
                            .map(|previous_state| previous_state.inline_images.clone())
                            // 기본값: 앞 이미지 상태가 없으면 그 사이 보관한 인라인 그림을 이어받는다.
                            .unwrap_or_else(|| std::mem::take(&mut preserved_inline_images));
                        let (cols, rows) = match calculate_terminal_size(
                            configuration.width, configuration.height, &new_state.metrics) {
                            Ok(size) => size,
                            // 셀 하나의 공간도 없는 래스터는 끌기 중 카드가 무너지는 과도 상태이다.
                            // 현재 격자를 유지해 정상 경로를 지나면(같은 크기 재조정은 아무 것도
                            // 바꾸지 않는다) 그리기가 IOSurface 경계로 잘려 들어가고, 프레임이
                            // 표시되므로 호스트의 표시 장벽이 멈추지 않는다(V5-96-14-6-4-8).
                            Err(_) => {
                                let screen = engine.screen();
                                (screen.cols, screen.rows)
                            }
                        };
                        crate::engine_trace::record(&surface_id, || format!("resize {cols} {rows}"));
                        engine.resize(cols, rows);
                        if let Err(error) = set_engine_metrics(&mut engine, &new_state) {
                            let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                            continue;
                        }
                        image_state = Some(new_state);
                        refresh_inline_image_positions(&mut engine, &mut image_state);
                        if let Some(sid) = session_id.as_ref() {
                            if let Err(error) = session_port.resize(sid, cols, rows).await {
                                let response = json!({"surface": surface_id,
                                    "body": {"error": format!("Resize failed: {error}")}});
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                            } else if !send_state(&surface_id, sid, cols, rows, image_state.as_ref().unwrap(), &output_tx).await {
                                return;
                            }
                            let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                            if !present_screen(&surface_id, &screen, image_state.as_mut().unwrap(), &output_tx, "resize", &performance).await {
                                return;
                            }
                        } else if !open_if_configured(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, performance: &performance }, open_requested, &requested_image, &requested_shell, &mut session_id,
                            &mut engine, &mut image_state).await {
                            return;
                        }
                    }
                    SurfaceCommand::Input { bytes } => {
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        // 입력은 뷰포트를 가장 새 출력으로 되돌린 뒤 쓴다.
                        if engine.scroll_to_newest() {
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                            if !hold_while_presenting(&mut image_state) {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if let Some(state) = image_state.as_mut() {
                                    if !present_screen(&surface_id, &screen, state, &output_tx, "input", &performance).await { return; }
                                } else if output_tx.send(screen_event(&surface_id, &screen).to_string()).await.is_err() { return; }
                            }
                        }
                        if let Some(ref sid) = session_id {
                            match session_port.write(sid, &bytes).await {
                                Ok(()) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"ack": true}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": format!("Write failed: {}", e)}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        } else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "Session not open"}
                            });
                            if output_tx.send(response.to_string()).await.is_err() {
                                return;
                            }
                        }
                    }
                    SurfaceCommand::Paste { text } => {
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        // 입력은 뷰포트를 가장 새 출력으로 되돌린 뒤 쓴다.
                        if engine.scroll_to_newest() {
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                            if !hold_while_presenting(&mut image_state) {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if let Some(state) = image_state.as_mut() {
                                    if !present_screen(&surface_id, &screen, state, &output_tx, "paste", &performance).await { return; }
                                } else if output_tx.send(screen_event(&surface_id, &screen).to_string()).await.is_err() { return; }
                            }
                        }
                        if let Some(ref sid) = session_id {
                            let bytes = match encoding::encode_paste(&text, &engine.modes()) {
                                Ok(bytes) => bytes,
                                Err(error) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": error}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        return;
                                    }
                                    continue;
                                }
                            };
                            match session_port.write(sid, &bytes).await {
                                Ok(()) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"ack": true, "event": "paste"}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        return;
                                    }
                                }
                                Err(error) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": format!("Paste failed: {error}")}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        } else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "Session not open"}
                            });
                            if output_tx.send(response.to_string()).await.is_err() {
                                return;
                            }
                        }
                    }
                    SurfaceCommand::Mouse { input_id, phase, x, y, pressed, shift, alt, ctrl } => {
                        let modes = engine.modes();
                        let cell = match image_state.as_ref()
                            .ok_or_else(|| "mouse image is not configured".to_string())
                            .and_then(|state| state.selection_cell(x, y)) {
                            Ok(cell) => cell,
                            Err(reason) => {
                                let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                                continue;
                            }
                        };
                        // 프로그램이 마우스 보고를 켜면 일반 드래그를 프로그램에 보내고, Shift 드래그는 글자를 선택한다.
                        let report = match phase {
                            MousePhase::Down if modes.mouse_report() && !shift => {
                                mouse_gesture = true;
                                mouse_gesture_motion = modes.mouse_drag || modes.mouse_motion;
                                if engine.selection_clear() {
                                    last_cursor_frame = None;
                                    if let Some(state) = image_state.as_mut() {
                                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                        if !present_screen(&surface_id, &screen, state, &output_tx, "mouse", &performance).await { return; }
                                    }
                                }
                                Some((encoding::MouseButton::Left, encoding::MouseAction::Press))
                            }
                            MousePhase::Down => None,
                            MousePhase::Move if mouse_cell == Some(cell) => None,
                            MousePhase::Move if mouse_drag_motion_owned(mouse_gesture, mouse_gesture_motion, pressed) =>
                                Some((encoding::MouseButton::Left, encoding::MouseAction::Motion)),
                            MousePhase::Move if !pressed && !mouse_gesture && modes.mouse_motion =>
                                Some((encoding::MouseButton::None, encoding::MouseAction::Motion)),
                            MousePhase::Move => None,
                            MousePhase::Up if mouse_gesture => {
                                mouse_gesture = false;
                                mouse_gesture_motion = false;
                                Some((encoding::MouseButton::Left, encoding::MouseAction::Release))
                            }
                            MousePhase::Up => None,
                        };
                        mouse_cell = Some(cell);
                        if let Some((button, action)) = report {
                            let encoded = encoding::encode_mouse(&encoding::MouseReport {
                                button, action, col: cell.0, row: cell.1, alt, ctrl,
                            }, &modes);
                            let phase_name = match phase { MousePhase::Down => "down", MousePhase::Move => "move", MousePhase::Up => "up" };
                            match encoded {
                                Ok(bytes) => {
                                    let written = if let Some(ref sid) = session_id {
                                        if let Err(error) = session_port.write(sid, &bytes).await {
                                            let response = json!({"surface": surface_id, "body": {"error": "mouse write failed", "reason": error}});
                                            if output_tx.send(response.to_string()).await.is_err() { return; }
                                            false
                                        } else { true }
                                    } else { false };
                                    let response = json!({"surface": surface_id, "body": {
                                        "event": "mouse", "inputId": input_id, "phase": phase_name, "x": cell.0, "y": cell.1,
                                        "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                        "reported": true, "written": written,
                                        "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                        "bytes": base64_encode(&bytes)
                                    }});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                }
                                Err(reason) => {
                                    let response = json!({"surface": surface_id, "body": {"event": "mouse", "inputId": input_id, "phase": phase_name,
                                        "x": cell.0, "y": cell.1, "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                        "reported": true, "written": false,
                                        "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                        "bytes": null, "error": reason}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                }
                            }
                        } else {
                            // 현재 mouse mode가 의도적으로 text selection으로 보내는 phase를 포함하여
                            // 모든 pointer phase에 대해 측정값을 내보낸다. 이렇게 하면 보고되지 않은 phase가
                            // 사라지지 않고 소유권을 관찰할 수 있다.
                            let phase_name = match phase {
                                MousePhase::Down => "down",
                                MousePhase::Move => "move",
                                MousePhase::Up => "up",
                            };
                            let response = json!({"surface": surface_id, "body": {
                                "event": "mouse", "inputId": input_id, "phase": phase_name, "x": cell.0, "y": cell.1,
                                "pressed": pressed, "shift": shift, "alt": alt, "ctrl": ctrl,
                                "reported": false, "written": false,
                                "modes": {"click": modes.mouse_click, "drag": modes.mouse_drag, "motion": modes.mouse_motion},
                                "bytes": null, "error": null
                            }});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                    // 표면의 현재 세션에서 읽지 않은 입력 바이트 수와 reader 가 읽은 자식 출력 누적 바이트
                    // 수를 한 번에 측정해 응답한다. 쓰기 성공, 자식 수신, 자식 출력을 구분하는 진단 관측이다.
                    SurfaceCommand::PtyPending => {
                        // 오류 응답도 같은 event 표식을 실어 보낸다. 측정 요청의 답임이 응답만으로 드러나야 한다.
                        let response = match session_id.as_ref() {
                            Some(sid) => match session_port.pty_measurement(sid).await {
                                Ok(measurement) => json!({"surface": surface_id, "body": {"event": "pty.pending", "pending": measurement.pending, "written": measurement.written}}),
                                Err(error) => json!({"surface": surface_id, "body": {"event": "pty.pending", "error": error}}),
                            },
                            None => json!({"surface": surface_id, "body": {"event": "pty.pending", "error": "Session not open"}}),
                        };
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    // 마우스 보고 제스처는 글자를 선택하지 않는다. 페이지는 선택 연산의 답을 기다린다.
                    SurfaceCommand::SelectionStart { .. } if mouse_gesture => {
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.start"}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::SelectionUpdate { .. } if mouse_gesture => {
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.update"}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::SelectionEnd if mouse_gesture => {
                        let response = json!({"surface": surface_id, "body": {"event": "selection.end", "copied": false}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::SelectionStart { x, y } => {
                        let result = image_state.as_ref().ok_or_else(|| "selection image is not configured".to_string())
                            .and_then(|state| state.selection_edge(x, y))
                            .and_then(|(edge, row)| engine.selection_start(edge, row));
                        if let Err(error) = result {
                            let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        } else {
                            cursor_activity = Instant::now();
                            last_cursor_frame = None;
                            if let Some(state) = image_state.as_mut() {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if !present_screen(&surface_id, &screen, state, &output_tx, "selection", &performance).await { return; }
                            }
                            let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.start"}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                    SurfaceCommand::SelectionUpdate { x, y } => {
                        let result = image_state.as_ref().ok_or_else(|| "selection image is not configured".to_string())
                            .and_then(|state| state.selection_edge(x, y))
                            .and_then(|(edge, row)| engine.selection_update(edge, row));
                        if let Err(error) = result {
                            let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        } else {
                            cursor_activity = Instant::now();
                            last_cursor_frame = None;
                            if let Some(state) = image_state.as_mut() {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if !present_screen(&surface_id, &screen, state, &output_tx, "selection", &performance).await { return; }
                            }
                            let response = json!({"surface": surface_id, "body": {"ack": true, "event": "selection.update"}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                    SurfaceCommand::SelectionEnd => {
                        if pending_selection_event.is_some() {
                            let response = json!({"surface": surface_id, "body": {
                                "error": "invalidParams",
                                "reason": "selection presentation is still pending"
                            }});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                            continue;
                        }
                        match engine.selection_end() {
                            Ok(text) => {
                                if let Some(state) = image_state.as_mut() {
                                    let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                    if !present_screen(&surface_id, &screen, state, &output_tx, "selection", &performance).await { return; }
                                    let body = match text {
                                        Some(text) => json!({"event": "selection.copy", "text": text, "userInitiated": true}),
                                        None => json!({"event": "selection.end", "copied": false}),
                                    };
                                    pending_selection_event = Some(json!({"surface": surface_id, "body": body}).to_string());
                                    continue;
                                }
                                // text가 없는 selection은 아무것도 copy하지 않고 release를 보고한다.
                                let body = match text {
                                    Some(text) => json!({"event": "selection.copy", "text": text, "userInitiated": true}),
                                    None => json!({"event": "selection.end", "copied": false}),
                                };
                                let response = json!({"surface": surface_id, "body": body});
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                            }
                            Err(error) => {
                                let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                            }
                        }
                    }
                    SurfaceCommand::Viewport { offset } => {
                        // 엔진이 보관된 기록 범위 안으로 제한한다.
                        let delta = i64::from(offset) - i64::from(engine.viewport_offset());
                        if delta != 0 {
                            engine.scroll_viewport(delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
                        }
                        refresh_inline_image_positions(&mut engine, &mut image_state);
                        if hold_while_presenting(&mut image_state) { continue; }
                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                        if let Some(state) = image_state.as_mut() {
                            if !present_screen(&surface_id, &screen, state, &output_tx, "viewport", &performance).await { return; }
                        } else if output_tx.send(screen_event(&surface_id, &screen).to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::Scroll { lines, col, row } => {
                        let modes = engine.modes();
                        // 마우스 보고, 대체 화면의 대체 스크롤, 기본 화면의 뷰포트 순으로 적용한다.
                        let bytes = if modes.mouse_report() {
                            match encoding::encode_wheel(&modes, lines > 0, col, row) {
                                Ok(event) => Some(event.repeat(lines.unsigned_abs() as usize)),
                                Err(error) => {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                    continue;
                                }
                            }
                        } else if modes.alt_screen && modes.alternate_scroll {
                            let key = if lines > 0 { "Up" } else { "Down" };
                            let keys = vec![InputKey { key: key.to_string(), text: String::new(), shift: false, alt: false, ctrl: false }; lines.unsigned_abs() as usize];
                            match encode_keys(&keys, &modes) {
                                Ok(bytes) => Some(bytes),
                                Err(error) => {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": error}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                    continue;
                                }
                            }
                        } else {
                            None
                        };
                        match bytes {
                            Some(bytes) => {
                                if let Some(ref sid) = session_id {
                                    if let Err(error) = session_port.write(sid, &bytes).await {
                                        let response = json!({"surface": surface_id, "body": {"error": "scroll write failed", "reason": error}});
                                        if output_tx.send(response.to_string()).await.is_err() { return; }
                                    }
                                }
                            }
                            None => {
                                engine.scroll_viewport(lines);
                                refresh_inline_image_positions(&mut engine, &mut image_state);
                                if hold_while_presenting(&mut image_state) { continue; }
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if let Some(state) = image_state.as_mut() {
                                    if !present_screen(&surface_id, &screen, state, &output_tx, "scroll", &performance).await { return; }
                                } else if output_tx.send(screen_event(&surface_id, &screen).to_string()).await.is_err() { return; }
                            }
                        }
                    }
                    SurfaceCommand::Copy => {
                        // 선택이 없으면 복사하지 않고 그렇다고 알린다. 클립보드는 바꾸지 않는다.
                        let body = match engine.selection_text() {
                            Some(text) => json!({"event": "copy", "text": text, "userInitiated": true}),
                            None => json!({"event": "copy", "copied": false}),
                        };
                        let response = json!({"surface": surface_id, "body": body});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::InputKeys { keys } => {
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        // 입력은 뷰포트를 가장 새 출력으로 되돌린 뒤 쓴다.
                        if engine.scroll_to_newest() {
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                            if !hold_while_presenting(&mut image_state) {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, 0);
                                if let Some(state) = image_state.as_mut() {
                                    if !present_screen(&surface_id, &screen, state, &output_tx, "input", &performance).await { return; }
                                } else if output_tx.send(screen_event(&surface_id, &screen).to_string()).await.is_err() { return; }
                            }
                        }
                        if let Some(ref sid) = session_id {
                            let modes = engine.modes();
                            match encode_keys(&keys, &modes) {
                                Ok(bytes) => {
                                    if !bytes.is_empty() {
                                        match session_port.write(sid, &bytes).await {
                                            Ok(()) => {
                                                let response = json!({
                                                    "surface": surface_id,
                                                    "body": {"ack": true}
                                                });
                                                if output_tx.send(response.to_string()).await.is_err() {
                                                    return;
                                                }
                                            }
                                            Err(e) => {
                                                let response = json!({
                                                    "surface": surface_id,
                                                    "body": {"error": format!("Write failed: {}", e)}
                                                });
                                                if output_tx.send(response.to_string()).await.is_err() {
                                                    return;
                                                }
                                            }
                                        }
                                    } else {
                                        let response = json!({
                                            "surface": surface_id,
                                            "body": {"ack": true}
                                        });
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": e}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        } else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "Session not open"}
                            });
                            if output_tx.send(response.to_string()).await.is_err() {
                                return;
                            }
                        }
                    }
                    SurfaceCommand::Compose { preedit: next } => {
                        preedit = next;
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        if let Some(state) = image_state.as_mut() {
                            if state.pending_draw {
                                state.dirty = true;
                            } else {
                                let screen = decorate_screen(
                                    engine.screen(),
                                    focused,
                                    &preedit,
                                    &cursor_policy,
                                    0,
                                );
                                if !present_screen(&surface_id, &screen, state, &output_tx, "input", &performance).await {
                                    return;
                                }
                            }
                        }
                        let response = json!({"surface": surface_id, "body": {"ack": true}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::Focus { focused: next } => {
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        // ?1004 를 켠 프로그램에는 초점이 바뀔 때마다 알린다.
                        if next != focused && engine.modes().focus_in_out {
                            if let Some(ref sid) = session_id {
                                let report: &[u8] = if next { b"\x1b[I" } else { b"\x1b[O" };
                                if let Err(error) = session_port.write(sid, report).await {
                                    let response = json!({"surface": surface_id, "body": {"error": "focus report write failed", "reason": error}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                }
                            }
                        }
                        focused = next;
                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                        if let Some(ref mut state) = image_state {
                            if !present_screen(&surface_id, &screen, state, &output_tx, "focus", &performance).await { return; }
                        }
                        let response = json!({"surface": surface_id, "body": {"ack": true}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::Theme { theme } => {
                        let theme = *theme;
                        current_theme = theme;
                        engine.set_theme(theme);
                        if let Some(state) = image_state.as_mut() {
                            state.theme = theme;
                            if state.pending_draw {
                                state.dirty = true;
                            } else {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                if !present_screen(&surface_id, &screen, state, &output_tx, "theme", &performance).await { return; }
                            }
                        }
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "theme", "mode": theme.mode.name(), "background": format!("#{:02x}{:02x}{:02x}", theme.background[0], theme.background[1], theme.background[2])}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::Font { font, system, skipped, size } => {
                        let family = match font.family() {
                            Ok(family) => family,
                            Err(reason) => {
                                let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                                continue;
                            }
                        };
                        // 현재 raster 크기를 유지하고 새 글꼴의 셀 메트릭으로 열과 행을 다시 계산한다.
                        if let Some(state) = image_state.as_mut() {
                            let metrics = match crate::platform::metrics_for(&font, size, state.scale) {
                                Ok(metrics) => metrics,
                                Err(reason) => {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                    continue;
                                }
                            };
                            let (cols, rows) = match calculate_terminal_size(state.width_px, state.height_px, &metrics) {
                                Ok(size) => size,
                                // 서브셀 래스터에서 글꼴이 바뀌어도 격자를 유지한다. 다음 유효한
                                // 래스터가 새 글꼴의 격자로 다시 계산한다(V5-96-14-6-4-8).
                                Err(_) => {
                                    let screen = engine.screen();
                                    (screen.cols, screen.rows)
                                }
                            };
                            state.metrics = metrics;
                            crate::engine_trace::record(&surface_id, || format!("resize {cols} {rows}"));
                            engine.resize(cols, rows);
                            if let Err(error) = set_engine_metrics(&mut engine, state) {
                                let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
                                if output_tx.send(response.to_string()).await.is_err() { return; }
                                continue;
                            }
                            terminal_font = font;
                            terminal_font_size = size;
                            refresh_inline_image_positions(&mut engine, &mut image_state);
                            if let Some(sid) = session_id.as_ref() {
                                if let Err(error) = session_port.resize(sid, cols, rows).await {
                                    let response = json!({"surface": surface_id, "body": {"error": format!("Resize failed: {error}")}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                } else if !send_state(&surface_id, sid, cols, rows, image_state.as_ref().unwrap(), &output_tx).await {
                                    return;
                                }
                            }
                            let state = image_state.as_mut().unwrap();
                            if state.pending_draw {
                                state.dirty = true;
                            } else {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                if !present_screen(&surface_id, &screen, state, &output_tx, "metrics", &performance).await { return; }
                            }
                        } else {
                            terminal_font = font;
                            terminal_font_size = size;
                        }
                        let response = json!({"surface": surface_id, "body": {"ack": true, "event": "font", "family": family, "system": system, "skipped": skipped, "size": size}});
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::Cursor { policy } => {
                        cursor_policy = policy;
                        cursor_activity = Instant::now();
                        last_cursor_frame = None;
                        let screen = decorate_screen(
                            engine.screen(),
                            focused,
                            &preedit,
                            &cursor_policy,
                            0,
                        );
                        if let Some(state) = image_state.as_mut() {
                            if state.pending_draw {
                                state.dirty = true;
                            } else if !present_screen(&surface_id, &screen, state, &output_tx, "cursor", &performance).await {
                                return;
                            }
                        }
                        let response = json!({
                            "surface": surface_id,
                            "body": {
                                "ack": true,
                                "event": "cursor",
                                "shape": match policy.shape {
                                    CursorShape::Block => "block",
                                    CursorShape::Underline => "underline",
                                    CursorShape::Beam => "beam",
                                    CursorShape::HollowBlock => "block",
                                    CursorShape::Hidden => "block",
                                },
                                "blink": match policy.blink {
                                    CursorBlinkPolicy::Never => "Never",
                                    CursorBlinkPolicy::Off => "Off",
                                    CursorBlinkPolicy::On => "On",
                                    CursorBlinkPolicy::Always => "Always",
                                },
                                "interval": policy.interval_ms,
                                "idleTimeout": policy.idle_timeout_ms,
                                "unfocused": match policy.unfocused {
                                    UnfocusedCursor::Hollow => "hollow",
                                    UnfocusedCursor::Solid => "solid",
                                    UnfocusedCursor::Underline => "underline",
                                    UnfocusedCursor::Beam => "beam",
                                    UnfocusedCursor::Unchanged => "unchanged",
                                },
                            }
                        });
                        if output_tx.send(response.to_string()).await.is_err() { return; }
                    }
                    SurfaceCommand::ClipboardResolve { request_id, text } => {
                        if let Err(error) = engine.resolve_clipboard(request_id, &text) {
                            let response = json!({"surface": surface_id, "body": {"error": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        } else if !send_engine_events(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, performance: &performance }, session_id.as_deref(), &mut engine, true, &mut image_state, &mut multipart).await {
                            return;
                        }
                    }
                    SurfaceCommand::ClipboardReject { request_id, reason } => {
                        if let Err(error) = engine.reject_clipboard(request_id, &reason) {
                            let response = json!({"surface": surface_id, "body": {"error": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        } else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"event": "clipboard.rejected", "requestId": request_id, "reason": reason}
                            });
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                    SurfaceCommand::ScreenRead => {
                        let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                        let response = screen_event(&surface_id, &screen);
                        if output_tx.send(response.to_string()).await.is_err() {
                            return;
                        }
                    }
                    SurfaceCommand::InlineImageDelete { name } => {
                        let Some(state) = image_state.as_mut() else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "imageNotConfigured", "reason": "inline image region is not configured"}
                            });
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                            continue;
                        };
                        let Some(index) = state.inline_images.iter().position(|image| image.name == name) else {
                            let response = json!({
                                "surface": surface_id,
                                "body": {"error": "imageNotFound", "reason": format!("inline image {name:?} is not owned by this surface")}
                            });
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                            continue;
                        };
                        state.inline_images.remove(index);
                        // 삭제 이벤트는 그림을 지운 표시와 함께 보낸다.
                        state.presentation_events.push(json!({
                            "surface": surface_id,
                            "body": {"event": "image.inline.deleted", "name": name}
                        }).to_string());
                        if state.pending_draw {
                            state.dirty = true;
                        } else {
                            let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                            if !present_screen(&surface_id, &screen, state, &output_tx, "image", &performance).await { return; }
                        }
                    }
                    SurfaceCommand::SessionClose => {
                        let close_error = if let Some(ref sid) = session_id {
                            session_port.close(sid).await.err()
                        } else {
                            None
                        };
                        let body = close_error
                            .map(|error| json!({"error": format!("Close failed: {error}")}))
                            // 기본값: 닫기에 오류가 없으면 빈 본문으로 답한다.
                            .unwrap_or_else(|| json!({}));
                        let response = json!({"surface": surface_id, "body": body});
                        if output_tx.send(response.to_string()).await.is_err() {
                            return;
                        }
                        break;
                    }
                    SurfaceCommand::SessionDetach => {
                        if let Some(ref sid) = session_id {
                            if let Err(error) = session_port.detach(sid).await {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"error": format!("Detach failed: {error}")}
                                });
                                if output_tx.send(response.to_string()).await.is_err() {
                                    return;
                                }
                            }
                        }
                        break;
                    }
                    SurfaceCommand::ImageResponse { body } => {
                        let image_obj = body.get("image").and_then(|v| v.as_object());
                        let consumed = image_obj.and_then(|o| o.get("consumed")).and_then(|v| v.as_object());
                        let is_error = image_obj.is_some_and(|o| o.get("error").is_some());
                        // consumed 는 안쪽 객체에, 오류는 바깥에 이름과 순번을 실어 보낸다.
                        let name = image_obj
                            .and_then(|o| o.get("name"))
                            .and_then(|v| v.as_str())
                            .or_else(|| consumed.and_then(|r| r.get("name")).and_then(|v| v.as_str()));
                        // image 전송은 직렬화된다. pending_draw는 정확히 하나의
                        // envelope가 host response를 기다린다는 뜻이다. layout 교체 중에는
                        // 그 envelope의 generation/raster가 이미 바뀐 뒤에 host가 envelope를
                        // 거부할 수 있으므로, metadata를 정확히 일치시키면
                        // sidecar가 pending_draw=true로 영구히 막힌다. 현재 image 이름에 대한
                        // response는 진행 중인 그 전송 하나를 해제한다. response error는
                        // host log로 계속 관찰할 수 있고, 대기 중인 configuration 또는 dirty frame은
                        // 아래에서 render된다.
                        let releases_pending = image_state.as_ref().is_some_and(|state|
                            state.pending_draw
                                && Some(state.name.as_str()) == name
                                && (consumed.is_some() || is_error));
                        if releases_pending {
                            // 전송 완료 대기의 계기(V5-104): 보낸 순간부터 consumed/오류 응답까지.
                            if let Some(sent) = image_state.as_mut().unwrap().sent_at.take() {
                                performance.line("consumed_wait", serde_json::json!({
                                    "surface": surface_id,
                                    "wait_us": sent.elapsed().as_micros() as u64,
                                    "answer": if consumed.is_some() { "consumed" } else { "error" },
                                }));
                            }
                            image_state.as_mut().unwrap().pending_draw = false;
                            if let Some(configuration) = pending_configuration.take() {
                                let new_state = match ImageState::new(&configuration, &terminal_font, terminal_font_size) {
                                    Ok(state) => state,
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id,
                                            "body": {"error": "image creation failed", "reason": reason}});
                                        if output_tx.send(response.to_string()).await.is_err() { return; }
                                        continue;
                                    }
                                };
                                let mut new_state = new_state;
                                new_state.theme = current_theme;
                                let (cols, rows) = match calculate_terminal_size(
                                    configuration.width, configuration.height, &new_state.metrics) {
                                    Ok(size) => size,
                                    // Configure 와 같은 유지 규칙: 서브셀 래스터는 현재 격자를
                                    // 유지하고 프레임을 표시한다(V5-96-14-6-4-8).
                                    Err(_) => {
                                        let screen = engine.screen();
                                        (screen.cols, screen.rows)
                                    }
                                };
                                crate::engine_trace::record(&surface_id, || format!("resize {cols} {rows}"));
                                engine.resize(cols, rows);
                                if let Err(error) = set_engine_metrics(&mut engine, &new_state) {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalid renderer metrics", "reason": error}});
                                    if output_tx.send(response.to_string()).await.is_err() { return; }
                                    continue;
                                }
                                image_state = Some(new_state);
                                if let Some(sid) = session_id.as_ref() {
                                    if let Err(error) = session_port.resize(sid, cols, rows).await {
                                        let response = json!({"surface": surface_id,
                                            "body": {"error": format!("Resize failed: {error}")}});
                                        if output_tx.send(response.to_string()).await.is_err() { return; }
                                    } else if !send_state(&surface_id, sid, cols, rows, image_state.as_ref().unwrap(), &output_tx).await {
                                        return;
                                    }
                                    let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                    if !present_screen(&surface_id, &screen, image_state.as_mut().unwrap(), &output_tx, "image", &performance).await {
                                        return;
                                    }
                                }
                            } else if image_state.as_ref().unwrap().dirty {
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);
                                // 기다린 동안의 변경을 담은 화면 하나를 래스터와 함께 보낸다.
                                if !present_screen(&surface_id, &screen, image_state.as_mut().unwrap(), &output_tx, "image", &performance).await {
                                    return;
                                }
                            }
                            if image_state.as_ref().is_none_or(|state| !state.pending_draw) {
                                if let Some(event) = pending_selection_event.take() {
                                    if output_tx.send(event).await.is_err() { return; }
                                }
                            }
                        }
                    }
                }
            }
            Some(event) = daemon_events_rx.recv() => {
                match event {
                    DaemonEvent::Output { session_id: ref recv_sid, data, sequence: _, truncated } => {
                        if let Some(ref sid) = session_id {
                            if recv_sid == sid {
                                cursor_activity = Instant::now();
                                last_cursor_frame = None;
                                if truncated {
                                    crate::engine_trace::record(&surface_id, || "reset".to_string());
                                    engine.reset();
                                }
                                crate::engine_trace::record(&surface_id, || format!("feed {}", base64_encode(&data)));
                                engine.feed(&data);
                                if !send_engine_events(SurfacePorts { surface_id: &surface_id, session_port: &session_port, output_tx: &output_tx, performance: &performance }, session_id.as_deref(), &mut engine, !headless, &mut image_state, &mut multipart).await { return; }
                                if headless || hold_while_presenting(&mut image_state) { continue; }
                                let screen = decorate_screen(engine.screen(), focused, &preedit, &cursor_policy, cursor_activity.elapsed().as_millis() as u64);

                                if !headless { if let Some(ref mut img_state) = image_state {
                                        // 그림이 호스트에 있으면 돌려받을 때까지 그리지 않고 변경 사실만 남긴다.
                                        if img_state.pending_draw {
                                            img_state.dirty = true;
                                        } else if !present_screen(&surface_id, &screen, img_state, &output_tx, "image", &performance).await {
                                            return;
                                        }
                                } }

                                if headless || image_state.is_some() { continue; }
                                let response = screen_event(&surface_id, &screen);
                                if output_tx.send(response.to_string()).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    DaemonEvent::Exit { session_id: ref recv_sid } => {
                        if let Some(ref sid) = session_id {
                            if recv_sid == sid {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"event": "exit"}
                                });
                                if output_tx.send(response.to_string()).await.is_err() {
                                    // output channel이 닫혔으므로 그대로 종료한다
                                }
                                break;
                            }
                        }
                    }
                    DaemonEvent::Error { session_id: ref recv_sid, error } => {
                        if session_id.as_deref() == Some(recv_sid) {
                            let response = json!({"surface": surface_id, "body": {"event": "error", "reason": error}});
                            if output_tx.send(response.to_string()).await.is_err() { return; }
                        }
                    }
                }
            }
            else => break,
        }
    }
}

/// 서비스 루프. stdin에서 JSON을 읽고 stdout으로 응답을 씀.
pub async fn serve<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_performance(
        engine_factory,
        reader,
        writer,
        session_port_factory,
        crate::performance::PerformanceTrace::disabled(),
    )
    .await
}

/// 성능 트레이스를 직접 받는 검사용 진입점. 플래그 파일 경로의 트레이스를 넣으면
/// 프레임 계기가 대상 파일에 기록된다(V5-104).
pub async fn serve_with_performance<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    performance: crate::performance::PerformanceTrace,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_options(
        reader,
        writer,
        ServeOptions {
            engine_factory,
            session_port_factory,
            owner_close: None,
            registry: None,
            owner: String::new(),
            performance,
        },
    )
    .await
}

pub async fn serve_with_owner_close<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    owner_close: Option<Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_options(
        reader,
        writer,
        ServeOptions {
            engine_factory,
            session_port_factory,
            owner_close,
            registry: None,
            owner: String::new(),
            performance: crate::performance::PerformanceTrace::disabled(),
        },
    )
    .await
}

/// 영속 연결의 소유자. 연결이 끝나도 그 소유자의 표면은 registry 에 남는다.
pub struct PersistentOwner {
    /// 소유자가 닫기를 요청하면 부르는 함수.
    pub close: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub registry: Arc<PersistentRegistry>,
    pub owner: String,
    pub performance: crate::performance::PerformanceTrace,
}

pub async fn serve_with_registry<R, W>(
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    reader: R,
    writer: W,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    owner: PersistentOwner,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_options(
        reader,
        writer,
        ServeOptions {
            engine_factory,
            session_port_factory,
            owner_close: Some(owner.close),
            registry: Some(owner.registry),
            owner: owner.owner,
            performance: owner.performance,
        },
    )
    .await
}

/// 연결 하나의 입력 처리에 쓰는 공장과 소유자. registry 가 없으면 연결이 끝날 때 표면도 끝난다.
struct ServeOptions {
    engine_factory: Arc<dyn Fn() -> Box<dyn Engine> + Send + Sync>,
    session_port_factory: Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync>,
    owner_close: Option<Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
    registry: Option<Arc<PersistentRegistry>>,
    owner: String,
    performance: crate::performance::PerformanceTrace,
}

async fn serve_with_options<R, W>(
    reader: R,
    writer: W,
    options: ServeOptions,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let buf_reader = BufReader::new(reader);
    let (output_sender, output_rx) = mpsc::channel::<String>(100);
    let output_tx = if options.registry.is_some() {
        OutputSink::detachable(output_sender)
    } else {
        OutputSink::direct(output_sender)
    };

    let input_task = run_input_loop(buf_reader, output_tx, options);
    let output_task = run_output_loop(writer, output_rx);

    tokio::try_join!(input_task, output_task)?;
    Ok(())
}

async fn run_input_loop<R>(
    mut buf_reader: BufReader<R>,
    output_tx: OutputSink,
    options: ServeOptions,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let ServeOptions {
        engine_factory,
        session_port_factory,
        owner_close,
        registry,
        owner,
        performance,
    } = options;
    let mut surface_txs: HashMap<String, mpsc::Sender<SurfaceCommand>> = HashMap::new();
    let mut surface_epochs: HashMap<String, u64> = HashMap::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut line = String::new();
    let connection_sender = output_tx.sender().await;

    loop {
        line.clear();
        let n = buf_reader.read_line(&mut line).await?;

        if n == 0 {
            if registry.is_none() {
                for tx in surface_txs.values() {
                    if tx.send(SurfaceCommand::SessionDetach).await.is_err() {
                        // actor가 이미 종료되었다. 그 monitor가 actor error를 내보냈다.
                        continue;
                    }
                }
            } else {
                output_tx.detach_sender(&connection_sender).await;
            }
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if value.get("operation").and_then(Value::as_str) == Some("close-owner") {
                let Some(request) = value.get("request").and_then(Value::as_str) else {
                    let reply = json!({"error": "invalidParams", "reason": "the owner request has no request id"});
                    if output_tx.send(reply.to_string()).await.is_err() {
                        break;
                    }
                    continue;
                };
                let mut result = owner_close
                    .as_ref()
                    .map(|close| close())
                    // 기본값: 소유자 닫기가 없는 서비스는 그 요청을 오류로 답한다.
                    .unwrap_or_else(|| Err("owner close is unavailable".to_string()));
                if result.is_ok() {
                    if let Some(registry) = registry.as_ref() {
                        result = registry.close_owner(&owner).await;
                    }
                }
                let reply = match result {
                    Ok(()) => json!({"operation": "closed-owner", "request": request, "ok": true}),
                    Err(error) => {
                        json!({"operation": "closed-owner", "request": request, "ok": false, "error": error})
                    }
                };
                if output_tx.send(reply.to_string()).await.is_err() {
                    break;
                }
                continue;
            }
            if value.get("operation").and_then(Value::as_str) == Some("retain") {
                let Some(request) = value.get("request").and_then(Value::as_str) else {
                    let reply = json!({"error": "invalidParams", "reason": "the owner request has no request id"});
                    if output_tx.send(reply.to_string()).await.is_err() {
                        break;
                    }
                    continue;
                };
                let result = match (registry.as_ref(), retain_keys(&value)) {
                    (Some(registry), Ok(keep)) => registry.retain(&owner, &keep).await,
                    (None, _) => Err("retain is unavailable".to_string()),
                    (_, Err(error)) => Err(error),
                };
                let reply = match result {
                    Ok(closed) => {
                        json!({"operation": "retained", "request": request, "ok": true, "closed": closed})
                    }
                    Err(error) => {
                        json!({"operation": "retained", "request": request, "ok": false, "error": error})
                    }
                };
                if output_tx.send(reply.to_string()).await.is_err() {
                    break;
                }
                continue;
            }
            if value.get("operation").and_then(Value::as_str) == Some("shutdown") {
                let Some(request) = value.get("request").and_then(Value::as_str) else {
                    let reply = json!({"error": "invalidParams", "reason": "the owner request has no request id"});
                    if output_tx.send(reply.to_string()).await.is_err() {
                        break;
                    }
                    continue;
                };
                let result = if let Some(registry) = registry.as_ref() {
                    registry.request_shutdown();
                    Ok(())
                } else {
                    Err("shutdown is unavailable".to_string())
                };
                let reply = match result {
                    Ok(()) => json!({"operation": "shutdown", "request": request, "ok": true}),
                    Err(error) => {
                        json!({"operation": "shutdown", "request": request, "ok": false, "error": error})
                    }
                };
                if output_tx.send(reply.to_string()).await.is_err() {
                    break;
                }
                break;
            }
        }

        match serde_json::from_str::<Envelope>(trimmed) {
            Ok(env) => {
                let surface_id = env.surface.clone();

                if env.closed == Some(true) {
                    let registry_key =
                        local_surface_key(&surface_txs, env.root.as_deref(), &surface_id);
                    let close_result = if let Some(registry) = registry.as_ref() {
                        // 닫은 표면의 송신기와 부착 판도 이 연결에서 지운다. 남기면 같은 표면을 다시 열 때
                        // 지운 등록부 항목의 판과 비교되어 낡은 부착으로 거부된다.
                        let closed = registry.close_surface(&registry_key, &owner).await;
                        if closed.is_ok() {
                            surface_txs.remove(&registry_key);
                            surface_epochs.remove(&registry_key);
                        }
                        closed
                    } else if let Some(tx) = surface_txs.remove(&registry_key) {
                        tx.send(SurfaceCommand::SessionClose)
                            .await
                            .map_err(|_| "surface actor closed before close".to_string())
                    } else {
                        Ok(())
                    };
                    if let Err(error) = close_result {
                        let response = json!({"surface": surface_id, "body": {"error": error}});
                        if output_tx.send(response.to_string()).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    let response = json!({"surface": surface_id, "body": {}});
                    if output_tx.send(response.to_string()).await.is_err() {
                        // output channel이 닫혔으므로 serve를 끝낸다
                        break;
                    }
                } else if let Some(body) = env.body {
                    let registry_key =
                        local_surface_key(&surface_txs, env.root.as_deref(), &surface_id);
                    let tx = if let Some(tx) = surface_txs.get(&registry_key) {
                        if let Some(registry) = registry.as_ref() {
                            // 기본값: 부착 판을 기록하지 않은 표면은 0 이며 등록부의 현재 판과 다르면 낡은 부착으로 거부한다.
                            let epoch = surface_epochs.get(&registry_key).copied().unwrap_or(0);
                            if registry.current_epoch(&registry_key, &owner).await != Some(epoch) {
                                let response = json!({"surface": surface_id, "body": {"error": "stale attachment"}});
                                if output_tx.send(response.to_string()).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                        }
                        tx.clone()
                    } else if let Some(registry) = registry.as_ref() {
                        match registry.attach(&registry_key, &owner, &output_tx).await {
                            Ok((tx, epoch)) => {
                                surface_epochs.insert(registry_key.clone(), epoch);
                                if tx.send(SurfaceCommand::Reconnect).await.is_err() {
                                    let response = json!({"surface": surface_id, "body": {"error": "persistent surface actor closed before reconnect"}});
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                tx
                            }
                            Err(error) if registry.contains(&registry_key).await => {
                                let response = json!({"surface": surface_id, "body": {"error": "persistent attach failed", "reason": error}});
                                if output_tx.send(response.to_string()).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                            Err(error) => {
                                if error != "session surface not found" {
                                    let response = json!({"surface": surface_id, "body": {"error": "persistent attach failed", "reason": error}});
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }

                                // persistent service는 실제로 새로운 surface에 대해
                                // 새 actor를 만들어야 한다. 기존 registry entry만
                                // 다시 attach할 수 있다. 어떤 error도 기존 session의
                                // 암묵적 교체로 변환하지 않는다.
                                let (cmd_tx, cmd_rx) = mpsc::channel(10);
                                let session_port = session_port_factory();
                                let factory = engine_factory.clone();
                                let out_tx = output_tx.clone();
                                let sid = surface_id.clone();
                                let task_trace = performance.clone();
                                let actor = tokio::spawn(async move {
                                    surface_task(
                                        sid,
                                        task_trace,
                                        factory,
                                        session_port,
                                        cmd_rx,
                                        out_tx,
                                    )
                                    .await;
                                });
                                match registry
                                    .insert(
                                        registry_key.clone(),
                                        owner.clone(),
                                        cmd_tx.clone(),
                                        output_tx.clone(),
                                        actor,
                                    )
                                    .await
                                {
                                    Ok(epoch) => {
                                        surface_epochs.insert(registry_key.clone(), epoch);
                                        cmd_tx
                                    }
                                    Err(insert_error) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "persistent surface creation failed", "reason": insert_error}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                        continue;
                                    }
                                }
                            }
                        }
                    } else {
                        // 새 표면: 작업 생성
                        let (cmd_tx, cmd_rx) = mpsc::channel(10);
                        let session_port = session_port_factory();
                        let factory = engine_factory.clone();
                        let out_tx = output_tx.clone();
                        let sid = surface_id.clone();
                        let sid_for_monitor = sid.clone();
                        let task_trace = performance.clone();

                        // 실제 surface task를 별도 handle로 spawn한다
                        let surface_handle = tokio::spawn(async move {
                            surface_task(sid, task_trace, factory, session_port, cmd_rx, out_tx)
                                .await;
                        });

                        // panic을 감시하는 monitor task를 spawn한다
                        let out_tx_monitor = output_tx.clone();
                        tasks.spawn(async move {
                            match surface_handle.await {
                                Ok(_) => {},
                                Err(join_err) if join_err.is_panic() => {
                                    // task가 panic했으므로 error event를 보낸다
                                    let panic_msg = if let Ok(panic_obj) = join_err.try_into_panic() {
                                        if let Some(s) = panic_obj.downcast_ref::<String>() {
                                            s.clone()
                                        } else if let Some(&s) = panic_obj.downcast_ref::<&str>() {
                                            s.to_string()
                                        } else {
                                            "unknown".to_string()
                                        }
                                    } else {
                                        "unknown".to_string()
                                    };
                                    let response = json!({
                                        "surface": sid_for_monitor,
                                        "body": {"event": "error", "reason": format!("surface task ended: {}", panic_msg)}
                                    });
                                    if let Err(error) = out_tx_monitor.send(response.to_string()).await {
                                        eprintln!("surface panic report was not delivered: {error:?}");
                                    }
                                }
                                Err(error) => {
                                    eprintln!("surface task was cancelled: {error}");
                                }
                            }
                        });

                        surface_txs.insert(registry_key.clone(), cmd_tx.clone());
                        cmd_tx
                    };
                    surface_txs.insert(registry_key.clone(), tx.clone());

                    // operation field를 먼저 확인한다(request이다).
                    if let Some(operation) = body.get("operation").and_then(|v| v.as_str()) {
                        match operation {
                            "reconnect" => {
                                if tx.send(SurfaceCommand::Reconnect).await.is_err() {
                                    break;
                                }
                            }
                            "open" => {
                                let image = body
                                    .get("image")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                let Some(shell) = body
                                    .get("shell")
                                    .and_then(|v| v.as_str())
                                    .filter(|shell| !shell.is_empty())
                                else {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "open requires a shell"}});
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                    continue;
                                };
                                let directory = match body.get("directory") {
                                    None => None,
                                    Some(Value::String(directory))
                                        if directory.starts_with('/') =>
                                    {
                                        Some(directory.clone())
                                    }
                                    Some(_) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "open directory must be an absolute path"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                        continue;
                                    }
                                };
                                let request = ShellRequest {
                                    shell: shell.to_string(),
                                    directory,
                                };
                                if tx
                                    .send(SurfaceCommand::Open { image, request })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "input" => {
                                if let Some(value) = body.get("compose") {
                                    let parsed = if value.is_null() {
                                        Ok(None)
                                    } else {
                                        serde_json::from_value::<Preedit>(value.clone()).map(Some)
                                    };
                                    match parsed {
                                        Ok(preedit) => {
                                            if tx
                                                .send(SurfaceCommand::Compose { preedit })
                                                .await
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                        Err(error) => {
                                            let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": format!("compose: {error}")}});
                                            if output_tx.send(response.to_string()).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                }
                                if let Some(value) = body.get("focus") {
                                    if let Some(focused) =
                                        value.get("focused").and_then(Value::as_bool)
                                    {
                                        if tx.send(SurfaceCommand::Focus { focused }).await.is_err()
                                        {
                                            break;
                                        }
                                    } else {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "focus.focused must be boolean"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                                if let Some(value) = body.get("command") {
                                    if let Some(selector) =
                                        value.get("selector").and_then(Value::as_str)
                                    {
                                        match key_for_native_command(selector) {
                                            Ok(key) => {
                                                if tx
                                                    .send(SurfaceCommand::InputKeys {
                                                        keys: vec![key],
                                                    })
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(reason) => {
                                                let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                                if output_tx
                                                    .send(response.to_string())
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                        }
                                    } else {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "command.selector must be string"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                                // compose/focus/command가 있으면 bytes와 keys는 계속 선택 사항이다.
                                let has_bytes = body.get("bytes").is_some();
                                let has_keys = body.get("keys").is_some();
                                let has_native_input = body.get("compose").is_some()
                                    || body.get("focus").is_some()
                                    || body.get("command").is_some();

                                if !has_bytes && !has_keys && !has_native_input {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": "input requires bytes or keys field"}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                } else {
                                    // bytes를 먼저 처리한다(있는 경우)
                                    if let Some(bytes_b64) =
                                        body.get("bytes").and_then(|v| v.as_str())
                                    {
                                        match base64_decode(bytes_b64) {
                                            Ok(bytes) => {
                                                if tx
                                                    .send(SurfaceCommand::Input { bytes })
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                let response = json!({
                                                    "surface": surface_id,
                                                    "body": {"error": format!("Base64 error: {}", e)}
                                                });
                                                if output_tx
                                                    .send(response.to_string())
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                        }
                                    } else if has_bytes {
                                        // bytes field가 있지만 string이 아니다
                                        let response = json!({
                                            "surface": surface_id,
                                            "body": {"error": "invalidParams", "reason": "bytes must be a string"}
                                        });
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }

                                    // 그다음 keys를 처리한다(있는 경우)
                                    if let Some(keys_arr) =
                                        body.get("keys").and_then(|v| v.as_array())
                                    {
                                        match serde_json::from_value::<Vec<InputKey>>(Value::Array(
                                            keys_arr.clone(),
                                        )) {
                                            Ok(keys) => {
                                                if tx
                                                    .send(SurfaceCommand::InputKeys { keys })
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                let response = json!({
                                                    "surface": surface_id,
                                                    "body": {"error": format!("Keys parse error: {}", e)}
                                                });
                                                if output_tx
                                                    .send(response.to_string())
                                                    .await
                                                    .is_err()
                                                {
                                                    break;
                                                }
                                            }
                                        }
                                    } else if has_keys {
                                        // keys field가 있지만 array가 아니다
                                        let response = json!({
                                            "surface": surface_id,
                                            "body": {"error": "invalidParams", "reason": "keys must be an array"}
                                        });
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "theme" => {
                                let text = |name: &str| body.get(name).and_then(Value::as_str);
                                // 모드와 네 색이 모두 맞아야 적용한다. 하나라도 틀리면 아무것도 바꾸지 않는다.
                                match crate::palette::TerminalTheme::from_request(
                                    text("mode"),
                                    text("background"),
                                    text("foreground"),
                                    text("cursor"),
                                    text("selection"),
                                ) {
                                    Ok(theme) => {
                                        if tx
                                            .send(SurfaceCommand::Theme {
                                                theme: Box::new(theme),
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "font" => {
                                let size = match body.get("size").and_then(Value::as_f64) {
                                    Some(size) if (FONT_SIZE_MIN..=FONT_SIZE_MAX).contains(&size) => Ok(size as f32),
                                    _ => Err(format!("font.size must be a number from {FONT_SIZE_MIN} to {FONT_SIZE_MAX} points")),
                                };
                                let resolved = match body.get("family").and_then(Value::as_str) {
                                    Some(list) => crate::platform::resolve_font_list(list),
                                    None => Err("font.family must be a string".to_string()),
                                };
                                match size
                                    .and_then(|size| resolved.map(|selection| (selection, size)))
                                {
                                    Ok((selection, size)) => {
                                        // 설치되어 있지 않은 family 는 오류가 아니라 로그에 남긴다.
                                        for family in &selection.skipped {
                                            eprintln!(
                                                "terminal font family is not installed: {family}"
                                            );
                                        }
                                        if selection.system {
                                            eprintln!("no listed terminal font family is installed; using the system fixed-pitch font");
                                        }
                                        let (font, system, skipped) =
                                            (selection.font, selection.system, selection.skipped);
                                        if tx
                                            .send(SurfaceCommand::Font {
                                                font,
                                                system,
                                                skipped,
                                                size,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(reason) => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": reason, "operation": "font"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "cursor" => match parse_cursor_policy(&body) {
                                Ok(policy) => {
                                    if tx.send(SurfaceCommand::Cursor { policy }).await.is_err() {
                                        break;
                                    }
                                }
                                Err(reason) => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": reason}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                }
                            },
                            "paste" => match body.get("text").and_then(Value::as_str) {
                                Some(text) => {
                                    if tx
                                        .send(SurfaceCommand::Paste {
                                            text: text.to_string(),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                }
                                None => {
                                    let response = json!({
                                        "surface": surface_id,
                                        "body": {"error": "invalidParams", "reason": "paste requires text string"}
                                    });
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                }
                            },
                            "mouse" => {
                                let phase = match body.get("phase").and_then(Value::as_str) {
                                    Some("down") => Some(MousePhase::Down),
                                    Some("move") => Some(MousePhase::Move),
                                    Some("up") => Some(MousePhase::Up),
                                    _ => None,
                                };
                                let x = body
                                    .get("x")
                                    .and_then(Value::as_f64)
                                    .filter(|x| x.is_finite());
                                let y = body
                                    .get("y")
                                    .and_then(Value::as_f64)
                                    .filter(|y| y.is_finite());
                                let flag = |name: &str| body.get(name).and_then(Value::as_bool);
                                let input_id = body
                                    .get("inputId")
                                    .and_then(Value::as_str)
                                    .filter(|id| !id.is_empty());
                                match (
                                    input_id,
                                    phase,
                                    x,
                                    y,
                                    flag("pressed"),
                                    flag("shift"),
                                    flag("alt"),
                                    flag("ctrl"),
                                ) {
                                    (
                                        Some(input_id),
                                        Some(phase),
                                        Some(x),
                                        Some(y),
                                        Some(pressed),
                                        Some(shift),
                                        Some(alt),
                                        Some(ctrl),
                                    ) => {
                                        if tx
                                            .send(SurfaceCommand::Mouse {
                                                input_id: input_id.to_string(),
                                                phase,
                                                x,
                                                y,
                                                pressed,
                                                shift,
                                                alt,
                                                ctrl,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams",
                                            "reason": "mouse requires phase down, move, or up, finite x and y, boolean pressed, shift, alt, and ctrl, and non-empty inputId"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "selection.start" | "selection.update" => {
                                let x = body.get("x").and_then(Value::as_f64);
                                let y = body.get("y").and_then(Value::as_f64);
                                match (x, y) {
                                    (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                                        let command = if operation == "selection.start" {
                                            SurfaceCommand::SelectionStart { x, y }
                                        } else {
                                            SurfaceCommand::SelectionUpdate { x, y }
                                        };
                                        if tx.send(command).await.is_err() {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "selection requires finite x and y"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "selection.end" => {
                                if tx.send(SurfaceCommand::SelectionEnd).await.is_err() {
                                    break;
                                }
                            }
                            "copy" => {
                                if tx.send(SurfaceCommand::Copy).await.is_err() {
                                    break;
                                }
                            }
                            "viewport" => {
                                let Some(offset) = body
                                    .get("offset")
                                    .and_then(Value::as_u64)
                                    .and_then(|v| u32::try_from(v).ok())
                                else {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "viewport requires a nonnegative integer offset"}});
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                    continue;
                                };
                                if tx.send(SurfaceCommand::Viewport { offset }).await.is_err() {
                                    break;
                                }
                            }
                            "scroll" => {
                                let lines = body
                                    .get("lines")
                                    .and_then(Value::as_i64)
                                    .filter(|lines| *lines != 0 && i32::try_from(*lines).is_ok());
                                let col = body
                                    .get("col")
                                    .and_then(Value::as_u64)
                                    .and_then(|v| u16::try_from(v).ok());
                                let row = body
                                    .get("row")
                                    .and_then(Value::as_u64)
                                    .and_then(|v| u16::try_from(v).ok());
                                let (Some(lines), Some(col), Some(row)) = (lines, col, row) else {
                                    let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "scroll requires a nonzero integer lines and cell col and row"}});
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                    continue;
                                };
                                if tx
                                    .send(SurfaceCommand::Scroll {
                                        lines: lines as i32,
                                        col,
                                        row,
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            "screen.read" => {
                                if tx.send(SurfaceCommand::ScreenRead).await.is_err() {
                                    break;
                                }
                            }
                            "clipboard.resolve" => {
                                let request_id = body.get("requestId").and_then(Value::as_u64);
                                let text =
                                    body.get("text").and_then(Value::as_str).map(str::to_string);
                                match (request_id, text) {
                                    (Some(request_id), Some(text)) => {
                                        if tx
                                            .send(SurfaceCommand::ClipboardResolve {
                                                request_id,
                                                text,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "clipboard.resolve requires requestId and text"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "clipboard.reject" => {
                                let request_id = body.get("requestId").and_then(Value::as_u64);
                                let reason = body
                                    .get("reason")
                                    .and_then(Value::as_str)
                                    .map(str::to_string);
                                match (request_id, reason) {
                                    (Some(request_id), Some(reason)) if !reason.is_empty() => {
                                        if tx
                                            .send(SurfaceCommand::ClipboardReject {
                                                request_id,
                                                reason,
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    _ => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "clipboard.reject requires requestId and non-empty reason"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "image.inline.delete" => {
                                match body
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .filter(|name| !name.is_empty())
                                {
                                    Some(name) => {
                                        if tx
                                            .send(SurfaceCommand::InlineImageDelete {
                                                name: name.to_string(),
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    None => {
                                        let response = json!({"surface": surface_id, "body": {"error": "invalidParams", "reason": "image.inline.delete requires a non-empty name"}});
                                        if output_tx.send(response.to_string()).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            }
                            "pty.pending" => {
                                if tx.send(SurfaceCommand::PtyPending).await.is_err() {
                                    break;
                                }
                            }
                            "close" => {
                                let close_result = if let Some(registry) = registry.as_ref() {
                                    registry.close_surface(&registry_key, &owner).await
                                } else if let Some(tx) = surface_txs.remove(&registry_key) {
                                    tx.send(SurfaceCommand::SessionClose).await.map_err(|_| {
                                        "surface actor closed before close".to_string()
                                    })
                                } else {
                                    Ok(())
                                };
                                if let Err(error) = close_result {
                                    let response =
                                        json!({"surface": surface_id, "body": {"error": error}});
                                    if output_tx.send(response.to_string()).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            _ => {
                                let response = json!({
                                    "surface": surface_id,
                                    "body": {"error": format!("Unknown operation: {}", operation)}
                                });
                                if output_tx.send(response.to_string()).await.is_err() {
                                    break;
                                }
                            }
                        }
                    } else if let Some(image) = body.get("image").and_then(|v| v.as_object()) {
                        if let Some(configure) = image.get("configure").and_then(|v| v.as_object())
                        {
                            let parsed = (|| {
                                let name = configure.get("name")?.as_str()?.to_string();
                                let generation = configure.get("generation")?.as_u64()?;
                                let raster = configure.get("raster")?.as_u64()?;
                                let width =
                                    u32::try_from(configure.get("width")?.as_u64()?).ok()?;
                                let height =
                                    u32::try_from(configure.get("height")?.as_u64()?).ok()?;
                                let scale = configure.get("scale")?.as_f64()? as f32;
                                (generation > 0
                                    && raster > 0
                                    && width > 0
                                    && height > 0
                                    && scale.is_finite()
                                    && scale > 0.0)
                                    .then_some(ImageConfiguration {
                                        name,
                                        generation,
                                        raster,
                                        width,
                                        height,
                                        scale,
                                    })
                            })();
                            if let Some(configuration) = parsed {
                                if tx
                                    .send(SurfaceCommand::Configure(configuration))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            } else {
                                let response = json!({"surface": surface_id,
                                    "body": {"error": "invalidParams", "reason": "invalid image configure"}});
                                if output_tx.send(response.to_string()).await.is_err() {
                                    break;
                                }
                            }
                        } else if tx
                            .send(SurfaceCommand::ImageResponse { body: body.clone() })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        // operation도 image object도 없으면 알 수 없는 message이다.
                        let response = json!({
                            "surface": surface_id,
                            "body": {"error": "unknown operation"}
                        });
                        if output_tx.send(response.to_string()).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                let response = json!({
                    "surface": "",
                    "body": {"error": format!("Parse error: {}", e)}
                });
                if output_tx.send(response.to_string()).await.is_err() {
                    // output channel이 닫혔으므로 serve를 끝낸다
                    break;
                }
            }
        }
    }

    // persistent transport는 예기치 않은 client 연결 끊김 동안 surface actor와 PTY session을
    // 유지한다. 정상적인 application 종료는 명시적인 close-owner operation을 보내고
    // 그곳에서 그것들을 닫는다. persistent가 아닌 transport에는 복구 owner가 없으므로
    // 그 surface를 지금 닫아야 한다.
    if registry.is_none() {
        for tx in surface_txs.values() {
            tx.send(SurfaceCommand::SessionClose)
                .await
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        format!("close surface during serve shutdown: {error}"),
                    )
                })?;
        }
    }

    // 모든 monitor task가 완료될 때까지 기다린다
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn run_output_loop<W>(
    mut writer: W,
    mut output_rx: mpsc::Receiver<String>,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(output) = output_rx.recv().await {
        writer.write_all(output.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|error| format!("invalid base64 input: {error}"))
}

/// 기본 SessionPort 팩토리를 만든다 (DaemonFinder를 사용)
pub fn make_default_session_port_factory() -> Arc<dyn Fn() -> Arc<dyn SessionPort> + Send + Sync> {
    let service = Arc::new(crate::pty::PtyService::new());
    Arc::new(move || Arc::new(LocalSessionPort::new(service.clone())) as Arc<dyn SessionPort>)
}

/// in-process PTY session port이다.  service는 모든 surface port가 공유한다.
/// `open` 호출마다 여전히 독립된 PTY session 하나를 생성한다.
pub struct LocalSessionPort {
    service: Arc<crate::pty::PtyService>,
    owner: String,
    events_tx: mpsc::UnboundedSender<DaemonEvent>,
    events_rx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<DaemonEvent>>>>,
    attachments: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
}

impl LocalSessionPort {
    fn new(service: Arc<crate::pty::PtyService>) -> Self {
        Self::new_with_owner(service, String::new())
    }

    pub fn new_with_owner(service: Arc<crate::pty::PtyService>, owner: String) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            service,
            owner,
            events_tx,
            events_rx: Arc::new(tokio::sync::Mutex::new(Some(events_rx))),
            attachments: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl SessionPort for LocalSessionPort {
    async fn open(&self, request: &ShellRequest, cols: u16, rows: u16) -> Result<String, String> {
        let service = Arc::clone(&self.service);
        let owner = self.owner.clone();
        let shell = crate::pty::resolve_shell(&request.shell)?;
        let directory = request
            .directory
            .as_deref()
            .map(crate::pty::resolve_directory)
            .transpose()?;
        let events = self.events_tx.clone();
        let (session_id, attachment_id) = tokio::task::spawn_blocking(move || {
            service.open_shell(&owner, &shell, directory.as_deref(), cols, rows, events)
        })
        .await
        .map_err(|error| format!("open PTY task failed: {error}"))??;
        self.attachments
            .lock()
            .await
            .insert(session_id.clone(), attachment_id);
        Ok(session_id)
    }

    async fn write(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        self.service.write(session_id, data)
    }

    async fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), String> {
        self.service.resize(session_id, cols, rows)
    }

    async fn detach(&self, session_id: &str) -> Result<(), String> {
        if let Some(attachment_id) = self.attachments.lock().await.remove(session_id) {
            self.service.detach(session_id, &attachment_id)?;
        }
        Ok(())
    }

    async fn close(&self, session_id: &str) -> Result<(), String> {
        self.attachments.lock().await.remove(session_id);
        let service = Arc::clone(&self.service);
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || service.close(&session_id))
            .await
            .map_err(|error| format!("close PTY task failed: {error}"))?
    }

    async fn attach(&self, session_id: &str, from: i64) -> Result<String, String> {
        let attachment_id = self
            .service
            .attach(session_id, from, self.events_tx.clone())?;
        self.attachments
            .lock()
            .await
            .insert(session_id.to_string(), attachment_id.clone());
        Ok(attachment_id)
    }

    async fn pty_measurement(
        &self,
        session_id: &str,
    ) -> Result<crate::pty::PtyMeasurement, String> {
        self.service.pty_measurement(session_id)
    }

    async fn get_events(&self) -> mpsc::Receiver<DaemonEvent> {
        let (tx, rx) = mpsc::channel(128);
        let mut events_rx = self.events_rx.lock().await;
        if let Some(mut source) = events_rx.take() {
            tokio::spawn(async move {
                while let Some(event) = source.recv().await {
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
            });
        }
        rx
    }
}

/// Base64 encode
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
#[path = "../tests/support/protocol_test.rs"]
mod tests;
