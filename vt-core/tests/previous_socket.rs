// The removal of the socket of a previous service reports a failed check of its endpoint file.
#![cfg(target_os = "macos")]
#[path = "support/events_test.rs"]
mod events_test;

use std::os::unix::fs::PermissionsExt;

use soksak_sidecar_vt_core::platform::darwin::service::remove_previous_socket;

// A service folder without search permission makes the check of its endpoint file fail, which is not an absence.
#[test]
fn a_failed_check_of_the_previous_endpoint_is_recorded() {
    let events = events_test::TestEvents::new();
    let folder = tempfile::tempdir().expect("create the service folder");
    let service_dir = folder.path().join("service");
    std::fs::create_dir(&service_dir).expect("create the service directory");
    std::fs::write(service_dir.join("endpoint.json"), "{}").expect("write the endpoint");
    std::fs::set_permissions(&service_dir, std::fs::Permissions::from_mode(0o000))
        .expect("remove the search permission");
    remove_previous_socket(&service_dir, &events.handle());
    std::fs::set_permissions(&service_dir, std::fs::Permissions::from_mode(0o755))
        .expect("restore the search permission");
    let lines = events.lines();
    assert!(
        lines
            .iter()
            .any(|line| line["event"] == "sidecar.vt.record.error"
                && line["fields"]["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("previous endpoint:"))),
        "the failed check was not recorded: {lines:?}"
    );
}
