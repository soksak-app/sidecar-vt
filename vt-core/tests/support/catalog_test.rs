// The catalog of the events of the service (vt-alacritty/events.json) as the tests read it.
#![allow(dead_code)]

use serde_json::Value;

/// The catalog, read from the root of the repository.
pub fn catalog() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../vt-alacritty/events.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The findings of the lines against the catalog: an event that is not declared, a required field that is missing or has
/// another type. The events of the writer (`log.*`) belong to the contract, not to this catalog.
pub fn findings(lines: &[Value]) -> Vec<String> {
    let catalog = catalog();
    let mut findings = Vec::new();
    for line in lines {
        let name = line["event"].as_str().expect("an event has a name");
        if name.starts_with("log.") {
            continue;
        }
        let Some(declared) = catalog["events"].get(name) else {
            findings.push(format!("{name} is not declared"));
            continue;
        };
        let levels: Vec<&Value> = match &declared["level"] {
            Value::Array(levels) => levels.iter().collect(),
            level => vec![level],
        };
        if !levels.contains(&&line["level"]) {
            findings.push(format!(
                "{name} is written at level {} and declared at {}",
                line["level"], declared["level"]
            ));
        }
        // A field name that ends with `?` is optional; a type such as `string|null` accepts each of its parts.
        for (field, kind) in declared["fields"].as_object().expect("fields") {
            let optional = field.ends_with('?');
            let field = field.trim_end_matches('?');
            let value = &line["fields"][field];
            if value.is_null() && optional && line["fields"].get(field).is_none() {
                continue;
            }
            let found = type_name(value);
            if !kind
                .as_str()
                .expect("a type name")
                .split('|')
                .any(|part| part == found)
            {
                findings.push(format!("{name}.{field} is {found}, declared {kind}"));
            }
        }
        // An entrance of calls states its result in one form: `ok`, `micros`, and `error` only for a failure as
        // `<code>: <text>` (core docs/spec/logging.md#result-form).
        if declared["result"] == Value::Bool(true) {
            let fields = &line["fields"];
            match (fields["ok"].as_bool(), fields.get("error")) {
                (Some(true), Some(_)) => {
                    findings.push(format!("{name}: ok is true, so error must be absent"))
                }
                (Some(false), None) => {
                    findings.push(format!("{name}: ok is false, so error is required"))
                }
                (Some(false), Some(error)) => {
                    let text = error.as_str().unwrap_or("");
                    let coded = text.split_once(": ").is_some_and(|(code, rest)| {
                        !rest.is_empty()
                            && code
                                .trim_start_matches('-')
                                .chars()
                                .all(|c| c.is_ascii_digit())
                            && !code.is_empty()
                    });
                    if !coded {
                        findings.push(format!("{name}: error {text:?} is not <code>: <text>"));
                    }
                }
                _ => {}
            }
        }
    }
    findings
}
