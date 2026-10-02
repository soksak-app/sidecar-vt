use super::*;

pub struct FakeEngine {
    cols: u16,
    rows: u16,
    content: String,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self {
            cols: 80,
            rows: 24,
            content: String::new(),
        }
    }
}

impl Engine for FakeEngine {
    fn set_theme(&mut self, _theme: crate::palette::TerminalTheme) {}

    fn drain_events(&mut self) -> Vec<EngineEvent> {
        Vec::new()
    }

    fn resolve_clipboard(&mut self, _request_id: u64, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn reject_clipboard(&mut self, _request_id: u64, _reason: &str) -> Result<(), String> {
        Ok(())
    }

    fn selection_start(&mut self, _col: u16, _row: u16) -> Result<(), String> {
        Ok(())
    }
    fn selection_update(&mut self, _col: u16, _row: u16) -> Result<(), String> {
        Ok(())
    }
    fn selection_end(&mut self) -> Result<Option<String>, String> {
        Ok(Some("selected".to_string()))
    }
    fn selection_clear(&mut self) -> bool {
        false
    }
    fn selection_text(&self) -> Option<String> {
        Some("selected".to_string())
    }
    fn scroll_viewport(&mut self, _lines: i32) {}
    fn scroll_to_newest(&mut self) -> bool {
        false
    }

    fn cursor(&self) -> Cursor {
        Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: false,
            preedit: None,
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    fn set_cell_metrics(&mut self, _width: u16, _height: u16) -> Result<(), String> {
        Ok(())
    }

    fn feed(&mut self, bytes: &[u8]) {
        if let Ok(s) = std::str::from_utf8(bytes) {
            self.content.push_str(s);
        }
    }

    fn screen(&mut self) -> Screen {
        let mut lines = Vec::new();
        for line in self.content.lines() {
            let mut row = Vec::new();
            for ch in line.chars() {
                let width = if (ch as u32) > 127 { 2 } else { 1 };
                let cell = Cell {
                    ch: Some(ch.to_string()),
                    width,
                    ..Default::default()
                };
                row.push(cell);
            }
            lines.push(row);
        }
        Screen {
            cols: self.cols,
            rows: self.rows,
            cursor: Cursor {
                col: 0,
                row: 0,
                shape: CursorShape::Block,
                visible: true,
                blinking: false,
                blink_visible: true,
                focused: false,
                preedit: None,
            },
            scrollback: Default::default(),
            background: "#1e1e1e".to_string(),
            lines,
        }
    }

    fn modes(&self) -> Modes {
        Modes::default()
    }

    fn reset(&mut self) {
        self.content.clear();
    }
}

#[path = "session_port_test.rs"]
mod session_port;
use session_port::FakeSessionPort;

#[test]
fn mouse_drag_motion_uses_the_owner_latched_at_press() {
    assert!(mouse_drag_motion_owned(true, true, true));
    assert!(!mouse_drag_motion_owned(true, false, true));
    assert!(!mouse_drag_motion_owned(false, true, true));
    assert!(!mouse_drag_motion_owned(true, true, false));
}

#[test]
fn native_edit_commands_encode_and_unknown_commands_fail() {
    let key = key_for_native_command("insertNewline:").expect("newline command");
    assert_eq!(encode_keys(&[key], &Modes::default()).unwrap(), b"\r");
    let backspace = key_for_native_command("deleteBackward:").expect("backspace command");
    assert_eq!(
        encode_keys(&[backspace], &Modes::default()).unwrap(),
        b"\x7f"
    );
    let delete = key_for_native_command("deleteForward:").expect("delete command");
    assert_eq!(
        encode_keys(&[delete], &Modes::default()).unwrap(),
        b"\x1b[3~"
    );
    assert_eq!(
        key_for_native_command("cancelOperation:").unwrap_err(),
        "unsupported native command selector: cancelOperation:"
    );
}

#[test]
fn unsupported_ctrl_character_error_identifies_the_received_scalar() {
    let error = encode_keys(
        &[InputKey {
            key: "Char".to_string(),
            text: "ㅕ".to_string(),
            shift: false,
            alt: false,
            ctrl: true,
        }],
        &Modes::default(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        "unknown key: Char with ctrl: Unsupported (text 'ㅕ', U+3155)"
    );
}

#[test]
fn test_base64_decode() {
    assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
    assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
}

#[tokio::test]
async fn previous_client_eof_preserves_new_attachment_output() {
    let (previous_sender, _previous_receiver) = mpsc::channel(4);
    let output = OutputSink::detachable(previous_sender);
    let (client, reader) = tokio::io::duplex(64);
    let input = run_input_loop(
        BufReader::new(reader),
        Arc::new(|| Box::new(FakeEngine::new())),
        Arc::new(|| Arc::new(FakeSessionPort::new())),
        output.clone(),
        None,
        Some(PersistentRegistry::new()),
        "test-owner".to_string(),
        crate::performance::PerformanceTrace::disabled(),
    );
    tokio::pin!(input);
    // 이전 연결을 읽기 대기까지 진행한 뒤 새 부착과 EOF 순서를 제어한다.
    tokio::select! {
        biased;
        result = &mut input => panic!("previous input ended before EOF: {result:?}"),
        _ = std::future::ready(()) => {}
    }
    let (new_sender, mut new_receiver) = mpsc::channel(4);
    output.replace_sender(Some(new_sender.clone())).await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), input)
        .await
        .expect("previous EOF did not complete")
        .expect("previous input failed");
    assert!(
        output
            .sender()
            .await
            .is_some_and(|sender| sender.same_channel(&new_sender)),
        "previous client EOF removed the new attachment output sender"
    );
    output
        .send("new attachment screen".to_string())
        .await
        .expect("new output failed");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), new_receiver.recv())
            .await
            .expect("new attachment output timed out")
            .as_deref(),
        Some("new attachment screen")
    );
}

