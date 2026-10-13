// The hex of a chunk is built with a few allocations: it is made with a lookup table in one
// string, and not with one formatted string for each byte. The counting allocator makes the cost a measured number.
use soksak_sidecar_vt_core::events::hex;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded to the system allocator; the counter is an atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            System.alloc(layout)
        }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn the_hex_of_a_chunk_is_built_without_an_allocation_for_each_byte() {
    let data: Vec<u8> = (0..8192u32).map(|index| (index % 251) as u8).collect();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let text = hex(&data);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(text.len(), 16384);
    assert_eq!(&text[..8], "00010203");
    assert!(
        allocations <= 4,
        "{allocations} allocations built the hex of 8192 bytes"
    );
}
