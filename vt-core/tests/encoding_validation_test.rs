use soksak_sidecar_vt_core::protocol::base64_decode;

#[test]
fn malformed_base64_is_rejected_without_accepting_a_prefix() {
    for invalid in [
        "=discarded",
        "aGk=discarded",
        "aGk",
        "aG==",
        "a===",
        "aGk==",
        "aGk=\n",
    ] {
        assert!(
            base64_decode(invalid).is_err(),
            "accepted malformed input {invalid:?}"
        );
    }
}

#[test]
fn valid_base64_preserves_all_input_bytes() {
    assert_eq!(base64_decode("").unwrap(), b"");
    assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
    assert_eq!(base64_decode("AP8bAA==").unwrap(), [0, 255, 27, 0]);
}
