// The name rule of a message file (core docs/spec/messaging.md#message) against the shared cases.

/// Covers the shared cases of the name rule, core docs/spec/cases/messaging/names.json, copied as
/// tests/fixtures/messaging-names.json.
#[test]
fn each_shared_name_case_has_its_result() {
    use soksak_sidecar_vt_core::messaging::{NameRule, name_rule};
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/messaging-names.json"))
            .expect("the cases are JSON");
    for case in cases["cases"].as_array().expect("the cases are a list") {
        let name = case["name"].as_str().expect("a case has a name");
        let expected = match &case["result"] {
            serde_json::Value::String(result) if result == "ignored" => NameRule::Ignored,
            serde_json::Value::String(result) if result == "invalid-sender" => {
                NameRule::InvalidSender
            }
            result => NameRule::Message {
                from: result["from"]
                    .as_str()
                    .expect("a message case has from")
                    .to_string(),
            },
        };
        assert_eq!(name_rule(name), expected, "the case {name}");
    }
}
