// 터미널 locale 규칙(core docs/spec/terminal-runtime.md): service 환경에 LANG, LC_ALL, LC_CTYPE 이 모두 없으면
// PTY 자식은 UTF-8 locale 로 실행된다. 프로세스 환경을 바꾸는 test 는 이 binary 의 하나뿐이고, 나머지 test 는
// 프로세스 환경을 읽지 않는다.

use std::ffi::OsStr;

use soksak_sidecar_vt_core::locale::{shell_locale, ShellLocale};
use soksak_sidecar_vt_core::platform::darwin::locale::installed;
use soksak_sidecar_vt_core::pty::PtyService;
use soksak_sidecar_vt_core::DaemonEvent;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn a_shell_without_locale_variables_runs_in_a_utf8_locale() {
    for name in ["LANG", "LC_ALL", "LC_CTYPE"] {
        std::env::remove_var(name);
    }
    let service = PtyService::new();
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    service
        .open(
            "/bin/zsh",
            &[
                "-f".into(),
                "-c".into(),
                "word=한; print -r -- \"charmap=$(locale charmap) length=${#word}\"".into(),
            ],
            None,
            80,
            24,
            events,
        )
        .expect("the PTY session must open");
    let mut output = Vec::new();
    let finished = timeout(Duration::from_secs(5), async {
        while let Some(event) = received.recv().await {
            match event {
                DaemonEvent::Output { data, .. } => output.extend_from_slice(&data),
                DaemonEvent::Exit { .. } => break,
                DaemonEvent::Error { error, .. } => panic!("the PTY session failed: {error}"),
            }
        }
    })
    .await;
    let text = String::from_utf8_lossy(&output);
    assert!(finished.is_ok(), "the shell did not exit within 5 s; output so far: {text:?}");
    assert!(
        text.contains("charmap=UTF-8 length=1"),
        "the shell must run in a UTF-8 locale and count one character; output: {text:?}"
    );
}

fn environment<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a OsStr> {
    move |name| {
        values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsStr::new(*value))
    }
}

#[test]
fn a_non_empty_locale_variable_is_kept() {
    for name in ["LANG", "LC_ALL", "LC_CTYPE"] {
        let values = [(name, "C")];
        assert_eq!(
            shell_locale(environment(&values), Some("ko"), Some("KR"), |_| true),
            ShellLocale::Kept,
            "{name}"
        );
    }
}

#[test]
fn empty_locale_variables_set_the_installed_regional_locale() {
    let values = [("LANG", ""), ("LC_ALL", ""), ("LC_CTYPE", "")];
    assert_eq!(
        shell_locale(environment(&values), Some("ko"), Some("KR"), |name| name == "ko_KR.UTF-8"),
        ShellLocale::Lang("ko_KR.UTF-8".into())
    );
    assert_eq!(
        shell_locale(environment(&[]), Some("zh"), Some("CN"), |name| name == "zh_CN.UTF-8"),
        ShellLocale::Lang("zh_CN.UTF-8".into())
    );
}

#[test]
fn a_regional_locale_that_is_not_installed_sets_lc_ctype() {
    assert_eq!(
        shell_locale(environment(&[]), Some("en"), Some("KR"), |_| false),
        ShellLocale::CtypeUtf8 { name: "en_KR.UTF-8".into() }
    );
}

#[test]
fn a_macos_locale_without_a_language_or_region_sets_lc_ctype() {
    assert_eq!(
        shell_locale(environment(&[]), Some("en"), None, |_| true),
        ShellLocale::CtypeUtf8 { name: "en_.UTF-8".into() }
    );
    assert_eq!(
        shell_locale(environment(&[]), None, None, |_| true),
        ShellLocale::CtypeUtf8 { name: "_.UTF-8".into() }
    );
}

#[test]
fn the_c_library_knows_installed_locales_only() {
    assert!(installed("UTF-8"));
    assert!(installed("en_US.UTF-8"));
    assert!(!installed("xx_XX.UTF-8"));
    assert!(!installed("en_US.UTF-8\0"));
}
