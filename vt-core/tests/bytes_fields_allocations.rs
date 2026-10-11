// The fields of an event of bytes are built with a few allocations: the hex of a chunk is made with a lookup table in one
// string, and not with one formatted string for each byte. The counting allocator makes the cost a measured number.
use soksak_sidecar_vt_core::events::bytes_fields;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is forwarded to the system allocator; the counter is an atomic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout)
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn the_hex_of_a_chunk_is_built_without_an_allocation_for_each_byte() {
    let data: Vec<u8> = (0..8192u32).map(|index| (index % 251) as u8).collect();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let fields = bytes_fields(&data);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(fields["hex"].as_str().unwrap().len(), 16384);
    assert_eq!(&fields["hex"].as_str().unwrap()[..8], "00010203");
    assert!(
        allocations <= 32,
        "{allocations} allocations built the fields of 8192 bytes"
    );
}
