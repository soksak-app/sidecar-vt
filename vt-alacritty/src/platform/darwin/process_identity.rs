use std::ffi::{CStr, CString, c_char, c_int};

unsafe extern "C" {
    fn soksak_process_set_display_name(
        display_name: *const c_char,
        error: *mut c_char,
        error_capacity: usize,
    ) -> c_int;
}

pub fn set_process_display_name(display_name: &str) -> Result<(), String> {
    let display_name = CString::new(display_name)
        .map_err(|error| format!("display name contains a NUL byte: {error}"))?;
    let mut diagnostic = [0_i8; 1024];
    // SAFETY: the string and diagnostic pointers remain valid for the call, and capacity matches the array.
    let status = unsafe {
        soksak_process_set_display_name(
            display_name.as_ptr(),
            diagnostic.as_mut_ptr(),
            diagnostic.len(),
        )
    };
    if status == 0 {
        return Ok(());
    }
    // SAFETY: the provider guarantees a NUL-terminated diagnostic when capacity is nonzero.
    let detail = unsafe { CStr::from_ptr(diagnostic.as_ptr()) }.to_string_lossy();
    Err(format!(
        "LaunchServices registration returned {status}: {detail}"
    ))
}
