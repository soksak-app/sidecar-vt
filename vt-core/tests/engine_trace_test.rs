//! 진단 build 의 engine 입력 기록(engine_trace.rs). `--features diagnostics` 에서만 build 된다.
#![cfg(feature = "diagnostics")]

#[path = "support/events_test.rs"]
mod events_test;

use events_test::TestEvents;
use soksak_sidecar_vt_core::engine_trace::record;

// 기록 폴더는 process 의 환경 변수이므로, 폴더가 있을 때, 없을 때, 쓸 수 없을 때를 한 검사에서 차례로 확인한다. 두 검사로 나누면
// 병렬로 실행되는 다른 검사가 변수를 지워 기록이 빠진다.
#[test]
fn engine_inputs_are_appended_to_the_trace_of_their_surface_only_with_a_trace_folder() {
    std::env::remove_var("SOKSAK_VT_TRACE_DIR");
    let events = TestEvents::new();
    let log = events.handle();
    record(&log, "tab-c", || {
        panic!("the line must not be built without a trace folder")
    });
    let dir = std::env::temp_dir().join(format!("soksak-vt-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the trace folder");
    std::env::set_var("SOKSAK_VT_TRACE_DIR", &dir);
    record(&log, "tab-a", || "resize 80 24".to_string());
    record(&log, "tab-b", || "reset".to_string());
    record(&log, "tab-a", || "feed YQ==".to_string());
    std::env::remove_var("SOKSAK_VT_TRACE_DIR");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read a trace");
    assert_eq!(read("tab-a.trace"), "resize 80 24\nfeed YQ==\n");
    assert_eq!(read("tab-b.trace"), "reset\n");
    std::fs::remove_dir_all(&dir).expect("remove the trace folder");
    // A trace folder that cannot be written is reported as an error record that names the file, not dropped.
    std::env::set_var("SOKSAK_VT_TRACE_DIR", &dir);
    record(&log, "tab-x", || "reset".to_string());
    std::env::remove_var("SOKSAK_VT_TRACE_DIR");
    let records = events.named("sidecar.vt.record.error");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["fields"]["where"], "engine trace");
    let text = records[0]["fields"]["text"].as_str().unwrap();
    assert!(
        text.contains(&dir.join("tab-x.trace").display().to_string()),
        "{text}"
    );
}