#[tokio::test]
async fn a_persistent_sink_detaches_when_its_client_output_closed_and_a_direct_sink_fails() {
    // 영속 서비스의 세션은 클라이언트가 끊겨도 유지된다. 출력 쪽이 먼저 닫힌 연결로 보낸 출력은 분리로 다룬다.
    let (sender, receiver) = mpsc::channel(4);
    let persistent = OutputSink::detachable(sender);
    drop(receiver);
    assert!(persistent.send("screen".to_string()).await.is_ok());
    assert!(
        persistent.sender().await.is_none(),
        "the closed client must be detached"
    );
    let (sender, receiver) = mpsc::channel(4);
    let direct = OutputSink::direct(sender);
    drop(receiver);
    assert!(
        direct.send("screen".to_string()).await.is_err(),
        "a closed direct output is an error"
    );
}

#[tokio::test]
async fn persistent_registry_rejects_stale_owner_and_awaits_actor_close() {
    let registry = PersistentRegistry::new();
    let (tx, mut rx) = mpsc::channel(4);
    let actor = tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            if matches!(command, SurfaceCommand::SessionClose) {
                break;
            }
        }
    });
    let (output_sender, _output_receiver) = mpsc::channel(4);
    registry.entries.lock().await.insert(
        "root\0surface".to_string(),
        PersistentEntry {
            tx,
            output: OutputSink::direct(output_sender),
            actor,
            epoch: 1,
            owner: "client-a".to_string(),
        },
    );
    let (new_sender, _new_receiver) = mpsc::channel(4);
    let attached = registry
        .attach("root\0surface", "client-a", &OutputSink::direct(new_sender))
        .await
        .unwrap();
    assert!(attached.1 > 1);
    assert!(registry
        .attach(
            "root\0surface",
            "client-b",
            &OutputSink::direct(mpsc::channel(1).0)
        )
        .await
        .is_err());
    registry.close_owner("client-a").await.unwrap();
    assert!(!registry.contains("root\0surface").await);
}

fn closing_actor() -> (mpsc::Sender<SurfaceCommand>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = mpsc::channel(1);
    let actor = tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            if matches!(command, SurfaceCommand::SessionClose) {
                break;
            }
        }
    });
    (tx, actor)
}

