//! 진단 build 의 engine 입력 기록. `SOKSAK_VT_TRACE_DIR` 이 있으면 표면마다 `<dir>/<surface>.trace` 에 engine 에 준
//! 입력을 순서대로 한 줄씩 쓴다: `resize <cols> <rows>`, `reset`, `feed <base64>`. 기록은 engine replay test 의
//! fixture 가 된다. 일반 build 에는 기록 코드가 없다.

/// 표면 하나의 engine 입력 한 줄을 기록한다. 쓰지 못하면 그 실패를 표준 오류에 쓴다.
#[cfg(feature = "diagnostics")]
pub fn record(surface: &str, line: impl FnOnce() -> String) {
    use std::io::Write;
    let Some(dir) = std::env::var_os("SOKSAK_VT_TRACE_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{surface}.trace"));
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{}", line()));
    if let Err(error) = written {
        eprintln!("engine trace {}: {error}", path.display());
    }
}

/// 일반 build 에서는 아무것도 기록하지 않는다.
#[cfg(not(feature = "diagnostics"))]
pub fn record(_surface: &str, _line: impl FnOnce() -> String) {}
