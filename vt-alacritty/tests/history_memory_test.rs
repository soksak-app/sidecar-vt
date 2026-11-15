#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use alacritty_terminal::term::Config;
use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::Engine;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts the live bytes of this test binary, which runs this one test, so the number states what the
/// engine holds at a time.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to the system allocator with the same arguments.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

const COLUMNS: u16 = 120;
const ROWS: u16 = 40;
/// The output past the history limit that each measured interval feeds.
const EXTRA_LINES: usize = 2000;
/// The number of lines that one feed call carries.
const LINES_PER_FEED: usize = 1000;

/// Feeds `lines` lines of `line` to the engine in feed calls of `LINES_PER_FEED` lines.
fn feed_lines(engine: &mut AlacrittyEngine, line: &[u8], lines: usize) {
    let mut fed = 0;
    while fed < lines {
        let count = LINES_PER_FEED.min(lines - fed);
        engine.feed(&line.repeat(count));
        fed += count;
    }
}

/// S171: memory that the history holds stays bounded by the history limit. The history of one terminal
/// stops growing at its limit, so the live bytes after the limit is reached do not grow when the same
/// amount of further output follows.
#[test]
fn history_at_its_limit_allocates_no_more() {
    let limit = Config::default().scrolling_history;
    let line = format!("{}\r\n", "x".repeat(usize::from(COLUMNS))).into_bytes();
    let lines = limit + EXTRA_LINES;

    let mut engine = AlacrittyEngine::new();
    engine.resize(COLUMNS, ROWS);
    feed_lines(&mut engine, &line, lines);
    let (_, history) = engine.scrollback();
    assert!(
        history >= limit,
        "{lines} lines of {COLUMNS} columns left {history} history lines; the limit is {limit}"
    );
    let a = LIVE.load(Ordering::Relaxed);

    feed_lines(&mut engine, &line, lines);
    let b = LIVE.load(Ordering::Relaxed);
    let (_, history_after) = engine.scrollback();
    assert!(
        history_after >= limit,
        "{lines} more lines left {history_after} history lines; the limit is {limit}"
    );

    println!(
        "live bytes {a} at {history} history lines ({:.1} bytes per history line); {b} after {lines} more lines",
        a as f64 / history as f64
    );
    let bound = a + a / 20;
    assert!(
        b <= bound,
        "live bytes grew from {a} to {b} after {lines} more lines at the history limit ({limit}); the bound is {bound}"
    );
}