#[tokio::test]
async fn persistent_registry_retain_closes_only_the_owners_unlisted_sessions() {
    let registry = PersistentRegistry::new();
    for (key, owner) in [
        ("root\0kept", "client-a"),
        ("root\0orphan", "client-a"),
        ("root\0other", "client-b"),
    ] {
        let (tx, actor) = closing_actor();
        let (output, _events) = mpsc::channel(1);
        registry.entries.lock().await.insert(
            key.to_string(),
            PersistentEntry {
                tx,
                output: OutputSink::direct(output),
                actor,
                epoch: 1,
                owner: owner.to_string(),
            },
        );
    }
    let keep = std::collections::HashSet::from(["root\0kept".to_string()]);
    assert_eq!(registry.retain("client-a", &keep).await, Ok(1));
    assert!(registry.contains("root\0kept").await);
    assert!(!registry.contains("root\0orphan").await);
    assert!(
        registry.contains("root\0other").await,
        "another client's session stays"
    );
}

async fn assert_cleanup_closes_remaining_actors(retain: bool) {
    let registry = PersistentRegistry::new();
    let closed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for key in ["first", "second", "third"] {
        let (tx, mut rx) = mpsc::channel(1);
        let seen = Arc::clone(&closed);
        let actor = tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                if matches!(command, SurfaceCommand::SessionClose) {
                    seen.fetch_add(1, Ordering::SeqCst);
                    break;
                }
            }
        });
        let (output, _events) = mpsc::channel(1);
        registry.entries.lock().await.insert(
            key.to_string(),
            PersistentEntry {
                tx,
                actor,
                output: OutputSink::direct(output),
                epoch: 1,
                owner: "client".to_string(),
            },
        );
    }
    // HashMap 순서는 바꾸지 않고 첫 항목의 액터만 이미 종료된 상태로 만든다.
    let (dead_tx, dead_rx) = mpsc::channel(1);
    drop(dead_rx);
    {
        let mut entries = registry.entries.lock().await;
        let entry = entries.values_mut().next().expect("first entry");
        entry.tx = dead_tx;
        entry.actor = tokio::spawn(async {});
    }
    let result = if retain {
        registry
            .retain("client", &std::collections::HashSet::new())
            .await
            .map(|_| ())
    } else {
        registry.close_owner("client").await
    };
    assert!(
        result.is_err(),
        "the dead actor must be reported, retain={retain}"
    );
    assert_eq!(
        closed.load(Ordering::SeqCst),
        2,
        "remaining actors did not receive and finish SessionClose, retain={retain}"
    );
    assert!(
        registry.close_owner("client").await.is_ok(),
        "subsequent cleanup failed"
    );
}

#[tokio::test]
async fn cleanup_reports_each_failed_surface() {
    for retain in [false, true] {
        let registry = PersistentRegistry::new();
        for key in ["failed-a", "failed-b"] {
            let (tx, rx) = mpsc::channel(1);
            drop(rx);
            let (output, _events) = mpsc::channel(1);
            registry.entries.lock().await.insert(
                key.to_string(),
                PersistentEntry {
                    tx,
                    actor: tokio::spawn(async {}),
                    output: OutputSink::direct(output),
                    epoch: 1,
                    owner: "client".to_string(),
                },
            );
        }
        let error = if retain {
            registry
                .retain("client", &std::collections::HashSet::new())
                .await
                .map(|_| ())
        } else {
            registry.close_owner("client").await
        }
        .expect_err("failed actors must be reported");
        for key in ["failed-a", "failed-b"] {
            assert!(error.contains(key), "missing failed surface {key}: {error}");
        }
    }
}

#[tokio::test]
async fn owner_cleanup_closes_remaining_actors_after_an_actor_failure() {
    assert_cleanup_closes_remaining_actors(false).await;
}

#[tokio::test]
async fn retain_closes_remaining_actors_after_an_actor_failure() {
    assert_cleanup_closes_remaining_actors(true).await;
}

