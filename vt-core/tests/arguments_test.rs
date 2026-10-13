// The command line of the service (core docs/spec/sidecars.md): `--service-dir <dir> --log-dir <dir>`. A missing
// option, a missing value, a repeated option and an unknown argument fail with an error that names the argument.
use soksak_sidecar_vt_core::arguments::{Arguments, parse};
use std::path::PathBuf;

fn parse_of(arguments: &[&str]) -> Result<Arguments, String> {
    parse(arguments.iter().map(|argument| argument.to_string()))
}

#[test]
fn both_directories_are_read_in_either_order() {
    for arguments in [
        ["--service-dir", "/s", "--log-dir", "/l"],
        ["--log-dir", "/l", "--service-dir", "/s"],
    ] {
        let parsed = parse_of(&arguments).unwrap();
        assert_eq!(parsed.service_dir, PathBuf::from("/s"));
        assert_eq!(parsed.log_dir, PathBuf::from("/l"));
    }
}

#[test]
fn a_service_without_log_dir_refuses_to_start() {
    let error = parse_of(&["--service-dir", "/s"]).unwrap_err();
    assert!(error.contains("--log-dir is required"), "{error}");
}

#[test]
fn a_service_without_service_dir_refuses_to_start() {
    let error = parse_of(&["--log-dir", "/l"]).unwrap_err();
    assert!(error.contains("--service-dir is required"), "{error}");
}

#[test]
fn an_option_without_a_value_is_rejected() {
    let error = parse_of(&["--service-dir", "/s", "--log-dir"]).unwrap_err();
    assert!(error.contains("--log-dir requires a directory"), "{error}");
    let error = parse_of(&["--service-dir", "--log-dir", "/l"]).unwrap_err();
    assert!(
        error.contains("--service-dir requires a directory"),
        "{error}"
    );
}

#[test]
fn an_unknown_or_repeated_argument_is_rejected() {
    let error = parse_of(&["--service-dir", "/s", "--log-dir", "/l", "--verbose"]).unwrap_err();
    assert!(error.contains("unknown argument \"--verbose\""), "{error}");
    let error =
        parse_of(&["--service-dir", "/s", "--log-dir", "/l", "--log-dir", "/m"]).unwrap_err();
    assert!(error.contains("--log-dir is given twice"), "{error}");
}
