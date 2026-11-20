// Runs the shared cases of the process list (`tests/fixtures/process-list-jobs.json`, the copy of core
// `docs/spec/cases/process-list/jobs.json`) through the rule of the entries: each case gives the session leader, the
// foreground group and the members of the terminal with their group and state, and the foreground and background
// entries that the rule names (core docs/spec/process-list.md#background-jobs).

use serde::Deserialize;
use soksak_sidecar_vt_core::process_list::{Entry, JobState, Member, entries};

const CASES: &str = include_str!("fixtures/process-list-jobs.json");

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    leader: i32,
    foreground: i32,
    members: Vec<Member>,
    expected: Expected,
}

#[derive(Deserialize)]
struct Expected {
    foreground: Option<Named>,
    background: Vec<Named>,
}

#[derive(Deserialize)]
struct Named {
    group: i32,
    pid: i32,
    state: Option<JobState>,
}

#[test]
fn the_shared_cases_name_the_foreground_program_and_the_background_jobs() {
    let cases: Cases =
        serde_json::from_str(CASES).expect("the shared cases are JSON of the case form");
    assert_eq!(cases.cases.len(), 11, "the copy holds every shared case");
    for case in cases.cases {
        let (foreground, background) = entries(case.leader, case.foreground, &case.members);
        assert_eq!(
            foreground.map(|entry| (entry.group, entry.pid)),
            case.expected
                .foreground
                .map(|named| (named.group, named.pid)),
            "foreground of the case {}",
            case.name
        );
        let expected: Vec<Entry> = case
            .expected
            .background
            .iter()
            .map(|named| Entry {
                group: named.group,
                pid: named.pid,
                state: named
                    .state
                    .expect("a background entry of a case has a state"),
            })
            .collect();
        assert_eq!(background, expected, "background of the case {}", case.name);
    }
}
