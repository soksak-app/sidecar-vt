// The `screen` member of a range read (core docs/spec/terminal-read.md): it is required and names the primary or the
// alternate screen; a request without it or with another value is refused with the value it found.
use serde_json::json;
use soksak_sidecar_vt_core::RowScreen;

#[test]
fn the_screen_member_names_the_primary_or_the_alternate_screen() {
    assert_eq!(
        RowScreen::parse(Some(&json!("primary"))),
        Ok(RowScreen::Primary)
    );
    assert_eq!(
        RowScreen::parse(Some(&json!("alternate"))),
        Ok(RowScreen::Alternate)
    );
    assert_eq!(RowScreen::Primary.name(), "primary");
    assert_eq!(RowScreen::Alternate.name(), "alternate");
}

#[test]
fn a_range_read_without_a_screen_or_with_another_screen_is_refused() {
    assert_eq!(
        RowScreen::parse(None),
        Err("screen.read requires screen primary or alternate, got none".to_string())
    );
    assert_eq!(
        RowScreen::parse(Some(&json!("history"))),
        Err("screen.read requires screen primary or alternate, got \"history\"".to_string())
    );
}
