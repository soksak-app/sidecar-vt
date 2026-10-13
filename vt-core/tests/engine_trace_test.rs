//! 진단 build 의 engine 입력 기록(engine_trace.rs). `--features diagnostics` 에서만 build 된다.
#![cfg(feature = "diagnostics")]

#[path = "support/events_test.rs"]
mod events_test;

use events_test::TestEvents;
use soksak_sidecar_vt_core::engine_trace::record;

// 기록 폴더는 writer 의 설정이다. 폴더가 없는 writer, 폴더가 있는 writer, 쓸 수 없는 폴더의 writer 를 차례로 확인한다.
#[test]
fn engine_inputs_are_appended_to_the_trace_of_their_surface_only_with_a_trace_folder() {
    let without_folder = TestEvents::new();
    record(&without_folder.handle(), "tab-c", || {
        panic!("the line must not be built without a trace folder")
    });
    let dir = std::env::temp_dir().join(format!("soksak-vt-trace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the trace folder");
    let with_folder = TestEvents::with_trace_dir(&dir);
    let log = with_folder.handle();
    record(&log, "tab-a", || "resize 80 24".to_string());
    record(&log, "tab-b", || "reset".to_string());
    record(&log, "tab-a", || "feed YQ==".to_string());
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read a trace");
    assert_eq!(read("tab-a.trace"), "resize 80 24\nfeed YQ==\n");
    assert_eq!(read("tab-b.trace"), "reset\n");
    std::fs::remove_dir_all(&dir).expect("remove the trace folder");
    // A trace folder that cannot be written is reported as an error record that names the file, not dropped.
    let unwritable = TestEvents::with_trace_dir(&dir);
    record(&unwritable.handle(), "tab-x", || "reset".to_string());
    let records = unwritable.named("sidecar.vt.record.error");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["fields"]["where"], "engine trace");
    let text = records[0]["fields"]["text"].as_str().unwrap();
    assert!(
        text.contains(&dir.join("tab-x.trace").display().to_string()),
        "{text}"
    );
}
