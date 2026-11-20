// The hand-off record of a task (core docs/spec/messaging.md#hand-off-record) in a real folder.

use soksak_sidecar_vt_core::handoff::{self, Change, State};

/// The names of the files of `folder`, sorted.
fn names(folder: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .expect("the folder is readable")
        .map(|entry| {
            entry
                .expect("the entry is readable")
                .file_name()
                .into_string()
                .expect("the name is UTF-8")
        })
        .collect();
    names.sort();
    names
}

/// Covers F912-2: a result of attempt 1 after attempt 2 was assigned is refused with `stale-attempt`, which names
/// both attempts, and the record holds the current attempt only.
#[test]
fn a_result_of_an_earlier_attempt_is_refused_with_stale_attempt() {
    let dir = tempfile::tempdir().expect("temporary directory must be created");
    let folder = dir.path();
    let first = handoff::assign(folder, "build").expect("the first assignment is written");
    assert_eq!((first.attempt, first.state), (1, State::Assigned));
    handoff::update(folder, "build", 1, Change::Started).expect("attempt 1 starts");
    handoff::update(folder, "build", 1, Change::Abandoned).expect("attempt 1 is abandoned");
    let second = handoff::assign(folder, "build").expect("the second assignment is written");
    assert_eq!((second.attempt, second.state), (2, State::Assigned));

    let refused = handoff::update(folder, "build", 1, Change::Result("old".to_string()))
        .expect_err("a result of attempt 1 is refused");
    assert_eq!(refused.code, "stale-attempt", "{refused:?}");
    assert!(
        refused.reason.contains("attempt 1") && refused.reason.contains("attempt 2"),
        "{refused:?}"
    );

    let record = handoff::read(folder, "build")
        .expect("the record is read")
        .expect("the record exists");
    assert_eq!(record, second, "the refusal changed the record");
    let stored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join("build.handoff.json")).expect("the file is read"),
    )
    .expect("the file is JSON");
    assert_eq!(
        stored,
        serde_json::json!({"task": "build", "attempt": 2, "state": "assigned", "result": null})
    );
    assert_eq!(
        names(folder),
        ["build.handoff.json"],
        "a temporary file remained"
    );
}

/// Covers the transitions of core docs/spec/messaging.md#hand-off-record: each listed change is written, each other
/// change and an assignment of an attempt that has not ended are refused with `invalid-transition`.
#[test]
fn the_record_follows_the_listed_changes_and_refuses_the_others() {
    let dir = tempfile::tempdir().expect("temporary directory must be created");
    let folder = dir.path();
    handoff::assign(folder, "build").expect("the assignment is written");
    let early = handoff::update(folder, "build", 1, Change::Integrated)
        .expect_err("assigned does not change to integrated");
    assert_eq!(early.code, "invalid-transition", "{early:?}");
    handoff::update(folder, "build", 1, Change::Started).expect("attempt 1 starts");
    let busy =
        handoff::assign(folder, "build").expect_err("a started attempt is not assigned again");
    assert_eq!(busy.code, "invalid-transition", "{busy:?}");
    let result = handoff::update(folder, "build", 1, Change::Result("done".to_string()))
        .expect("attempt 1 gives its result");
    assert_eq!(
        (result.state, result.result.as_deref()),
        (State::Result, Some("done"))
    );
    let integrated =
        handoff::update(folder, "build", 1, Change::Integrated).expect("the result is integrated");
    assert_eq!(
        (integrated.state, integrated.result.as_deref()),
        (State::Integrated, Some("done"))
    );
    let late = handoff::update(folder, "build", 1, Change::Abandoned)
        .expect_err("an integrated attempt is not abandoned");
    assert_eq!(late.code, "invalid-transition", "{late:?}");
    let again =
        handoff::assign(folder, "build").expect_err("an integrated task is not assigned again");
    assert_eq!(again.code, "invalid-transition", "{again:?}");
}

/// Covers the refusals of a name and of a missing or invalid record in core docs/spec/messaging.md#hand-off-record.
#[test]
fn an_invalid_task_a_missing_record_and_an_invalid_record_are_refused() {
    let dir = tempfile::tempdir().expect("temporary directory must be created");
    let folder = dir.path();
    for task in ["", "a.b", "a/b", "a\0b"] {
        let refused = handoff::assign(folder, task).expect_err("the name is refused");
        assert_eq!(refused.code, "invalid-task", "{task:?}: {refused:?}");
    }
    let unknown = handoff::update(folder, "none", 1, Change::Started)
        .expect_err("a task without a record is refused");
    assert_eq!(unknown.code, "unknown-task", "{unknown:?}");
    assert_eq!(
        handoff::read(folder, "none").expect("no record is no error"),
        None
    );

    let path = folder.join("bad.handoff.json");
    std::fs::write(
        &path,
        r#"{"task": "bad", "attempt": 0, "state": "assigned", "result": null}"#,
    )
    .expect("the file is written");
    let invalid = handoff::read(folder, "bad").expect_err("an attempt of 0 is refused");
    assert_eq!(invalid.code, "invalid-record", "{invalid:?}");
    assert!(
        invalid
            .reason
            .contains(path.to_str().expect("path is UTF-8")),
        "{invalid:?}"
    );
    let kept = handoff::assign(folder, "bad").expect_err("an invalid record is not replaced");
    assert_eq!(kept.code, "invalid-record", "{kept:?}");
}
