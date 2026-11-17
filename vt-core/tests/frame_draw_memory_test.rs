// A draw releases the objects that it creates before it returns: the thread that draws in the sidecar has no autorelease
// pool of its own, so an object that a draw leaves to an outer pool stays allocated for the life of the thread.
use soksak_sidecar_vt_core::TerminalTheme;
use soksak_sidecar_vt_core::platform::darwin::frame::{
    CursorRender, Frame, UnfocusedCursor, metrics,
};
use soksak_sidecar_vt_core::protocol::{Cell, Cursor, CursorShape, Screen};

#[repr(C)]
#[derive(Default)]
struct MallocStatistics {
    blocks_in_use: u32,
    size_in_use: usize,
    max_size_in_use: usize,
    size_allocated: usize,
}

unsafe extern "C" {
    fn malloc_zone_statistics(zone: *mut std::ffi::c_void, stats: *mut MallocStatistics);
}

fn blocks_in_use() -> u32 {
    let mut stats = MallocStatistics::default();
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
    stats.blocks_in_use
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
