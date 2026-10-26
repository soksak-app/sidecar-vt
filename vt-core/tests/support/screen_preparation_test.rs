use super::{
    Cell, Cursor, CursorShape, Screen, Scrollback, prepare_screen_line, screen_message_work,
};
use serde_json::{Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell as ThreadCell;

struct ScreenPreparationAllocator;

std::thread_local! {
    static COUNT_ALLOCATIONS: ThreadCell<bool> = const { ThreadCell::new(false) };
    static ALLOCATIONS: ThreadCell<usize> = const { ThreadCell::new(0) };
}

// SAFETY: every operation delegates to the system allocator; counting is enabled only by the focused owner.
unsafe impl GlobalAlloc for ScreenPreparationAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.try_with(ThreadCell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static SCREEN_PREPARATION_ALLOCATOR: ScreenPreparationAllocator = ScreenPreparationAllocator;

#[test]
fn screen_message_work_does_not_clone_the_original_cells() {
    let screen = Screen {
        cols: 128,
        rows: 64,
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
        scrollback: Scrollback::default(),
        background: "#000000".to_string(),
        lines: (0..64)
            .map(|_| {
                (0..128)
                    .map(|_| Cell {
                        ch: Some("a".to_string()),
                        ..Cell::default()
                    })
                    .collect()
            })
            .collect(),
    };
    let layout = crate::platform::darwin::frame::render_layout(&screen).expect("render layout");
    let frame = json!({"generation": 1, "raster": 1, "sequence": 1});
    let expected_frame = frame.clone();
    ALLOCATIONS.with(|count| count.set(0));
    COUNT_ALLOCATIONS.with(|counting| counting.set(true));
    let work = screen_message_work(
        "s1".to_string(),
        "boot".to_string(),
        1,
        frame,
        Value::Null,
        screen,
        layout,
    );
    COUNT_ALLOCATIONS.with(|counting| counting.set(false));
    let allocations = ALLOCATIONS.with(ThreadCell::get);
    println!("{allocations} allocations transfer an 8,192-cell screen to message preparation");
    assert!(
        allocations <= 64,
        "{allocations} allocations copied an already owned 8,192-cell screen"
    );
    let line = work().expect("prepare screen message");
    let message: Value = serde_json::from_str(&line).expect("decode screen message");
    assert_eq!(message["surface"], "s1");
    assert_eq!(message["body"]["event"], "screen");
    assert_eq!(message["body"]["frame"], expected_frame);
    assert_eq!(message["body"]["rows"], 64);
    assert_eq!(message["body"]["cols"], 128);
    assert_eq!(message["body"]["cells"].as_array().unwrap().len(), 8192);
    assert_eq!(message["body"]["cells"][0]["grapheme"], "a");
}

#[tokio::test]
async fn screen_preparation_runs_outside_the_runtime_worker() {
    let worker = std::thread::current().id();
    let line = prepare_screen_line(move || {
        assert_ne!(
            std::thread::current().id(),
            worker,
            "screen preparation must leave the async worker available for image output"
        );
        Ok("complete screen".to_string())
    })
    .await
    .expect("prepare screen");
    assert_eq!(line, "complete screen");
}

#[tokio::test]
async fn screen_preparation_preserves_the_error() {
    let error = prepare_screen_line(|| Err("screen cell is invalid".to_string()))
        .await
        .expect_err("reject invalid screen");
    assert_eq!(error, "screen cell is invalid");
}

#[tokio::test]
async fn screen_preparation_reports_a_failed_task() {
    let error = prepare_screen_line(|| panic!("fixture screen preparation panic"))
        .await
        .expect_err("report failed preparation task");
    assert!(
        error.starts_with("screen preparation task failed:"),
        "{error}"
    );
    assert!(
        error.contains("fixture screen preparation panic"),
        "{error}"
    );
}
