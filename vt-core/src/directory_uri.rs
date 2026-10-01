//! OSC 7 디렉터리 URI 를 이 컴퓨터의 경로로 바꾼다. 다른 컴퓨터의 디렉터리는 경로가 없다.

/// 이 컴퓨터의 호스트 이름(gethostname).
pub fn host_name() -> Result<String, String> {
    let mut buffer = [0u8; 256];
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return Err(format!("gethostname: {}", std::io::Error::last_os_error()));
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        // 기본값: 이름이 버퍼를 채워 NUL 이 없으면 버퍼 전체가 이름이다.
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

/// `file` 또는 `kitty-shell-cwd` URI 의 경로. 호스트가 비었거나 `localhost` 이거나 이 컴퓨터의 호스트
/// 이름이면 퍼센트 디코딩한 절대 경로를, 다른 호스트이면 None 을 돌려준다. 다른 형식은 오류다.
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
