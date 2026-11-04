# Golden fixtures of the event contract

`catalog.json`, `key.jsonl` and `dropped.jsonl` are copies of the golden fixtures of the event contract (core
`docs/spec/cases/log-contract`; the contract is core `docs/spec/logging.md`). `vt-core/tests/event_contract.rs`
checks that the lines of the writer of this repository have the structure of these lines. A change of the contract
replaces the three files with the new fixtures.

`policy-cases.json` is the copy of `docs/spec/cases/log-contract/policy/cases.json`, `replay-inputs.json` the copy of
`docs/spec/cases/log-contract/replay/inputs.json`, and `screen-hash.json` the copy of
`docs/spec/cases/log-contract/replay/screen-hash.json`. `screen-message.json` is the copy of core
`docs/spec/cases/log-contract/replay/screen-message.json`, whose change sequences `vt-core/tests/screen_message_test.rs`
reproduces. Core owns all four; a change of the contract replaces the copies.
