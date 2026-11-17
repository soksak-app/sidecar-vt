// A draw releases the objects that it creates before it returns: the thread that draws in the sidecar has no autorelease
// pool of its own, so an object that a draw leaves to an outer pool stays allocated for the life of the thread.
use soksak_sidecar_vt_core::TerminalTheme;
use soksak_sidecar_vt_core::platform::darwin::frame::{
    CursorRender, Frame, UnfocusedCursor, metrics,
};
use soksak_sidecar_vt_core::protocol::{Cell, Cursor, CursorShape, Screen};

// Counts the allocated blocks of every malloc zone of this process with the enumerator of the zone, which `heap` uses.
// `malloc_zone_statistics` is not used: on macOS 15 its count of blocks in use grows by blocks that no live object holds.
type Task = u32;
type Reader = Option<unsafe extern "C" fn(Task, usize, usize, *mut *mut std::ffi::c_void) -> i32>;
type Recorder = unsafe extern "C" fn(Task, *mut std::ffi::c_void, u32, *const Range, u32);

#[repr(C)]
struct Range {
    address: usize,
    size: usize,
}

#[repr(C)]
struct Introspection {
    enumerator:
        unsafe extern "C" fn(Task, *mut std::ffi::c_void, u32, usize, Reader, Recorder) -> i32,
}

// The fields of malloc_zone_t up to its introspection pointer.
#[repr(C)]
struct Zone {
    functions: [usize; 12],
    introspect: *const Introspection,
}

const MALLOC_PTR_IN_USE_RANGE_TYPE: u32 = 1;

unsafe extern "C" {
    static mach_task_self_: Task;
    fn malloc_get_all_zones(
        task: Task,
        reader: Reader,
        addresses: *mut *mut usize,
        count: *mut u32,
    ) -> i32;
}

unsafe extern "C" fn record(
    _: Task,
    context: *mut std::ffi::c_void,
    _: u32,
    _: *const Range,
    count: u32,
) {
    unsafe { *(context as *mut u64) += count as u64 };
}

fn blocks_in_use() -> u64 {
    let mut addresses: *mut usize = std::ptr::null_mut();
    let mut count = 0u32;
    let task = unsafe { mach_task_self_ };
    let result = unsafe { malloc_get_all_zones(task, None, &mut addresses, &mut count) };
    assert_eq!(result, 0, "malloc_get_all_zones failed with {result}");
    let mut blocks = 0u64;
    for index in 0..count as usize {
        let zone = unsafe { *addresses.add(index) } as *const Zone;
        let introspect = unsafe { (*zone).introspect };
        let result = unsafe {
            ((*introspect).enumerator)(
                task,
                &mut blocks as *mut u64 as *mut std::ffi::c_void,
                MALLOC_PTR_IN_USE_RANGE_TYPE,
                zone as usize,
                None,
                record,
            )
        };
        assert_eq!(
            result, 0,
            "the enumerator of zone {index} failed with {result}"
        );
    }
    blocks
}

// A screen whose text holds its number, so every row differs from the row of the screen before it. The draws cycle through
// SCREENS screens, so a cache of the text that the system keeps is full after the first cycles and the draws that the test
// counts add no text to it.
const SCREENS: usize = 10;
const WINDOWS: usize = 3;

fn screen(draw: usize) -> Screen {
    let cols = 20;
    let rows = 40;
    let text = format!("{draw:020}");
    let lines = (0..rows)
        .map(|_| {
            text.chars()
                .map(|ch| Cell {
                    ch: Some(ch.to_string().into()),
                    ..Cell::default()
                })
                .collect()
        })
        .collect();
    Screen {
        cols,
        rows,
        cursor: Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
            blink_visible: true,
            focused: true,
            preedit: None,
        },
        scrollback: Default::default(),
        background: "#1e1e1e".to_string(),
        lines,
    }
}

#[test]
fn repeated_strip_draws_keep_no_allocation() {
    // A new thread has no autorelease pool, as the drawing thread of the sidecar.
    let (kept, draws) = std::thread::spawn(|| {
        let metrics = metrics(13.0, 2.0);
        let width = metrics.cell_width as u32 * 20;
        let height = metrics.cell_height as u32 * 40;
        let theme = TerminalTheme::dark();
        let cursor = CursorRender {
            visible: true,
            focused: true,
            blink_visible: true,
            shape: CursorShape::Block,
            unfocused: UnfocusedCursor::Hollow,
        };
        let frame = Frame::new_strip(width, height).expect("strip frame");
        let screens: Vec<Screen> = (0..SCREENS).map(screen).collect();
        for screen in screens.iter().cycle().take(2 * SCREENS) {
            frame
                .draw_with_theme(screen, &metrics, cursor, &theme)
                .expect("draw");
        }
        // The blocks that each window of draws keeps: a cache of the system fills in the first windows and keeps no more
        // in the last one, while an object that each draw keeps adds a block in every window.
        let draws = 400;
        let kept: Vec<i64> = (0..WINDOWS)
            .map(|_| {
                let before = blocks_in_use();
                for screen in screens.iter().cycle().take(draws) {
                    frame
                        .draw_with_theme(screen, &metrics, cursor, &theme)
                        .expect("draw");
                }
                blocks_in_use() as i64 - before as i64
            })
            .collect();
        (kept, draws)
    })
    .join()
    .expect("drawing thread");
    println!("allocated blocks kept by each window of {draws} strip draws of 40 rows: {kept:?}");
    let last = kept[WINDOWS - 1];
    if last >= draws as i64 {
        report_leaks();
    }
    // A draw that leaves one object to an outer pool keeps one block per draw.
    assert!(
        last < draws as i64,
        "the last window of {draws} strip draws of 40 rows kept {last} allocated blocks; each window kept {kept:?}"
    );
}

// Prints the unreferenced blocks of this process that `leaks` finds, counted by type, and the classes of the most live
// objects, so a failing run names the objects that the draws keep.
fn report_leaks() {
    let output = std::process::Command::new("/usr/bin/leaks")
        .arg(std::process::id().to_string())
        .output()
        .expect("run leaks");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut types = std::collections::BTreeMap::<String, usize>::new();
    for line in text
        .lines()
        .filter(|line| line.trim_start().starts_with("Leak:"))
    {
        let name = line.rsplit("  ").next().unwrap_or(line).trim().to_string();
        *types.entry(name).or_default() += 1;
    }
    println!(
        "leaks found {} leaked block lines by type: {types:?}",
        types.values().sum::<usize>()
    );
    for line in text
        .lines()
        .filter(|line| line.contains("leaks for") || line.trim_start().starts_with("Leak:"))
        .take(40)
    {
        println!("leaks: {line}");
    }
    eprintln!(
        "leaks standard error: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `leaks` counts only unreferenced blocks; the classes with the most live objects name a kept object that a pool or
    // a cache still references.
    let output = std::process::Command::new("/usr/bin/heap")
        .arg(std::process::id().to_string())
        .output()
        .expect("run heap");
    let text = String::from_utf8_lossy(&output.stdout);
    let classes = text
        .lines()
        .skip_while(|line| !line.contains("CLASS_NAME"))
        .skip(2)
        .take(15);
    for line in classes {
        println!("heap: {line}");
    }
}
