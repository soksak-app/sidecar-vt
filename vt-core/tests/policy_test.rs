// The reader of the policy file follows the golden cases of core (docs/spec/logging.md#policy-file): the texts it accepts with
// the classes they give and the pairs of class and level they store, the texts it refuses with the error text, and the texts
// that a writer has not finished.
use serde_json::Value;
use soksak_sidecar_vt_core::events::Level;
use soksak_sidecar_vt_core::policy::{Class, Policy, is_being_written, parse_policy};

fn cases() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/log-contract/policy-cases.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn class(name: &Value) -> Class {
    Class::from_name(name.as_str().unwrap()).unwrap_or_else(|| panic!("class {name}"))
}

fn level(name: &Value) -> Level {
    Level::from_name(name.as_str().unwrap()).unwrap_or_else(|| panic!("level {name}"))
}

#[test]
fn a_valid_text_gives_the_classes_and_stores_the_pairs_of_the_golden_case() {
    let cases = cases();
    let valid = cases["valid"].as_array().unwrap();
    assert!(valid.len() >= 4);
    for case in valid {
        let name = case["name"].as_str().unwrap();
        let policy = parse_policy("log-policy.json", case["text"].as_str().unwrap())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(policy.classes(), case["classes"], "{name}");
        for pair in case["allows"].as_array().unwrap() {
            assert!(
                policy.allows(class(&pair[0]), level(&pair[1])),
                "{name}: {pair}"
            );
        }
        for pair in case["refuses"].as_array().unwrap() {
            assert!(
                !policy.allows(class(&pair[0]), level(&pair[1])),
                "{name}: {pair}"
            );
        }
    }
}

#[test]
fn a_refused_text_gets_the_error_of_the_golden_case_with_the_name_of_the_file() {
    let cases = cases();
    let invalid = cases["invalid"].as_array().unwrap();
    assert!(invalid.len() >= 15);
    for case in invalid {
        let name = case["name"].as_str().unwrap();
        let error =
            parse_policy("log-policy.json", case["text"].as_str().unwrap()).expect_err(name);
        if let Some(expected) = case["error"].as_str() {
            assert_eq!(
                error,
                expected.replace("<file>", "log-policy.json"),
                "{name}"
            );
        } else {
            let prefix = case["error_prefix"].as_str().unwrap();
            assert!(
                error.starts_with(&prefix.replace("<file>", "log-policy.json")),
                "{name}: {error}"
            );
        }
    }
}

#[test]
fn a_text_that_a_writer_has_not_finished_is_waited_on_and_a_policy_is_not() {
    let cases = cases();
    let waiting = cases["unchanged_by_watcher"].as_array().unwrap();
    assert!(!waiting.is_empty());
    for case in waiting {
        assert!(is_being_written(case["text"].as_str().unwrap()), "{case}");
    }
    for case in cases["valid"].as_array().unwrap() {
        assert!(!is_being_written(case["text"].as_str().unwrap()), "{case}");
    }
}

#[test]
fn the_default_policy_stores_every_level_of_every_class() {
    let policy = Policy::default();
    for class in Class::ALL {
        for level in [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            assert!(policy.allows(class, level), "{class:?} {level:?}");
        }
    }
    assert_eq!(
        policy.classes(),
        serde_json::json!({"lifecycle": "trace", "input": "trace", "call": "trace", "io": "trace", "frame": "trace"})
    );
}
