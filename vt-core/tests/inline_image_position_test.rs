//! The position of an inline image while output moves it.

use soksak_sidecar_vt_core::protocol::inline_image_top;

const CELL: u32 = 20;
const FRAME: u32 = 200;

#[test]
fn an_inline_image_at_its_anchor_stands_at_its_row() {
    assert_eq!(inline_image_top(3, 0, 0, CELL, 40, FRAME), Some(60));
}

#[test]
fn an_inline_image_scrolled_partly_past_the_top_edge_stays_visible() {
    assert_eq!(inline_image_top(1, 2, 0, CELL, 60, FRAME), Some(-20));
}

#[test]
fn an_inline_image_whose_last_row_left_the_top_edge_is_not_visible() {
    assert_eq!(inline_image_top(1, 4, 0, CELL, 60, FRAME), None);
}

#[test]
fn an_inline_image_below_the_frame_is_not_visible() {
    assert_eq!(inline_image_top(4, 0, 6, CELL, 60, FRAME), None);
}

#[test]
fn the_viewport_offset_moves_an_inline_image_down() {
    assert_eq!(inline_image_top(1, 2, 3, CELL, 60, FRAME), Some(40));
}