#[tokio::test]
async fn persistent_registry_close_owner_keeps_other_client_sessions() {
    let registry = PersistentRegistry::new();
    let (a_tx, mut a_rx) = mpsc::channel(1);
    let a_actor = tokio::spawn(async move {
        while let Some(command) = a_rx.recv().await {
            if matches!(command, SurfaceCommand::SessionClose) {
                break;
            }
        }
    });
    let (b_tx, mut b_rx) = mpsc::channel(1);
    let b_actor = tokio::spawn(async move {
        while let Some(command) = b_rx.recv().await {
            if matches!(command, SurfaceCommand::SessionClose) {
                break;
            }
        }
    });
    let (a_output, _a_events) = mpsc::channel(1);
    let (b_output, _b_events) = mpsc::channel(1);
    registry.entries.lock().await.insert(
        "root\0a".to_string(),
        PersistentEntry {
            tx: a_tx,
            output: OutputSink::direct(a_output),
            actor: a_actor,
            epoch: 1,
            owner: "client-a".to_string(),
        },
    );
    registry.entries.lock().await.insert(
        "root\0b".to_string(),
        PersistentEntry {
            tx: b_tx,
            output: OutputSink::direct(b_output),
            actor: b_actor,
            epoch: 1,
            owner: "client-b".to_string(),
        },
    );

    registry.close_owner("client-a").await.unwrap();
    assert!(!registry.contains("root\0a").await);
    assert!(registry.contains("root\0b").await);
    registry.close_owner("client-b").await.unwrap();
    assert!(!registry.contains("root\0b").await);
}

#[tokio::test]
async fn persistent_surface_close_does_not_wait_forever_for_actor_exit() {
    let registry = PersistentRegistry::new();
    let (tx, mut rx) = mpsc::channel(1);
    let actor = tokio::spawn(async move {
        while let Some(_command) = rx.recv().await {
            std::future::pending::<()>().await;
        }
    });
    let (output, _events) = mpsc::channel(1);
    registry.entries.lock().await.insert(
        "root\0surface".to_string(),
        PersistentEntry {
            tx,
            output: OutputSink::direct(output),
            actor,
            epoch: 1,
            owner: "client".to_string(),
        },
    );

    let result = tokio::time::timeout(
        Duration::from_millis(200),
        registry.close_surface("root\0surface", "client"),
    )
    .await;
    result
        .expect("surface close blocked the persistent input loop")
        .expect("surface close could not queue actor cleanup");
}

#[tokio::test]
async fn a_surface_reopened_after_its_closed_notice_is_not_a_stale_attachment() {
    // 창이 닫혀 closed 알림을 받은 표면을 같은 연결에서 다시 열면 낡은 부착이 아니라 새 표면이다.
    let (sender, mut receiver) = mpsc::channel(64);
    let output = OutputSink::direct(sender);
    let open = r#"{"surface":"s1","root":"/tmp","body":{"operation":"open","shell":"/bin/sh","image":"view"}}"#;
    let input = format!(
        "{open}\n{}\n{open}\n{}\n",
        r#"{"surface":"s1","root":"/tmp","closed":true}"#,
        r#"{"surface":"s1","root":"/tmp","body":{"image":{"configure":{"name":"view","generation":1,"raster":1,"width":800,"height":384,"scale":1.0}}}}"#,
    );
    run_input_loop(
        BufReader::new(input.as_bytes()),
        Arc::new(|| Box::new(FakeEngine::new())),
        Arc::new(|| Arc::new(FakeSessionPort::new())),
        output,
        None,
        Some(PersistentRegistry::new()),
        "test-owner".to_string(),
        crate::performance::PerformanceTrace::disabled(),
    )
    .await
    .expect("input loop failed");
    let mut replies = Vec::new();
    while let Ok(line) = receiver.try_recv() {
        replies.push(line);
    }
    assert!(
        replies
            .iter()
            .all(|line| !line.contains("stale attachment")),
        "the reopened surface was rejected: {replies:?}"
    );
}
