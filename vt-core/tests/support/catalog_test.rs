// The catalog of the events of the service (events.json at the root of the repository) as the tests read it.
#![allow(dead_code)]

use serde_json::Value;

/// The catalog, read from the root of the repository.
pub fn catalog() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../events.json");
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
        if line["level"] != declared["level"] {
            findings.push(format!(
                "{name} is written at level {} and declared at {}",
                line["level"], declared["level"]
            ));
        }
        for (field, kind) in declared["fields"].as_object().expect("fields") {
            let found = type_name(&line["fields"][field]);
            if found != kind.as_str().expect("a type name") {
                findings.push(format!("{name}.{field} is {found}, declared {kind}"));
            }
        }
    }
    findings
}
