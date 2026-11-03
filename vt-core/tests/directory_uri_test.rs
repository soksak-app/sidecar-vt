// The rule that converts an OSC 7 directory URI to a path on this computer.
use soksak_sidecar_vt_core::directory_uri::{host_name, local_path};

#[test]
fn a_local_file_uri_becomes_its_decoded_path() {
    let host = host_name().unwrap();
    for uri in [
        "file:///tmp/a%20b".to_string(),
        "file://localhost/tmp/a%20b".to_string(),
        format!("file://{host}/tmp/a%20b"),
    ] {
        assert_eq!(
            local_path(&uri).unwrap().as_deref(),
            Some("/tmp/a b"),
            "{uri}"
        );
    }
    assert_eq!(
        local_path("file:///%ED%95%9C%EA%B8%80").unwrap().as_deref(),
        Some("/한글")
    );
}

#[test]
fn a_directory_of_another_machine_has_no_local_path() {
    assert_eq!(
        local_path("file://another-machine.invalid/tmp").unwrap(),
        None
    );
}

#[test]
fn a_kitty_shell_cwd_uri_is_rejected() {
    let host = host_name().unwrap();
    let error = local_path(&format!("kitty-shell-cwd://{host}/tmp/a b")).unwrap_err();
    assert!(error.contains("OSC 7"), "{error}");
}

#[test]
fn invalid_directory_uris_are_rejected() {
    for uri in [
        "",
        "/tmp",
        "http://localhost/tmp",
        "file://",
        "file:///tmp/%zz",
        "file:///tmp/%ff",
        "file://localhost",
    ] {
        let error = local_path(uri).unwrap_err();
        assert!(error.contains("OSC 7"), "{uri:?}: {error}");
    }
}
