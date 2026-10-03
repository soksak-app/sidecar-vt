//! 진단 build 의 engine 입력 기록(engine_trace.rs). `--features diagnostics` 에서만 build 된다.
#![cfg(feature = "diagnostics")]

use soksak_sidecar_vt_core::engine_trace::record;

// 기록 폴더는 process 의 환경 변수이므로, 폴더가 있을 때와 없을 때를 한 검사에서 차례로 확인한다. 두 검사로 나누면
// 병렬로 실행되는 다른 검사가 변수를 지워 기록이 빠진다.
#[test]
fn engine_inputs_are_appended_to_the_trace_of_their_surface_only_with_a_trace_folder() {
    std::env::remove_var("SOKSAK_VT_TRACE_DIR");
    record("tab-c", || {
        panic!("the line must not be built without a trace folder")
    });
    let dir = std::env::temp_dir().join(format!("soksak-vt-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the trace folder");
    std::env::set_var("SOKSAK_VT_TRACE_DIR", &dir);
    record("tab-a", || "resize 80 24".to_string());
    record("tab-b", || "reset".to_string());
    record("tab-a", || "feed YQ==".to_string());
    std::env::remove_var("SOKSAK_VT_TRACE_DIR");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read a trace");
    assert_eq!(read("tab-a.trace"), "resize 80 24\nfeed YQ==\n");
    assert_eq!(read("tab-b.trace"), "reset\n");
    std::fs::remove_dir_all(&dir).expect("remove the trace folder");
}
