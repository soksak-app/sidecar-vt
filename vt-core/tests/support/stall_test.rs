use super::*;
use std::sync::{Arc, Mutex};

#[test]
fn a_blocked_runtime_is_recorded_with_the_stacks_of_its_threads_and_its_end() {
    let events = crate::events::test_support::TestEvents::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let dumps = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&dumps);
    watch(
        &events.handle(),
        runtime.handle(),
        move |path: &std::path::Path| {
            recorded.lock().unwrap().push(path.to_path_buf());
            Ok(())
        },
    )
    .unwrap();
    runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        // The only thread of the runtime is held by a blocking call, so no task of the runtime runs.
        std::thread::sleep(std::time::Duration::from_millis(1600));
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    });
    let lines = events.lines();
    let named = |name: &str| {
        lines
            .iter()
            .filter(|line| line["event"] == name)
            .collect::<Vec<_>>()
    };
    let stalls = named("sidecar.vt.runtime_stall");
    assert_eq!(
        stalls.len(),
        1,
        "one stall of the blocked runtime: {lines:?}"
    );
    assert!(stalls[0]["fields"]["late_ms"].as_u64().unwrap() >= 1000);
    let dump = named("sidecar.vt.stall_dump");
    assert_eq!(dump.len(), 1);
    let path = dump[0]["fields"]["path"].as_str().unwrap();
    assert!(
        path.contains("stall-vt-"),
        "the dump file is named after the service: {path}"
    );
    assert_eq!(dumps.lock().unwrap().len(), 1);
    let resumed = named("sidecar.vt.runtime_resumed");
    assert_eq!(resumed.len(), 1, "the end of the stall is recorded");
    assert!(resumed[0]["fields"]["stalled_ms"].as_u64().unwrap() >= 1000);
}
