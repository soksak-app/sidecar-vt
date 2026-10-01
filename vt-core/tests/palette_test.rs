// 페이지가 보낸 테마 색을 읽고 검사하는지 확인한다.
use soksak_sidecar_vt_core::{parse_hex, TerminalTheme, ThemeMode, DEFAULT_PALETTE, LIGHT_PALETTE};

#[test]
fn a_theme_request_sets_the_four_colors_and_keeps_the_mode_palette() {
    let theme = TerminalTheme::from_request(
        Some("light"),
        Some("#e8f5ee"),
        Some("#12684a"),
        Some("#12684b"),
        Some("#d8dbe4"),
    )
    .expect("a complete light theme");
    assert_eq!(theme.background, [0xe8, 0xf5, 0xee]);
    assert_eq!(theme.foreground, [0x12, 0x68, 0x4a]);
    assert_eq!(theme.cursor, [0x12, 0x68, 0x4b]);
    assert_eq!(theme.selection, [0xd8, 0xdb, 0xe4]);
    assert_eq!(theme.mode, ThemeMode::Light);
    // 인덱스 ANSI 색은 테마가 아니라 모드가 고른다.
    assert_eq!(theme.palette, LIGHT_PALETTE);
    let dark = TerminalTheme::from_request(
        Some("dark"),
        Some("#000000"),
        Some("#ffffff"),
        Some("#ffffff"),
        Some("#333333"),
    )
    .expect("a complete dark theme");
    assert_eq!(dark.palette, DEFAULT_PALETTE);
    assert_eq!(
        dark.color(257),
        Some([0, 0, 0]),
        "the default background slot is the theme background"
    );
}

#[test]
fn a_theme_request_without_a_valid_mode_or_color_is_rejected() {
    let ok = Some("#102030");
    assert_eq!(
        TerminalTheme::from_request(Some("sepia"), ok, ok, ok, ok),
        Err("theme.mode must be dark or light".to_string())
    );
    for (index, name) in ["background", "foreground", "cursor", "selection"]
        .iter()
        .enumerate()
    {
        for bad in [
            None,
            Some("red"),
            Some("#12345"),
            Some("#1234567"),
            Some("#12345g"),
        ] {
            let mut colors = [ok; 4];
            colors[index] = bad;
            assert_eq!(
                TerminalTheme::from_request(
                    Some("dark"),
                    colors[0],
                    colors[1],
                    colors[2],
                    colors[3]
                ),
                Err(format!("theme.{name} must be a #rrggbb color")),
                "{name} {bad:?}"
            );
        }
    }
    assert_eq!(parse_hex("#A0b1C2"), Some([0xa0, 0xb1, 0xc2]));
}
