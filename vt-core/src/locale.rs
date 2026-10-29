//! Locale rules of the terminal shell (core docs/spec/terminal-runtime.md). The character encoding
//! of the terminal is UTF-8.

use std::ffi::OsStr;

/// If any of these variables has a non-empty value, the shell environment locale is not changed.
pub const VARIABLES: [&str; 3] = ["LANG", "LC_ALL", "LC_CTYPE"];

/// Locale to set in the shell environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellLocale {
    /// The environment already sets the locale.
    Kept,
    /// `<language>_<region>.UTF-8` to set in `LANG`.
    Lang(String),
    /// Sets `LC_CTYPE=UTF-8` because the regional locale is unavailable. `name` is the unavailable
    /// locale name.
    CtypeUtf8 { name: String },
}

/// Chooses the shell locale from the child environment variables (`variable`), the macOS locale
/// language and region codes, and the check for installed locales.
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
    // default: an unknown language is empty, and the locale name built from it is then not
    // installed.
    let language = language.unwrap_or("");
    // default: an unknown region is empty, and the locale name built from it is then not installed.
    let region = region.unwrap_or("");
    let name = format!("{language}_{region}.UTF-8");
    if !language.is_empty() && !region.is_empty() && installed(&name) {
        ShellLocale::Lang(name)
    } else {
        ShellLocale::CtypeUtf8 { name }
    }
}
