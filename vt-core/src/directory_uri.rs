//! Converts an OSC 7 directory URI to a path on this computer. A directory on another computer has
//! no path.

/// Host name of this computer (gethostname).
pub fn host_name() -> Result<String, String> {
    let mut buffer = [0u8; 256];
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return Err(format!("gethostname: {}", std::io::Error::last_os_error()));
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        // default: a name that fills the buffer has no NUL, so the whole buffer is the name.
        .unwrap_or(buffer.len());
    String::from_utf8(buffer[..end].to_vec()).map_err(|_| "the host name is not UTF-8".to_string())
}

fn decode(path: &str) -> Result<String, String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = path
                .get(index + 1..index + 3)
                // silent: a digit pair that does not parse is reported by the ok_or_else that
                // follows.
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                .ok_or_else(|| format!("OSC 7 path has an invalid percent escape: {path}"))?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| format!("OSC 7 path is not UTF-8: {path}"))
}

/// Path of a `file` or `kitty-shell-cwd` URI. When the host is empty, `localhost`, or the host name
/// of this computer, returns the percent-decoded absolute path. For another host, returns None. Any
/// other format is an error.
pub fn local_path(uri: &str) -> Result<Option<String>, String> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("kitty-shell-cwd://"))
        .ok_or_else(|| format!("OSC 7 directory URI is not a file URI: {uri:?}"))?;
    let slash = rest
        .find('/')
        .ok_or_else(|| format!("OSC 7 directory URI has no absolute path: {uri:?}"))?;
    let (host, path) = rest.split_at(slash);
    let path = decode(path)?;
    if host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case(&host_name()?)
    {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}
