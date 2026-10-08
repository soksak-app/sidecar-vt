//! 현재 macOS locale 의 언어 코드와 지역 코드를 읽고, C library 가 locale 이름을 아는지 확인한다.

use std::ffi::{c_char, c_void, CStr, CString};

type CFTypeRef = *const c_void;
type CFIndex = isize;

// CFString.h 의 kCFStringEncodingUTF8.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFLocaleLanguageCode: CFTypeRef;
    static kCFLocaleCountryCode: CFTypeRef;
    fn CFLocaleCopyCurrent() -> CFTypeRef;
    fn CFLocaleGetValue(locale: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFGetTypeID(value: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetCString(
        string: CFTypeRef,
        buffer: *mut c_char,
        size: CFIndex,
        encoding: u32,
    ) -> u8;
    fn CFRelease(value: CFTypeRef);
}

/// locale 값이 CFString 이면 그 UTF-8 문자열. 언어 코드와 지역 코드는 짧은 ASCII 이다.
fn text(value: CFTypeRef) -> Option<String> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    let mut buffer = [0 as c_char; 64];
    let copied = unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as CFIndex,
            CF_STRING_ENCODING_UTF8,
        )
    };
    if copied == 0 {
        return None;
    }
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(value.to_string_lossy().into_owned())
}

/// 현재 macOS locale 의 언어 코드(`ko`)와 지역 코드(`KR`). locale 에 없는 코드는 `None` 이다.
pub fn codes() -> (Option<String>, Option<String>) {
    unsafe {
        let locale = CFLocaleCopyCurrent();
        if locale.is_null() {
            return (None, None);
        }
        let language = text(CFLocaleGetValue(locale, kCFLocaleLanguageCode));
        let region = text(CFLocaleGetValue(locale, kCFLocaleCountryCode));
        CFRelease(locale);
        (language, region)
    }
}

/// C library 가 `name` 의 LC_CTYPE 을 만들 수 있으면 참이다. `newlocale` 은 process locale 을 바꾸지 않는다.
pub fn installed(name: &str) -> bool {
    let Ok(name) = CString::new(name) else {
        return false;
    };
    let locale =
        unsafe { libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut()) };
    if locale.is_null() {
        return false;
    }
    unsafe { libc::freelocale(locale) };
    true
}
