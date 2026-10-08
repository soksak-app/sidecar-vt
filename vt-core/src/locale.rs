//! 터미널 shell 의 locale 규칙(core docs/spec/terminal-runtime.md). 터미널의 문자 인코딩은 UTF-8 이다.

use std::ffi::OsStr;

/// 이 셋 가운데 비어 있지 않은 값이 하나라도 있으면 shell 환경의 locale 을 바꾸지 않는다.
pub const VARIABLES: [&str; 3] = ["LANG", "LC_ALL", "LC_CTYPE"];

/// shell 환경에 정할 locale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellLocale {
    /// 환경이 이미 locale 을 정한다.
    Kept,
    /// `LANG` 에 정할 `<language>_<region>.UTF-8`.
    Lang(String),
    /// 지역 locale 을 쓸 수 없어 `LC_CTYPE=UTF-8` 을 정한다. `name` 은 쓸 수 없던 locale 이름이다.
    CtypeUtf8 { name: String },
}

/// 자식 환경의 변수(`variable`), macOS locale 의 언어 코드와 지역 코드, 설치된 locale 판별로 shell locale 을 정한다.
pub fn shell_locale<'a>(
    variable: impl Fn(&str) -> Option<&'a OsStr>,
    language: Option<&str>,
    region: Option<&str>,
    installed: impl Fn(&str) -> bool,
) -> ShellLocale {
    if VARIABLES
        .iter()
        .any(|name| variable(name).is_some_and(|value| !value.is_empty()))
    {
        return ShellLocale::Kept;
    }
    let language = language.unwrap_or("");
    let region = region.unwrap_or("");
    let name = format!("{language}_{region}.UTF-8");
    if !language.is_empty() && !region.is_empty() && installed(&name) {
        ShellLocale::Lang(name)
    } else {
        ShellLocale::CtypeUtf8 { name }
    }
}
