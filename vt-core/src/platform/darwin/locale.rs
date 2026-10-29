//! Reads the language and region codes of the current macOS locale, and checks whether the C
//! library recognizes a locale name.

use std::ffi::{CStr, CString, c_char, c_void};

type CFTypeRef = *const c_void;
type CFIndex = isize;

// kCFStringEncodingUTF8 from CFString.h.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
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

/// The UTF-8 string of the locale value when the value is a CFString. Language and region codes are
/// short ASCII strings.
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

/// Language code (`ko`) and region code (`KR`) of the current macOS locale. A code that the locale
/// does not have is `None`.
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

/// True when the C library can create the LC_CTYPE for `name`. `newlocale` does not change the
/// process locale.
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
