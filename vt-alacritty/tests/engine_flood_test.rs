#[allow(dead_code)]
#[path = "../src/engine.rs"]
mod engine;

use engine::AlacrittyEngine;
use soksak_sidecar_vt_core::Engine;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts the live bytes, their peak and the allocations of this test binary, which runs this one
/// test, so the numbers state what the engine allocates.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to the system allocator with the same arguments.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const CHUNK_BYTES: usize = 64 * 1024;
const CHUNKS: usize = 16;

/// The bound on what a flood adds above a full history: the rows that the library caches, 1000 rows
/// of 80 cells of 24 bytes, with the same again for the rows and the allocator granularity.
const FLOOD_PEAK_BOUND: usize = 4 * 1024 * 1024;
/// A chunk that recycles the rows of the history allocates no row per line.
const FLOOD_ALLOCATIONS_PER_CHUNK: usize = 64;
/// The held sequence and the buffer of the parser each grow by doubling up to the bound of a
/// sequence, about 1.4 MiB.
const HELD_SEQUENCE_PEAK_BOUND: usize = 6 * 1024 * 1024;

#[test]
fn a_flood_at_the_history_limit_allocates_no_rows_per_chunk() {
    let mut engine = AlacrittyEngine::new();
    let chunk = b"y\n".repeat(CHUNK_BYTES / 2);
    // The history fills first, so the flood below only recycles rows.
    for _ in 0..2 {
        engine.feed(&b"y\n".repeat(30_000));
    }
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let started = std::time::Instant::now();
    for _ in 0..CHUNKS {
        engine.feed(&chunk);
    }
    let elapsed = started.elapsed();
    let peak = PEAK.load(Ordering::Relaxed) - live;
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
    println!(
        "flood of {CHUNKS} chunks of {CHUNK_BYTES} bytes: peak {peak} bytes over a full history, \
         {allocated} allocations, {elapsed:?}"
    );
    assert!(
        peak <= FLOOD_PEAK_BOUND,
        "peak {peak} bytes over a full history"
    );
    assert!(
        allocated <= CHUNKS * FLOOD_ALLOCATIONS_PER_CHUNK,
        "{allocated} allocations for {CHUNKS} chunks"
    );
}

#[test]
fn the_end_of_a_held_update_keeps_at_most_the_cached_rows() {
    let mut engine = AlacrittyEngine::new();
    for _ in 0..2 {
        engine.feed(&b"y\n".repeat(30_000));
    }
    let mut held = b"\x1b[?2026h".to_vec();
    held.extend(b"y\n".repeat(1024 * 1024 - 8));
    engine.feed(&held);
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    engine.stop_sync();
    let peak = PEAK.load(Ordering::Relaxed) - live;
    println!("the end of a held update: peak {peak} bytes over a full history");
    assert!(
        peak <= FLOOD_PEAK_BOUND,
        "peak {peak} bytes over a full history"
    );
}

#[test]
fn rows_past_the_count_of_a_held_update_are_reported() {
    let mut engine = AlacrittyEngine::new();
    for _ in 0..2 {
        engine.feed(&b"y\n".repeat(30_000));
    }
    engine.drain_events();
    let mut held = b"\x1b[?2026h".to_vec();
    held.extend(b"y\n".repeat(5_000));
    engine.feed(&held);
    engine.stop_sync();
    let events = engine.drain_events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, soksak_sidecar_vt_core::EngineEvent::Error(reason) if reason.contains("row count may be low"))),
        "{events:?}"
    );
}

#[test]
fn a_flood_of_line_feeds_reports_no_lost_rows() {
    let mut engine = AlacrittyEngine::new();
    for _ in 0..2 {
        engine.feed(&b"\n".repeat(30_000));
    }
    assert!(engine.drain_events().is_empty());
}

/// An OSC that never ends holds at most the bound of a sequence.
#[test]
fn an_osc_that_never_ends_holds_at_most_the_bound_of_a_sequence() {
    let mut engine = AlacrittyEngine::new();
    let chunk = vec![b'x'; CHUNK_BYTES];
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    engine.feed(b"\x1b]0;");
    for _ in 0..256 {
        engine.feed(&chunk);
    }
    let peak = PEAK.load(Ordering::Relaxed) - live;
    println!("an OSC of 16 MiB: peak {peak} bytes");
    assert!(peak <= HELD_SEQUENCE_PEAK_BOUND, "peak {peak} bytes");
}
